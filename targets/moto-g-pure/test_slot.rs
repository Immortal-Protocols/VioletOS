//! Host tests for slot.rs (rustc --test, under a second, no factory files).
//! The golden record was computed with Python's zlib.crc32; the bit-for-bit
//! cases replay what the stock XT2163DL boot control HAL does to a slot's
//! halfword (disassembled from android.hardware.boot@1.0-impl-1.2-mtkimpl.so,
//! see slot.rs).
mod slot;
use slot::{gpt_find, update, Control, Error, Slot, ACTIVE_TRIES};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
const GOLDEN: &str = "5f62000042434142013a00007700f700000000000000000000000000e4fcd57e";

#[test]
fn golden_record_round_trips() {
    let bytes = hex(GOLDEN);
    let c = Control::decode(&bytes).unwrap();
    assert_eq!(c.encode().to_vec(), bytes);
    assert_eq!(Control::initial(1).encode().to_vec(), bytes);
    assert_eq!(c.suffix(), Some(1));
    assert_eq!(c.slot(0), Slot { priority: 7, tries: 7, successful: false, verity_corrupted: false });
    assert_eq!(c.slot(1), Slot { priority: 7, tries: 7, successful: true, verity_corrupted: false });
}

#[test]
fn bad_records_are_refused() {
    let good = hex(GOLDEN);
    let mut magic = good.clone();
    magic[4] ^= 1;
    assert_eq!(Control::decode(&magic), Err(Error::Magic));
    let mut version = good.clone();
    version[8] = 2;
    assert_eq!(Control::decode(&version), Err(Error::Version));
    for at in [0, 9, 12, 13, 20, 27, 28, 31] {
        let mut bad = good.clone();
        bad[at] ^= 0x40;
        assert!(Control::decode(&bad).is_err(), "flip at {at} accepted");
    }
    // Three slots: a valid CRC, but not the layout VioletOS writes.
    let mut c = Control::decode(&good).unwrap().encode();
    c[9] = (c[9] & !7) | 3;
    let crc = slot::crc32(&c[..28]);
    c[28..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(Control::decode(&c), Err(Error::Slots));
    assert_eq!(Control::decode(&good[..31]), Err(Error::Magic));
    assert_eq!(Control::decode(&[0u8; 32]), Err(Error::Magic));
}

/// Apply one of our operations to a slot halfword and return the new halfword.
fn after(h: u16, op: impl FnOnce(&mut Control)) -> u16 {
    let mut c = Control::initial(0);
    let mut raw = c.encode();
    raw[12..14].copy_from_slice(&h.to_le_bytes());
    raw[14..16].copy_from_slice(&0x0077u16.to_le_bytes());
    let crc = slot::crc32(&raw[..28]);
    raw[28..].copy_from_slice(&crc.to_le_bytes());
    c = Control::decode(&raw).unwrap();
    op(&mut c);
    let out = c.encode();
    u16::from_le_bytes([out[12], out[13]])
}

#[test]
fn operations_match_the_stock_hal_bit_for_bit() {
    for h in 0..=0xffffu16 {
        // MarkBootSuccessful: orr 0x90, bic 0x60 (we skip the write when already proven).
        let stock = (h | 0x90) & !0x60;
        let ours = after(h, |c| {
            c.mark_successful(0).unwrap();
        });
        if h & 0x80 == 0 {
            assert_eq!(ours, stock, "mark {h:#x}");
        } else {
            assert_eq!(ours, h, "mark leaves a proven slot alone {h:#x}");
        }
        // SetSlotAsUnbootable: bic 0xf0.
        assert_eq!(after(h, |c| { c.set_unbootable(0).unwrap(); }), h & !0xf0, "unbootable {h:#x}");
        // SetActiveBootSlot on a slot that is not the current one: low 7 bits
        // 0x6f, then verity_corrupted (bit 8) cleared.
        assert_eq!(after(h, |c| c.set_active(0, 1).unwrap()), ((h & !0x7f) | 0x6f) & !0x100, "active {h:#x}");
        assert_eq!(after(h, |c| c.set_active(0, 0).unwrap()), (h & !0x7f) | 0x6f, "active current {h:#x}");
        // Another slot at priority 15 drops to 14 (orr 0xe, bic 1).
        let other = after(h, |c| c.set_active(1, 0).unwrap());
        assert_eq!(other, if h & 15 == 15 { (h | 0xe) & !1 } else { h }, "demote {h:#x}");
    }
}

#[test]
fn a_trial_that_never_succeeds_falls_back() {
    let mut c = Control::initial(0);
    c.mark_successful(0).unwrap();
    // The updater: B unbootable while it is written, then active.
    assert!(c.set_unbootable(1).unwrap());
    assert!(!c.bootable(1));
    assert_eq!(c.clone().choose(), Some(0));
    c.set_active(1, 0).unwrap();
    for attempt in 0..ACTIVE_TRIES {
        assert_eq!(c.choose(), Some(1), "attempt {attempt}");
        assert_eq!(c.suffix(), Some(1));
    }
    // Out of tries and never marked: A again, which is still proven.
    assert_eq!(c.choose(), Some(0));
    assert_eq!(c.suffix(), Some(0));
    assert!(!c.bootable(1));
    // A was proven before the update; booting it again spends nothing.
    assert_eq!(c.slot(0), Slot { priority: 7, tries: 7, successful: true, verity_corrupted: false });
}

#[test]
fn a_trial_that_succeeds_stays() {
    let mut c = Control::initial(0);
    c.set_unbootable(1).unwrap();
    c.set_active(1, 0).unwrap();
    assert_eq!(c.choose(), Some(1));
    assert!(c.mark_successful(1).unwrap());
    assert!(!c.mark_successful(1).unwrap(), "a second mark changes nothing");
    for _ in 0..20 {
        assert_eq!(c.choose(), Some(1));
    }
    assert_eq!(c.slot(1), Slot { priority: 15, tries: 1, successful: true, verity_corrupted: false });
    assert_eq!(c.slot(0).priority, 7);
    assert!(c.bootable(0), "the old slot is still there to fall back to by hand");
}

#[test]
fn nothing_bootable_chooses_nothing() {
    let mut c = Control::initial(0);
    c.set_unbootable(0).unwrap();
    c.set_unbootable(1).unwrap();
    assert_eq!(c.choose(), None);
    assert_eq!(c.set_active(2, 0), Err(Error::Slot));
    assert_eq!(c.mark_successful(5), Err(Error::Slot));
}

#[test]
fn update_writes_once_reads_back_and_refuses_garbage() {
    let cell = std::cell::Cell::new([0u8; 512]);
    let mut first = [0u8; 512];
    first[..32].copy_from_slice(&Control::initial(0).encode());
    first[100] = 0x5a; // bytes after the record are kept
    cell.set(first);
    let mut writes = 0;
    let r = update(|| Ok(cell.get()), |s| { writes += 1; cell.set(*s); Ok(()) }, |c| { c.set_unbootable(1)?; c.set_active(1, 0)?; Ok(true) });
    assert_eq!(r, Ok(true));
    assert_eq!(writes, 1);
    let disk = cell.get();
    assert_eq!(disk[100], 0x5a);
    assert_eq!(Control::decode(&disk).unwrap().slot(1).tries, ACTIVE_TRIES);
    // Already proven: no write at all.
    let mut proven = [0u8; 512];
    proven[..32].copy_from_slice(&Control::initial(0).encode());
    let r = update(|| Ok(proven), |_| panic!("no write expected"), |c| c.mark_successful(0));
    assert_eq!(r, Ok(false));
    // A sector that is not a record: never written.
    let r = update(|| Ok([0xff; 512]), |_| panic!("no write expected"), |c| c.mark_successful(0));
    assert_eq!(r, Err(Error::Magic));
    // A read error, a write error, and a write that does not stick.
    assert_eq!(update(|| Err(()), |_| panic!(), |c| c.mark_successful(0)), Err(Error::Io));
    let base = { let mut s = [0u8; 512]; s[..32].copy_from_slice(&Control::initial(1).encode()); s };
    assert_eq!(update(|| Ok(base), |_| Err(()), |c| c.mark_successful(0)), Err(Error::Io));
    assert_eq!(update(|| Ok(base), |_| Ok(()), |c| c.mark_successful(0)), Err(Error::Readback));
}

/// A primary GPT with the given (name, first, last) entries.
fn gpt(parts: &[(&str, u64, u64)]) -> Vec<[u8; 512]> {
    let mut disk = vec![[0u8; 512]; 34];
    for (i, (name, first, last)) in parts.iter().enumerate() {
        let e = &mut disk[2 + i / 4][(i % 4) * 128..(i % 4) * 128 + 128];
        e[0] = 1 + i as u8;
        e[16] = 0x40 + i as u8;
        e[32..40].copy_from_slice(&first.to_le_bytes());
        e[40..48].copy_from_slice(&last.to_le_bytes());
        for (k, c) in name.bytes().enumerate() {
            e[56 + 2 * k] = c;
        }
    }
    let entries: Vec<u8> = disk[2..34].concat();
    let h = &mut disk[1];
    h[..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&1u64.to_le_bytes());
    h[72..80].copy_from_slice(&2u64.to_le_bytes());
    h[80..84].copy_from_slice(&128u32.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    h[88..92].copy_from_slice(&slot::crc32(&entries).to_le_bytes());
    let sum = slot::crc32(&h[..92]);
    h[16..20].copy_from_slice(&sum.to_le_bytes());
    disk
}

#[test]
fn gpt_finds_misc_and_both_boot_partitions_by_name() {
    let disk = gpt(&[("para", 100, 1123), ("misc", 2000, 3023), ("boot_a", 1_935_360, 2_000_895), ("boot_b", 2_000_896, 2_066_431), ("miscdata", 5000, 6000)]);
    let mut found = [None; 3];
    gpt_find(|lba| disk.get(lba as usize).copied().ok_or(()), 64_000_000, &[b"misc", b"boot_a", b"boot_b"], &mut found).unwrap();
    assert_eq!(found, [Some((2000, 3023)), Some((1_935_360, 2_000_895)), Some((2_000_896, 2_066_431))]);
    // Missing names stay None; the caller refuses then.
    let mut none = [None; 1];
    gpt_find(|lba| disk.get(lba as usize).copied().ok_or(()), 64_000_000, &[b"vbmeta"], &mut none).unwrap();
    assert_eq!(none, [None]);
}

#[test]
fn gpt_damage_or_ambiguity_is_refused() {
    let base = gpt(&[("misc", 2000, 3023), ("boot_a", 10, 20)]);
    let find = |disk: &Vec<[u8; 512]>, cap| {
        let mut f = [None; 1];
        gpt_find(|lba| disk.get(lba as usize).copied().ok_or(()), cap, &[b"misc"], &mut f).map(|_| f)
    };
    assert_eq!(find(&base, 64_000_000), Ok([Some((2000, 3023))]));
    let mut entry = base.clone();
    entry[2][40] ^= 1; // the entry array's CRC no longer matches
    assert_eq!(find(&entry, 64_000_000), Err(Error::Crc));
    let mut header = base.clone();
    header[1][24] ^= 1;
    assert_eq!(find(&header, 64_000_000), Err(Error::Crc));
    let mut magic = base.clone();
    magic[1][0] = b'X';
    assert_eq!(find(&magic, 64_000_000), Err(Error::Magic));
    assert_eq!(find(&gpt(&[("misc", 1, 2), ("misc", 3, 4)]), 64_000_000), Err(Error::Slot));
    assert_eq!(find(&base, 3000), Err(Error::Slot), "misc past the end of the card");
    assert_eq!(find(&gpt(&[("misc", 9, 8)]), 64_000_000), Err(Error::Slot));
    let short: Vec<[u8; 512]> = base[..20].to_vec();
    assert_eq!(find(&short, 64_000_000), Err(Error::Io));
}
