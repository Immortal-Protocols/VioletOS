//! What Runtime refuses while the phone is locked (runtime/process.rs).
//!
//! The lock screen is drawn by the Open Runtime (runtime/open/trusted_ui.rs),
//! which tells Runtime with every focus poll whether it is up. Runtime also
//! counts the phone as locked from the moment it sleeps until the Open
//! Runtime has seen that sleep and answered (a passcode locks on screen-off).
//! While locked and awake the open apps are in front (Runtime moves the focus
//! there at the poll, at the wake, and refuses to move it away), so no
//! built-in screen (the launcher, an app from the catalog) is shown or
//! touched. Below is what is refused from everyone else on top of that:
//! nothing else comes to the front, only the Open Runtime draws or uses
//! sound, and the processes that are not on screen do not change what is
//! installed or Phone Internet. Reading (GET), files, the radio switch (the
//! launcher's saved-choice restore and USB Wi-Fi jobs, which show nothing and
//! are not retried) and sleep and wake are not refused.

/// The request words this file judges (runtime/protocol.rs; process.rs
/// checks at build time that they are the same).
pub const EXECUTE: u32 = 3;
pub const OPEN: u32 = 4;
pub const QUIT: u32 = 10;
pub const INSTALL: u32 = 17;
pub const REMOVE: u32 = 18;
pub const RELAY: u32 = 29;
pub const PRESENT: u32 = 6;
pub const VOLUME: u32 = 30;
pub const PLAYBACK: u32 = 31;
pub const MICROPHONE: u32 = 32;

/// The value the Open Runtime sends with GET FOCUS while its lock screen is up.
pub const LOCKED: u32 = 1;

/// Whether a request is refused while the phone is locked. `from_open`: it
/// comes from the Open Runtime (the only client on screen then).
pub fn locked_refuses(op: u32, key: u32, from_open: bool) -> bool {
    if op != EXECUTE {
        return false;
    }
    match key {
        // Nothing moves the focus, whoever asks.
        OPEN | QUIT => true,
        // The Open Runtime's own requests under these keys are the lock
        // screen's drawing, a full-system update's check (INSTALL) and Phone
        // Internet (RELAY). From anyone else they are refused even if the
        // focus rule failed.
        PRESENT | VOLUME | PLAYBACK | MICROPHONE | INSTALL | REMOVE | RELAY => !from_open,
        _ => false,
    }
}
