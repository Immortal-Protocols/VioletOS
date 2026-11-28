//! Host tests for the Sentinel (`make test-unit`), a few seconds, no shared
//! state: each test thread has its own Sentinel (hostlink.rs).
//!
//! Built from the shipped sources: the Open Runtime's enforcement stub
//! (runtime/open/policy.rs with system/sentinel/client.rs), the wire format
//! (common.rs), the Sentinel's engine, decisions, sealing and scorer turn
//! (engine.rs, decide.rs, seal.rs, scoring.rs) and the Shield's scorer
//! (runtime/open/scorer.rs). Only the kernel edge is replaced: the two shared
//! windows and the IPC call are memory here.
#![allow(dead_code)]
extern crate alloc;

#[path = "../../../runtime/open/policy.rs"]
mod policy;
#[path = "../../../runtime/open/shield.rs"]
mod shield;
#[path = "../../../runtime/open/scorer_model.rs"]
mod scorer_model;
#[path = "../../../runtime/open/scorer.rs"]
mod scorer;
#[path = "../engine.rs"]
mod engine;
#[path = "../scoring.rs"]
mod scoring;

use engine::common as wire;
use engine::decide;
use engine::{Engine, NoExtension};
use policy::port::{self, Fault};
use policy::{Cap, Decision, Origin};

const KEY: [u8; 32] = [7; 32];
const INSTALLED: &str = "caps: net mic camera audio-out hci wifi-admin system\ndata: networks\n";
const SETTINGS: &str = "caps: wifi-admin system audio-out\ndata: networks relay\ntmp: .ota-chunk\n";
const CAPS: [Cap; 7] = [Cap::Net, Cap::AudioOut, Cap::Mic, Cap::Camera, Cap::Hci, Cap::WifiAdmin, Cap::System];
const ORIGINS: [Origin; 3] = [Origin::Image, Origin::Installed, Origin::Other];
const PATHS: [&str; 22] = [
    "/", "/apps", "/apps/notes/main.py", "/apps/.agent-install", "/bin/python3", "/lib/libc.so", "/etc/hosts", "/data", "/data/notes", "/data/notes/a.txt",
    "/data/notes.db", "/data/other/secret", "/data/.grants", "/data/.settings", "/data/networks", "/data/relay", "/tmp", "/tmp/x", "/tmp/.ota-chunk", "/tmp/.locomo.1",
    "/dev/violet/mic", "/proc/self/maps",
];

fn frame(seq: u32, op: u8, a: u8, b: u8, fields: &[&[u8]]) -> Vec<u8> {
    let mut window = vec![0u8; wire::WINDOW];
    let len = wire::encode(&mut window, seq, op, a, b, fields).expect("fits");
    window.truncate(len);
    window
}
/// Serve one well-formed request directly (no stub).
fn serve(e: &mut Engine, seq: u32, op: u8, a: u8, b: u8, fields: &[&[u8]]) -> ([u32; 6], Vec<u8>) {
    let f = frame(seq, op, a, b, fields);
    let mut out = Vec::new();
    let words = e.serve(seq, wire::crc32(&f), &f, &mut out, &mut NoExtension);
    (words, out)
}

// ---- Sealing ----

#[test]
fn hmac_matches_rfc_4231() {
    // Test case 2 padded to a 32-byte key is not in the RFC; case 1's key is
    // 20 bytes of 0x0b, which HMAC pads with zeros exactly as a 32-byte key
    // ending in zeros.
    let mut key = [0u8; 32];
    key[..20].fill(0x0b);
    let mac = engine::seal::hmac(&key, &[b"Hi ", b"There"]);
    let hex: String = mac.iter().map(|b| format!("{:02x}", b)).collect();
    assert_eq!(hex, "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
}

#[test]
fn sealed_text_opens_only_with_its_key_label_and_every_bit() {
    let s = engine::seal::Sealer::new(KEY);
    for plain in [&b""[..], b"x", b"enforce", &[0xa5u8; 31], &[1u8; 32], &[2u8; 33], &[3u8; 700]] {
        let text = s.seal("shield-mode", plain);
        assert!(text.starts_with("vss1.") && text.is_ascii());
        assert_eq!(s.open("shield-mode", &text).as_deref(), Some(plain));
        // Storage hands text back with or without a final newline.
        assert_eq!(s.open("shield-mode", &(text.clone() + "\n")).as_deref(), Some(plain));
        assert_eq!(s.open("grants", &text), None, "another label");
        assert_eq!(engine::seal::Sealer::new([8; 32]).open("shield-mode", &text), None, "another key");
        // The plaintext is not in the text (for anything long enough to tell).
        if plain.len() >= 7 {
            let hex: String = plain.iter().map(|b| format!("{:02x}", b)).collect();
            assert!(!text.contains(&hex));
        }
        // Any one character changed, dropped or added: refused.
        let bytes = text.as_bytes();
        for at in 5..bytes.len() {
            let mut t = bytes.to_vec();
            t[at] = if t[at] == b'0' { b'1' } else { b'0' };
            assert_eq!(s.open("shield-mode", std::str::from_utf8(&t).unwrap()), None, "changed at {}", at);
        }
        assert_eq!(s.open("shield-mode", &text[..text.len() - 2]), None);
        assert_eq!(s.open("shield-mode", &(text.clone() + "00")), None);
    }
    for junk in ["", "vss1.", "vss1.zz", "enforce", "notes\t1\t0\n", "vss1.00", &("vss1.".to_string() + &"0".repeat(96))] {
        assert_eq!(s.open("shield-mode", junk), None, "{:?}", junk);
    }
}

// ---- The engine against hostile requests ----

#[test]
fn a_request_with_one_bit_changed_is_never_answered() {
    let mut e = Engine::new(KEY);
    // Home, from the image, asks for the network: allowed when intact.
    let f = frame(5, wire::OP_ASK, Origin::Image as u8, Cap::Net as u8, &[b"home", b"caps: net\n"]);
    let crc = wire::crc32(&f);
    let mut out = Vec::new();
    assert_eq!(e.serve(5, crc, &f, &mut out, &mut NoExtension)[2], wire::V_ALLOW);
    for bit in 0..f.len() * 8 {
        let mut bad = f.clone();
        bad[bit / 8] ^= 1 << (bit % 8);
        let words = e.serve(5, crc, &bad, &mut out, &mut NoExtension);
        assert_eq!((words[2], out.len()), (wire::V_BAD, 0), "bit {}", bit);
    }
    // Truncated, extended, or announced under another sequence number.
    assert_eq!(e.serve(5, crc, &f[..f.len() - 1], &mut out, &mut NoExtension)[2], wire::V_BAD);
    let mut long = f.clone();
    long.push(0);
    assert_eq!(e.serve(5, wire::crc32(&long), &long, &mut out, &mut NoExtension)[2], wire::V_BAD);
    assert_eq!(e.serve(6, crc, &f, &mut out, &mut NoExtension)[2], wire::V_BAD);
    assert_eq!(e.serve(5, crc, &[], &mut out, &mut NoExtension)[2], wire::V_BAD);
}

#[test]
fn well_summed_nonsense_is_refused_not_obeyed() {
    let mut e = Engine::new(KEY);
    let bad = |e: &mut Engine, op: u8, a: u8, b: u8, fields: &[&[u8]]| {
        let (words, out) = serve(e, 1, op, a, b, fields);
        assert_eq!((words[0], words[1], words[2], out.len()), (wire::REPLY_MAGIC, 1, wire::V_BAD, 0), "op {} a {} b {} {:?}", op, a, b, fields);
    };
    // No such origin, no such capability (3 is a hole in the numbering), wrong field counts.
    bad(&mut e, wire::OP_ASK, 3, Cap::Net as u8, &[b"home", b"caps: net"]);
    bad(&mut e, wire::OP_ASK, 0, 3, &[b"home", b"caps: net"]);
    bad(&mut e, wire::OP_ASK, 0, 8, &[b"home", b"caps: net"]);
    bad(&mut e, wire::OP_ASK, 0, 0, &[b"home"]);
    bad(&mut e, wire::OP_ASK, 0, 0, &[b"home", b"caps: net", b"extra"]);
    // Text that is not text; an owner or manifest longer than any real one.
    bad(&mut e, wire::OP_ASK, 0, 0, &[b"ho\xffme", b"caps: net"]);
    bad(&mut e, wire::OP_ASK, 0, 0, &[b"home", b"caps: \xc3"]);
    bad(&mut e, wire::OP_ASK, 0, 0, &[&[b'a'; wire::OWNER_MAX + 1], b"caps: net"]);
    bad(&mut e, wire::OP_ASK, 0, 0, &[b"home", &[b' '; wire::MANIFEST_MAX + 1]]);
    bad(&mut e, wire::OP_PATH, 0, 2, &[b"home", b"", b"/tmp/x"]);
    bad(&mut e, wire::OP_PATH, 0, 0, &[b"home", b""]);
    bad(&mut e, wire::OP_PATH, 0, 0, &[b"home", b"", &[b'/'; wire::PATH_MAX + 1]]);
    bad(&mut e, wire::OP_EXEC, 2, 0, &[b"notes", b"/apps/other/x"]);
    bad(&mut e, wire::OP_SYS, 0, 0, &[]);
    bad(&mut e, wire::OP_RECORD, 3, 1, &[b"notes"]);
    bad(&mut e, wire::OP_RECORD, 0, 2, &[b"notes"]);
    bad(&mut e, wire::OP_EXPORT, 0, 0, &[b"x"]);
    // Operations that do not exist, and the scorer's where there is none.
    for op in [0u8, 13, 14, 200, 255, wire::OP_SCORE] {
        bad(&mut e, op, 0, 0, &[b"home", b"caps: net"]);
    }
    // The runtime cannot have a consent store made up, or read one, through
    // the state-sealing operations.
    bad(&mut e, wire::OP_SEAL, 0, 0, &[b"grants", b"g 9\nnotes\t1\t0\n"]);
    bad(&mut e, wire::OP_SEAL, 0, 0, &[b"anything", b"x"]);
    bad(&mut e, wire::OP_SEAL, 0, 0, &[b"shield-mode", &[0u8; wire::SEAL_MAX + 1]]);
    let (_, sealed) = serve(&mut e, 1, wire::OP_EXPORT, 0, 0, &[]);
    bad(&mut e, wire::OP_UNSEAL, 0, 0, &[b"grants", &sealed]);
    // A frame whose lengths do not add up, with a CRC that does.
    let mut f = frame(1, wire::OP_DEVICE, 0, 0, &[b"/dev/violet/mic"]);
    f[12] += 1;
    let mut out = Vec::new();
    assert_eq!(e.serve(1, wire::crc32(&f), &f, &mut out, &mut NoExtension)[2], wire::V_BAD);
    let mut f = frame(1, wire::OP_DEVICE, 0, 0, &[b"/dev/violet/mic"]);
    f[14] = 1; // a length for a field the count does not include
    f.push(b'x');
    assert_eq!(e.serve(1, wire::crc32(&f), &f, &mut out, &mut NoExtension)[2], wire::V_BAD);
}

#[test]
fn random_frames_never_panic_and_never_change_the_answers_held() {
    let mut e = Engine::new(KEY);
    serve(&mut e, 1, wire::OP_RECORD, Cap::Net as u8, 0, &[b"notes"]);
    let held = e.grants().clone();
    let mut scoring = scoring::Scoring::new();
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut out = Vec::new();
    let mut allowed = 0;
    for round in 0..20_000u32 {
        let len = (next() % 300) as usize;
        let mut f: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        // Half of them carry the magic and a plausible header, so decoding goes deep.
        if round % 2 == 0 && f.len() >= wire::HEAD {
            f[..4].copy_from_slice(&wire::REQ_MAGIC.to_le_bytes());
            f[4..8].copy_from_slice(&round.to_le_bytes());
            f[8] = (next() % 14) as u8;
            f[11] = (next() % 5) as u8;
            // Never an answer or a forget: those are allowed to change the store.
            if matches!(f[8], wire::OP_RECORD | wire::OP_FORGET) {
                f[8] = wire::OP_PATH;
            }
        }
        let words = e.serve(round, wire::crc32(&f), &f, &mut out, &mut scoring);
        assert_eq!((words[0], words[1]), (wire::REPLY_MAGIC, round));
        assert!(matches!(words[2], wire::V_ALLOW | wire::V_DENY | wire::V_ASK | wire::V_BAD));
        allowed += (words[2] == wire::V_ALLOW) as u32;
        assert!(out.len() <= wire::WINDOW && wire::crc32(&out) == words[5]);
    }
    assert_eq!(e.grants(), &held);
    // Random bytes are almost never a request at all.
    assert!(allowed < 2_000, "{}", allowed);
}

// ---- The stub and the engine, joined ----

/// What the stub answers is what the decisions say, for every origin,
/// capability and path: the wire adds and loses nothing.
#[test]
fn the_stub_answers_exactly_what_the_sentinel_decides() {
    port::restart();
    let mut oracle = decide::Grants::default();
    let mut grants = policy::Grants::default();
    for (owner, cap, allow) in [("notes", Cap::Net, true), ("notes", Cap::Mic, false), ("maps", Cap::Camera, true)] {
        oracle.record(owner, decide::Cap::from_wire(cap as u8).unwrap(), allow);
        grants.record(owner, cap, allow);
    }
    assert_eq!(grants.rows.len(), 2);
    let d = |c: decide::Decision| match c {
        decide::Decision::Allow => Decision::Allow,
        decide::Decision::Deny => Decision::Deny,
        decide::Decision::Ask => Decision::Ask,
    };
    let o = |o: Origin| decide::Origin::from_wire(o as u8).unwrap();
    let c = |c: Cap| decide::Cap::from_wire(c as u8).unwrap();
    let mut seen = [0u32; 3];
    for manifest in ["", INSTALLED, SETTINGS, "caps: net\n", "caps: mic camera\n# caps: system\n"] {
        for origin in ORIGINS {
            let (stub, real) = (policy::Profile::parse(manifest, origin), decide::Profile::parse(manifest, o(origin)));
            for owner in ["notes", "maps", "settings", "home", "~1a2b3c4d", ""] {
                for cap in CAPS {
                    let got = policy::ask(&stub, &grants, owner, cap);
                    assert_eq!(got, d(decide::ask(&real, &oracle, owner, c(cap))), "{:?} {:?} {} {:?}", manifest, origin, owner, cap);
                    seen[got as usize] += 1;
                }
                for path in PATHS {
                    for write in [false, true] {
                        assert_eq!(policy::path_rule(owner, &stub, path, write), decide::path_rule(owner, &real, path, write), "{:?} {:?} {} {} {}", manifest, origin, owner, path, write);
                    }
                }
            }
        }
    }
    // The matrix reaches every answer.
    assert!(seen.iter().all(|n| *n > 0), "{:?}", seen);
    for path in ["/dev/violet/screen", "/dev/violet/events", "/dev/violet/sys", "/dev/violet/audio", "/dev/violet/mic", "/dev/violet/camera", "/dev/violet/hci", "/dev/violet/nfc", "/dev/violet/", "/dev/null", "/data/x"] {
        assert_eq!(policy::device_cap(path).map(|c| c.map(|c| c as u8)), decide::device_cap(path).map(|c| c.map(|c| c as u8)), "{}", path);
    }
    let long = format!("log {}", "x".repeat(5000));
    let accented = format!("{} join", "\u{e9}".repeat(40));
    for line in [
        "open /apps/notes", "launch notes", "install x", "uninstall x", "shield-agent push abc", "shield-mode strict", "shield-release notes", "update check", "wifi join home", "wifi leave",
        "wifi scan", "wifi", "relay on", "relay", "mic on", "sound 440", "volume 5", "home", "status", "get city", "set city Paris", "time", "wakelock on", "", "   ", "  wifi   join  x ",
        long.as_str(), accented.as_str(),
    ] {
        assert_eq!(policy::sys_cap(line).map(|c| c as u8), decide::sys_cap(line).map(|c| c as u8), "{:?}", line);
    }
    for (caller, core, path) in [("notes", false, "/apps/maps/main"), ("notes", false, "/apps/notes/helper"), ("notes", false, "/bin/python3"), ("home", true, "/apps/maps/main"), ("", false, "/apps/x")] {
        assert_eq!(policy::exec_crosses(caller, core, path), decide::exec_crosses(caller, core, path), "{} {} {}", caller, core, path);
    }
}

/// The edge broken every way hostlink.rs knows: every question is refused,
/// and none of the refusals is the engine's doing where the request never
/// arrived intact.
#[test]
fn a_broken_or_silent_sentinel_grants_nothing() {
    let image = policy::Profile::parse(SETTINGS, Origin::Image);
    let grants = policy::Grants::default();
    let all_refused = || {
        for cap in CAPS {
            assert_eq!(policy::ask(&image, &grants, "settings", cap), Decision::Deny, "{:?}", cap);
        }
        for path in PATHS {
            for write in [false, true] {
                assert_eq!(policy::path_rule("settings", &image, path, write), Err(policy::EACCES), "{} {}", path, write);
            }
        }
        assert_eq!(policy::device_cap("/dev/violet/screen"), Err(()));
        assert_eq!(policy::device_cap("/dev/violet/mic"), Err(()));
        // A command nobody needs a capability for needs `system` now, and
        // the ask for `system` is refused too (above).
        assert_eq!(policy::sys_cap("status"), Some(Cap::System));
        assert_eq!(policy::sys_cap("wifi join x"), Some(Cap::System));
        // An execve is taken to cross apps: minimal profile, caller's name.
        assert!(policy::exec_crosses("notes", false, "/apps/notes/helper"));
        assert!(policy::exec_crosses("home", true, "/apps/maps/main"));
        assert_eq!(policy::seal_state("shield-mode", b"monitor"), None);
        assert_eq!(policy::unseal_state("shield-mode", b"vss1.00"), None);
    };
    for fault in [
        Fault::Unreachable, Fault::CorruptRequest, Fault::WrongLength, Fault::ZeroReply, Fault::OnesReply, Fault::StaleReply, Fault::BogusVerdict, Fault::HugeOutput,
    ] {
        port::restart();
        // Intact, Settings from the image may do all of this.
        assert_eq!(policy::ask(&image, &grants, "settings", Cap::System), Decision::Allow);
        assert_eq!(policy::path_rule("settings", &image, "/data/networks", true), Ok(()));
        assert_eq!(policy::device_cap("/dev/violet/screen"), Ok(None));
        assert_eq!(policy::sys_cap("status"), None);
        assert!(!policy::exec_crosses("home", true, "/apps/maps/main"));
        port::set_fault(fault);
        all_refused();
    }
    // Output the Sentinel did not sum: what carries output is refused.
    port::restart();
    let sealed = policy::seal_state("shield-mode", b"strict").unwrap();
    port::set_fault(Fault::CorruptOutput);
    assert_eq!(policy::unseal_state("shield-mode", sealed.as_bytes()), None);
    assert_eq!(policy::seal_state("shield-mode", b"strict"), None);
    let mut g = policy::Grants::default();
    g.record("notes", Cap::Net, true);
    assert_eq!((g.rows.len(), g.format()), (0, String::new()));
    port::set_fault(Fault::None);
    assert_eq!(policy::unseal_state("shield-mode", sealed.as_bytes()).as_deref(), Some(&b"strict"[..]));
}

/// A manifest longer than the Sentinel reads is the minimal profile, not a
/// refusal of everything; and a request too big to send is refused without
/// asking (so without the once-a-boot "no valid answer from the Sentinel" line).
#[test]
fn a_long_manifest_is_the_minimal_profile_and_a_request_too_big_is_never_sent() {
    port::restart();
    let p = policy::Profile::parse("caps: net\ndata: x\n", Origin::Installed);
    let g = policy::Grants::default();
    assert_eq!(policy::ask(&p, &g, "notes", Cap::Net), Decision::Ask);
    let at_limit = format!("caps: net\n{}", "#".repeat(wire::MANIFEST_MAX - 10));
    assert_eq!(policy::ask(&policy::Profile::parse(&at_limit, Origin::Installed), &g, "notes", Cap::Net), Decision::Ask);
    // Over the limit: the minimal profile. Its own /data still opens.
    let huge = policy::Profile::parse(&"caps: net\n".repeat(900), Origin::Installed);
    assert_eq!(huge, policy::Profile::minimal(Origin::Installed));
    assert_eq!(policy::ask(&huge, &g, "notes", Cap::Net), Decision::Deny);
    assert_eq!(policy::path_rule("notes", &huge, "/data/notes", true), Ok(()));
    // Longer than any request may be: refused here, nothing reaches the Sentinel.
    let before = port::served();
    assert_eq!(policy::path_rule("notes", &p, &format!("/tmp/{}", "x".repeat(wire::PATH_MAX)), false), Err(policy::EACCES));
    assert_eq!(policy::seal_state("shield-mode", &[b'x'; wire::WINDOW]), None);
    assert_eq!(policy::unseal_state("shield-mode", &[b'0'; wire::WINDOW]), None);
    assert_eq!(port::served(), before);
}

/// The Sentinel changes its store before it replies. A lost reply must not
/// leave /data/.grants saying something else than what is enforced, and a
/// store that did not load is never overwritten with nothing.
#[test]
fn what_is_saved_is_what_the_sentinel_holds_even_when_a_reply_is_lost() {
    port::restart();
    let app = policy::Profile::parse("caps: net mic\n", Origin::Installed);
    let mut g = policy::Grants::default();
    g.record("notes", Cap::Net, true);
    let before = g.format();
    for fault in [Fault::CorruptOutput, Fault::StaleReply, Fault::HugeOutput, Fault::ZeroReply] {
        port::set_fault_once(fault);
        g.record("notes", Cap::Mic, false);
        assert_eq!(policy::ask(&app, &g, "notes", Cap::Mic), Decision::Deny, "{:?}", fault);
        assert_eq!(g.format().as_bytes(), &port::exported()[..], "{:?}: saved is not enforced", fault);
        assert_ne!(g.format(), before);
    }
    // A restart takes both answers back from what was saved.
    let saved = g.format();
    port::restart();
    let back = policy::Grants::parse(&saved);
    assert_eq!(back.rows.len(), 1);
    assert_eq!(policy::ask(&app, &back, "notes", Cap::Net), Decision::Allow);
    assert_eq!(policy::ask(&app, &back, "notes", Cap::Mic), Decision::Deny);
    // The load's reply is lost: the text read stays the text to save, and a
    // forget nobody answers leaves it so.
    port::restart();
    port::set_fault_once(Fault::Unreachable);
    let mut lost = policy::Grants::parse(&saved);
    assert_eq!((lost.rows.len(), lost.format()), (0, saved.clone()));
    port::set_fault(Fault::Unreachable);
    lost.forget("notes");
    lost.record("notes", Cap::Net, false);
    assert_eq!(lost.format(), saved);
    port::set_fault(Fault::None);
}

/// No sealing key (QEMU with no valid root key blob): answers hold for this
/// start, nothing is sealed, opened or loaded, and nothing empty is saved.
#[test]
fn a_sentinel_with_no_key_seals_nothing() {
    port::restart();
    let mut g = policy::Grants::default();
    g.record("notes", Cap::Net, true);
    let sealed = g.format();
    let mode = policy::seal_state("shield-mode", b"strict").unwrap();
    port::restart_unkeyed();
    let app = policy::Profile::parse("caps: net\n", Origin::Installed);
    let mut h = policy::Grants::parse(&sealed);
    assert_eq!(h.rows.len(), 0);
    assert_eq!(policy::ask(&app, &h, "notes", Cap::Net), Decision::Ask);
    h.record("notes", Cap::Net, true);
    assert_eq!(policy::ask(&app, &h, "notes", Cap::Net), Decision::Allow);
    assert_eq!(h.format(), sealed, "the text read stays; nothing new and nothing empty");
    assert_eq!(policy::seal_state("shield-mode", b"strict"), None);
    assert_eq!(policy::unseal_state("shield-mode", mode.as_bytes()), None);
    port::restart();
}

/// The sealing key comes from the device root key blob (tools/root_key.py),
/// so a store sealed on one phone does not open on another.
#[test]
fn the_sealing_key_is_the_phones() {
    use engine::seal;
    // tools/root_key.py blob(bytes(range(32))), and Python's
    // hmac.new(key, b"violet sentinel seal v1", sha256).
    let blob = unhex("56524b3101000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1fc2cdb6c60ad47d77ebb28dccaabdcb15e6e286cce6506275");
    let key: [u8; 32] = core::array::from_fn(|i| i as u8);
    assert_eq!(seal::root_key(&blob), Some(key));
    let mut page = blob.clone();
    page.resize(4096, 0);
    assert_eq!(seal::root_key(&page), Some(key));
    assert_eq!(hexs(&seal::master(&key)), "71c773015ebb637816d78ccc0359a1da1e245a43ba5b561aecf980c197bcaf5a");
    for bit in 0..64 * 8 {
        let mut bad = blob.clone();
        bad[bit / 8] ^= 1 << (bit % 8);
        assert_eq!(seal::root_key(&bad), None, "bit {}", bit);
    }
    assert_eq!(seal::root_key(&[0; 4096]), None, "an empty page");
    assert_eq!(seal::root_key(&blob[..63]), None);
    let mut zero = blob[..40].to_vec();
    zero[8..40].fill(0);
    let mut h = seal::sha256::Sha256::new();
    h.update(&zero);
    zero.extend_from_slice(&h.finish()[..24]);
    assert_eq!(seal::root_key(&zero), None, "an all-zero key");

    let mut other = key;
    other[0] ^= 1;
    let (mut mine, mut theirs) = (Engine::new(seal::master(&key)), Engine::new(seal::master(&other)));
    serve(&mut theirs, 1, wire::OP_RECORD, Cap::Mic as u8, 1, &[b"notes"]);
    let store = serve(&mut theirs, 2, wire::OP_EXPORT, 0, 0, &[]).1;
    assert_eq!(serve(&mut mine, 3, wire::OP_LOAD, 0, 0, &[&store]).0[2], wire::V_DENY, "another phone's store");
    let mode = serve(&mut theirs, 4, wire::OP_SEAL, 0, 0, &[b"shield-mode", b"monitor"]).1;
    assert_eq!(serve(&mut mine, 5, wire::OP_UNSEAL, 0, 0, &[b"shield-mode", &mode]).0[2], wire::V_DENY, "another phone's mode");
    let mut again = Engine::new(seal::master(&other));
    assert_eq!(serve(&mut again, 6, wire::OP_LOAD, 0, 0, &[&store]).0[2], wire::V_ALLOW, "the same phone after a restart");
    // Unkeyed: nothing.
    let mut none = Engine::unkeyed();
    for (op, fields) in [(wire::OP_LOAD, vec![&store[..]]), (wire::OP_EXPORT, vec![]), (wire::OP_SEAL, vec![&b"shield-mode"[..], b"x"]), (wire::OP_UNSEAL, vec![&b"shield-mode"[..], &mode[..]])] {
        let (words, out) = serve(&mut none, 7, op, 0, 0, &fields);
        assert_eq!((words[2], out.len()), (wire::V_DENY, 0), "op {}", op);
    }
    let (words, out) = serve(&mut none, 8, wire::OP_RECORD, Cap::Net as u8, 1, &[b"notes"]);
    assert_eq!((words[2], out.len()), (wire::V_DENY, 0));
    assert_eq!(none.grants().answer("notes", decide::Cap::Net), Some(true), "held for this start");
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
fn hexs(b: &[u8]) -> String {
    b.iter().map(|b| format!("{:02x}", b)).collect()
}

/// The consent store: kept by the Sentinel, saved by the runtime as sealed
/// text, taken back only intact, never rolled back within one start.
#[test]
fn consent_answers_survive_a_restart_only_sealed_and_intact() {
    port::restart();
    let app = policy::Profile::parse("caps: net mic camera\n", Origin::Installed);
    let mut g = policy::Grants::default();
    assert_eq!(policy::ask(&app, &g, "notes", Cap::Net), Decision::Ask);
    g.record("notes", Cap::Net, true);
    let first = g.format();
    g.record("notes", Cap::Mic, false);
    let saved = g.format();
    assert_eq!(policy::ask(&app, &g, "notes", Cap::Net), Decision::Allow);
    assert_eq!(policy::ask(&app, &g, "notes", Cap::Mic), Decision::Deny);
    assert_eq!(policy::ask(&app, &g, "notes", Cap::Camera), Decision::Ask);
    // What the runtime holds and writes to /data/.grants says nothing readable.
    assert!(saved.starts_with("vss1.") && saved.is_ascii() && !saved.contains("notes") && !saved.contains('\t'));
    assert_ne!(first, saved);
    // The answers are the Sentinel's: a privileged or undeclared power is
    // never among them, whatever is recorded.
    g.record("notes", Cap::System, true);
    g.record("notes", Cap::WifiAdmin, true);
    let admin = policy::Profile::parse("caps: system wifi-admin net\n", Origin::Installed);
    assert_eq!(policy::ask(&admin, &g, "notes", Cap::System), Decision::Deny);
    assert_eq!(policy::ask(&admin, &g, "notes", Cap::WifiAdmin), Decision::Deny);
    let saved = g.format();

    // An older store (the user's "no" to the microphone not yet in it) is
    // refused while the Sentinel that sealed the newer one runs.
    let stale = policy::Grants::parse(&first);
    assert_eq!(stale.rows.len(), 0);
    assert_eq!(policy::ask(&app, &g, "notes", Cap::Mic), Decision::Deny);

    // The Sentinel starts again (a fault rebuilds the system): it holds
    // nothing, so nothing is allowed until the store is handed back.
    port::restart();
    assert_eq!(policy::ask(&app, &policy::Grants::default(), "notes", Cap::Net), Decision::Ask);
    // Plain text as wave 2 wrote it, an edited store, another key's: not loaded.
    for forged in ["notes\t5\t0\n", "g 99\nnotes\t5\t0\n", "", &saved[..saved.len() - 1], &saved.replacen("vss1.0", "vss1.1", 1).replacen("vss1.a", "vss1.b", 1)] {
        if forged == saved {
            continue;
        }
        let loaded = policy::Grants::parse(forged);
        // Not loaded; the text stays what is saved until the Sentinel hands back a store.
        assert_eq!((loaded.rows.len(), loaded.format()), (0, String::from(forged)), "{:?}", forged);
        assert_eq!(policy::ask(&app, &loaded, "notes", Cap::Net), Decision::Ask);
    }
    let other = {
        let mut e = Engine::new([9; 32]);
        serve(&mut e, 1, wire::OP_RECORD, Cap::Mic as u8, 1, &[b"notes"]);
        String::from_utf8(serve(&mut e, 2, wire::OP_EXPORT, 0, 0, &[]).1).unwrap()
    };
    assert_eq!(policy::Grants::parse(&other).rows.len(), 0);
    assert_eq!(policy::ask(&app, &g, "notes", Cap::Mic), Decision::Ask);
    // The store it sealed: loaded, with both answers.
    let mut loaded = policy::Grants::parse(&saved);
    assert_eq!((loaded.rows.len(), loaded.format()), (1, saved.clone()));
    assert_eq!(policy::ask(&app, &loaded, "notes", Cap::Net), Decision::Allow);
    assert_eq!(policy::ask(&app, &loaded, "notes", Cap::Mic), Decision::Deny);
    // Installing over the name forgets them.
    loaded.forget("notes");
    assert_eq!(loaded.rows.len(), 0);
    assert_eq!(policy::ask(&app, &loaded, "notes", Cap::Net), Decision::Ask);
    assert_ne!(loaded.format(), saved);
}

#[test]
fn the_shields_saved_mode_is_sealed() {
    port::restart();
    let sealed = policy::seal_state("shield-mode", b"strict").unwrap();
    assert!(sealed.starts_with("vss1.") && !sealed.contains("strict"));
    assert_eq!(policy::unseal_state("shield-mode", sealed.as_bytes()).as_deref(), Some(&b"strict"[..]));
    // What wave 2 saved (the plain word), an edit, a label that is not state.
    assert_eq!(policy::unseal_state("shield-mode", b"strict"), None);
    assert_eq!(policy::unseal_state("shield-mode", sealed.replace("vss1.", "vss1.00").as_bytes()), None);
    assert_eq!(policy::unseal_state("grants", sealed.as_bytes()), None);
    assert_eq!(policy::seal_state("grants", b"g 1\nnotes\t1\t0\n"), None);
    // A consent store does not open as state.
    let mut g = policy::Grants::default();
    g.record("notes", Cap::Net, true);
    assert_eq!(policy::unseal_state("shield-mode", g.format().as_bytes()), None);
}

// ---- The scorer's turn ----

use shield::{EventKind, Mode, ShieldPolicy, Subject, TelemetryEvent};

fn added(pid: u32, name: &str) -> Subject {
    Subject::derive(pid, &format!("/apps/{}", name), name, [pid as u8; 4], false, false)
}
fn ev(pid: u32, now_ns: u64, kind: EventKind, flags: u8, detail: &str, port: u16, aux: u32) -> TelemetryEvent {
    let mut e = TelemetryEvent::new(now_ns, policy::AppId(pid), kind, flags, detail.as_bytes());
    e.port = port;
    e.aux_u32 = aux;
    e
}
/// A program the user added reads another app's file and sends a megabyte to
/// an address on an odd port (scorer_replay.rs `exfil`).
fn exfil(pid: u32, now: u64) -> Vec<TelemetryEvent> {
    vec![
        ev(pid, now, EventKind::Exec, 4, "/apps/photosync", 0, 0),
        ev(pid, now, EventKind::OpenRead, 4 | 2, "/data/notes/keys.txt", 0, 0),
        ev(pid, now, EventKind::NetConnect, 4 | 1, "185.220.101.4", 4444, 0),
        ev(pid, now, EventKind::NetTxSummary, 4 | 1, "185.220.101.4", 4444, 1 << 20),
    ]
}

struct Turn {
    scored: u32,
    warned: u32,
    model: u32,
    pending: bool,
    locals: Vec<scorer::Local>,
    events: Vec<TelemetryEvent>,
    /// The output as it came, for the runtime's decoding (common.rs `score_results`).
    raw: Vec<u8>,
}
/// One OP_SCORE request as guard.rs sends it, through the real wire.
fn turn(e: &mut Engine, s: &mut scoring::Scoring, seq: u32, mode: Mode, more: bool, idle: u32, now: u64, subjects: &[Subject], events: &[TelemetryEvent]) -> Option<Turn> {
    let head = wire::score_request_head(mode as u8, more as u8, idle, now);
    let subjects: Vec<u8> = subjects.iter().flat_map(|s| s.to_wire()).collect();
    let events: Vec<u8> = events.iter().flat_map(|e| e.to_wire()).collect();
    let f = frame(seq, wire::OP_SCORE, 0, 0, &[&head, &subjects, &events]);
    let mut out = Vec::new();
    let words = e.serve(seq, wire::crc32(&f), &f, &mut out, s);
    if words[2] != wire::V_ALLOW {
        assert_eq!((words[2], out.len()), (wire::V_BAD, 0));
        return None;
    }
    let u32_at = |n: usize| u32::from_le_bytes(out[n..n + 4].try_into().unwrap());
    let count = out[13] as usize;
    assert_eq!(out.len(), wire::SCORE_OUT_HEAD + count * wire::SCORE_OUT_ITEM);
    assert_eq!(words[3] as usize, count);
    let mut t = Turn { scored: u32_at(0), warned: u32_at(4), model: u32_at(8), pending: out[12] != 0, locals: Vec::new(), events: Vec::new(), raw: out.clone() };
    for item in out[wire::SCORE_OUT_HEAD..].chunks(wire::SCORE_OUT_ITEM) {
        t.locals.push(scorer::Local { pid: u32::from_le_bytes(item[..4].try_into().unwrap()), action: item[4], enforced: item[5] != 0, verdict: item[6], margin: i32::from_le_bytes(item[8..12].try_into().unwrap()) });
        t.events.push(TelemetryEvent::from_wire(&item[12..]).unwrap());
    }
    Some(t)
}

#[test]
fn records_and_subjects_cross_the_wire_whole() {
    let e = ev(77, 123_456_789_000, EventKind::NetTxSummary, 0x2d, "a-host.example.test", 4444, 0xdead_beef);
    let back = TelemetryEvent::from_wire(&e.to_wire()).unwrap();
    assert_eq!(format!("{:?}", back), format!("{:?}", e));
    assert_eq!(TelemetryEvent::from_wire(&[0; 63]).map(|_| ()), None);
    let s = Subject::derive(9, "/apps/agent", "agent", [1, 2, 3, 4], true, true);
    assert_eq!(format!("{:?}", Subject::from_wire(&s.to_wire()).unwrap()), format!("{:?}", s));
    // A name longer than the name is not a subject (it would be sliced).
    let mut w = s.to_wire();
    w[10] = 25;
    assert!(Subject::from_wire(&w).is_none());
}

/// The scorer in the Sentinel, fed over the wire, reaches the same verdicts,
/// the same records and the same counts as the scorer run next to the policy
/// (what the Open Runtime did before, and what the host replay still does),
/// in every mode; and the policy, told only what the Sentinel decided, ends
/// in the same state.
#[test]
fn the_remote_scorer_decides_what_the_local_one_did() {
    let s = 1_000_000_000u64;
    for mode in [Mode::Monitor, Mode::Enforce, Mode::Strict] {
        let (mut engine, mut remote) = (Engine::new(KEY), scoring::Scoring::new());
        // The reference: scorer and policy side by side.
        let (mut local, mut reference) = (scorer::Scorer::new(), ShieldPolicy::with_mode(mode));
        // The runtime's policy, which only hears the Sentinel's results.
        let mut runtime = ShieldPolicy::with_mode(mode);
        let subjects = [added(1, "chat"), added(3, "photosync"), Subject::derive(5, "/apps/notes", "notes", [5; 4], true, true), Subject::derive(6, "/apps/home", "home", [6; 4], true, true)];
        let mut acted = 0;
        for tick in 0..40u64 {
            let now = s + tick * 100_000_000;
            let mut events = vec![ev(1, now, EventKind::OpenRead, 4, "/data/chat/log", 0, 0)];
            if tick == 2 {
                events.extend(exfil(3, now));
            }
            if tick == 5 {
                // The same from an image app, and from Home.
                events.extend(exfil(5, now).into_iter().map(|e| TelemetryEvent { flags: e.flags & !4, ..e }));
                events.extend(exfil(6, now).into_iter().map(|e| TelemetryEvent { flags: e.flags & !4, ..e }));
            }
            if tick >= 20 && tick % 4 == 0 {
                events.push(ev(3, now, EventKind::NetTxSummary, 5, "185.220.101.4", 4444, 96 * 1024));
            }
            let want = local.step(&mut reference, &events, 7, now, &subjects);
            let want_events: Vec<TelemetryEvent> = reference.ring.drain().into_iter().filter(|e| e.kind == EventKind::LocalScore as u8).collect();
            let got = turn(&mut engine, &mut remote, tick as u32, mode, false, 7, now, &subjects, &events).expect("a good request");
            assert_eq!(got.locals, want, "{:?} tick {}", mode, tick);
            assert_eq!(format!("{:?}", got.events), format!("{:?}", want_events));
            assert_eq!((got.scored, got.warned, got.model), (local.scored, local.warned, scorer_model::MODEL_ID));
            assert_eq!(got.pending, local.windows.iter().any(|w| w.pending()));
            // guard.rs `score_tick`: the results as it decodes them, recorded,
            // then contained by the policy.
            let acting: Vec<&scorer::Local> = got.locals.iter().filter(|l| (1..=scorer::FREEZE).contains(&l.action)).collect();
            let results: Vec<wire::ScoreResult> = wire::score_results(&got.raw).collect();
            assert_eq!(results.len(), acting.len());
            for (r, l) in results.iter().zip(acting) {
                assert_eq!((r.pid, r.action, r.verdict, r.margin), (l.pid, l.action, l.verdict, l.margin));
                runtime.ring.emit(TelemetryEvent::from_wire(r.record).unwrap());
                let who = subjects.iter().find(|s| s.app.0 == r.pid).unwrap();
                let contained = r.action >= wire::SCORE_QUARANTINE && runtime.trip(now, who, shield::ID_SCORER);
                assert_eq!(contained, l.enforced, "the Sentinel's word and the policy's deed");
                acted += 1;
            }
            runtime.ring.drain();
            for who in &subjects {
                assert_eq!(runtime.is_quarantined(now, who.key()), reference.is_quarantined(now, who.key()), "{:?} tick {} {}", mode, tick, who.name());
            }
            // As the 100 ms tick does.
            assert_eq!(runtime.expire_quarantines(now), reference.expire_quarantines(now));
        }
        assert!(acted >= 2, "{:?}: the stream must make the scorer act ({})", mode, acted);
        assert_eq!((runtime.quarantines, runtime.shadow_hits), (reference.quarantines, reference.shadow_hits), "{:?}", mode);
        // Enforcing contains the added program and never Home; strict also the image app.
        let q = |name: &str| runtime.guards.iter().any(|g| g.name() == name && g.level > 0);
        assert_eq!((q("photosync"), q("notes"), q("home")), (mode != Mode::Monitor, mode == Mode::Strict, false), "{:?}", mode);
    }
}

/// guard.rs's side of a turn as it ships (common.rs `score_turn`,
/// `score_head`, `score_results`): more than a window of events goes in
/// requests of a hundred, all but the last sent ahead; the results it
/// carries out are the scorer's; one unanswered request ends the turn.
#[test]
fn the_runtimes_side_of_a_turn_chunks_and_decodes_as_the_scorer_means() {
    let now = 1_000_000_000u64;
    let subjects = [added(1, "chat"), added(3, "photosync")];
    let mut events: Vec<TelemetryEvent> = (0..246).map(|n| ev(1, now, EventKind::OpenRead, 4, &format!("/data/chat/{}", n), 0, 0)).collect();
    events.extend(exfil(3, now));
    let bytes: Vec<u8> = events.iter().flat_map(|e| e.to_wire()).collect();
    // The reference: the scorer beside the policy, all events at once.
    let (mut local, mut reference) = (scorer::Scorer::new(), ShieldPolicy::with_mode(Mode::Enforce));
    let want: Vec<scorer::Local> = local.step(&mut reference, &events, 0, now, &subjects).into_iter().filter(|l| (1..=scorer::FREEZE).contains(&l.action)).collect();
    let want_records: Vec<TelemetryEvent> = reference.ring.drain().into_iter().filter(|e| e.kind == EventKind::LocalScore as u8).collect();
    assert!(!want.is_empty(), "the exfiltration must be acted on");

    let (mut engine, mut remote) = (Engine::new(KEY), scoring::Scoring::new());
    let subject_bytes: Vec<u8> = subjects.iter().flat_map(|s| s.to_wire()).collect();
    let mut sent = Vec::new();
    let mut seq = 0;
    let out = wire::score_turn(&bytes, |more, chunk| {
        sent.push((more, chunk.len() / wire::SCORE_EVENT));
        seq += 1;
        let head = wire::score_request_head(Mode::Enforce as u8, more, 0, now);
        let f = frame(seq, wire::OP_SCORE, 0, 0, &[&head, &subject_bytes, chunk]);
        let mut out = Vec::new();
        let words = engine.serve(seq, wire::crc32(&f), &f, &mut out, &mut remote);
        (words[2] == wire::V_ALLOW).then_some(out)
    })
    .expect("answered");
    assert_eq!(sent, [(1, 100), (1, 100), (0, 50)]);
    let (scored, warned, model, pending) = wire::score_head(&out).unwrap();
    assert_eq!((scored, warned, model, pending), (local.scored, local.warned, scorer_model::MODEL_ID, local.windows.iter().any(|w| w.pending())));
    let got: Vec<wire::ScoreResult> = wire::score_results(&out).collect();
    assert_eq!(got.len(), want.len());
    for ((g, w), rec) in got.iter().zip(&want).zip(&want_records) {
        assert_eq!((g.pid, g.action, g.verdict, g.margin), (w.pid, w.action, w.verdict, w.margin));
        assert_eq!(format!("{:?}", TelemetryEvent::from_wire(g.record).unwrap()), format!("{:?}", rec));
    }
    // No events and nothing pending is one request; exactly a window's worth is one.
    let mut calls = Vec::new();
    wire::score_turn(&[], |m, c| { calls.push((m, c.len())); Some(vec![0; 16]) });
    wire::score_turn(&bytes[..100 * wire::SCORE_EVENT], |m, c| { calls.push((m, c.len() / wire::SCORE_EVENT)); Some(vec![0; 16]) });
    assert_eq!(calls, [(0, 0), (0, 100)]);
    // The second request goes unanswered: no third, no results.
    let mut n = 0;
    assert!(wire::score_turn(&bytes, |_, _| { n += 1; (n == 1).then(|| vec![0; 16]) }).is_none());
    assert_eq!(n, 2);

    // Decoding: whole items only, actions warn..freeze only, each field where the Sentinel put it.
    let mut out = vec![0u8; wire::SCORE_OUT_HEAD];
    out[..4].copy_from_slice(&7u32.to_le_bytes());
    out[12] = 1;
    for (pid, action) in [(10u32, 0u8), (11, 1), (12, 4), (13, 3), (14, 2), (15, 0xff)] {
        out.extend_from_slice(&pid.to_le_bytes());
        out.extend_from_slice(&[action, 1, 40 + action, 0]);
        out.extend_from_slice(&(-(pid as i32)).to_le_bytes());
        out.extend_from_slice(&[pid as u8; wire::SCORE_EVENT]);
    }
    out.extend_from_slice(&[9; wire::SCORE_OUT_ITEM - 1]);
    assert_eq!(wire::score_head(&out), Some((7, 0, 0, true)));
    let got: Vec<(u32, u8, u8, i32, u8, usize)> = wire::score_results(&out).map(|r| (r.pid, r.action, r.verdict, r.margin, r.record[0], r.record.len())).collect();
    assert_eq!(got, [(11, 1, 41, -11, 11, 64), (13, 3, 43, -13, 13, 64), (14, 2, 42, -14, 14, 64)]);
    assert_eq!(wire::score_head(&out[..15]), None);
    assert_eq!(wire::score_results(&out[..15]).count(), 0);
}

#[test]
fn events_sent_ahead_are_scored_on_the_last_request() {
    let (mut engine, mut remote) = (Engine::new(KEY), scoring::Scoring::new());
    let who = [added(3, "photosync")];
    let events = exfil(3, 1_000_000_000);
    let first = turn(&mut engine, &mut remote, 1, Mode::Enforce, true, 0, 1_000_000_000, &who, &events[..3]).unwrap();
    assert!(first.locals.is_empty() && first.pending && first.scored == 0);
    let last = turn(&mut engine, &mut remote, 2, Mode::Enforce, false, 0, 1_000_000_000, &who, &events[3..]).unwrap();
    assert!(matches!(last.locals[..], [scorer::Local { pid: 3, action: scorer::QUARANTINE, enforced: true, .. }]), "{:?}", last.locals);
    assert_eq!(last.events[0].detail_str(), scorer::score_text(scorer::QUARANTINE, last.locals[0].verdict, last.locals[0].margin));
    assert!(last.events[0].detail_str().starts_with("quarantine v=") && last.events[0].detail_str().ends_with(&format!("model={:08x}", scorer_model::MODEL_ID)));
    // The program exits: its window goes, nothing is pending.
    let exit = [ev(3, 2_000_000_000, EventKind::Exit, 0, "", 0, 0)];
    let gone = turn(&mut engine, &mut remote, 3, Mode::Enforce, false, 0, 2_000_000_000, &[], &exit).unwrap();
    assert!(gone.locals.is_empty() && !gone.pending);
}

#[test]
fn malformed_scorer_requests_are_refused() {
    let (mut engine, mut remote) = (Engine::new(KEY), scoring::Scoring::new());
    let who = added(3, "photosync");
    let good_head = {
        let mut h = [0u8; wire::SCORE_HEAD];
        h[0] = 1;
        h
    };
    let event = exfil(3, 5)[0].to_wire();
    let mut bad_name = who.to_wire();
    bad_name[10] = 200;
    let cases: [(&str, Vec<u8>, Vec<u8>, Vec<u8>); 8] = [
        ("no such mode", { let mut h = good_head; h[0] = 3; h.to_vec() }, who.to_wire().to_vec(), event.to_vec()),
        ("no such flag", { let mut h = good_head; h[1] = 2; h.to_vec() }, who.to_wire().to_vec(), event.to_vec()),
        ("spare bytes set", { let mut h = good_head; h[3] = 1; h.to_vec() }, who.to_wire().to_vec(), event.to_vec()),
        ("short header", good_head[..15].to_vec(), who.to_wire().to_vec(), event.to_vec()),
        ("part of a subject", good_head.to_vec(), who.to_wire()[..35].to_vec(), event.to_vec()),
        ("a name longer than its field", good_head.to_vec(), bad_name.to_vec(), event.to_vec()),
        ("part of an event", good_head.to_vec(), who.to_wire().to_vec(), event[..63].to_vec()),
        ("too many events", good_head.to_vec(), who.to_wire().to_vec(), event.repeat(wire::SCORE_EVENTS_MAX + 1)),
    ];
    for (what, head, subjects, events) in cases {
        let f = frame(1, wire::OP_SCORE, 0, 0, &[&head, &subjects, &events]);
        let mut out = Vec::new();
        let words = engine.serve(1, wire::crc32(&f), &f, &mut out, &mut remote);
        assert_eq!((words[2], out.len()), (wire::V_BAD, 0), "{}", what);
    }
    // Two fields instead of three.
    let f = frame(1, wire::OP_SCORE, 0, 0, &[&good_head, &who.to_wire()]);
    let mut out = Vec::new();
    assert_eq!(engine.serve(1, wire::crc32(&f), &f, &mut out, &mut remote)[2], wire::V_BAD);
    // The most a request may carry fits the window, and so does its answer.
    let events = event.repeat(wire::SCORE_EVENTS_MAX);
    let subjects = who.to_wire().repeat(12);
    let f = frame(1, wire::OP_SCORE, 0, 0, &[&good_head, &subjects, &events]);
    assert_eq!(engine.serve(1, wire::crc32(&f), &f, &mut out, &mut remote)[2], wire::V_ALLOW);
}

#[test]
fn any_bytes_as_events_never_panic_the_scorer() {
    let (mut engine, mut remote) = (Engine::new(KEY), scoring::Scoring::new());
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for round in 0..400u32 {
        let subjects: Vec<Subject> = (0..(next() % 13) as u32).map(|n| {
            let mut w = [0u8; wire::SCORE_SUBJECT];
            w.iter_mut().for_each(|b| *b = next() as u8);
            w[..4].copy_from_slice(&(n % 6).to_le_bytes());
            w[10] %= 25;
            Subject::from_wire(&w).unwrap()
        }).collect();
        let events: Vec<TelemetryEvent> = (0..(next() % 101) as usize).map(|_| {
            let mut w = [0u8; 64];
            w.iter_mut().for_each(|b| *b = next() as u8);
            w[8..12].copy_from_slice(&((next() % 20) as u32).to_le_bytes());
            w[12] = (next() % 24) as u8;
            TelemetryEvent::from_wire(&w).unwrap()
        }).collect();
        let mode = [Mode::Monitor, Mode::Enforce, Mode::Strict][(next() % 3) as usize];
        let t = turn(&mut engine, &mut remote, round, mode, next() % 4 == 0, next() as u32, next(), &subjects, &events).expect("well formed");
        assert!(t.locals.len() <= scorer::PER_TICK);
    }
}
