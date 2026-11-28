//! VioletOS host integration tests: the shipped component sources, built for
//! this machine and joined by in-memory fakes of the kernel and hardware
//! edges (kernel/). See README.md beside this file.
//!
//!     tests/integration/run.sh           build and run (make test-integration)
#![allow(dead_code, unused_imports, unused_macros, static_mut_refs)]
extern crate alloc;

/// The Open Runtime's `log!` (runtime/open/mod.rs writes its log page): the
/// same lines, kept per machine for the tests to read.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        $crate::kernel::serial::write(format!($($arg)*));
    }};
}

#[path = "../../runtime/protocol.rs"]
pub mod protocol;
#[path = "../../services/storage/tests/testdisk.rs"]
pub mod testdisk;
#[path = "../../services/storage/blockdev.rs"]
pub mod blockdev;

pub mod storage;
/// Power's policy: wake locks and the thermal and frequency governors.
#[path = "../../services/power/policy.rs"]
pub mod power;
/// The supervisor's check of full-system update manifests.
#[path = "../../system/package-manager/ota.rs"]
pub mod ota;
/// system/package-manager/verify.rs built as the image builds it (not as a
/// test), so its C entry `ota_verify` is linked here (run.sh builds it with
/// VIOLET_SYSTEM_VERSION=4).
extern crate violet_verify;

pub mod kernel;
pub mod open;
pub mod world;

mod scenarios;
