//! The stand-in for the part of runtime/open/mod.rs that applies
//! lifecycle.rs to running apps (own_facts, life_facts, root, candidates,
//! fault_victim_for, close_for_memory, park, unpark, lifecycle_tick,
//! update_pressure) and for `launch`. Each function follows the one of the
//! same name there; the decisions are lifecycle.rs's.
use super::lifecycle::{self, Pressure, State};
use super::{procmgr, signal, vma, App, Runtime, INSTALLER, MAX_APPS, SETTINGS_APP};
use crate::kernel::clock;
use alloc::string::String;
use alloc::vec::Vec;

impl Runtime {
    /// How many apps may run at once (mod.rs `app_limit`; the emulator's is 8).
    pub fn app_limit(&self) -> usize {
        8
    }
    fn own_facts(&self, i: usize, app: &App) -> lifecycle::Facts {
        let reason = app.path == INSTALLER || app.reason;
        lifecycle::Facts { front: self.foreground == Some(i), home: self.home == Some(i), reason, shield: false, left_ns: app.used_ns }
    }
    pub fn life_facts(&self, i: usize, app: &App) -> lifecycle::Facts {
        let own = self.own_facts(i, app);
        let r = self.root(i);
        match self.apps[r].as_ref() {
            Some(top) if r != i => lifecycle::inherit(own, &self.own_facts(r, top)),
            _ => own,
        }
    }
    pub fn root(&self, i: usize) -> usize {
        let index_of = |pid: u32| (0..MAX_APPS).find(|&p| pid != 0 && self.apps[p].as_ref().is_some_and(|q| q.pid == pid));
        let parent: Vec<Option<usize>> = (0..MAX_APPS).map(|n| self.apps[n].as_ref().and_then(|a| index_of(a.sig.ppid))).collect();
        lifecycle::root_of(&parent, i)
    }
    pub fn candidates(&self, caller: Option<usize>) -> Vec<Option<lifecycle::Candidate>> {
        let now = clock::now_ns();
        let family = caller.map(|c| self.root(c));
        (0..MAX_APPS)
            .map(|i| {
                let app = self.apps[i].as_ref()?;
                let f = self.life_facts(i, app);
                let r = self.root(i);
                let kept = self.foreground == Some(r) || self.home == Some(r) || Some(r) == family;
                Some(lifecycle::Candidate {
                    state: lifecycle::next_state(&f, now, self.life.pressure),
                    protected: f.front || f.home || f.shield || kept || app.path == INSTALLER,
                    core: app.path == SETTINGS_APP,
                    audio: app.fds.iter().flatten().any(|f| matches!(f, super::File::Device { node, .. } if node == "audio")),
                    used_ns: app.used_ns,
                    heap: 0,
                })
            })
            .collect()
    }
    pub fn fault_victim_for(&self, i: usize) -> Option<usize> {
        let r = self.root(i);
        let privileged = self.foreground == Some(r) || self.home == Some(r) || self.apps[r].as_ref().is_some_and(|a| a.path == INSTALLER);
        lifecycle::fault_victim(&self.candidates(Some(i)), privileged)
    }
    pub fn close_for_memory(&mut self, i: usize) {
        let Some(app) = self.apps[i].as_ref() else { return };
        let (name, kib) = (app.name.clone(), app.space.usage().0 * (crate::kernel::sys::PAGE >> 10));
        let place = if self.foreground == Some(i) { "in front" } else { "in the background" };
        // Background apps hear LOW_MEMORY before anything is closed.
        self.update_pressure();
        crate::log!("low on app memory: closed {} {}", name, place);
        self.end(i, lifecycle::EXIT_CLOSED);
        self.life.note(lifecycle::Closed { name, reason: "low memory", kib, at_s: clock::now_ns() / 1_000_000_000 });
    }
    pub fn lifecycle_tick(&mut self) {
        let now = clock::now_ns();
        if let Some(f) = self.foreground {
            if let Some(app) = self.apps[f].as_mut() {
                app.life.state = State::Foreground;
            }
        }
        if now.wrapping_sub(self.life.last_ns) < 1_000_000_000 {
            return;
        }
        self.life.last_ns = now;
        let p = self.update_pressure();
        for i in 0..MAX_APPS {
            let Some(app) = self.apps[i].as_ref() else { continue };
            let (cur, next) = (app.life.state, lifecycle::next_state(&self.life_facts(i, app), now, p));
            if cur == next {
                continue;
            }
            if next == State::Frozen {
                crate::log!("lifecycle: froze {} (pid {}) in the background", app.name, app.pid);
            }
            self.apps[i].as_mut().unwrap().life.state = next;
        }
        if p == Pressure::Critical {
            if let Some(v) = lifecycle::pick_victim(&self.candidates(None), 1) {
                self.close_for_memory(v);
            }
        }
    }
    pub fn update_pressure(&mut self) -> Pressure {
        let was = self.life.pressure;
        let p = lifecycle::pressure(self.pool.available(), self.reserve, was);
        self.life.pressure = p;
        if was == Pressure::Normal && p != Pressure::Normal {
            crate::log!("lifecycle: app memory low ({} KiB free): background apps asked to free memory", self.pool.available() * (crate::kernel::sys::PAGE >> 10));
            for i in 0..MAX_APPS {
                if self.foreground != Some(i) {
                    if let Some(app) = self.apps[i].as_mut() {
                        app.low_memory += 1;
                    }
                }
            }
        }
        p
    }
    /// device.rs `set_foreground`: the app leaving the front starts its grace now.
    pub fn set_foreground(&mut self, next: Option<usize>) {
        let now = clock::now_ns();
        if let Some(old) = self.foreground.filter(|&o| Some(o) != next) {
            if let Some(app) = self.apps[old].as_mut() {
                app.used_ns = now;
            }
        }
        self.foreground = next;
        if let Some(app) = next.and_then(|n| self.apps[n].as_mut()) {
            app.used_ns = now;
            app.life.state = State::Foreground;
        }
    }

    /// mod.rs `launch` and `start`: the program's owner and identity
    /// (shield.rs, manifest.rs), a random layout from the generator
    /// (entropy.rs, vma.rs), and the shipped loader (procmgr.rs `load`). No
    /// app starts while the generator is unseeded.
    pub fn launch(&mut self, path: &str, caller: Option<usize>) -> Result<usize, String> {
        let image = self.fs.contents(path).ok_or_else(|| alloc::format!("{} not found", path))?.to_vec();
        if !self.entropy.seeded() {
            return Err(String::from("the random generator is not seeded"));
        }
        let slot = (0..self.app_limit()).find(|&n| self.apps[n].is_none()).ok_or("too many apps")?;
        let digest = super::sha256(&image);
        let new_hash = self.fs.from_layer(path);
        let owner = self.shield_owner(path, &digest, caller);
        let (owner, new_hash) = match caller {
            Some(c) if self.system_caller(c) => (owner, new_hash),
            Some(c) => self.exec_identity(c, path, owner, new_hash),
            None => (owner, new_hash),
        };
        let mut random = [0u8; 40];
        self.entropy.fill(&mut random);
        let layout = vma::Layout::random(&random[..24]);
        let mut at_random = [0u8; 16];
        at_random.copy_from_slice(&random[24..]);
        let mut space = procmgr::Space::new(&mut self.k, 1).map_err(|e| alloc::format!("no address space ({})", e))?;
        let args = [String::from(path)];
        let loaded = procmgr::load(&mut space, &mut self.k, &mut self.pool, &image, None, &args, &[], at_random, layout).map_err(String::from)?;
        // mod.rs `start`: the trampoline page, the runtime's own region.
        space.add(layout.stubs, layout.stubs + crate::kernel::sys::PAGE as u32, procmgr::PROT_READ | procmgr::PROT_EXEC);
        if space.resolve(&mut self.k, &mut self.pool, layout.stubs).is_none() {
            space.destroy(&mut self.k, &mut self.pool);
            return Err(String::from("out of memory"));
        }
        let pid = self.next_pid;
        self.next_pid += 1;
        let name = String::from(path.rsplit('/').next().unwrap_or(path));
        let now = clock::now_ns();
        // Scratch for scripted calls' strings, below the stack pointer.
        let scratch = (loaded.sp - 0x2000) & !0xfff;
        crate::log!("started {} as pid {}", path, pid);
        self.apps[slot] = Some(App {
            pid,
            ppid: caller.and_then(|c| self.apps[c].as_ref()).map_or(0, |a| a.pid),
            name,
            path: String::from(path),
            owner,
            digest,
            new_hash,
            cwd: String::from("/"),
            space,
            fds: Vec::new(),
            exit_code: None,
            left_ns: now,
            used_ns: now,
            reason: false,
            life: lifecycle::Life::default(),
            sig: signal::Proc::new(0),
            thread: signal::Thread::new(0),
            delivered: Vec::new(),
            low_memory: 0,
            scratch,
            random: at_random,
            entry: loaded.entry,
            sp: loaded.sp,
        });
        Ok(slot)
    }
    /// Home, Settings or the Agent from the image asked (they open apps for the user).
    fn system_caller(&self, c: usize) -> bool {
        self.subject(c).role & super::shield::ROLE_CORE != 0
    }
}
