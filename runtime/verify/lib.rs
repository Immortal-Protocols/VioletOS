//! Host-side proofs for the Runtime routing policy. The Runtime sources are
//! included unchanged; see README.md for what is proven and what is assumed.
#[allow(dead_code)]
#[path = "../protocol.rs"]
pub mod protocol;
#[path = "../router.rs"]
pub mod router;

#[cfg(any(kani, test))]
pub mod model;
#[cfg(kani)]
mod proofs;
#[cfg(test)]
mod tests;
