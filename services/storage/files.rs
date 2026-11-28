//! App-private flat directories. Copy data to the inactive bank, flush, then
//! publish its directory entry. A reset always leaves a complete older file.
//!
//! One message carries 24 bytes, so a caller that replaces a file with plain
//! FILE_WRITEs commits it 24 bytes at a time: a power cut in the middle leaves
//! a file that is part new, part old, and each piece rewrites the whole file.
//! A staged replacement (FILE_STAGED in `arg`) avoids both: the pieces
//! collect in the inactive bank, unseen, and one commit publishes the file.
use crate::{
    block::Disk,
    journal::{crc32, Sector},
    protocol::*,
};
const SLOTS: usize = 4;
const PAGES: usize = 8;
const MAX: usize = PAGES * 512;
/// All open apps share one owner (protocol::region(OPEN_APPS)), so it gets more
/// files: its first four keep their old place, the rest follow every owner's
/// fixed slots (sectors 608 to 1112 of the 2048-sector volume).
const OPEN_APPS: u32 = 7;
const OPEN_SLOTS: usize = 32;
const EXTRA: usize = 32 + 8 * SLOTS * 2 * (PAGES + 1);
fn slots(owner: u32) -> usize {
    if owner == OPEN_APPS { OPEN_SLOTS } else { SLOTS }
}
#[derive(Clone, Copy, Default)]
struct Entry {
    generation: u32,
    len: usize,
    name: [u8; 16],
    sums: [u32; PAGES],
}
/// A file being replaced whole: its new bytes go to the slot's inactive bank
/// as they arrive, and nothing is published before the commit. Each owner has
/// one (no owner can disturb another's), so one file at a time per owner: a
/// second replacement takes the place of the first, whose commit then fails
/// and leaves the old file untouched.
#[derive(Clone, Copy)]
pub struct Stage {
    active: bool,
    name: [u8; 16],
    /// The slot's first sector and its current bank, as found at the start.
    loc: usize,
    bank: usize,
    generation: u32,
    len: usize,
    sums: [u32; PAGES],
    /// The sector being filled, zero beyond `len`.
    buf: Sector,
}
impl Stage {
    pub const fn new() -> Self {
        Stage { active: false, name: [0; 16], loc: 0, bank: 0, generation: 0, len: 0, sums: [0; PAGES], buf: [0; 512] }
    }
}
fn u32at(bytes: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(bytes[n..n + 4].try_into().unwrap())
}
fn base(owner: u32, slot: usize, bank: usize) -> usize {
    if slot >= SLOTS {
        return EXTRA + ((slot - SLOTS) * 2 + bank) * (PAGES + 1);
    }
    32 + ((owner as usize * SLOTS + slot) * 2 + bank) * (PAGES + 1)
}
fn read_entry(d: &mut Disk, sector: usize) -> Result<Option<Entry>, u32> {
    let b = d.read(sector).map_err(|_| OFFLINE)?;
    if &b[..4] != b"VFS1" || u32at(&b, 508) != crc32(&b[..508]) {
        return Ok(None);
    }
    let mut e = Entry {
        generation: u32at(&b, 4),
        len: u32at(&b, 8) as usize,
        ..Entry::default()
    };
    if e.len > MAX {
        return Ok(None);
    }
    e.name.copy_from_slice(&b[12..28]);
    for (i, sum) in e.sums.iter_mut().enumerate() {
        *sum = u32at(&b, 28 + i * 4);
    }
    Ok(Some(e))
}
fn publish(d: &mut Disk, sector: usize, e: Entry) -> Result<(), u32> {
    let mut h = [0; 512];
    h[..4].copy_from_slice(b"VFS1");
    h[4..8].copy_from_slice(&e.generation.to_le_bytes());
    h[8..12].copy_from_slice(&(e.len as u32).to_le_bytes());
    h[12..28].copy_from_slice(&e.name);
    for (i, v) in e.sums.iter().enumerate() {
        h[28 + 4 * i..32 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    let sum = crc32(&h[..508]);
    h[508..].copy_from_slice(&sum.to_le_bytes());
    d.commit(sector, &mut h).map_err(|_| OFFLINE)
}
fn current(d: &mut Disk, offset: usize) -> Result<(usize, Entry), u32> {
    let a = read_entry(d, offset)?.unwrap_or_default();
    let b = read_entry(d, offset + PAGES + 1)?.unwrap_or_default();
    Ok(if b.generation > a.generation {
        (1, b)
    } else {
        (0, a)
    })
}
fn page(d: &mut Disk, offset: usize, e: Entry, i: usize) -> Result<Sector, u32> {
    if i * 512 >= e.len {
        return Ok([0; 512]);
    }
    let b = d.read(offset + 1 + i).map_err(|_| OFFLINE)?;
    if crc32(&b) != e.sums[i] {
        return Err(OFFLINE);
    }
    Ok(b)
}
pub fn dispatch(d: &mut Disk, stages: &mut [Stage; 8], owner: u32, m: Message) -> Result<Message, u32> {
    if owner >= 8 {
        return Err(DENIED);
    }
    let stage = &mut stages[owner as usize];
    let bytes = m.text();
    let name: [u8; 16] = bytes[..16].try_into().unwrap();
    let end = name.iter().position(|b| *b == 0).unwrap_or(16);
    if m.key != FILE_LIST
        && (end == 0
            || name[end..].iter().any(|b| *b != 0)
            || name[..end]
                .iter()
                .any(|b| !b.is_ascii_alphanumeric() && !b"._-".contains(b)))
    {
        return Err(INVALID);
    }
    let staged = m.arg & FILE_STAGED != 0 && matches!(m.key, FILE_WRITE | FILE_TRUNCATE);
    if staged {
        // A replacement in progress goes on without looking the name up
        // again; one that starts (offset 0) or is for another name begins below.
        if stage.active && stage.name == name && m.value != 0 {
            return stage_step(d, stage, &m, &bytes);
        }
        stage.active = false;
        if m.value != 0 {
            return Err(EMPTY);
        }
    } else if matches!(m.key, FILE_WRITE | FILE_TRUNCATE | FILE_DELETE) {
        // A plain change writes the inactive bank too: the replacement is void.
        stage.active = false;
    }
    let mut empty = None;
    let mut found = None;
    for slot in 0..slots(owner) {
        let loc = base(owner, slot, 0);
        if loc + 2 * (PAGES + 1) > d.sectors {
            break; // a small disk holds fewer files
        }
        let (bank, e) = current(d, loc)?;
        if m.key == FILE_LIST && slot == m.value as usize {
            let mut r = Message::result(e.len as u32);
            r.put_text(&e.name);
            return Ok(r);
        }
        if e.name == name && end > 0 {
            found = Some((loc, bank, e));
            break;
        }
        if e.name[0] == 0 && empty.is_none() {
            empty = Some((loc, bank, e));
        }
    }
    if m.key == FILE_LIST {
        return Err(INVALID);
    }
    let (loc, bank, mut e) = found
        .or_else(|| if m.key == FILE_WRITE || staged { empty } else { None })
        .ok_or(EMPTY)?;
    if staged {
        *stage = Stage { active: true, name, loc, bank, generation: e.generation, ..Stage::new() };
        return stage_step(d, stage, &m, &bytes);
    }
    let pos = m.value as usize;
    let n = m.arg as usize;
    let src = loc + bank * (PAGES + 1);
    let dst = loc + (1 - bank) * (PAGES + 1);
    if m.key == FILE_READ {
        if pos > e.len {
            return Err(INVALID);
        }
        let n = (e.len - pos).min(40);
        let mut r = Message::result(n as u32);
        r.arg = e.len as u32;
        let mut buf = [0; 40];
        let mut read = 0;
        while read < n {
            let idx = pos + read;
            let b = page(d, src, e, idx / 512)?;
            let take = (512 - idx % 512).min(n - read);
            buf[read..read + take].copy_from_slice(&b[idx % 512..idx % 512 + take]);
            read += take;
        }
        r.put_text(&buf);
        return Ok(r);
    }
    e.generation = e.generation.checked_add(1).ok_or(INVALID)?;
    if m.key == FILE_DELETE {
        e.name = [0; 16];
        e.len = 0;
        publish(d, dst, e)?;
        return Ok(Message::result(0));
    }
    let len = match m.key {
        FILE_TRUNCATE if pos <= e.len => pos,
        FILE_WRITE if pos <= e.len && n <= 24 && pos + n <= MAX => (pos + n).max(e.len),
        _ => return Err(INVALID),
    };
    for i in 0..(len + 511) / 512 {
        let mut b = page(d, src, e, i)?;
        if m.key == FILE_WRITE {
            let lo = pos.max(i * 512);
            let hi = (pos + n).min((i + 1) * 512);
            if hi > lo {
                b[lo - i * 512..hi - i * 512].copy_from_slice(&bytes[16 + lo - pos..16 + hi - pos]);
            }
        }
        if len < (i + 1) * 512 {
            b[len % 512..].fill(0);
        }
        e.sums[i] = crc32(&b);
        d.write(dst + 1 + i, &mut b).map_err(|_| OFFLINE)?;
    }
    d.flush().map_err(|_| OFFLINE)?;
    e.len = len;
    e.name = name;
    publish(d, dst, e)?;
    Ok(Message::result(len as u32))
}
/// One message of a staged replacement: FILE_WRITE appends up to 24 bytes at
/// `value` (which must be where the last one ended), FILE_TRUNCATE commits the
/// file at length `value` (which must be all that was appended). Anything out
/// of step drops the replacement; the published file is not touched.
fn stage_step(d: &mut Disk, stage: &mut Stage, m: &Message, bytes: &[u8; 40]) -> Result<Message, u32> {
    let dst = stage.loc + (1 - stage.bank) * (PAGES + 1);
    let n = (m.arg & !FILE_STAGED) as usize;
    let fail = |stage: &mut Stage, code| {
        stage.active = false;
        Err(code)
    };
    if m.value as usize != stage.len || n > 24 || stage.len + n > MAX || (m.key == FILE_TRUNCATE && n != 0) {
        return fail(stage, INVALID);
    }
    for &byte in &bytes[16..16 + n] {
        stage.buf[stage.len % 512] = byte;
        stage.len += 1;
        if stage.len % 512 == 0 {
            let page = stage.len / 512 - 1;
            stage.sums[page] = crc32(&stage.buf);
            if d.write(dst + 1 + page, &mut stage.buf).is_err() {
                return fail(stage, OFFLINE);
            }
            stage.buf = [0; 512];
        }
    }
    if m.key == FILE_WRITE {
        return Ok(Message::result(stage.len as u32));
    }
    stage.active = false;
    if stage.len % 512 != 0 {
        let page = stage.len / 512;
        stage.sums[page] = crc32(&stage.buf);
        d.write(dst + 1 + page, &mut stage.buf).map_err(|_| OFFLINE)?;
    }
    d.flush().map_err(|_| OFFLINE)?;
    let e = Entry {
        generation: stage.generation.checked_add(1).ok_or(INVALID)?,
        len: stage.len,
        name: stage.name,
        sums: stage.sums,
    };
    publish(d, dst, e)?;
    Ok(Message::result(e.len as u32))
}
