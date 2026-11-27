//! Host test for the Open Runtime's capability policy (`make test-unit`):
//! policy.rs's table, manifests, consent answers and the /data namespace,
//! and fs.rs's path resolution feeding it, as the runtime joins them
//! (manifest.rs: `at` normalizes, then `path_rule` decides).
#![allow(dead_code)]
extern crate alloc;

#[path = "../policy.rs"]
mod policy;
#[path = "../fs.rs"]
mod fs;
#[path = "../sysui.rs"]
mod sysui;
/// The errors fs.rs names (runtime/open/syscall.rs).
mod syscall {
    pub const ENOENT: i32 = -2;
    pub const EEXIST: i32 = -17;
    pub const ENOTDIR: i32 = -20;
    pub const EISDIR: i32 = -21;
    pub const ENOTEMPTY: i32 = -39;
}

use policy::*;

fn image(text: &str) -> Profile {
    Profile::parse(text, Origin::Image)
}
fn installed(text: &str) -> Profile {
    Profile::parse(text, Origin::Installed)
}
/// The resolver and the rule, as open() runs them for an app whose working
/// directory is `cwd`.
fn reach(owner: &str, p: &Profile, cwd: &str, path: &str, write: bool) -> Result<(), i32> {
    path_rule(owner, p, &fs::normalize(cwd, path), write)
}

#[test]
fn manifests_parse_and_never_widen() {
    let p = image("# Settings\ncaps: wifi-admin system, audio-out\ndata: networks relay\n");
    assert_eq!(p.caps, Cap::WifiAdmin.bit() | Cap::System.bit() | Cap::AudioOut.bit());
    assert_eq!(p.shared, ["networks", "relay"]);
    // Unknown words, other keys, comments and malformed lines add nothing.
    let p = image("caps: root net everything # mic\ncapz: camera\ncaps mic\nnet\n");
    assert_eq!(p.caps, Cap::Net.bit());
    // Privileged powers and shared files count only from the image.
    let p = installed("caps: net system wifi-admin mic hci\ndata: agent networks\n");
    assert_eq!(p.caps, Cap::Net.bit() | Cap::Mic.bit() | Cap::Hci.bit());
    assert!(p.shared.is_empty());
    assert_eq!(Profile::parse("caps: net system\ndata: x", Origin::Other), Profile::minimal(Origin::Other));
    // Shared names are names directly in /data, never hidden ones or paths.
    assert_eq!(image("data: .grants ../agent a/b ok-1 .x\n").shared, ["ok-1"]);
    assert!(image("").caps == 0 && image("garbage\0\u{1}").caps == 0);
    for (c, n) in CAPS {
        assert_eq!(Cap::parse(n), Some(c));
        assert_eq!(c.name(), n);
    }
}

#[test]
fn cap_bits_match_the_trusted_ui() {
    // sysui.rs Cap: NET 1<<0, MIC 1<<2, CAMERA 1<<4, HCI 1<<5.
    assert_eq!((Cap::Net.bit(), Cap::Mic.bit(), Cap::Camera.bit(), Cap::Hci.bit()), (1, 4, 16, 32));
    let all: u8 = CAPS.iter().map(|(c, _)| c.bit()).fold(0, |a, b| { assert_eq!(a & b, 0); a | b });
    assert_eq!(all & PRIVILEGED, PRIVILEGED);
}

#[test]
fn image_table_finds_each_app() {
    let table = "@home\ncaps: net system\n@settings\ncaps: wifi-admin system\ndata: networks\n@music\ncaps: net audio-out\n";
    assert_eq!(image_manifest(table, "home"), Some("caps: net system\n"));
    assert_eq!(image_manifest(table, "settings"), Some("caps: wifi-admin system\ndata: networks\n"));
    assert_eq!(image_manifest(table, "music"), Some("caps: net audio-out\n"));
    assert_eq!(image_manifest(table, "mus"), None);
    assert_eq!(image_manifest(table, "net"), None);
    assert_eq!(image_manifest("", "home"), None);
    assert_eq!(image_manifest("@a\n@b\ncaps: mic", "a"), Some(""));
    assert_eq!(image_manifest("@a\n@b\ncaps: mic", "b"), Some("caps: mic"));
    // An installed app may not take a name whose /data an image app shares.
    assert!(shared_by_image(table, "networks"));
    assert!(shared_by_image("@s\ndata: relay.saved x\n", "relay"));
    assert!(!shared_by_image(table, "network") && !shared_by_image(table, "settings") && !shared_by_image(table, "home"));
}

#[test]
fn decisions_by_origin() {
    let g = Grants::default();
    let home = image("caps: net system");
    assert_eq!(ask(&home, &g, "home", Cap::Net), Decision::Allow);
    assert_eq!(ask(&home, &g, "home", Cap::System), Decision::Allow);
    assert_eq!(ask(&home, &g, "home", Cap::Mic), Decision::Deny);
    let app = installed("caps: net mic camera hci audio-out");
    // Bluetooth too: raw HCI can scan for and track nearby devices.
    for c in [Cap::Net, Cap::Mic, Cap::Camera, Cap::Hci] {
        assert_eq!(ask(&app, &g, "scorer", c), Decision::Ask, "{:?}", c);
    }
    assert_eq!(ask(&image("caps: hci"), &g, "bitchat", Cap::Hci), Decision::Allow);
    assert_eq!(ask(&app, &g, "scorer", Cap::AudioOut), Decision::Allow);
    assert_eq!(ask(&app, &g, "scorer", Cap::WifiAdmin), Decision::Deny);
    // A profile built by hand with a privileged bit is still refused it.
    let forged = Profile { origin: Origin::Installed, caps: 0xff, shared: Vec::new(), tmp: Vec::new() };
    assert_eq!(ask(&forged, &g, "x", Cap::System), Decision::Deny);
    let other = Profile { origin: Origin::Other, caps: 0xff, shared: Vec::new(), tmp: Vec::new() };
    for (c, _) in CAPS {
        assert_eq!(ask(&other, &g, "x", c), Decision::Deny);
        assert_eq!(ask(&Profile::minimal(Origin::Image), &g, "x", c), Decision::Deny);
    }
}

#[test]
fn consent_answers_are_kept_per_app() {
    let app = installed("caps: net mic");
    let mut g = Grants::default();
    g.record("scorer", Cap::Net, true);
    g.record("scorer", Cap::Mic, false);
    assert_eq!(ask(&app, &g, "scorer", Cap::Net), Decision::Allow);
    assert_eq!(ask(&app, &g, "scorer", Cap::Mic), Decision::Deny);
    assert_eq!(ask(&app, &g, "other", Cap::Net), Decision::Ask);
    // Undeclared stays denied whatever the user said; privileged is never kept.
    assert_eq!(ask(&installed("caps: mic"), &g, "scorer", Cap::Net), Decision::Deny);
    g.record("scorer", Cap::System, true);
    g.record("scorer", Cap::Hci, true);
    let back = Grants::parse(&g.format());
    assert_eq!(back, g);
    assert_eq!(back.answer("scorer", Cap::System), None);
    assert_eq!(back.answer("scorer", Cap::Hci), Some(true));
    // Changing an answer.
    g.record("scorer", Cap::Mic, true);
    assert_eq!(g.answer("scorer", Cap::Mic), Some(true));
    g.forget("scorer");
    assert_eq!(ask(&app, &g, "scorer", Cap::Net), Decision::Ask);
    // A grants file that says more than consent, or both ways, grants nothing more.
    let g = Grants::parse("a\tff\t0\nb\t1\t1\nbad line\nc\tzz\t0\n\t1\t0\n");
    assert_eq!(g.rows, [("a".into(), CONSENT, 0), ("b".into(), 0, 1)]);
    assert_eq!(g.answer("b", Cap::Net), Some(false));
    // Bounded.
    let mut g = Grants::default();
    for n in 0..100 {
        g.record(&std::format!("app{n}"), Cap::Net, true);
    }
    assert_eq!(g.rows.len(), 64);
    assert_eq!(g.answer("app99", Cap::Net), Some(true));
    assert_eq!(g.answer("app0", Cap::Net), None);
}

#[test]
fn envelopes() {
    let mut blob = b"#!/bin/python3\nprint(1)\n".to_vec();
    blob.extend_from_slice(b"caps: net");
    blob.extend_from_slice(&9u16.to_le_bytes());
    blob.extend_from_slice(b"VMF1");
    let (m, prog) = envelope(&blob);
    assert_eq!(m, Some("caps: net"));
    assert_eq!(prog, b"#!/bin/python3\nprint(1)\n");
    assert_eq!(envelope(b"\x7fELF"), (None, &b"\x7fELF"[..]));
    assert_eq!(envelope(b"\x00\x00VMF1"), (Some(""), &b""[..]));
    assert_eq!(envelope(b"x\xff\xffVMF1"), (Some(""), &b""[..]));
    assert_eq!(envelope(b"VMF1"), (Some(""), &b""[..]));
    assert_eq!(envelope(b"#!\xff\xfe\x02\x00VMF1"), (Some(""), &b""[..]));
}

/// tools/build_initfs.py `envelope`, byte for byte.
fn wrap(program: &[u8], manifest: &str) -> Vec<u8> {
    let mut b = program.to_vec();
    b.extend_from_slice(manifest.as_bytes());
    b.extend_from_slice(&(manifest.len() as u16).to_le_bytes());
    b.extend_from_slice(b"VMF1");
    b
}

#[test]
fn installs_get_back_the_program_exactly() {
    // appstore.rs splits at install and at every load: the program, not its tail.
    let python = b"#!/bin/python3\nimport violet\nviolet.beep()\n".to_vec();
    let mut elf = b"\x7fELF\x01\x01\x01".to_vec();
    elf.extend((0..5000u32).map(|n| (n * 7) as u8));
    for program in [python, elf] {
        for manifest in ["caps: audio-out\n", "", "caps: net mic camera audio-out hci\n"] {
            let (m, p) = split_manifest(wrap(&program, manifest));
            assert_eq!(m, manifest);
            assert_eq!(p, program, "{manifest:?}");
        }
        // No manifest: the program as it is.
        assert_eq!(split_manifest(program.clone()), (String::new(), program.clone()));
    }
    // Damaged: nothing runs.
    assert_eq!(split_manifest(b"#!x\xff\xfe\x02\x00VMF1".to_vec()).1, b"");
}

#[test]
fn devices_and_commands() {
    assert_eq!(device_cap("/dev/violet/screen"), Ok(None));
    assert_eq!(device_cap("/dev/violet/events"), Ok(None));
    assert_eq!(device_cap("/dev/violet/sys"), Ok(None));
    assert_eq!(device_cap("/dev/violet/audio"), Ok(Some(Cap::AudioOut)));
    assert_eq!(device_cap("/dev/violet/mic"), Ok(Some(Cap::Mic)));
    assert_eq!(device_cap("/dev/violet/camera"), Ok(Some(Cap::Camera)));
    assert_eq!(device_cap("/dev/violet/hci"), Ok(Some(Cap::Hci)));
    assert_eq!(device_cap("/dev/violet/mic2"), Err(()));
    assert_eq!(device_cap("/dev/null"), Ok(None));
    assert_eq!(device_cap("/data/mic"), Ok(None));
    for line in ["open /apps/x", "launch /apps/x", "install x X", "uninstall x", "shield-agent pull", "shield-mode strict", "shield-release x", "update info", "update write 8", "update apply"] {
        assert_eq!(sys_cap(line), Some(Cap::System), "{line}");
    }
    assert_eq!(sys_cap("wifi join 3 abcd"), Some(Cap::WifiAdmin));
    assert_eq!(sys_cap("wifi leave"), Some(Cap::WifiAdmin));
    assert_eq!(sys_cap("relay on 1234"), Some(Cap::WifiAdmin));
    assert_eq!(sys_cap("relay off"), Some(Cap::WifiAdmin));
    assert_eq!(sys_cap("mic record"), Some(Cap::Mic));
    assert_eq!(sys_cap("sound chime"), Some(Cap::AudioOut));
    assert_eq!(sys_cap("volume up"), Some(Cap::AudioOut));
    for line in ["home", "status", "get city", "set city Tokyo", "log hi", "time", "wakelock 1", "wifi", "wifi scan", "wifi list", "relay", "running", "installed", "shield", "shield-local", "audio", "  "] {
        assert_eq!(sys_cap(line), None, "{line}");
    }
}

#[test]
fn data_namespace() {
    let none = Profile::minimal(Origin::Other);
    // Own: /data/OWNER (file or folder) and /data/OWNER.*.
    for (path, write) in [("/data/agent", true), ("/data/agent", false), ("/data/agent.keys", true), ("/data/agent/x/y", true)] {
        assert_eq!(path_rule("agent", &none, path, write), Ok(()), "{path}");
    }
    // Another app's, or a name that only starts like ours: hidden, refused.
    // Writes too: an O_CREAT|O_RDONLY open is a read when the file exists
    // and a write when it does not, so a different answer would tell.
    for path in ["/data/agentx", "/data/bitchat", "/data/notes/keys.txt", "/data/settings", "/data/networks", "/data/agen"] {
        assert_eq!(path_rule("agent", &none, path, false), Err(ENOENT), "{path}");
        assert_eq!(path_rule("agent", &none, path, true), Err(ENOENT), "{path}");
    }
    // The runtime's dot files, whoever asks and whatever is shared.
    let wide = image("data: networks .grants");
    for path in ["/data/.grants", "/data/.settings", "/data/.shield-mode", "/data/.agent", "/data/.x/y"] {
        assert_eq!(path_rule("agent", &wide, path, false), Err(ENOENT), "{path}");
        assert_eq!(path_rule(".agent", &wide, path, true), Err(ENOENT), "{path}");
    }
    // /data itself: listed and stat'ed, never made or removed.
    assert_eq!(path_rule("agent", &none, "/data", false), Ok(()));
    assert_eq!(path_rule("agent", &none, "/data", true), Err(EACCES));
    // Shared files: only an image manifest shares.
    let settings = image("caps: wifi-admin\ndata: networks relay");
    assert_eq!(path_rule("settings", &settings, "/data/networks", true), Ok(()));
    assert_eq!(path_rule("settings", &settings, "/data/relay", false), Ok(()));
    assert_eq!(path_rule("settings", &settings, "/data/networks.bak", false), Err(ENOENT));
    let forged = Profile { origin: Origin::Installed, caps: 0, shared: alloc::vec!["networks".into()], tmp: alloc::vec![".ota-chunk".into()] };
    assert_eq!(path_rule("x", &forged, "/data/networks", false), Err(ENOENT));
    assert_eq!(path_rule("x", &forged, "/tmp/.ota-chunk", false), Err(ENOENT));
    // An owner that could not be an app name owns nothing in /data.
    for owner in ["a.b", "a/b"] {
        assert_eq!(path_rule(owner, &none, &std::format!("/data/{owner}"), true), Err(ENOENT), "{owner:?}");
    }
    assert!(path_rule("", &none, "/data/", true).is_err() && !owns_data("", "x"));
    assert!(owns_data("~1a2b3c4d", "~1a2b3c4d") && !owns_data("", ""));
}

#[test]
fn paths_resolve_before_they_are_judged() {
    let none = Profile::minimal(Origin::Other);
    // "..", ".", "//" and relative paths are resolved first, so none of them escape.
    assert_eq!(reach("agent", &none, "/", "/data/agent/../bitchat", false), Err(ENOENT));
    assert_eq!(reach("agent", &none, "/data/agent", "../bitchat", false), Err(ENOENT));
    assert_eq!(reach("agent", &none, "/data/agent", "../../data/./agent//state", true), Ok(()));
    assert_eq!(reach("agent", &none, "/data", "agent", true), Ok(()));
    assert_eq!(reach("agent", &none, "/data", "bitchat", true), Err(ENOENT));
    assert_eq!(reach("agent", &none, "/", "/data/../../../data/.grants", false), Err(ENOENT));
    assert_eq!(reach("agent", &none, "/tmp", "../data/../apps/home", true), Err(EROFS));
    assert_eq!(reach("agent", &none, "/", "data", false), Ok(()));
    // Anything that did not come through the resolver is refused outright.
    for raw in ["data/agent", "/data/agent/../bitchat", "/data//agent", "/data/./agent", "/data/agent/", ""] {
        assert!(path_rule("agent", &none, raw, true).is_err() && path_rule("agent", &none, raw, false).is_err(), "{raw:?}");
    }
    // The resolver's own rules.
    assert_eq!(fs::normalize("/a/b", "../../.."), "/");
    assert_eq!(fs::normalize("/", "/data/x/.."), "/data");
    assert_eq!(fs::normalize("/x", "y//z/./"), "/x/y/z");
}

#[test]
fn image_trees_are_read_only() {
    let home = image("caps: net system");
    let none = Profile::minimal(Origin::Other);
    for path in ["/apps/home", "/apps/settings", "/apps/agent", "/apps/.home", "/apps/.manifests", "/apps/new", "/bin/python3",
                 "/lib/x", "/etc/hosts", "/newfile", "/apps", "/bin", "/proc/self/exe", "/dev/sda"] {
        assert!(path_rule("home", &home, path, true).is_err(), "{path}");
        assert!(path_rule("x", &none, path, true).is_err(), "{path}");
    }
    assert_eq!(path_rule("x", &none, "/apps/home", true), Err(EROFS));
    assert_eq!(path_rule("x", &none, "/proc/self/exe", false), Ok(()));
    for path in ["/apps/home", "/apps/.home", "/bin/python3", "/lib/python3.12/os.py", "/etc/resolv.conf", "/", "/apps"] {
        assert_eq!(path_rule("x", &none, path, false), Ok(()), "{path}");
    }
    // The Agent's staging file: only an image app with the system power.
    let agent = image("caps: net mic system");
    assert_eq!(path_rule("agent", &agent, STAGED, true), Ok(()));
    assert_eq!(path_rule("agent", &agent, STAGED, false), Ok(()));
    assert_eq!(path_rule("x", &none, STAGED, true), Err(ENOENT));
    assert_eq!(path_rule("x", &none, STAGED, false), Err(ENOENT));
    assert_eq!(path_rule("x", &installed("caps: system"), STAGED, true), Err(ENOENT));
    // /tmp is shared and writable; /tmp itself is not removable.
    assert_eq!(path_rule("x", &none, "/tmp/scratch", true), Ok(()));
    assert_eq!(path_rule("x", &none, "/tmp/sub/dir", true), Ok(()));
    assert_eq!(path_rule("x", &none, "/tmp", true), Err(EACCES));
}

#[test]
fn reserved_tmp_names() {
    let none = Profile::minimal(Origin::Other);
    let settings = image(include_str!("../../../apps/open/settings/manifest.txt"));
    let agent = image(include_str!("../../../apps/open/agent/manifest.txt"));
    let music = image(include_str!("../../../apps/open/music/manifest.txt"));
    // Settings' update pieces: no other app writes or sees them.
    assert_eq!(path_rule("settings", &settings, "/tmp/.ota-chunk", true), Ok(()));
    for p in [&none, &agent, &music, &installed("caps: net system\ntmp: .ota-chunk")] {
        assert_eq!(path_rule("x", p, "/tmp/.ota-chunk", true), Err(ENOENT));
        assert_eq!(path_rule("x", p, "/tmp/.ota-chunk", false), Err(ENOENT));
    }
    // The Music and Agent exchange: those two only, and the .next files with it.
    for path in ["/tmp/.locomo.make", "/tmp/.locomo.track.next", "/tmp/.locomo"] {
        assert_eq!(path_rule("music", &music, path, true), Ok(()), "{path}");
        assert_eq!(path_rule("agent", &agent, path, true), Ok(()), "{path}");
        assert_eq!(path_rule("x", &none, path, true), Err(ENOENT), "{path}");
        assert_eq!(path_rule("settings", &settings, path, false), Err(ENOENT), "{path}");
    }
    // A name that only starts like a reserved one, and any other dot name.
    assert_eq!(path_rule("music", &music, "/tmp/.locomox", true), Err(ENOENT));
    assert_eq!(path_rule("x", &none, "/tmp/.anything", true), Err(ENOENT));
    // tmp: lines take only dot names, and count only in the image.
    assert!(image("tmp: plain .ok ../x").tmp == [".ok"]);
    assert!(installed("tmp: .ok").tmp.is_empty());
}

#[test]
fn starting_another_apps_program_is_not_that_app() {
    // A plain app that execve's another app's program keeps its own owner.
    assert!(exec_crosses("scorer", false, "/apps/victim"));
    assert!(exec_crosses("scorer", false, "/apps/pynet/main.py"));
    assert!(exec_crosses("~1a2b3c4d", false, "/apps/settings"));
    // Its own program, or one outside /apps (which is its own anyway), or a core caller.
    assert!(!exec_crosses("scorer", false, "/apps/scorer"));
    assert!(!exec_crosses("scorer", false, "/apps/scorer/helper"));
    assert!(!exec_crosses("scorer", false, "/bin/python3"));
    assert!(!exec_crosses("scorer", false, "/tmp/x"));
    assert!(!exec_crosses("agent", true, "/apps/victim"));
    // Kept as the caller's owner from /apps/victim, the program is outside
    // its owner's folder: manifest.rs `profile` gives it Origin::Other.
    assert_eq!(Profile::parse("caps: net mic", Origin::Other), Profile::minimal(Origin::Other));
}

#[test]
fn a_new_program_under_a_name_inherits_no_answers() {
    // manifest.rs `forget_consent` clears both: the kept answers and the trusted UI's.
    let mut c = sysui::Consent::default();
    assert_eq!(c.request(5, "cam", sysui::Cap::CAMERA, true, false), sysui::Decision::Prompt);
    c.answer(true);
    assert_eq!(c.request(5, "cam", sysui::Cap::CAMERA, true, false), sysui::Decision::Allow);
    c.forget("cam");
    assert_eq!(c.decided("cam", sysui::Cap::CAMERA), None);
    assert_eq!(c.request(6, "cam", sysui::Cap::CAMERA, true, false), sysui::Decision::Prompt);
    // A prompt up for the name goes too; other apps' answers stay.
    c.answer(false);
    c.request(7, "other", sysui::Cap::NET, true, false);
    c.answer(true);
    c.request(8, "cam", sysui::Cap::MIC, true, false);
    c.forget("cam");
    assert!(c.prompt.is_none());
    assert_eq!(c.decided("other", sysui::Cap::NET), Some(true));
    let mut g = Grants::default();
    g.record("cam", Cap::Camera, true);
    g.forget("cam");
    assert_eq!(ask(&installed("caps: camera"), &g, "cam", Cap::Camera), Decision::Ask);
    // Device nodes open (their capabilities are `device_cap`); nothing new in /dev.
    let none = Profile::minimal(Origin::Other);
    for path in ["/dev/null", "/dev/urandom", "/dev/tty", "/dev/violet/sys", "/dev/violet/screen"] {
        assert_eq!(path_rule("x", &none, path, true), Ok(()), "{path}");
    }
    assert_eq!(path_rule("x", &none, "/dev/newfile", true), Err(EROFS));
}

#[test]
fn image_contents_ignore_what_was_written_over() {
    let mut cpio = alloc::vec::Vec::new();
    for (name, data, mode) in [("apps", &b""[..], 0o040755u32), ("apps/.manifests", b"@x\ncaps: net\n", 0o100644)] {
        let mut name = name.as_bytes().to_vec();
        name.push(0);
        let fields = [0, mode, 0, 0, 1, 0, data.len() as u32, 0, 0, 0, 0, name.len() as u32, 0];
        let mut e = b"070701".to_vec();
        for f in fields {
            e.extend_from_slice(std::format!("{f:08x}").as_bytes());
        }
        e.extend_from_slice(&name);
        while e.len() % 4 != 0 {
            e.push(0);
        }
        e.extend_from_slice(data);
        while e.len() % 4 != 0 {
            e.push(0);
        }
        cpio.extend_from_slice(&e);
    }
    let image: &'static [u8] = alloc::boxed::Box::leak(cpio.into_boxed_slice());
    let mut f = fs::Fs::new(image);
    assert_eq!(f.image_contents("/apps/.manifests"), Some(&b"@x\ncaps: net\n"[..]));
    *f.writable("/apps/.manifests", false, true, 0).unwrap() = b"@x\ncaps: system\n".to_vec();
    assert_eq!(f.contents("/apps/.manifests"), Some(&b"@x\ncaps: system\n"[..]));
    assert_eq!(f.image_contents("/apps/.manifests"), Some(&b"@x\ncaps: net\n"[..]));
    assert_eq!(f.image_contents("/apps/none"), None);
}

#[test]
fn built_in_manifests_grant_what_the_apps_use() {
    let m = |text: &str| image(text);
    let settings = m(include_str!("../../../apps/open/settings/manifest.txt"));
    assert_eq!(settings.caps, Cap::Net.bit() | Cap::WifiAdmin.bit() | Cap::System.bit() | Cap::AudioOut.bit());
    assert_eq!(settings.shared, ["networks", "relay"]);
    let home = m(include_str!("../../../apps/open/home/manifest.txt"));
    assert_eq!(home.caps, Cap::Net.bit() | Cap::System.bit());
    let agent = m(include_str!("../../../apps/open/agent/manifest.txt"));
    for c in [Cap::Net, Cap::Mic, Cap::Camera, Cap::AudioOut, Cap::System] {
        assert!(agent.has(c), "{:?}", c);
    }
    assert!(!agent.has(Cap::WifiAdmin) && !home.has(Cap::WifiAdmin));
    assert!(m(include_str!("../../../apps/open/bitchat/manifest.txt")).has(Cap::Hci));
    assert_eq!(m(include_str!("../../../apps/open/music/manifest.txt")).caps, Cap::Net.bit() | Cap::AudioOut.bit());
    assert_eq!(m(include_str!("../../../apps/open/ladybird/manifest.txt")).caps, Cap::Net.bit());
    // Settings reaches its saved networks; Home does not.
    assert_eq!(path_rule("settings", &settings, "/data/networks", true), Ok(()));
    assert_eq!(path_rule("home", &home, "/data/networks", false), Err(ENOENT));
}
