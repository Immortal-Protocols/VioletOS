//! Applies the capability policy (policy.rs, whose decisions the host tests
//! run) to running apps: which manifest an app runs under, the checks the
//! system calls make, and the runtime-owned file of the user's answers.
//!
//! An app is the owner Violet Shield derives for it (shield.rs `owner_of`,
//! `App::owner`): the NAME of /apps/NAME. Its manifest is
//!   - the image's (/apps/.manifests, read from the image itself) when its
//!     program is that app's file from the boot image, unchanged;
//!   - the one installed with it (appstore.rs `Entry::manifest`) when it is
//!     the Agent-installed /apps/NAME;
//!   - none otherwise: its own /data, the screen and touch.
use super::fs::Fs;
use super::policy::{self, Cap, Decision, Grants, Origin, Profile};
use super::Runtime;
use alloc::string::String;
use alloc::vec::Vec;
use crate::log;

/// The image's manifests (tools/build_initfs.py), "@NAME" then its lines.
pub const MANIFESTS: &str = "/apps/.manifests";
/// The user's answers to consent prompts (policy.rs `Grants`). No app can
/// reach a /data name that starts with '.' (policy.rs `path_rule`).
pub const GRANTS: &str = "/data/.grants";
/// Shared settings ("get" and "set" in /dev/violet/sys). They used to be
/// /data/settings, which is the Settings app's own name now.
pub const SETTINGS: &str = "/data/.settings";
const OLD_SETTINGS: &str = "/data/settings";

#[derive(Default)]
pub struct Book {
    table: &'static str,
    pub grants: Grants,
    /// (pid, capability bit) already logged as refused, so a retrying app
    /// does not fill the log.
    told: Vec<(u32, u8)>,
}

impl Book {
    /// At start, after /data and the installed apps are loaded.
    pub fn load(fs: &mut Fs) -> Book {
        let table = fs.image_contents(MANIFESTS).and_then(|d| core::str::from_utf8(d).ok()).unwrap_or("");
        let grants = fs.contents(GRANTS).and_then(|d| core::str::from_utf8(d).ok()).map(Grants::parse).unwrap_or_default();
        // Once, on the first start with per-app /data: the shared settings
        // move out of Settings' namespace (a rename is kept as two commits,
        // the new name first, so a power cut leaves at least one).
        if fs.stat(SETTINGS).is_none() && fs.contents(OLD_SETTINGS).is_some() {
            match fs.rename(OLD_SETTINGS, SETTINGS) {
                Ok(()) => log!("policy: shared settings moved to {}", SETTINGS),
                Err(e) => log!("policy: could not move the shared settings ({})", e),
            }
        }
        log!("policy: {} app manifests in the image, {} apps with answers", table.matches("\n@").count() + table.starts_with('@') as usize, grants.rows.len());
        Book { table, grants, told: Vec::new() }
    }
    /// A built-in app shares /data that an app of this name would own.
    pub fn reserved(&self, name: &str) -> bool {
        policy::shared_by_image(self.table, name)
    }
}

impl Runtime {
    /// The manifest app `i` runs under.
    pub fn profile(&self, i: usize) -> Profile {
        let Some(app) = self.apps[i].as_ref() else { return Profile::minimal(Origin::Other) };
        let own = alloc::format!("/apps/{}", app.owner);
        let in_folder = app.path == own || app.path.strip_prefix(own.as_str()).is_some_and(|r| r.starts_with('/'));
        if !in_folder {
            return Profile::minimal(Origin::Other);
        }
        if !app.new_hash {
            return match policy::image_manifest(self.book.table, &app.owner) {
                Some(m) => Profile::parse(m, Origin::Image),
                None => Profile::minimal(Origin::Image),
            };
        }
        match self.store.entries.iter().find(|e| e.name == app.owner && !e.package) {
            Some(e) if app.path == own => Profile::parse(&e.manifest, Origin::Installed),
            _ => Profile::minimal(Origin::Other),
        }
    }

    /// Does app `i` hold `cap` now, without asking anyone?
    pub fn holds(&self, i: usize, cap: Cap) -> bool {
        self.apps[i].as_ref().is_some_and(|a| policy::ask(&self.profile(i), &self.book.grants, &a.owner, cap) == Decision::Allow)
    }

    /// May app `i` use `cap`? Asks the user (the trusted UI) when only the
    /// user can say; anything but a plain yes is a no.
    pub fn may(&mut self, i: usize, cap: Cap) -> bool {
        let Some((pid, owner)) = self.apps[i].as_ref().map(|a| (a.pid, a.owner.clone())) else { return false };
        let d = match policy::ask(&self.profile(i), &self.book.grants, &owner, cap) {
            // An Allow from the prompt is always the user's, and is kept. A
            // Deny may only mean "not now" (the app is not in front, the
            // phone is locked): the prompt keeps a user's no with
            // `record_consent`.
            Decision::Ask => match consent(pid, &owner, cap) {
                Decision::Allow => {
                    self.record_consent(&owner, cap, true);
                    Decision::Allow
                }
                d => d,
            },
            d => d,
        };
        if d != Decision::Allow && !self.book.told.contains(&(pid, cap.bit())) {
            if self.book.told.len() >= 32 {
                self.book.told.remove(0);
            }
            self.book.told.push((pid, cap.bit()));
            log!("policy: {} (pid {}) may not use {}{}", owner, pid, cap.name(), if d == Decision::Ask { " (asking)" } else { "" });
        }
        d == Decision::Allow
    }

    /// May app `i` reach `path` (as the resolver returned it)? Err is the
    /// errno to answer with.
    pub fn path_allowed(&self, i: usize, path: &str, write: bool) -> Result<(), i32> {
        let Some(app) = self.apps[i].as_ref() else { return Err(policy::EACCES) };
        // Only /data's shared files, /tmp's reserved names and the staging file depend on the manifest.
        let needs = path.starts_with("/data/") || path.starts_with("/tmp/.") || path == policy::STAGED;
        let profile = if needs { self.profile(i) } else { Profile::minimal(Origin::Other) };
        policy::path_rule(&app.owner, &profile, path, write)
    }

    /// open(): the path by the namespace, a device node by its capability.
    pub fn open_allowed(&mut self, i: usize, path: &str, write: bool) -> Result<(), i32> {
        self.path_allowed(i, path, write)?;
        match policy::device_cap(path) {
            Err(()) => Err(policy::ENOENT),
            Ok(Some(cap)) if !self.may(i, cap) => Err(policy::EACCES),
            _ => Ok(()),
        }
    }

    /// A directory's entries as app `i` may see them (getdents).
    pub fn visible(&self, i: usize, dir: &str, entries: Vec<(String, bool)>) -> Vec<(String, bool)> {
        if dir != "/data" && dir != "/apps" && dir != "/tmp" {
            return entries;
        }
        entries.into_iter().filter(|(n, _)| self.path_allowed(i, &alloc::format!("{}/{}", dir, n), false).is_ok()).collect()
    }

    /// A /dev/violet/sys command app `i` may not give, answered.
    pub fn sys_refused(&mut self, i: usize, line: &str) -> Option<String> {
        let cap = policy::sys_cap(line)?;
        (!self.may(i, cap)).then(|| alloc::format!("error not allowed (needs {})\n", cap.name()))
    }

    /// The user answered a consent prompt for app `owner` (the trusted UI
    /// calls this): kept in /data/.grants for the next time.
    pub fn record_consent(&mut self, owner: &str, cap: Cap, allow: bool) {
        self.book.grants.record(owner, cap, allow);
        self.book.told.retain(|t| t.1 != cap.bit());
        self.save_grants();
    }

    /// "consent CAP" in /dev/violet/sys: the app asks ahead. Only a power
    /// its manifest declares and the user has yet to answer raises a prompt;
    /// anything else is the policy's answer as it stands.
    pub fn consent_verb(&mut self, i: usize, cap: Cap) -> Decision {
        let Some((pid, owner)) = self.apps[i].as_ref().map(|a| (a.pid, a.owner.clone())) else { return Decision::Deny };
        match policy::ask(&self.profile(i), &self.book.grants, &owner, cap) {
            Decision::Ask => consent(pid, &owner, cap),
            d => d,
        }
    }

    /// A program was installed as app `name` (a new one, or over the old):
    /// it starts with none of the answers given to whatever had that name,
    /// neither those kept in /data/.grants nor this boot's in the trusted UI.
    pub fn forget_consent(&mut self, name: &str) {
        super::trusted_ui::forget_consent(name);
        self.book.grants.forget(name);
        self.save_grants();
    }

    /// An execve by app `i` of `path`, which Violet Shield gave `owner`
    /// (shield.rs `owner_of`) and `new_hash` (written since boot): the
    /// identity the new program runs under. Another app's program started
    /// by a non-core app keeps the caller's owner and counts as written
    /// since boot, so it gets the minimal profile (`profile`: not in its
    /// own folder), no image role in the Shield and not Settings' or the
    /// Agent's path checks (policy.rs `exec_crosses`).
    pub fn exec_identity(&self, i: usize, path: &str, owner: String, new_hash: bool) -> (String, bool) {
        let Some(app) = self.apps[i].as_ref() else { return (owner, true) };
        let core = self.subject(i).role & super::shield::ROLE_CORE != 0;
        if policy::exec_crosses(&app.owner, core, path) {
            (app.owner.clone(), true)
        } else {
            (owner, new_hash)
        }
    }

    /// App `name` was uninstalled: the next app of that name starts with
    /// none of its answers and none of its /data.
    pub fn forget_app(&mut self, name: &str) {
        self.forget_consent(name);
        for (n, dir) in self.fs.list("/data") {
            if policy::owns_data(name, &n) {
                self.remove_tree(&alloc::format!("/data/{}", n), dir);
            }
        }
    }
    fn remove_tree(&mut self, path: &str, dir: bool) {
        if dir {
            for (n, d) in self.fs.list(path) {
                self.remove_tree(&alloc::format!("{}/{}", path, n), d);
            }
        }
        let _ = self.fs.unlink(path, dir);
    }
    fn save_grants(&mut self) {
        let text = self.book.grants.format();
        if let Some(f) = self.fs.writable(GRANTS, true, true, 0) {
            *f = text.into_bytes();
        }
    }
}

/// The trusted UI's consent prompt (trusted_ui.rs). It answers Allow or Deny
/// for this call, or Ask while the prompt waits for the user.
fn consent(pid: u32, owner: &str, cap: Cap) -> Decision {
    super::trusted_ui::request_consent(pid, owner, cap)
}
