//! Installed apps: programs added to /apps after the image was built (by the
//! agent, from the paired Mac) that survive a restart. The Storage service
//! keeps a disk extent for them (the last 12 MiB of boot_b on the handset, see
//! targets/moto-g-pure/storage_volume.rs; sectors 4096 onward in QEMU) and
//! moves its sectors through a 64 KiB window shared with this process.
//!
//! Layout, in 512-byte sectors: two copies of the directory (sectors 0-3 and
//! 4-7), then the programs. A directory is 2048 bytes: "VAPPS1\0\0", its
//! generation, the CRC-32 of the rest, then 31 entries of 64 bytes (name,
//! title, first sector, length, CRC-32 of the program, time). An install
//! writes the program into free sectors and flushes, then writes the newer
//! directory over the older copy and flushes again, so a power cut leaves
//! either the old set of apps or the new one, never a torn program.
//!
//! A package is an entry whose data is "VGZ1", the gzip length and the gzip of
//! a cpio archive (Python: /bin/python3 and /lib). It is unpacked into /bin
//! and /lib instead of becoming /apps/NAME, and gets no Home icon.
//!
//! A program may also be kept compressed: "VZ1\0", its length (u32 LE), then
//! raw DEFLATE (the form the agent receives it in). /apps/NAME gets the program
//! itself; the store keeps the compressed bytes, about half the size.
use crate::log;
use crate::protocol::*;
use alloc::string::String;
use alloc::vec::Vec;

use super::fs::Fs;

const WINDOW: *mut u8 = 0x0913_0000 as *mut u8;
const WINDOW_SECTORS: u32 = 128;
const DIR_SECTORS: u32 = 4;
const FIRST_DATA: u32 = 2 * DIR_SECTORS;
const ENTRIES: usize = 31;
const MAGIC: &[u8; 8] = b"VAPPS1\0\0";
/// Programs larger than this are refused (the extent is 12 MiB).
pub const MAX_PROGRAM: usize = 8 << 20;
pub const TITLES: &str = "/apps/.home";
/// The only program that may install or remove apps.
pub const INSTALLER: &str = "/apps/agent";

#[derive(Clone, Default)]
pub struct Entry {
    pub name: String,
    pub title: String,
    first: u32,
    len: u32,
    crc: u32,
    pub time: u32,
    /// Unpacked into /bin and /lib (not stored in the directory: read from the data).
    pub package: bool,
}

#[derive(Default)]
pub struct Store {
    /// Sectors in the extent; 0 when there is none (no Storage, or a small disk).
    sectors: u32,
    generation: u32,
    /// Which directory copy holds `generation` (0 or 1).
    copy: u32,
    pub entries: Vec<Entry>,
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// One request to Storage through Runtime, waiting for its answer.
fn call(op: u32, first: u32, count: u32) -> Message {
    let m = Message::new(op, APPSTORE, first).with_arg(count);
    let r = m.call(super::sys::SLOT_RUNTIME);
    if r.op != OK {
        return r;
    }
    loop {
        let p = Message::new(POLL, 0, r.value).call(super::sys::SLOT_RUNTIME);
        if p.op != BUSY {
            return p;
        }
        super::sys::pause();
    }
}

fn read(first: u32, count: u32, out: &mut Vec<u8>) -> bool {
    let mut at = 0;
    while at < count {
        let n = (count - at).min(WINDOW_SECTORS);
        if call(GET, first + at, n).op != OK {
            return false;
        }
        let bytes = unsafe { core::slice::from_raw_parts(WINDOW, n as usize * 512) };
        out.extend_from_slice(bytes);
        at += n;
    }
    true
}

fn write(first: u32, data: &[u8]) -> bool {
    for (k, chunk) in data.chunks(WINDOW_SECTORS as usize * 512).enumerate() {
        let n = ((chunk.len() + 511) / 512) as u32;
        unsafe {
            core::ptr::copy_nonoverlapping(chunk.as_ptr(), WINDOW, chunk.len());
            core::ptr::write_bytes(WINDOW.add(chunk.len()), 0, n as usize * 512 - chunk.len());
        }
        if call(EXECUTE, first + k as u32 * WINDOW_SECTORS, n).op != OK {
            return false;
        }
    }
    true
}

fn flush() -> bool {
    call(EXECUTE, 0, 0).op == OK
}

fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}
fn word(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// A program name /apps can hold: 1-23 of a-z 0-9 _ -, not starting with '.'.
pub fn valid_name(name: &str) -> bool {
    (1..=23).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
}

fn decode(raw: &[u8]) -> Option<(u32, Vec<Entry>)> {
    if raw.len() < 2048 || &raw[..8] != MAGIC || crc32(&raw[16..2048]) != word(raw, 12) {
        return None;
    }
    let mut entries = Vec::new();
    for e in raw[32..32 + ENTRIES * 64].chunks_exact(64) {
        let name = text(&e[..24]);
        if name.is_empty() {
            continue;
        }
        entries.push(Entry { name, title: text(&e[24..48]), first: word(e, 48), len: word(e, 52), crc: word(e, 56), time: word(e, 60), package: false });
    }
    Some((word(raw, 8), entries))
}

fn encode(generation: u32, entries: &[Entry]) -> Vec<u8> {
    let mut raw = alloc::vec![0u8; 2048];
    raw[..8].copy_from_slice(MAGIC);
    raw[8..12].copy_from_slice(&generation.to_le_bytes());
    for (e, slot) in entries.iter().zip(raw[32..32 + ENTRIES * 64].chunks_exact_mut(64)) {
        let n = e.name.len().min(23);
        slot[..n].copy_from_slice(&e.name.as_bytes()[..n]);
        let mut t = e.title.len().min(23);
        while !e.title.is_char_boundary(t) {
            t -= 1;
        }
        slot[24..24 + t].copy_from_slice(&e.title.as_bytes()[..t]);
        for (k, v) in [e.first, e.len, e.crc, e.time].iter().enumerate() {
            slot[48 + 4 * k..52 + 4 * k].copy_from_slice(&v.to_le_bytes());
        }
    }
    let crc = crc32(&raw[16..2048]);
    raw[12..16].copy_from_slice(&crc.to_le_bytes());
    raw
}

impl Store {
    /// Read the directory and put every intact program into /apps.
    pub fn load(fs: &mut Fs) -> Store {
        let size = call(GET, 0, 0);
        let mut store = Store::default();
        if size.op != OK || size.value <= FIRST_DATA {
            log!("installed apps: no app extent");
            return store;
        }
        store.sectors = size.value;
        let mut best: Option<(u32, u32, Vec<Entry>)> = None;
        for copy in 0..2 {
            let mut raw = Vec::new();
            if !read(copy * DIR_SECTORS, DIR_SECTORS, &mut raw) {
                continue;
            }
            if let Some((generation, entries)) = decode(&raw) {
                if best.as_ref().map_or(true, |b| generation.wrapping_sub(b.0) as i32 > 0) {
                    best = Some((generation, copy, entries));
                }
            }
        }
        let Some((generation, copy, entries)) = best else {
            log!("installed apps: none yet ({} KiB free)", (store.sectors - FIRST_DATA) / 2);
            return store;
        };
        store.generation = generation;
        store.copy = copy;
        let start = super::sys::now_ns();
        for e in entries {
            let mut data = Vec::new();
            let sectors = (e.len + 511) / 512;
            if e.first < FIRST_DATA
                || e.first.saturating_add(sectors) > store.sectors
                || !read(e.first, sectors, &mut data)
            {
                log!("installed apps: cannot read {}", e.name);
                continue;
            }
            data.truncate(e.len as usize);
            if crc32(&data) != e.crc {
                log!("installed apps: {} is damaged and was skipped", e.name);
                continue;
            }
            let mut e = e;
            if is_package(&data) {
                match unpack(fs, &data) {
                    Ok(n) => log!("installed apps: package {} ({} files)", e.name, n),
                    Err(why) => {
                        log!("installed apps: package {}: {}", e.name, why);
                        continue;
                    }
                }
                e.package = true;
            } else {
                let program = match expand(data) {
                    Ok(p) => p,
                    Err(why) => {
                        log!("installed apps: {}: {}", e.name, why);
                        continue;
                    }
                };
                let path = alloc::format!("/apps/{}", e.name);
                if let Some(f) = fs.writable(&path, true, true, 0) {
                    *f = program;
                }
            }
            store.entries.push(e);
        }
        store.titles(fs, None);
        log!("installed apps: {} loaded in {} ms", store.entries.len(), (super::sys::now_ns() - start) / 1_000_000);
        store
    }

    /// Keep /apps/.home's titles for installed apps in step with the directory.
    fn titles(&self, fs: &mut Fs, dropped: Option<&str>) {
        let old = fs.contents(TITLES).map(|d| String::from_utf8_lossy(d).into_owned()).unwrap_or_default();
        let mut lines: Vec<String> = old
            .lines()
            .filter(|l| {
                let name = l.split('\t').next().unwrap_or("");
                !self.entries.iter().any(|e| e.name == name) && Some(name) != dropped
            })
            .map(String::from)
            .collect();
        for e in self.entries.iter().filter(|e| !e.package) {
            lines.push(alloc::format!("{}\t{}", e.name, if e.title.is_empty() { &e.name } else { &e.title }));
        }
        let mut text = lines.join("\n");
        text.push('\n');
        if let Some(f) = fs.writable(TITLES, true, true, 0) {
            *f = text.into_bytes();
        }
    }

    fn commit(&mut self, entries: Vec<Entry>) -> bool {
        let generation = self.generation.wrapping_add(1);
        let target = 1 - self.copy;
        if !write(target * DIR_SECTORS, &encode(generation, &entries)) || !flush() {
            return false;
        }
        self.generation = generation;
        self.copy = target;
        self.entries = entries;
        true
    }

    /// First free run of `sectors` after the directories, between the programs kept.
    fn place(&self, keep: &[Entry], sectors: u32) -> Option<u32> {
        let mut used: Vec<(u32, u32)> = keep.iter().map(|e| (e.first, e.first + (e.len + 511) / 512)).collect();
        used.sort();
        let mut at = FIRST_DATA;
        for (a, b) in used {
            if a >= at + sectors {
                break;
            }
            at = at.max(b);
        }
        (at + sectors <= self.sectors).then_some(at)
    }

    /// Keep /apps/NAME (already in the file system) across restarts, listed as `title`.
    pub fn install(&mut self, fs: &mut Fs, name: &str, title: &str, time: u32) -> Result<usize, &'static str> {
        if self.sectors == 0 {
            return Err("no app storage");
        }
        if !valid_name(name) {
            return Err("bad name");
        }
        let path = alloc::format!("/apps/{}", name);
        let data = fs.contents(&path).ok_or("no such program")?.to_vec();
        if data.is_empty() || data.len() > MAX_PROGRAM {
            return Err("program too large or empty");
        }
        let keep: Vec<Entry> = self.entries.iter().filter(|e| e.name != name).cloned().collect();
        if keep.len() >= ENTRIES {
            return Err("too many installed apps");
        }
        let sectors = ((data.len() + 511) / 512) as u32;
        let first = self.place(&keep, sectors).ok_or("no room for the program")?;
        let package = is_package(&data);
        if package {
            // Unpacked first: a package that does not unpack is never stored.
            unpack(fs, &data)?;
            let _ = fs.unlink(&path, false);
        } else if is_compressed(&data) {
            // Runs from /apps as the program itself; the store keeps the compressed form.
            let program = expand(data.clone())?;
            if let Some(f) = fs.writable(&path, true, true, 0) {
                *f = program;
            }
        }
        if !write(first, &data) || !flush() {
            return Err("write failed");
        }
        let mut entries = keep;
        let title: String = title.chars().filter(|c| *c != '\t' && *c != '\n').take(23).collect();
        entries.push(Entry { name: String::from(name), title, first, len: data.len() as u32, crc: crc32(&data), time, package });
        if !self.commit(entries) {
            return Err("directory write failed");
        }
        self.titles(fs, None);
        Ok(data.len())
    }

    /// Forget an installed program and remove it from /apps.
    pub fn remove(&mut self, fs: &mut Fs, name: &str) -> Result<(), &'static str> {
        if !self.entries.iter().any(|e| e.name == name) {
            return Err("not installed");
        }
        let keep: Vec<Entry> = self.entries.iter().filter(|e| e.name != name).cloned().collect();
        if !self.commit(keep) {
            return Err("directory write failed");
        }
        // A removed package's files stay until the next restart.
        let _ = fs.unlink(&alloc::format!("/apps/{}", name), false);
        self.titles(fs, Some(name));
        Ok(())
    }

    /// "name<TAB>bytes<TAB>title" lines (bytes as stored), then ".free<TAB>bytes<TAB>"
    /// with the room left for programs.
    pub fn list(&self) -> String {
        let mut s = String::new();
        for e in &self.entries {
            // A package (Python) is marked, so Settings does not offer it as an app.
            s.push_str(&alloc::format!("{}\t{}\t{}{}\n", e.name, e.len, e.title, if e.package { "\tpackage" } else { "" }));
        }
        let used: u32 = self.entries.iter().map(|e| (e.len + 511) / 512).sum();
        let free = self.sectors.saturating_sub(FIRST_DATA).saturating_sub(used);
        s.push_str(&alloc::format!(".free\t{}\t\n", u64::from(free) * 512));
        s
    }
}

pub fn is_compressed(data: &[u8]) -> bool {
    data.len() > 8 && &data[..4] == b"VZ1\0"
}

/// A stored program as it runs: compressed ones inflated, others as they are.
fn expand(data: Vec<u8>) -> Result<Vec<u8>, &'static str> {
    if !is_compressed(&data) {
        return Ok(data);
    }
    let len = word(&data, 4) as usize;
    if len > MAX_PROGRAM * 2 {
        return Err("compressed program too large");
    }
    let mut out = alloc::vec![0u8; len];
    let n = super::inflate::inflate_into(&data[8..], &mut out)?;
    if n != len {
        return Err("compressed program damaged");
    }
    Ok(out)
}

pub fn is_package(data: &[u8]) -> bool {
    data.len() > 8 && &data[..4] == b"VGZ1"
}

/// Where a package may put files: /bin and /lib, never /apps or /data.
pub fn package_path(path: &str) -> bool {
    (path.starts_with("/bin/") || path.starts_with("/lib/") || path == "/bin" || path == "/lib") && !path.contains("/../")
}

/// Unpack a package's archive into the file system. The unpacked bytes stay
/// for the rest of the boot (the file system refers to them in place).
fn unpack(fs: &mut Fs, data: &[u8]) -> Result<usize, &'static str> {
    let len = word(data, 4) as usize;
    let packed = data.get(8..8 + len).ok_or("package truncated")?;
    let size = super::inflate::gzip_size(packed).ok_or("package truncated")?;
    if size > 64 << 20 {
        return Err("package too large");
    }
    let mut dest = alloc::vec![0u8; size];
    let n = super::inflate::gunzip(packed, &mut dest)?;
    dest.truncate(n);
    let image: &'static [u8] = alloc::boxed::Box::leak(dest.into_boxed_slice());
    match fs.add_archive(image, package_path) {
        0 => Err("package holds no files for /bin or /lib"),
        n => Ok(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directory_round_trip() {
        let e = Entry { name: "scorekeeper".into(), title: "Scorekeeper".into(), first: 8, len: 1000, crc: 7, time: 9, package: false };
        let raw = encode(3, &[e]);
        let (g, entries) = decode(&raw).unwrap();
        assert_eq!(g, 3);
        assert_eq!(entries[0].name, "scorekeeper");
        assert_eq!(entries[0].title, "Scorekeeper");
        assert_eq!(entries[0].len, 1000);
        let mut bad = raw.clone();
        bad[100] ^= 1;
        assert!(decode(&bad).is_none());
    }
    #[test]
    fn packages_stay_in_bin_and_lib() {
        assert!(package_path("/bin/python3"));
        assert!(package_path("/lib/python312.zip"));
        assert!(!package_path("/apps/agent"));
        assert!(!package_path("/data/agent"));
        assert!(!package_path("/binx"));
    }
    #[test]
    fn crc_matches_zlib() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }
}
