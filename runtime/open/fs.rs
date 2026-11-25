//! The file system open apps see: a read-only image (a cpio "newc" archive
//! built on the Mac, holding /apps, /bin and /lib) under a writable in-memory
//! layer. Writing to an image file copies it up; deleting one hides it.
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFCHR: u32 = 0o020000;

pub struct ImageEntry {
    pub mode: u32,
    pub data: &'static [u8],
    pub mtime: u32,
}

pub struct Layer {
    pub data: Vec<u8>,
    pub mode: u32,
    pub mtime: u32,
}

pub struct Fs {
    image: BTreeMap<String, ImageEntry>,
    layer: BTreeMap<String, Layer>,
    hidden: BTreeSet<String>,
    /// Paths under /data written, removed or renamed since the last save.
    pub changed: BTreeSet<String>,
    /// What Storage holds for each file in /data, to write only what differs.
    pub stored: BTreeMap<String, Vec<u8>>,
    /// Every directory, from the image, the layer and the parents of files.
    dirs: BTreeSet<String>,
}

#[derive(Clone, Copy)]
pub struct Stat {
    pub mode: u32,
    pub size: u64,
    pub mtime: u32,
    pub inode: u64,
}

fn hex(b: &[u8]) -> Option<u32> {
    let mut v = 0u32;
    for &c in b {
        v = v << 4 | (c as char).to_digit(16)?;
    }
    Some(v)
}
fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}
fn inode(path: &str) -> u64 {
    // FNV-1a: stable, so tools that compare inodes see one file per path.
    let mut h = 0xcbf29ce484222325u64;
    for b in path.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    h | 1
}

/// Make `path` absolute and drop ".", ".." and repeated slashes.
pub fn normalize(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let joined;
    let full = if path.starts_with('/') {
        path
    } else {
        joined = alloc::format!("{}/{}", cwd, path);
        &joined
    };
    for part in full.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

impl Fs {
    pub fn new(image: &'static [u8]) -> Fs {
        let mut fs = Fs {
            image: BTreeMap::new(),
            layer: BTreeMap::new(),
            hidden: BTreeSet::new(),
            changed: BTreeSet::new(),
            stored: BTreeMap::new(),
            dirs: BTreeSet::new(),
        };
        for d in ["/", "/apps", "/bin", "/lib", "/tmp", "/data", "/dev", "/dev/violet", "/proc", "/proc/self"] {
            fs.dirs.insert(String::from(d));
        }
        fs.add_archive(image, |_| true);
        fs
    }
    /// Add the files of a cpio "newc" archive to the image layer, those whose
    /// path `keep` accepts (an installed package may only add to /bin and /lib).
    /// Returns how many files were added.
    pub fn add_archive(&mut self, image: &'static [u8], keep: impl Fn(&str) -> bool) -> usize {
        let mut added = 0;
        let mut at = 0usize;
        while at + 110 <= image.len() && &image[at..at + 6] == b"070701" {
            let field = |i: usize| hex(&image[at + 6 + i * 8..at + 14 + i * 8]);
            let (Some(mode), Some(mtime), Some(size), Some(namesize)) =
                (field(1), field(5), field(6), field(11))
            else {
                break;
            };
            let name_start = at + 110;
            let name_end = name_start + namesize as usize;
            if name_end > image.len() || namesize == 0 {
                break;
            }
            let name = core::str::from_utf8(&image[name_start..name_end - 1]).unwrap_or("");
            let data_start = (name_end + 3) & !3;
            let data_end = data_start + size as usize;
            if data_end > image.len() {
                break;
            }
            at = (data_end + 3) & !3;
            if name == "TRAILER!!!" {
                break;
            }
            let path = normalize("/", name);
            if !keep(&path) {
                continue;
            }
            if mode & S_IFMT == S_IFDIR {
                self.add_dir(&path);
            } else {
                self.add_dir(parent(&path));
                self.hidden.remove(&path);
                self.layer.remove(&path);
                self.image.insert(path, ImageEntry { mode, data: &image[data_start..data_end], mtime });
                added += 1;
            }
        }
        added
    }
    fn add_dir(&mut self, path: &str) {
        let mut p = String::from(path);
        while self.dirs.insert(p.clone()) && p != "/" {
            p = String::from(parent(&p));
        }
    }
    pub fn files(&self) -> usize {
        self.image.len()
    }
    /// The file was written since boot rather than shipped in the image.
    pub fn from_layer(&self, path: &str) -> bool {
        self.layer.contains_key(path)
    }

    pub fn stat(&self, path: &str) -> Option<Stat> {
        if path.starts_with("/dev/violet/") || matches!(path, "/dev/null" | "/dev/urandom" | "/dev/random" | "/dev/tty") {
            return Some(Stat { mode: S_IFCHR | 0o666, size: 0, mtime: 0, inode: inode(path) });
        }
        if self.hidden.contains(path) {
            return None;
        }
        if let Some(l) = self.layer.get(path) {
            return Some(Stat { mode: l.mode, size: l.data.len() as u64, mtime: l.mtime, inode: inode(path) });
        }
        if let Some(e) = self.image.get(path) {
            return Some(Stat { mode: e.mode, size: e.data.len() as u64, mtime: e.mtime, inode: inode(path) });
        }
        if self.dirs.contains(path) {
            return Some(Stat { mode: S_IFDIR | 0o755, size: 4096, mtime: 0, inode: inode(path) });
        }
        None
    }
    /// The bytes of a regular file.
    pub fn contents(&self, path: &str) -> Option<&[u8]> {
        if self.hidden.contains(path) {
            return None;
        }
        if let Some(l) = self.layer.get(path) {
            return Some(&l.data);
        }
        self.image.get(path).map(|e| e.data)
    }
    /// A writable copy of a file, created empty (or copied up from the image).
    pub fn writable(&mut self, path: &str, create: bool, truncate: bool, now: u32) -> Option<&mut Vec<u8>> {
        if path.starts_with("/data/") {
            self.changed.insert(String::from(path));
        }
        if !self.layer.contains_key(path) {
            let existing = if self.hidden.contains(path) { None } else { self.image.get(path) };
            let (data, mode) = match existing {
                Some(e) => (if truncate { Vec::new() } else { e.data.to_vec() }, e.mode),
                None if create && self.dirs.contains(parent(path)) => (Vec::new(), S_IFREG | 0o644),
                None => return None,
            };
            self.hidden.remove(path);
            self.layer.insert(String::from(path), Layer { data, mode, mtime: now });
        }
        let l = self.layer.get_mut(path)?;
        if l.mode & S_IFMT == S_IFDIR {
            return None;
        }
        if truncate {
            l.data.clear();
        }
        l.mtime = now;
        Some(&mut l.data)
    }
    pub fn mkdir(&mut self, path: &str) -> Result<(), i32> {
        if self.stat(path).is_some() {
            return Err(super::syscall::EEXIST);
        }
        if !self.dirs.contains(parent(path)) {
            return Err(super::syscall::ENOENT);
        }
        self.hidden.remove(path);
        self.dirs.insert(String::from(path));
        Ok(())
    }
    pub fn unlink(&mut self, path: &str, dir: bool) -> Result<(), i32> {
        let st = self.stat(path).ok_or(super::syscall::ENOENT)?;
        let is_dir = st.mode & S_IFMT == S_IFDIR;
        if is_dir != dir {
            return Err(if dir { super::syscall::ENOTDIR } else { super::syscall::EISDIR });
        }
        if dir && !self.list(path).is_empty() {
            return Err(super::syscall::ENOTEMPTY);
        }
        self.layer.remove(path);
        if path.starts_with("/data/") {
            self.changed.insert(String::from(path));
        }
        if dir {
            self.dirs.remove(path);
        }
        self.hidden.insert(String::from(path));
        Ok(())
    }
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), i32> {
        if self.contents(from).is_none() {
            return Err(super::syscall::ENOENT);
        }
        if !self.dirs.contains(parent(to)) {
            return Err(super::syscall::ENOENT);
        }
        let mode = self.stat(from).map(|s| s.mode).unwrap_or(S_IFREG | 0o644);
        // Move a file from the layer without copying it (installs are megabytes).
        let data = match self.layer.remove(from) {
            Some(l) => l.data,
            None => self.contents(from).unwrap_or(&[]).to_vec(),
        };
        self.hidden.insert(String::from(from));
        self.hidden.remove(to);
        self.layer.insert(String::from(to), Layer { data, mode, mtime: 0 });
        for p in [from, to] {
            if p.starts_with("/data/") {
                self.changed.insert(String::from(p));
            }
        }
        Ok(())
    }
    /// Names directly inside a directory, with whether each is a directory.
    pub fn list(&self, dir: &str) -> Vec<(String, bool)> {
        let prefix = if dir == "/" { String::from("/") } else { alloc::format!("{}/", dir) };
        let mut out: BTreeMap<String, bool> = BTreeMap::new();
        let mut consider = |path: &String, is_dir: bool| {
            if let Some(rest) = path.strip_prefix(prefix.as_str()) {
                if !rest.is_empty() && !rest.contains('/') && !self.hidden.contains(path) {
                    out.insert(String::from(rest), is_dir);
                }
            }
        };
        for p in self.dirs.iter() {
            consider(p, true);
        }
        for p in self.image.keys() {
            consider(p, false);
        }
        for (p, l) in self.layer.iter() {
            consider(p, l.mode & S_IFMT == S_IFDIR);
        }
        out.into_iter().collect()
    }
}
