//! Single choke-point policy hook for the Open Runtime.
//!
//! Because seL4 isolates every app in hardware and stamps every trapped syscall
//! with an unforgeable badge (`AppId`), all external access flows through
//! `Policy::check`. Today VioletOS ships `OpenPolicy` (100% open); a future
//! policy layer plugs in here with zero changes to apps or hardware services.

/// Unforgeable caller identity assigned by seL4 endpoint badging.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AppId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMode {
    Read,
    Write,
    Exec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Screen,
    Audio,
    Events,
    SystemControl,
}

/// Every externally visible action an app can ask the Open Runtime to perform.
#[derive(Clone, Copy, Debug)]
pub enum Action<'a> {
    OpenPath { path: &'a str, mode: AccessMode },
    OpenDevice(DeviceKind),
    Connect { host: &'a str, port: u16 },
    Bind { port: u16 },
    BackgroundAudio,
    AllocMemory { total_bytes: usize },
    SpawnThread { total_threads: usize },
    SpawnApp { path: &'a str },
    SystemControl { command: &'a str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Returned to the app as standard POSIX `EACCES` (-13).
    Deny,
    /// Future: suspend caller's reply cap while system UI asks the user once.
    Prompt,
}

pub trait Policy {
    fn check(&self, app: AppId, action: &Action<'_>) -> Decision;
}

/// Default VioletOS policy: assume every app is trustworthy and allow everything.
pub struct OpenPolicy;

impl Policy for OpenPolicy {
    #[inline]
    fn check(&self, _app: AppId, _action: &Action<'_>) -> Decision {
        Decision::Allow
    }
}

// Future sketch (not active today):
//
// pub struct ProfilePolicy { /* per-AppId allowed path prefixes, domains, RAM cap */ }
//
// Mixing trusted personal scripts and sandboxed third-party apps simply assigns
// `Open` vs `Sandboxed` profiles per `AppId` while using the exact same app ABI.
