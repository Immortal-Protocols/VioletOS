//! Reading a program's ELF headers for the Open Runtime's loader
//! (system/supervisor/procmgr.rs), with no kernel calls so tests/abi_host.rs
//! runs it on the host. The file comes from an app, and the loader runs in the
//! process that holds every app's memory: every offset is checked, nothing
//! indexes past the file, nothing overflows.
//!
//! A program may be static (ET_EXEC or static PIE) or dynamically linked
//! against musl: its PT_INTERP must name exactly LOADER, the one dynamic
//! loader the system image ships (musl's libc.so). Any other interpreter is
//! refused.
use super::vma::{self, Layout, Vma, MMAP_BASE, PROT_EXEC, PROT_READ, PROT_WRITE};
use alloc::vec::Vec;

/// The one dynamic loader a program may name: musl's for the ABI this
/// runtime serves (AArch32 armhf, or AArch64 when vma::WIDE).
pub const LOADER: &str = if vma::WIDE { LOADER64 } else { LOADER32 };
pub const LOADER32: &str = "/lib/ld-musl-armhf.so.1";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Seg {
    pub vaddr: u32,
    pub offset: u32,
    pub filesz: u32,
    pub memsz: u32,
    /// PF_X 1, PF_W 2, PF_R 4.
    pub flags: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    /// ET_DYN: loaded at a base the loader picks.
    pub pie: bool,
    pub entry: u32,
    pub phoff: u32,
    pub phentsize: u32,
    pub phnum: u32,
    /// PT_PHDR's address, if there is one.
    pub phdr: Option<u32>,
    /// PT_LOAD segments with memory.
    pub segs: Vec<Seg>,
    /// It has PT_INTERP (which names LOADER).
    pub interp: bool,
}

fn u16_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?])))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// The program headers of a program for the ABI this runtime serves: a
/// 32-bit ARM executable, or (vma::WIDE) a 64-bit AArch64 one, checked by
/// `plan64` and placed below 4 GiB like any other (`narrow`).
pub fn plan(image: &[u8]) -> Result<Plan, &'static str> {
    if vma::WIDE {
        narrow(plan64(image, u64::from(vma::USER_TOP))?)
    } else {
        plan32(image)
    }
}

/// An AArch64 program's headers in the runtime's 32-bit form: every app's
/// address space is below 4 GiB (vma.rs), so a program whose addresses,
/// sizes or offsets do not fit 32 bits is refused, never cut.
pub fn narrow(p: Plan64) -> Result<Plan, &'static str> {
    let fit = |v: u64| u32::try_from(v).map_err(|_| "an address or size above 4 GiB");
    let mut segs = Vec::with_capacity(p.segs.len());
    for s in &p.segs {
        segs.push(Seg { vaddr: fit(s.vaddr)?, offset: fit(s.offset)?, filesz: fit(s.filesz)?, memsz: fit(s.memsz)?, flags: s.flags });
    }
    let phdr = match p.phdr { Some(v) => Some(fit(v)?), None => None };
    Ok(Plan { pie: p.pie, entry: fit(p.entry)?, phoff: fit(p.phoff)?, phentsize: p.phentsize, phnum: p.phnum, phdr, segs, interp: p.interp })
}

/// The program headers of a 32-bit little-endian ARM executable.
pub fn plan32(image: &[u8]) -> Result<Plan, &'static str> {
    if image.len() < 52 || &image[..4] != b"\x7fELF" || image[4] != 1 || image[5] != 1 {
        return Err("not a 32-bit little-endian ELF file");
    }
    let field = |at| u32_at(image, at).ok_or("bad ELF header");
    let half = |at| u16_at(image, at).ok_or("bad ELF header");
    if half(18)? != 40 {
        return Err("not an ARM executable");
    }
    let pie = match half(16)? {
        2 => false,
        3 => true,
        _ => return Err("not an executable"),
    };
    let (phoff, phentsize, phnum) = (field(28)?, half(42)?, half(44)?);
    let headers_end = (phentsize as usize).checked_mul(phnum as usize).and_then(|n| n.checked_add(phoff as usize));
    if phentsize < 32 || headers_end.map_or(true, |e| e > image.len()) {
        return Err("bad program headers");
    }
    let mut p = Plan { pie, entry: field(24)?, phoff, phentsize, phnum, phdr: None, segs: Vec::new(), interp: false };
    for i in 0..phnum as usize {
        let h = phoff as usize + i * phentsize as usize;
        let w = |k: usize| u32_at(image, h + 4 * k).ok_or("bad program headers");
        let (ptype, offset, vaddr, filesz, memsz, flags) = (w(0)?, w(1)?, w(2)?, w(4)?, w(5)?, w(6)?);
        match ptype {
            1 if memsz != 0 => {
                let in_file = offset.checked_add(filesz).is_some_and(|e| e as usize <= image.len());
                if !in_file || filesz > memsz {
                    return Err("bad segment");
                }
                if vaddr.checked_add(memsz).is_none() {
                    return Err("segment too high");
                }
                if flags & 3 == 3 {
                    return Err("a segment is writable and executable");
                }
                if p.segs.len() >= 16 {
                    return Err("too many segments");
                }
                p.segs.push(Seg { vaddr, offset, filesz, memsz, flags });
            }
            3 => {
                let path = (offset as usize).checked_add(filesz as usize).and_then(|e| image.get(offset as usize..e));
                let ok = !p.interp && path.is_some_and(|s| s.len() == LOADER32.len() + 1 && s.ends_with(b"\0") && &s[..LOADER32.len()] == LOADER32.as_bytes());
                if !ok {
                    return Err("dynamically linked against an unsupported loader (only /lib/ld-musl-armhf.so.1)");
                }
                p.interp = true;
            }
            6 => p.phdr = Some(vaddr),
            _ => {}
        }
    }
    if p.segs.is_empty() {
        return Err("no loadable segments");
    }
    Ok(p)
}

impl Plan {
    /// The lowest and highest address the segments use, rounded out to 4 KiB.
    pub fn span(&self) -> (u32, u32) {
        let lo = self.segs.iter().map(|s| s.vaddr).min().unwrap_or(0) & !0xfff;
        let hi = self.segs.iter().map(|s| s.vaddr.saturating_add(s.memsz)).max().unwrap_or(0);
        (lo, hi.saturating_add(0xfff) & !0xfff)
    }
    /// Where the program headers are once loaded at `base`: PT_PHDR, else
    /// the segment that holds them in the file.
    pub fn phdr_at(&self, base: u32) -> u32 {
        if let Some(v) = self.phdr {
            return base.wrapping_add(v);
        }
        self.segs
            .iter()
            .find(|s| s.offset <= self.phoff && self.phoff - s.offset < s.filesz)
            .map_or(0, |s| base.wrapping_add(s.vaddr).wrapping_add(self.phoff - s.offset))
    }
    /// Whether this can be the dynamic loader: position-independent, with no
    /// interpreter of its own.
    pub fn loader_ok(&self) -> bool {
        self.pie && !self.interp
    }
}

// ---- AArch64 programs (ELF64) ----

/// Why a dynamically linked program cannot start (procmgr.rs `load` too).
pub const NO_LOADER: &str = if vma::WIDE {
    "its dynamic loader /lib/ld-musl-aarch64.so.1 is not in the system image"
} else {
    "its dynamic loader /lib/ld-musl-armhf.so.1 is not in the system image"
};
pub const NOT_LOADER: &str = if vma::WIDE { "/lib/ld-musl-aarch64.so.1 is not a dynamic loader" } else { "/lib/ld-musl-armhf.so.1 is not a dynamic loader" };

/// The dynamic loader an AArch64 program may name: musl's, as shipped.
pub const LOADER64: &str = "/lib/ld-musl-aarch64.so.1";
/// e_machine EM_AARCH64.
pub const EM_AARCH64: u32 = 183;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Seg64 {
    pub vaddr: u64,
    pub offset: u64,
    pub filesz: u64,
    pub memsz: u64,
    /// PF_X 1, PF_W 2, PF_R 4.
    pub flags: u32,
}

/// `Plan` for a 64-bit program: the same facts with 64-bit addresses.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan64 {
    pub pie: bool,
    pub entry: u64,
    pub phoff: u64,
    pub phentsize: u32,
    pub phnum: u32,
    pub phdr: Option<u64>,
    pub segs: Vec<Seg64>,
    pub interp: bool,
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at.checked_add(8)?)?;
    let mut w = [0u8; 8];
    w.copy_from_slice(s);
    Some(u64::from_le_bytes(w))
}

/// The program headers of a 64-bit little-endian AArch64 executable, with
/// the checks `plan` makes: in-file and non-overflowing segments, no segment
/// both writable and executable, at most 16, PT_INTERP naming exactly
/// LOADER64. A fixed-address (ET_EXEC) program must also lie below
/// `user_top`; a PIE's segments are checked again where they are placed.
pub fn plan64(image: &[u8], user_top: u64) -> Result<Plan64, &'static str> {
    if image.len() < 64 || &image[..4] != b"\x7fELF" || image[4] != 2 || image[5] != 1 {
        return Err("not a 64-bit little-endian ELF file");
    }
    let half = |at| u16_at(image, at).ok_or("bad ELF header");
    let long = |at| u64_at(image, at).ok_or("bad ELF header");
    if half(18)? != EM_AARCH64 {
        return Err("not an AArch64 executable");
    }
    let pie = match half(16)? {
        2 => false,
        3 => true,
        _ => return Err("not an executable"),
    };
    let (phoff, phentsize, phnum) = (long(32)?, half(54)?, half(56)?);
    let headers_end = usize::try_from(phoff).ok().and_then(|o| (phentsize as usize).checked_mul(phnum as usize)?.checked_add(o));
    if phentsize < 56 || headers_end.map_or(true, |e| e > image.len()) {
        return Err("bad program headers");
    }
    let mut p = Plan64 { pie, entry: long(24)?, phoff, phentsize, phnum, phdr: None, segs: Vec::new(), interp: false };
    for i in 0..phnum as usize {
        let h = phoff as usize + i * phentsize as usize;
        let w32 = |at: usize| u32_at(image, h + at).ok_or("bad program headers");
        let w64 = |at: usize| u64_at(image, h + at).ok_or("bad program headers");
        let (ptype, flags, offset, vaddr, filesz, memsz) = (w32(0)?, w32(4)?, w64(8)?, w64(16)?, w64(32)?, w64(40)?);
        match ptype {
            1 if memsz != 0 => {
                let in_file = offset.checked_add(filesz).is_some_and(|e| e <= image.len() as u64);
                if !in_file || filesz > memsz {
                    return Err("bad segment");
                }
                let end = vaddr.checked_add(memsz).ok_or("segment too high")?;
                if !pie && end > user_top {
                    return Err("segment too high");
                }
                if flags & 3 == 3 {
                    return Err("a segment is writable and executable");
                }
                if p.segs.len() >= 16 {
                    return Err("too many segments");
                }
                p.segs.push(Seg64 { vaddr, offset, filesz, memsz, flags });
            }
            3 => {
                let path = usize::try_from(offset).ok().and_then(|o| image.get(o..o.checked_add(usize::try_from(filesz).ok()?)?));
                let ok = !p.interp && path.is_some_and(|s| s.len() == LOADER64.len() + 1 && s.ends_with(b"\0") && &s[..LOADER64.len()] == LOADER64.as_bytes());
                if !ok {
                    return Err("dynamically linked against an unsupported loader (only /lib/ld-musl-aarch64.so.1)");
                }
                p.interp = true;
            }
            6 => p.phdr = Some(vaddr),
            _ => {}
        }
    }
    if p.segs.is_empty() {
        return Err("no loadable segments");
    }
    Ok(p)
}

impl Plan64 {
    /// The lowest and highest address the segments use, rounded out to 4 KiB.
    pub fn span(&self) -> (u64, u64) {
        let lo = self.segs.iter().map(|s| s.vaddr).min().unwrap_or(0) & !0xfff;
        let hi = self.segs.iter().map(|s| s.vaddr.saturating_add(s.memsz)).max().unwrap_or(0);
        (lo, hi.saturating_add(0xfff) & !0xfff)
    }
    /// Where the program headers are once loaded at `base` (AT_PHDR).
    pub fn phdr_at(&self, base: u64) -> u64 {
        if let Some(v) = self.phdr {
            return base.wrapping_add(v);
        }
        self.segs
            .iter()
            .find(|s| s.offset <= self.phoff && self.phoff - s.offset < s.filesz)
            .map_or(0, |s| base.wrapping_add(s.vaddr).wrapping_add(self.phoff - s.offset))
    }
    pub fn loader_ok(&self) -> bool {
        self.pie && !self.interp
    }
    /// Whether two segments share a 4 KiB page with different protections
    /// (code with data): refused, since a page has one protection.
    pub fn mixed_pages(&self) -> bool {
        let pages = |s: &Seg64| (s.vaddr >> 12, (s.vaddr.saturating_add(s.memsz).saturating_add(0xfff)) >> 12);
        self.segs.iter().enumerate().any(|(i, a)| {
            self.segs[i + 1..].iter().any(|b| {
                let ((a0, a1), (b0, b1)) = (pages(a), pages(b));
                a0 < b1 && b0 < a1 && (a.flags ^ b.flags) & 1 != 0
            })
        })
    }
}

// ---- where the segments go (the loader's rules, with no memory touched) ----

/// Add the regions of segment `s` loaded at `base` (all below `limit`) to
/// `vmas`, as procmgr.rs `segments` does: its own protection (text
/// read-execute, data read-write), 4 KiB-rounded, and a data neighbour on
/// the same 4 KiB page keeping both protections. Code sharing a page with
/// data is refused. Returns (address, end, rounded start, rounded end).
pub fn region(vmas: &mut Vec<Vma>, s: &Seg, base: u32, limit: u32) -> Result<(u32, u32, u32, u32), &'static str> {
    let va = base.checked_add(s.vaddr).ok_or("bad segment address")?;
    let end = va.checked_add(s.memsz).filter(|&e| e <= limit).ok_or("segment too high")?;
    let mut prot = PROT_READ;
    if s.flags & 2 != 0 { prot |= PROT_WRITE; }
    if s.flags & 1 != 0 { prot |= PROT_EXEC; }
    let (start, stop) = (va & !0xfff, end.checked_add(0xfff).ok_or("segment too high")? & !0xfff);
    let shared: Vec<Vma> = vmas.iter().filter(|v| v.end > start && v.start < stop).copied().collect();
    // Never code with anything else: the merged page would run both.
    if shared.iter().any(|v| (v.prot ^ prot) & PROT_EXEC != 0) {
        return Err("code shares a 64 KiB page with data");
    }
    vma::insert(vmas, start, stop, prot);
    for v in shared {
        vma::insert(vmas, v.start.max(start), v.end.min(stop), v.prot | prot);
    }
    Ok((va, end, start, stop))
}

/// A free range of `len` bytes (rounded up to 64 KiB pages) in the mmap
/// area below `top`, highest first (procmgr.rs `free_range`).
pub fn free_in(vmas: &[Vma], top: u32, len: u32) -> Option<u32> {
    let page = 1u32 << 16;
    let len = len.checked_add(page - 1)? & !(page - 1);
    let mut top = top;
    for v in vmas.iter().rev() {
        if v.start >= top {
            continue;
        }
        let floor = v.end.max(MMAP_BASE).checked_add(page - 1)? & !(page - 1);
        if floor <= top && top - floor >= len {
            return Some(top - len);
        }
        top = top.min(v.start & !(page - 1));
        if top <= MMAP_BASE {
            return None;
        }
    }
    (top >= MMAP_BASE.checked_add(len)?).then(|| top - len)
}

/// Everything procmgr.rs `load` checks before it touches memory, on a fresh
/// address space at `layout`: the headers, every segment's place and the
/// W^X rules for the program and its loader (`ld`, which a dynamically
/// linked program needs), and room for the loader. execve runs this before
/// it destroys the caller, so only running out of memory can fail after.
pub fn check(image: &[u8], ld: Option<&[u8]>, layout: &Layout) -> Result<(), &'static str> {
    let p = plan(image)?;
    let base = if p.pie { layout.pie_base } else { 0 };
    let mut vmas = Vec::new();
    for s in &p.segs {
        region(&mut vmas, s, base, MMAP_BASE)?;
    }
    if p.interp {
        let l = plan(ld.ok_or(NO_LOADER)?)?;
        if !l.loader_ok() {
            return Err(NOT_LOADER);
        }
        let (lo, hi) = l.span();
        let lo = lo & !0xffff;
        let at = free_in(&vmas, layout.mmap_top, hi - lo).ok_or("no room for the dynamic loader")?;
        for s in &l.segs {
            region(&mut vmas, s, at.wrapping_sub(lo), layout.mmap_top)?;
        }
    }
    if vma::mixed(&vmas) {
        return Err("code shares a 64 KiB page with data");
    }
    Ok(())
}
