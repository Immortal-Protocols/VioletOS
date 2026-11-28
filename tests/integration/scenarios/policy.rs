//! The capability policy from inside an app: what targets/qemu/test_policy.py
//! and its probe and victim apps checked in QEMU, against the shipped
//! policy.rs, manifest.rs, fs.rs, shield.rs and sysui.rs.
use crate::open::policy::Cap;
use crate::world::*;

const AF_INET: u32 = 2;
const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;
/// A refused write to another app's name looks missing, unless Violet
/// Shield refuses first (EACCES).
const HIDDEN: [i32; 2] = [ENOENT, EACCES];
const READ_ONLY: [i32; 2] = [EROFS, EACCES];

fn phone() -> (World, usize) {
    let mut w = World::boot(Image::new().app("victim", "").app("probe", ""));
    // The victim keeps private files under its own name and goes Home.
    let victim = w.open_app("victim");
    assert_eq!(w.app(victim).write_file("/data/victim", b"secret\n"), Ok(()));
    assert_eq!(w.app(victim).write_file("/data/victim.keys", b"api-key\n"), Ok(()));
    assert_eq!(w.app(victim).sys("home"), "ok");
    // What the runtime and Settings keep in /data.
    assert_eq!(w.app(victim).sys("set city lisbon"), "error not allowed", "an app behind Home sets nothing");
    let settings = w.open_app("settings");
    assert_eq!(w.app(settings).write_file("/data/networks", b"home-wifi psk\n"), Ok(()));
    assert_eq!(w.app(settings).sys("set city lisbon"), "ok");
    w.front(None);
    let probe = w.open_app("probe");
    (w, probe)
}

#[test]
fn an_app_reaches_only_its_own_data() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    assert_eq!(p.write_file("/data/probe", b"x"), Ok(()));
    assert_eq!(p.write_file("/data/probe.state", b"x"), Ok(()));
    assert_eq!(p.read_file("/data/probe"), Ok(b"x".to_vec()));
    // Another app's files, Settings' and the runtime's own do not exist to it.
    for path in ["/data/victim", "/data/victim.keys", "/data/networks", "/data/agent", "/data/.settings", "/data/.grants", "/data/.shield-mode", "/data/.lock"] {
        assert_eq!(p.read_file(path), Err(ENOENT), "read {path}");
        assert_eq!(p.stat(path), Err(ENOENT), "stat {path}");
    }
    assert!(HIDDEN.contains(&p.write_file("/data/victim", b"x").unwrap_err()));
    assert!(HIDDEN.contains(&p.write_file("/data/probex", b"x").unwrap_err()), "a file under another name");
    assert!(HIDDEN.contains(&p.unlink("/data/victim").unwrap_err()));
    assert!(HIDDEN.contains(&p.rename("/data/probe", "/data/victim").unwrap_err()));
    assert_eq!(p.read_file("/data/probe.d/../victim"), Err(ENOENT), "climbing out of its own folder");
    // The victim's files are as it left them.
    assert_eq!(w.rt.fs.contents("/data/victim"), Some(&b"secret\n"[..]));
    assert_eq!(w.rt.fs.contents("/data/victim.keys"), Some(&b"api-key\n"[..]));
}

#[test]
fn an_o_creat_open_does_not_tell_whether_a_hidden_file_exists() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    for (there, missing) in [("/data/victim", "/data/victimq"), ("/data/.settings", "/data/.nothing")] {
        let (a, b) = (p.open(there, O_CREAT | O_RDONLY), p.open(missing, O_CREAT | O_RDONLY));
        assert!(a.is_err(), "{there} opened");
        assert_eq!(a, b, "the answer tells {there} from {missing}");
    }
}

#[test]
fn listing_data_shows_only_the_apps_own_names() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    p.write_file("/data/probe", b"x").unwrap();
    p.write_file("/data/probe.state", b"x").unwrap();
    let listed = p.listdir("/data").unwrap();
    assert_eq!(listed, ["probe", "probe.state"]);
}

#[test]
fn tmp_is_shared_but_its_dot_names_are_not() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    assert_eq!(p.write_file("/tmp/probe-scratch", b"x"), Ok(()));
    for path in ["/tmp/.ota-chunk", "/tmp/.locomo.make"] {
        assert_eq!(p.write_file(path, b"x"), Err(ENOENT), "write {path}");
        assert_eq!(p.read_file(path), Err(ENOENT), "read {path}");
    }
    // The image apps whose manifests name them do reach them.
    let settings = w.open_app("settings");
    assert_eq!(w.app(settings).write_file("/tmp/.ota-chunk", b"piece"), Ok(()));
}

#[test]
fn the_images_trees_are_read_only() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    assert!(READ_ONLY.contains(&p.write_file("/apps/home", b"x").unwrap_err()), "overwrite Home");
    assert!(READ_ONLY.contains(&p.rename("/apps/probe", "/apps/settings").unwrap_err()), "rename itself to Settings");
    assert!(READ_ONLY.contains(&p.write_file("/apps/.agent-install", b"x").unwrap_err()), "stage an install");
    assert!(READ_ONLY.contains(&p.unlink("/apps/.manifests").unwrap_err()), "remove the manifests");
    assert!(READ_ONLY.contains(&p.mkdir("/bin/x").unwrap_err()));
    assert!(READ_ONLY.contains(&p.write_file("/x", b"x").unwrap_err()));
    assert!(p.read_file("/apps/home").is_ok(), "programs can be read");
    assert!(w.rt.fs.contents("/apps/home").is_some_and(|d| d.starts_with(b"\x7fELF")), "Home is as shipped");
}

#[test]
fn devices_and_sockets_need_a_capability_and_refusals_are_logged() {
    let (mut w, probe) = phone();
    let pid = w.app(probe).pid();
    let mut p = w.app(probe);
    for node in ["mic", "camera", "hci", "audio"] {
        assert_eq!(p.open(&format!("/dev/violet/{node}"), O_RDWR), Err(EACCES), "{node}");
    }
    assert_eq!(p.open("/dev/violet/nothing", O_RDWR), Err(ENOENT));
    assert_eq!(p.socket(AF_INET, SOCK_STREAM), Err(EACCES));
    assert_eq!(p.socket(AF_INET, SOCK_DGRAM), Err(EACCES));
    assert!(p.open("/dev/violet/screen", O_RDWR).is_ok() && p.open("/dev/violet/events", O_RDONLY).is_ok());
    // manifest.rs logs each refused capability once per app.
    for cap in ["net", "mic", "camera", "hci", "audio-out"] {
        assert_eq!(w.logged(&format!("policy: probe (pid {pid}) may not use {cap}")), 1, "{cap}\n{}", w.log());
    }
    assert_eq!(w.logged("trusted ui: prompt"), 0, "no prompt for an app policy decides alone");
}

#[test]
fn the_systems_sys_commands_are_not_an_apps() {
    let (mut w, probe) = phone();
    let pid = w.app(probe).pid();
    let mut p = w.app(probe);
    for line in ["open /apps/victim", "launch /apps/victim", "install probe Probe", "uninstall victim", "wifi join 0 00", "relay on 123456789abc",
                 "mic record", "sound chime", "shield-mode monitor", "shield-agent pull", "update info"] {
        let answer = p.sys(line);
        assert!(answer.starts_with("error not allowed (needs "), "'{line}' answered {answer:?}");
    }
    // What every app may do, and asking ahead for a power it does not declare.
    for (line, want) in [("set probecolor violet", "ok"), ("get probecolor", "violet"), ("get city", "lisbon"), ("consent net", "deny"), ("consent hci", "deny")] {
        assert_eq!(p.sys(line), want, "{line}");
    }
    for cap in ["system", "wifi-admin", "mic"] {
        assert!(w.logged(&format!("policy: probe (pid {pid}) may not use {cap}")) >= 1, "{cap}");
    }
    assert_eq!(w.logged("trusted ui: prompt"), 0);
    // The shared settings live where no app reaches them.
    assert!(w.rt.fs.contents("/data/.settings").is_some_and(|d| d.windows(17).any(|l| l == b"probecolor=violet")));
}

#[test]
fn image_apps_get_what_their_manifests_declare() {
    let (mut w, _) = phone();
    let settings = w.open_app("settings");
    let mut s = w.app(settings);
    assert!(s.holds(Cap::System) && s.holds(Cap::WifiAdmin) && s.holds(Cap::Net) && !s.holds(Cap::Mic) && !s.holds(Cap::Hci));
    assert_eq!(s.read_file("/data/networks"), Ok(b"home-wifi psk\n".to_vec()));
    assert_eq!(s.read_file("/data/victim"), Err(ENOENT), "Settings has no more of another app's /data than anyone");
    assert!(s.open("/dev/violet/audio", O_RDWR).is_ok());
    assert_eq!(s.open("/dev/violet/mic", O_RDONLY), Err(EACCES), "not in its manifest");
    assert_eq!(s.open("/dev/violet/hci", O_RDWR), Err(EACCES));
    assert!(s.socket(AF_INET, SOCK_STREAM).is_ok(), "it downloads system updates");
    assert_eq!(s.sys("wifi join 0 00"), "ok");
    // Home opens apps for the user and has no Wi-Fi powers.
    let home = w.rt.home.unwrap();
    assert!(w.app(home).holds(Cap::System) && !w.app(home).holds(Cap::WifiAdmin) && !w.app(home).holds(Cap::Mic));
}

#[test]
fn another_apps_program_started_by_an_app_is_not_that_app() {
    // policy.rs `exec_crosses`: Settings' program started by the probe runs
    // as the probe, with the minimal profile.
    let (mut w, probe) = phone();
    let i = w.start("/apps/settings", Some(probe)).unwrap();
    assert!(!w.app(i).holds(Cap::System) && !w.app(i).holds(Cap::WifiAdmin));
    assert_eq!(w.app(i).read_file("/data/networks"), Err(ENOENT));
    assert!(w.app(i).sys("wifi join 0 00").starts_with("error not allowed"));
}

#[test]
fn an_installed_apps_declared_power_is_the_users_to_grant() {
    let mut w = World::boot(Image::new());
    w.install("notes", "caps: net system wifi-admin\ndata: networks\n");
    let notes = w.open_app("notes");
    let pid = w.app(notes).pid();
    // Privileged powers and shared files count only from the image.
    assert!(!w.app(notes).holds(Cap::System) && !w.app(notes).holds(Cap::WifiAdmin));
    assert_eq!(w.app(notes).read_file("/data/networks"), Err(ENOENT));
    // Its declared net is asked of the user: the call waits (EAGAIN) while the prompt is up.
    assert_eq!(w.app(notes).socket(AF_INET, SOCK_STREAM), Err(EAGAIN));
    assert_eq!(w.mode(), crate::open::sysui::Mode::Prompt);
    assert_eq!(w.logged(&format!("policy: notes (pid {pid}) may not use net (asking)")), 1);
    w.answer_prompt(true);
    assert_eq!(w.mode(), crate::open::sysui::Mode::App);
    assert!(w.app(notes).socket(AF_INET, SOCK_STREAM).is_ok());
    // The answer is the runtime's file, kept across a restart...
    assert!(w.rt.fs.contents("/data/.grants").is_some_and(|d| !d.is_empty()));
    assert_eq!(w.app(notes).read_file("/data/.grants"), Err(ENOENT));
    let mut w = w.reboot();
    let notes = w.open_app("notes");
    assert!(w.app(notes).socket(AF_INET, SOCK_STREAM).is_ok(), "the kept answer");
    assert_eq!(w.logged("trusted ui: prompt"), 0);
    // ...and gone when another program is installed under the name.
    w.install("notes", "caps: net\n");
    let again = w.open_app("notes");
    assert_eq!(w.app(again).socket(AF_INET, SOCK_STREAM), Err(EAGAIN), "a new program starts with no answers");
    w.answer_prompt(false);
    assert_eq!(w.app(again).socket(AF_INET, SOCK_STREAM), Err(EACCES));
}

#[test]
fn uninstalling_an_app_removes_its_data_and_answers() {
    let mut w = World::boot(Image::new());
    w.install("notes", "caps: mic\n");
    let notes = w.open_app("notes");
    w.app(notes).write_file("/data/notes", b"diary").unwrap();
    w.app(notes).write_file("/data/notes.cache", b"c").unwrap();
    assert_eq!(w.app(notes).sys("consent mic"), "pending");
    w.answer_prompt(true);
    assert_eq!(w.app(notes).sys("consent mic"), "allow");
    w.rt.end(notes, 0);
    w.uninstall("notes");
    assert!(w.rt.fs.contents("/data/notes").is_none() && w.rt.fs.contents("/data/notes.cache").is_none());
    w.install("notes", "caps: mic\n");
    let fresh = w.open_app("notes");
    assert_eq!(w.app(fresh).read_file("/data/notes"), Err(ENOENT));
    assert_eq!(w.app(fresh).sys("consent mic"), "pending", "asked again");
}

// ---- the Sentinel (system/sentinel): the process that decides ----

use crate::open::policy::port::{set_fault, Fault};

#[test]
fn with_no_valid_answer_from_the_sentinel_everything_is_refused() {
    for fault in [Fault::Unreachable, Fault::CorruptRequest, Fault::ZeroReply, Fault::OnesReply, Fault::StaleReply] {
        let (mut w, probe) = phone();
        assert_eq!(w.app(probe).write_file("/data/probe", b"before"), Ok(()));
        set_fault(fault);
        let mut p = w.app(probe);
        // Its own file, a new one, /tmp, a device, a socket, a sys command: nothing is allowed.
        assert!(p.read_file("/data/probe").is_err(), "{fault:?}: read its own file");
        assert!(p.write_file("/data/probe.new", b"x").is_err(), "{fault:?}: made a file");
        assert!(p.write_file("/tmp/anything", b"x").is_err(), "{fault:?}: wrote /tmp");
        assert!(p.unlink("/data/probe").is_err(), "{fault:?}: removed a file");
        assert!(p.socket(AF_INET, SOCK_STREAM).is_err(), "{fault:?}: a socket");
        assert!(p.open("/dev/violet/mic", O_RDONLY).is_err(), "{fault:?}: the microphone");
        assert_eq!(p.listdir("/data").unwrap_or_default(), Vec::<String>::new(), "{fault:?}: /data listed");
        assert_eq!(p.open("/dev/violet/sys", O_RDWR), Err(EACCES), "{fault:?}: sys commands");
        set_fault(Fault::None);
        // Nothing was made or removed while it could not be asked.
        assert_eq!(w.rt.fs.contents("/data/probe"), Some(&b"before"[..]), "{fault:?}");
        assert!(w.rt.fs.contents("/data/probe.new").is_none() && w.rt.fs.contents("/tmp/anything").is_none(), "{fault:?}");
        assert_eq!(w.app(probe).read_file("/data/probe"), Ok(b"before".to_vec()), "{fault:?}: answers again");
    }
}

#[test]
fn consent_answers_are_kept_sealed_and_an_edited_store_is_asked_again() {
    let mut w = World::boot(Image::new());
    w.install("notes", "caps: net\n");
    let notes = w.open_app("notes");
    assert_eq!(w.app(notes).socket(AF_INET, SOCK_STREAM), Err(EAGAIN));
    w.answer_prompt(true);
    assert!(w.app(notes).socket(AF_INET, SOCK_STREAM).is_ok());
    // What /data holds is sealed text: not the app's name, not the answer.
    let stored = w.rt.fs.contents("/data/.grants").expect("the store is saved").to_vec();
    assert!(stored.starts_with(b"vss1."), "{:?}", String::from_utf8_lossy(&stored));
    assert!(!stored.windows(5).any(|x| x == b"notes"));
    // A restart (a new Sentinel) opens it.
    let (image, kept) = w.shutdown();
    let mut w = World::boot_with(image.clone(), kept.clone(), 2);
    let notes = w.open_app("notes");
    assert!(w.app(notes).socket(AF_INET, SOCK_STREAM).is_ok(), "the kept answer");
    drop(w);
    // One character changed, or wave 2's plain text put in its place: no answers, the user is asked.
    let mut edited = stored.clone();
    let last = edited.len() - 2;
    edited[last] = if edited[last] == b'0' { b'1' } else { b'0' };
    for forged in [edited, b"notes\t1\t0\n".to_vec()] {
        let mut kept = kept.clone();
        for f in &mut kept.data {
            if f.0 == "/data/.grants" {
                f.1 = forged.clone();
            }
        }
        let mut w = World::boot_with(image.clone(), kept, 3);
        let notes = w.open_app("notes");
        assert_eq!(w.app(notes).socket(AF_INET, SOCK_STREAM), Err(EAGAIN), "a store not sealed by the Sentinel was obeyed");
        assert_eq!(w.mode(), crate::open::sysui::Mode::Prompt);
    }
}

/// The Sentinel with no sealing key (no root key blob): consent still works
/// for the boot it was given in, the stored answers are never overwritten
/// with nothing, and nothing it could not seal is obeyed after a restart.
#[test]
fn a_sentinel_with_no_key_asks_every_boot_and_never_empties_the_store() {
    // A keyed boot leaves a sealed answer in /data.
    let mut w = World::boot(Image::new());
    w.install("notes", "caps: net\n");
    let notes = w.open_app("notes");
    assert_eq!(w.app(notes).socket(AF_INET, SOCK_STREAM), Err(EAGAIN));
    w.answer_prompt(true);
    assert!(w.app(notes).socket(AF_INET, SOCK_STREAM).is_ok());
    let sealed = w.rt.fs.contents("/data/.grants").expect("the store is saved").to_vec();
    assert!(sealed.starts_with(b"vss1."));
    let (image, kept) = w.shutdown();
    // Unkeyed: the store does not open, so the user is asked; the answer
    // holds for this boot; the sealed file is kept byte for byte.
    let mut w = World::boot_unkeyed(image.clone(), kept.clone(), 2);
    let notes = w.open_app("notes");
    assert_eq!(w.app(notes).socket(AF_INET, SOCK_STREAM), Err(EAGAIN), "an unopened store was obeyed");
    assert_eq!(w.mode(), crate::open::sysui::Mode::Prompt);
    w.answer_prompt(true);
    assert!(w.app(notes).socket(AF_INET, SOCK_STREAM).is_ok(), "the answer holds for this boot");
    assert_eq!(w.rt.fs.contents("/data/.grants"), Some(&sealed[..]), "the store was rewritten unkeyed");
    let (image, kept2) = w.shutdown();
    // Keyed again: the original sealed answer comes back.
    let mut w = World::boot_with(image.clone(), kept2, 3);
    let notes = w.open_app("notes");
    assert!(w.app(notes).socket(AF_INET, SOCK_STREAM).is_ok(), "the sealed answer was lost");
    drop(w);
    // An unkeyed first boot (empty /data): asked, allowed for the boot, and a
    // restart asks again; .grants is never a sealed store it could not make.
    let mut w = World::boot_unkeyed(image.clone(), Persisted::default(), 4);
    w.install("notes", "caps: net\n");
    let notes = w.open_app("notes");
    assert_eq!(w.app(notes).socket(AF_INET, SOCK_STREAM), Err(EAGAIN));
    w.answer_prompt(true);
    assert!(w.app(notes).socket(AF_INET, SOCK_STREAM).is_ok());
    assert!(!w.rt.fs.contents("/data/.grants").unwrap_or_default().starts_with(b"vss1."));
    let (image, kept) = w.shutdown();
    let mut w = World::boot_unkeyed(image, kept, 5);
    let notes = w.open_app("notes");
    assert_eq!(w.app(notes).socket(AF_INET, SOCK_STREAM), Err(EAGAIN), "an unsealed answer was kept");
}
