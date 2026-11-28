//! Storage's sources, unchanged: the boot slot writer with its SHA-256 and
//! the slot metadata codec (targets/moto-g-pure/slot.rs).
#[path = "../../../services/storage/slotwrite.rs"]
pub mod slotwrite;
/// The Open Runtime's write-back to Storage: `key` is what a restart keeps
/// of /data (World::shutdown asks it).
#[path = "../../../services/storage/writeback.rs"]
pub mod writeback;
