#[path = "../../services/storage/journal.rs"]
mod journal;
mod boot_history;
use std::{fs::OpenOptions, os::unix::fs::FileExt};

#[test]
fn interrupted_boot_recovered_after_reopening_disk() {
    let path = std::env::temp_dir().join(format!("violet-history-{}", std::process::id()));
    let initial = OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
    initial.set_len(1536).unwrap();
    initial.write_all_at(boot_history::TAG, 0).unwrap();
    drop(initial);
    let checkpoint = |stage, cut| {
        let file = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        boot_history::update(stage, |slot| {
            let mut b=[0;512];
            file.read_exact_at(&mut b, u64::from(slot)*512).map_err(|_| ())?;
            Ok(b)
        }, |slot, bytes| {
            file.write_all_at(&bytes[..cut], u64::from(slot)*512).map_err(|_| ())?;
            file.sync_data().map_err(|_| ())
        })
    };
    assert_eq!(checkpoint(1,512),Ok(0));
    assert_eq!(checkpoint(2,512),Ok(0));
    assert_eq!(checkpoint(3,40),Err(()));
    assert_eq!(checkpoint(1,512),Ok(2));
    assert_eq!(checkpoint(4,512),Ok(2));
    assert_eq!(checkpoint(1,512),Ok(4));
    // The AArch64 loader's stages: its first keeps the last stage before it,
    // and the next boot (either loader) reports how far it got.
    assert_eq!(checkpoint(21,512),Ok(1));
    assert_eq!(checkpoint(22,512),Ok(1));
    assert_eq!(checkpoint(23,512),Ok(1));
    assert_eq!(checkpoint(5,512),Ok(1));
    assert_eq!(checkpoint(1,512),Ok(5));
    assert_eq!(checkpoint(7,512),Err(()));
    assert_eq!(checkpoint(20,512),Err(()));
    std::fs::remove_file(path).unwrap();
}
#[test]
fn missing_marker_prevents_all_writes() {
    assert_eq!(boot_history::update(1, |_| Ok([0;512]), |_,_| panic!("unexpected write")), Err(()));
}
