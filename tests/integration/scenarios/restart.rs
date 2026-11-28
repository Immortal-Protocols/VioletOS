//! What a restart keeps and what it does not: the World's model of the disk
//! is the shipped write-back's rule (services/storage/writeback.rs `key`),
//! and a new boot starts with no memory claimed by the old one.
use crate::world::*;

#[test]
fn a_restart_keeps_what_the_write_back_keeps_and_nothing_else() {
    let mut w = World::boot(Image::new().app("notes", ""));
    let notes = w.open_app("notes");
    let mut n = w.app(notes);
    n.mkdir("/data/notes").unwrap();
    n.mkdir("/data/notes/2026").unwrap();
    n.write_file("/data/notes/2026/today", b"buy milk").unwrap();
    n.write_file("/data/notes.cfg", b"dark").unwrap();
    // A piece of a full-system update in progress is worth nothing after a
    // restart (runtime/open/update.rs `staging`); the runtime writes it.
    let f = w.rt.fs.writable("/data/system.ota-staging", true, true, 0).unwrap();
    f.extend_from_slice(b"half an image");
    let mut w = w.reboot();
    assert!(w.rt.fs.contents("/data/system.ota-staging").is_none(), "an update piece survived a restart");
    let notes = w.open_app("notes");
    assert_eq!(w.app(notes).read_file("/data/notes/2026/today"), Ok(b"buy milk".to_vec()), "a nested file was lost");
    assert_eq!(w.app(notes).read_file("/data/notes.cfg"), Ok(b"dark".to_vec()));
}

#[test]
fn a_new_boot_starts_with_every_page_free() {
    // The first boot's apps still hold memory when it goes down; procmgr.rs's
    // frame ledger must not carry their claims into the next boot.
    let image = Image::new().app("hog", "");
    let mut w = World::boot(image.clone());
    let fresh = w.rt.pool.available();
    let hog = w.open_app("hog");
    let base = w.app(hog).mmap(0, 64 * crate::kernel::sys::PAGE as u32, 3, 0x22, -1).unwrap();
    for k in 0..64 {
        let _ = w.app(hog).touch(base + k * crate::kernel::sys::PAGE as u32, crate::open::vma::Access::Write);
    }
    assert!(w.rt.pool.available() < fresh - 32);
    for boot in 0..3 {
        let mut w2 = World::boot_with(image.clone(), Persisted::default(), 10 + boot);
        // Home's own pages differ a little with its layout; the hog's 64 are back.
        assert!(w2.rt.pool.available() + 4 >= fresh, "boot {boot}: {} of {fresh} pages free", w2.rt.pool.available());
        let i = w2.open_app("hog");
        let all = (w2.rt.pool.available() / 2) as u32;
        let base = w2.app(i).mmap(0, all * crate::kernel::sys::PAGE as u32, 3, 0x22, -1).unwrap();
        for k in 0..all {
            assert_eq!(w2.app(i).touch(base + k * crate::kernel::sys::PAGE as u32, crate::open::vma::Access::Write), crate::open::front::Touched::Ok, "page {k} of boot {boot}");
        }
    }
    drop(w);
}
