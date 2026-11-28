//! The stand-in for runtime/open/syscall.rs's dispatch and device.rs's
//! /dev/violet/sys: the Linux ARM system call numbers a scripted app issues,
//! each calling the same shipped decisions in the same order as the phone
//! (the comments name the line of syscall.rs they follow). Arguments are
//! read from the app's own memory through the shipped address space
//! (procmgr.rs `Space`), so a pointer the app may not read is EFAULT here too.
//!
//! Every refusal comes from policy.rs, manifest.rs, shield.rs, sysui.rs,
//! vma.rs, procmgr.rs, slots.rs or fs.rs. What joins them is copied from the
//! shipped glue (guard.rs `gate_open`, `fd_write_error`, `gate_rename`;
//! syscall.rs `open`, `kill`, `mmap`, `ftruncate`, `fault_signal`'s
//! classification): a change to the shipped copy is not seen here, and
//! docs/test-map.md marks the properties that rest on it as stand-in.
use super::policy::{AccessMode, Action, Cap};
use super::shield::{self, ShieldVerdict};
use super::syscall::*;
use super::vma::{self, Access};
use super::{lifecycle, signal, trusted_ui, File, Runtime};
use crate::kernel::{clock, sys};
use alloc::string::String;
use alloc::vec::Vec;

/// A socket an app holds: its slot in the table Radio serves (slots.rs).
#[derive(Clone)]
pub struct Sock {
    pub slot: usize,
    pub udp: bool,
}
pub struct Listening;

/// The Shield's say (guard.rs `gate`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gate {
    Allow,
    Deny,
    Frozen,
    Killed,
}

/// What an access to app memory ended as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Touched {
    /// The hardware mapping serves it (after a fault the runtime resolved, or without one).
    Ok,
    /// A handler for the signal runs: (signal, si_code).
    Signal(u32, i32),
    /// No handler: the app was ended with this status.
    Ended(i32),
}

const MAP_FIXED: u32 = 0x10;
const MAP_ANONYMOUS: u32 = 0x20;
const USER_TOP: u32 = vma::USER_TOP as u32;

fn align_up(v: u32, a: u32) -> u32 {
    v.wrapping_add(a - 1) & !(a - 1)
}
fn setting_key(k: &str) -> bool {
    (1..=24).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl Runtime {
    fn rstr(&mut self, i: usize, ptr: u32) -> Option<String> {
        let app = self.apps[i].as_mut()?;
        app.space.read_string(&mut self.k, &mut self.pool, ptr.into(), 4096)
    }
    /// syscall.rs `at`.
    fn at(&mut self, i: usize, dirfd: i32, path_ptr: u32) -> Result<String, i32> {
        let path = self.rstr(i, path_ptr).ok_or(EFAULT)?;
        let base = if path.starts_with('/') || dirfd == AT_FDCWD {
            self.apps[i].as_ref().unwrap().cwd.clone()
        } else {
            match self.apps[i].as_ref().unwrap().fds.get(dirfd as usize).and_then(|f| f.as_ref()) {
                Some(File::Dir { path, .. }) | Some(File::Regular { path, .. }) => path.clone(),
                Some(_) => return Err(ENOTDIR),
                None => return Err(EBADF),
            }
        };
        let path = super::fs::normalize(&base, &path);
        if path == trusted_ui::LOCK_FILE {
            return Err(ENOENT);
        }
        Ok(path)
    }
    fn new_fd(&mut self, i: usize, f: File) -> i32 {
        let fds = &mut self.apps[i].as_mut().unwrap().fds;
        let n = fds.iter().position(|f| f.is_none()).unwrap_or_else(|| {
            fds.push(None);
            fds.len() - 1
        });
        fds[n] = Some(f);
        n as i32
    }

    /// guard.rs `gate_with_flags`: the Shield's verdict on `action`.
    pub fn gate(&mut self, i: usize, action: &Action<'_>) -> Gate {
        let who = self.subject(i);
        match self.shield.evaluate(clock::now_ns(), &who, action) {
            ShieldVerdict::Allow | ShieldVerdict::LogAlert { .. } => Gate::Allow,
            ShieldVerdict::Deny { shield_id } => {
                crate::log!("shield {}: denied (pid {})", shield_id, who.app.0);
                Gate::Deny
            }
            ShieldVerdict::FreezeApp { shield_id } => {
                crate::log!("shield {}: froze pid {}", shield_id, who.app.0);
                Gate::Frozen
            }
            ShieldVerdict::KillApp { shield_id } => {
                crate::log!("shield {}: ended pid {}", shield_id, who.app.0);
                self.end(i, 137);
                Gate::Killed
            }
        }
    }
    fn gate_errno(&mut self, i: usize, path: &str, mode: AccessMode) -> Option<i32> {
        match self.gate(i, &Action::OpenPath { path, mode }) {
            Gate::Allow => None,
            _ => Some(EACCES),
        }
    }
    /// guard.rs `gate_open`.
    fn gate_open(&mut self, i: usize, path: &str, flags: u32) -> Gate {
        let exists = self.fs.stat(path).is_some();
        let mode = shield::open_mode(flags, exists);
        if mode != AccessMode::Read && shield::fd_reads(flags) && exists && shield::cross_app(path, self.subject(i).name()) {
            match self.gate(i, &Action::OpenPath { path, mode: AccessMode::Read }) {
                Gate::Allow => {}
                other => return other,
            }
        }
        self.gate(i, &Action::OpenPath { path, mode })
    }

    /// guard.rs `fd_write_error`: a write or truncate through a descriptor
    /// only if it was opened for writing, and the built-in rules are asked
    /// again (the file may be another app's program).
    fn fd_write_error(&mut self, i: usize, path: &str, flags: u32) -> Option<i32> {
        if !shield::fd_writes(flags) {
            return Some(EBADF);
        }
        let who = self.subject(i);
        if self.shield.write_refused(clock::now_ns(), &who, path) {
            crate::log!("shield: denied write to {} through an open file (pid {})", path, who.app.0);
            return Some(EACCES);
        }
        None
    }

    /// An app ends (mod.rs `reap`, the part the tests see).
    pub fn end(&mut self, i: usize, status: i32) {
        let Some(app) = self.apps[i].as_mut() else { return };
        app.exit_code = Some(status);
        crate::log!("{} (pid {}) exited with status {}", app.name, app.pid, status);
        let pid = app.pid;
        app.space.destroy(&mut self.k, &mut self.pool);
        // powerctl.rs keeps its holders in a static (the runtime is one
        // thread): only the one scenario that serves Power touches it.
        if crate::kernel::ipc::served(sys::SLOT_RUNTIME) {
            super::powerctl::app_gone(pid);
        }
        for s in 0..super::slots::SLOTS {
            if self.slots.owner(s) == Some(pid) {
                self.slots.free(s);
            }
        }
        self.names.purge(pid);
        if self.foreground == Some(i) {
            self.foreground = None;
        }
        self.apps[i] = None;
    }

    /// One Linux ARM system call by app `i`: number `n`, arguments r0..r5.
    /// Returns what the app would see in r0.
    pub fn syscall(&mut self, i: usize, n: u32, a: [u32; 6]) -> i32 {
        if self.apps[i].is_none() {
            return ESRCH;
        }
        // A system call shows the program got past a fault (signal.rs `fault_reset`).
        if signal::fault_reset(n) {
            self.apps[i].as_mut().unwrap().thread.faults = 0;
        }
        match n {
            // open, openat (syscall.rs `open`)
            5 => self.open(i, AT_FDCWD, a[0], a[1]),
            322 => self.open(i, a[0] as i32, a[1], a[2]),
            6 => {
                let fds = &mut self.apps[i].as_mut().unwrap().fds;
                match fds.get_mut(a[0] as usize).and_then(|f| f.take()) {
                    Some(File::Socket { kind }) => {
                        self.slots.free(kind.slot);
                        0
                    }
                    Some(_) => 0,
                    None => EBADF,
                }
            }
            3 => self.read(i, a[0] as i32, a[1], a[2]),
            4 => self.write(i, a[0] as i32, a[1], a[2]),
            // access, faccessat, stat64, lstat64, fstatat64: seen only if the policy lets it be.
            33 | 334 | 195 | 196 | 327 => {
                let (dirfd, ptr) = if matches!(n, 334 | 327) { (a[0] as i32, a[1]) } else { (AT_FDCWD, a[0]) };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return e };
                if let Err(e) = self.path_allowed(i, &p, false) { return e; }
                if self.fs.stat(&p).is_some() { 0 } else { ENOENT }
            }
            12 => {
                let p = match self.at(i, AT_FDCWD, a[0]) { Ok(p) => p, Err(e) => return e };
                if let Err(e) = self.path_allowed(i, &p, false) { return e; }
                match self.fs.stat(&p) {
                    Some(s) if s.mode & super::fs::S_IFMT == super::fs::S_IFDIR => {
                        self.apps[i].as_mut().unwrap().cwd = p;
                        0
                    }
                    Some(_) => ENOTDIR,
                    None => ENOENT,
                }
            }
            39 | 323 => {
                let (dirfd, ptr) = if n == 39 { (AT_FDCWD, a[0]) } else { (a[0] as i32, a[1]) };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return e };
                if self.fs.stat(&p).is_none() { if let Some(e) = self.gate_errno(i, &p, AccessMode::Write) { return e; } }
                let seen = self.fs.stat(&p).is_some() && self.path_allowed(i, &p, false).is_ok();
                if !seen { if let Err(e) = self.path_allowed(i, &p, true) { return e; } }
                match self.fs.mkdir(&p) { Ok(()) => 0, Err(e) => e }
            }
            40 | 10 | 328 => {
                let (dirfd, ptr, dir) = match n {
                    40 => (AT_FDCWD, a[0], true),
                    10 => (AT_FDCWD, a[0], false),
                    _ => (a[0] as i32, a[1], a[2] & 0x200 != 0),
                };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return e };
                if let Some(e) = self.gate_errno(i, &p, AccessMode::Write) { return e; }
                if let Err(e) = self.path_allowed(i, &p, true) { return e; }
                match self.fs.unlink(&p, dir) { Ok(()) => 0, Err(e) => e }
            }
            38 | 329 | 382 => {
                let (fa, fp, ta, tp) = if n == 38 { (AT_FDCWD, a[0], AT_FDCWD, a[1]) } else { (a[0] as i32, a[1], a[2] as i32, a[3]) };
                let from = match self.at(i, fa, fp) { Ok(p) => p, Err(e) => return e };
                let to = match self.at(i, ta, tp) { Ok(p) => p, Err(e) => return e };
                // guard.rs `gate_rename`
                if shield::cross_app(&from, self.subject(i).name()) {
                    if let Some(e) = self.gate_errno(i, &from, AccessMode::Read) { return e; }
                }
                if let Some(e) = self.gate_errno(i, &from, AccessMode::Write) { return e; }
                if let Some(e) = self.gate_errno(i, &to, AccessMode::Write) { return e; }
                if let Err(e) = self.path_allowed(i, &from, true).and(self.path_allowed(i, &to, true)) { return e; }
                match self.fs.rename(&from, &to) { Ok(()) => 0, Err(e) => e }
            }
            // truncate, ftruncate, truncate64, ftruncate64 (syscall.rs 92 | 93 | 193 | 194)
            92 | 93 | 193 | 194 => {
                let len = if matches!(n, 92 | 93) { u64::from(a[1]) } else { u64::from(a[2]) | u64::from(a[3]) << 32 };
                let (path, flags) = if matches!(n, 93 | 194) {
                    match self.apps[i].as_ref().unwrap().fds.get(a[0] as usize).and_then(|f| f.as_ref()) {
                        Some(File::Regular { path, flags, .. }) => (path.clone(), Some(*flags)),
                        Some(_) => return EINVAL,
                        None => return EBADF,
                    }
                } else {
                    match self.at(i, AT_FDCWD, a[0]) { Ok(p) => (p, None), Err(e) => return e }
                };
                match flags {
                    // By path: the Shield (guard.rs `gate_change`), then the policy.
                    None => {
                        if let Some(e) = self.gate_errno(i, &path, AccessMode::Write) { return e; }
                        if let Err(e) = self.path_allowed(i, &path, true) { return e; }
                    }
                    // By descriptor: only if it was opened for writing, and the rules are asked again.
                    Some(flags) => {
                        if let Some(e) = self.fd_write_error(i, &path, flags) { return e; }
                    }
                }
                match self.fs.writable(&path, false, false, 0) {
                    Some(data) => {
                        data.resize(len as usize, 0);
                        0
                    }
                    None => ENOENT,
                }
            }
            // socket: a network socket needs the Net capability, then a slot.
            281 => {
                if a[0] != 1 && !self.may(i, Cap::Net) {
                    return if trusted_ui::pending(self.apps[i].as_ref().unwrap().pid, Cap::Net) { EAGAIN } else { EACCES };
                }
                let pid = self.apps[i].as_ref().unwrap().pid;
                match self.slots.alloc(pid, super::slots::SLOTS, self.app_limit()) {
                    Ok(slot) => self.new_fd(i, File::Socket { kind: Sock { slot, udp: a[1] & 0xf == 2 } }),
                    Err(e) => e,
                }
            }
            // ---- memory (syscall.rs "memory") ----
            45 => {
                let app = self.apps[i].as_mut().unwrap();
                app.space.set_brk(&mut self.k, &mut self.pool, a[0].into()) as i32
            }
            192 => self.mmap(i, a[0], a[1], a[2], a[3], a[4] as i32, a[5].wrapping_mul(4096)),
            91 => {
                if a[0] & 0xfff != 0 || a[1] == 0 {
                    return EINVAL;
                }
                let end = a[0].saturating_add(align_up(a[1], 4096)).min(USER_TOP);
                let app = self.apps[i].as_mut().unwrap();
                if app.space.pinned(a[0].into(), end.into()) {
                    return EINVAL;
                }
                app.space.remove(&mut self.k, &mut self.pool, a[0].into(), end.into());
                0
            }
            125 => {
                if a[2] & 6 == 6 && self.gate(i, &Action::MapWriteExec { addr: a[0], len: a[1] }) != Gate::Allow {
                    return EACCES;
                }
                let end = a[0].saturating_add(align_up(a[1], 4096));
                match self.apps[i].as_mut().unwrap().space.protect(a[0].into(), end.into(), a[2] & 7) {
                    Ok(()) => 0,
                    Err(e) => e,
                }
            }
            // getrandom: all of it must be writable before any of it is
            // written; nothing unless the generator was seeded.
            384 => {
                let len = a[1].min(256 << 10);
                if !self.apps[i].as_ref().unwrap().space.writable(a[0].into(), len as usize) {
                    return EFAULT;
                }
                let mut b = alloc::vec![0u8; len as usize];
                if !self.entropy.fill(&mut b) {
                    return EIO;
                }
                let app = self.apps[i].as_mut().unwrap();
                if app.space.write(&mut self.k, &mut self.pool, a[0].into(), &b) { len as i32 } else { EFAULT }
            }
            // exit, exit_group
            1 | 248 => {
                self.end(i, a[0] as i32);
                0
            }
            20 => self.apps[i].as_ref().unwrap().pid as i32,
            37 => self.kill(i, a[0] as i32, a[1]),
            174 => self.sigaction(i, a[0], a[1], a[3]),
            _ => ENOSYS,
        }
    }

    /// syscall.rs `sigaction` (setting an action).
    fn sigaction(&mut self, i: usize, sig: u32, act: u32, size: u32) -> i32 {
        if signal::bit(sig) == 0 || size != 8 {
            return EINVAL;
        }
        if act == 0 {
            return 0;
        }
        let mut b = [0u8; 20];
        {
            let app = self.apps[i].as_mut().unwrap();
            if !app.space.read(&mut self.k, &mut self.pool, act.into(), &mut b) {
                return EFAULT;
            }
        }
        if signal::bit(sig) & signal::UNBLOCKABLE != 0 {
            return EINVAL;
        }
        let w = |k: usize| u32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap());
        let action = signal::Action { handler: w(0).into(), flags: w(1), restorer: w(2).into(), mask: u64::from(w(3)) | u64::from(w(4)) << 32 };
        self.apps[i].as_mut().unwrap().sig.set_action(sig, action);
        0
    }
    /// syscall.rs `kill` and `descends`: an app may signal itself and its
    /// descendants, nothing else.
    fn kill(&mut self, i: usize, pid: i32, sig: u32) -> i32 {
        if sig > signal::NSIG {
            return EINVAL;
        }
        let me = self.apps[i].as_ref().unwrap().pid;
        let ppid_of = |rt: &Runtime, pid: u32| rt.index_of(pid).and_then(|k| rt.apps[k].as_ref()).map_or(0, |a| a.sig.ppid);
        let descends = |rt: &Runtime, pid: u32| {
            let (mut chain, mut p) = (Vec::new(), pid);
            for _ in 0..super::MAX_APPS * 2 {
                p = ppid_of(rt, p);
                if p == 0 {
                    break;
                }
                chain.push(p);
            }
            signal::may_signal(me, pid, &chain)
        };
        let targets: Vec<usize> = if pid > 0 {
            self.index_of(pid as u32).into_iter().collect()
        } else {
            (0..super::MAX_APPS).filter(|&k| self.apps[k].as_ref().is_some_and(|a| (k == i && pid != -1) || (k != i && descends(self, a.pid)))).collect()
        };
        if targets.is_empty() {
            return ESRCH;
        }
        if targets.iter().any(|&k| !descends(self, self.apps[k].as_ref().unwrap().pid)) {
            return EPERM;
        }
        if sig == 0 {
            return 0;
        }
        for k in targets {
            let app = self.apps[k].as_mut().unwrap();
            let action = app.sig.action(sig);
            match signal::disposition(&action, sig) {
                signal::Disposition::Handle => app.delivered.push((sig, action.handler as u32)),
                signal::Disposition::Terminate { core } => {
                    crate::log!("{} (pid {}) killed by signal {}", app.name, app.pid, sig);
                    let (pid, ppid, status) = (app.pid, app.sig.ppid, signal::signaled(sig, core));
                    // The parent finds the wait status (signal.rs `Child`).
                    if let Some(parent) = self.index_of(ppid).and_then(|p| self.apps[p].as_mut()) {
                        parent.sig.children.push(signal::Child { pid, status: Some(status), exit_signal: signal::SIGCHLD });
                    }
                    self.end(k, 128 + sig as i32);
                }
                _ => {}
            }
        }
        0
    }

    /// syscall.rs `open`.
    fn open(&mut self, i: usize, dirfd: i32, path_ptr: u32, flags: u32) -> i32 {
        let path = match self.at(i, dirfd, path_ptr) { Ok(p) => p, Err(e) => return e };
        let mode = shield::open_mode(flags, self.fs.stat(&path).is_some());
        match self.gate_open(i, &path, flags) {
            Gate::Allow => {}
            Gate::Killed => return ESRCH,
            _ => return EACCES,
        }
        if let Err(e) = self.open_allowed(i, &path, mode == AccessMode::Write) {
            let asking = super::policy::device_cap(&path).ok().flatten().is_some_and(|c| trusted_ui::pending(self.apps[i].as_ref().unwrap().pid, c));
            return if asking { EAGAIN } else { e };
        }
        if let Some(node) = path.strip_prefix("/dev/violet/") {
            if matches!(node, "mic" | "camera" | "hci" | "audio") {
                self.apps[i].as_mut().unwrap().reason = true;
            }
            return self.new_fd(i, File::Device { node: String::from(node), answer: Vec::new() });
        }
        let st = self.fs.stat(&path);
        if st.is_some_and(|s| s.mode & super::fs::S_IFMT == super::fs::S_IFDIR) {
            let entries = self.visible(i, &path, self.fs.list(&path));
            return self.new_fd(i, File::Dir { path, entries });
        }
        let write = flags & 3 != O_RDONLY || flags & O_TRUNC != 0;
        if st.is_none() && flags & O_CREAT == 0 {
            return ENOENT;
        }
        if write || st.is_none() {
            if self.fs.writable(&path, flags & O_CREAT != 0, flags & O_TRUNC != 0, 0).is_none() {
                return if st.is_none() { ENOENT } else { EACCES };
            }
        }
        self.new_fd(i, File::Regular { path, flags, pos: 0 })
    }

    fn read(&mut self, i: usize, fd: i32, buf: u32, len: u32) -> i32 {
        let Some(Some(f)) = self.apps[i].as_mut().unwrap().fds.get_mut(fd as usize) else { return EBADF };
        let data = match f {
            File::Regular { path, flags, pos } => {
                if !shield::fd_reads(*flags) {
                    return EBADF;
                }
                let all = self.fs.contents(path).unwrap_or(&[]);
                let part = all[(*pos).min(all.len())..(*pos + len as usize).min(all.len())].to_vec();
                *pos += part.len();
                part
            }
            File::Device { answer, .. } => core::mem::take(answer),
            _ => return EINVAL,
        };
        let app = self.apps[i].as_mut().unwrap();
        // Copies into app memory go by the app's own regions (procmgr.rs `write`).
        if app.space.write(&mut self.k, &mut self.pool, buf.into(), &data) { data.len() as i32 } else { EFAULT }
    }

    fn write(&mut self, i: usize, fd: i32, buf: u32, len: u32) -> i32 {
        let mut data = alloc::vec![0u8; len.min(64 << 10) as usize];
        {
            let app = self.apps[i].as_mut().unwrap();
            if !app.space.read(&mut self.k, &mut self.pool, buf.into(), &mut data) {
                return EFAULT;
            }
        }
        let Some(Some(f)) = self.apps[i].as_ref().unwrap().fds.get(fd as usize).cloned() else { return EBADF };
        match f {
            File::Regular { path, flags, pos } => {
                if let Some(e) = self.fd_write_error(i, &path, flags) {
                    return e;
                }
                let Some(file) = self.fs.writable(&path, false, false, 0) else { return EBADF };
                let end = pos + data.len();
                if file.len() < end {
                    file.resize(end, 0);
                }
                file[pos..end].copy_from_slice(&data);
                if let Some(Some(File::Regular { pos, .. })) = self.apps[i].as_mut().unwrap().fds.get_mut(fd as usize) {
                    *pos = end;
                }
                data.len() as i32
            }
            File::Device { node, .. } if node == "sys" => {
                let line = String::from_utf8_lossy(&data).trim().to_string();
                let answer = self.system(i, &line);
                if let Some(Some(File::Device { answer: a, .. })) = self.apps.get_mut(i).and_then(|a| a.as_mut()).and_then(|a| a.fds.get_mut(fd as usize)) {
                    *a = answer.into_bytes();
                }
                data.len() as i32
            }
            File::Device { .. } => data.len() as i32,
            _ => EINVAL,
        }
    }

    /// syscall.rs `mmap` (anonymous and file mappings).
    fn mmap(&mut self, i: usize, addr: u32, len: u32, prot: u32, flags: u32, fd: i32, offset: u32) -> i32 {
        if len == 0 || len > 0x8000_0000 {
            return EINVAL;
        }
        if prot & 6 == 6 && self.gate(i, &Action::MapWriteExec { addr, len }) != Gate::Allow {
            return EACCES;
        }
        let len = align_up(len, 4096);
        let (addr, len) = (u64::from(addr), u64::from(len));
        let fixed = flags & MAP_FIXED != 0;
        let app = self.apps[i].as_mut().unwrap();
        let start = if fixed {
            if addr & 0xfff != 0 || addr.checked_add(len).map_or(true, |e| e > u64::from(USER_TOP)) {
                return EINVAL;
            }
            addr
        } else {
            match app.space.free_range(len) {
                Some(a) => a,
                None => return ENOMEM,
            }
        };
        // The W^X policy (vma.rs).
        if app.space.refuses(start, start + len, prot & 7) {
            return EACCES;
        }
        if fixed {
            app.space.remove(&mut self.k, &mut self.pool, start, start + len);
        }
        app.space.add(start, start + len, prot & 7);
        if flags & MAP_ANONYMOUS == 0 {
            let (path, fflags) = match app.fds.get(fd as usize).and_then(|f| f.as_ref()) {
                Some(File::Regular { path, flags, .. }) => (path.clone(), *flags),
                Some(_) => return ENODEV,
                None => return EBADF,
            };
            if !shield::fd_reads(fflags) {
                return EACCES;
            }
            let data = self.fs.contents(&path).map(|c| {
                let s = (offset as usize).min(c.len());
                c[s..(s + len as usize).min(c.len())].to_vec()
            });
            if let Some(data) = data {
                let app = self.apps[i].as_mut().unwrap();
                if !app.space.poke(&mut self.k, &mut self.pool, start, &data) {
                    return ENOMEM;
                }
            }
        }
        start as i32
    }

    /// The app reads, writes or runs the byte at `va`: what the MMU and then
    /// the runtime's fault handler make of it (mod.rs, FAULT_VM). The fake
    /// hardware holds the mappings procmgr.rs made; an access they do not
    /// serve is a fault, which `Space::fault` resolves or refuses.
    pub fn touch(&mut self, i: usize, va: u32, access: Access) -> Touched {
        let va = u64::from(va);
        let (write, exec) = (access == Access::Write, access == Access::Exec);
        let pd = self.apps[i].as_ref().unwrap().space.pd;
        if sys::mmu(pd, va, write, exec).is_ok() {
            return Touched::Ok;
        }
        let mut resolved = self.apps[i].as_mut().unwrap().space.fault(&mut self.k, &mut self.pool, va, access);
        // Out of pages: close an app that matters less and try again
        // (lifecycle.rs `fault_victim`).
        let valid = self.apps[i].as_ref().unwrap().space.find(va).is_some_and(|v| access.allowed(v.prot));
        while !resolved && valid && self.pool.available() == 0 {
            match self.fault_victim_for(i) {
                Some(v) if v != i => self.close_for_memory(v),
                _ => break,
            }
            resolved = self.apps[i].as_mut().unwrap().space.fault(&mut self.k, &mut self.pool, va, access);
        }
        if !resolved && valid && self.pool.available() == 0 {
            self.close_for_memory(i);
            return Touched::Ended(lifecycle::EXIT_CLOSED);
        }
        if resolved {
            assert!(sys::mmu(pd, va, write, exec).is_ok(), "the fault handler said yes but the hardware would fault again at {va:#x}");
            return Touched::Ok;
        }
        // syscall.rs `fault_signal`, line for line (it is not host-buildable;
        // a copy, so docs/test-map.md lists what rests on it as stand-in only):
        // no region MAPERR, a region that refuses the access ACCERR, an
        // allowed access the hardware still refuses SIGBUS; caught only by a
        // handler (> SIG_IGN) the thread does not block, FAULT_LIMIT times.
        let (sig, code) = match self.apps[i].as_ref().unwrap().space.find(va) {
            None => (signal::SIGSEGV, signal::SEGV_MAPERR),
            Some(v) if !access.allowed(v.prot) => (signal::SIGSEGV, signal::SEGV_ACCERR),
            Some(_) => (signal::SIGBUS, signal::BUS_ADRERR),
        };
        let app = self.apps[i].as_mut().unwrap();
        let action = app.sig.action(sig);
        app.thread.faults += 1;
        if action.handler > signal::SIG_IGN && app.thread.blocked & signal::bit(sig) == 0 && app.thread.faults <= signal::FAULT_LIMIT {
            app.delivered.push((sig, action.handler as u32));
            return Touched::Signal(sig, code);
        }
        crate::log!("{} (pid {}): bad memory access ({:?}) at {:#x}", app.name, app.pid, access, va);
        self.end(i, 139);
        Touched::Ended(139)
    }

    /// device.rs `system`: a command written to /dev/violet/sys.
    pub fn system(&mut self, i: usize, line: &str) -> String {
        // The capability each command needs (policy.rs `sys_cap`).
        if let Some(refused) = self.sys_refused(i, line) {
            return refused;
        }
        // Who may give it, in front or not, locked or not (sysui.rs `may`).
        if let Err(refused) = self.ui_may(i, line) {
            return String::from(refused);
        }
        let mut words = line.split_whitespace();
        let verb = words.next().unwrap_or("");
        match verb {
            "home" => {
                if self.foreground == Some(i) {
                    self.set_foreground(self.home.filter(|&h| h != i));
                }
                String::from("ok\n")
            }
            "open" | "launch" => match words.next() {
                Some(path) => match self.launch(path, Some(i)) {
                    Ok(n) => {
                        self.set_foreground(Some(n));
                        String::from("ok\n")
                    }
                    Err(e) => alloc::format!("error {}\n", e),
                },
                None => String::from("error\n"),
            },
            "get" => match words.next().filter(|k| setting_key(k)) {
                Some(key) => alloc::format!("{}\n", self.setting(key).unwrap_or_default()),
                None => String::from("error\n"),
            },
            "set" => match (words.next().filter(|k| setting_key(k)), words.next()) {
                (Some(key), Some(value)) if value.len() <= 64 && words.next().is_none() => {
                    let mut lines: Vec<String> = self.settings_text().lines().filter(|l| l.split_once('=').map_or(false, |(k, _)| k != key)).map(String::from).collect();
                    lines.push(alloc::format!("{}={}", key, value));
                    let text = lines.join("\n") + "\n";
                    match self.fs.writable(super::manifest::SETTINGS, true, true, 0) {
                        Some(f) if text.len() <= 4096 => {
                            f.extend_from_slice(text.as_bytes());
                            String::from("ok\n")
                        }
                        _ => String::from("error\n"),
                    }
                }
                _ => String::from("error\n"),
            },
            "passcode" | "lock" | "consent" | "notify" | "notifications" => self.ui_sys(i, verb, line),
            "wakelock" | "wakelocks" | "power" if crate::kernel::ipc::served(sys::SLOT_RUNTIME) => {
                let (pid, name) = self.apps[i].as_ref().map(|a| (a.pid, a.name.clone())).unwrap();
                super::powerctl::command(pid, &name, line)
            }
            // A command the policy and the trusted UI let through and that
            // this stand-in does not carry out.
            _ => String::from("ok\n"),
        }
    }
    fn settings_text(&self) -> String {
        self.fs.contents(super::manifest::SETTINGS).map(|d| String::from_utf8_lossy(d).into_owned()).unwrap_or_default()
    }
    fn setting(&self, key: &str) -> Option<String> {
        self.settings_text().lines().find_map(|l| l.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| String::from(v)))
    }
}
