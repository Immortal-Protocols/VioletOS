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

pub const LOADER: &str = "/lib/ld-musl-armhf.so.1";

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

/// The program headers of a 32-bit little-endian ARM executable.
pub fn plan(image: &[u8]) -> Result<Plan, &'static str> {
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
                let ok = !p.interp && path.is_some_and(|s| s.len() == LOADER.len() + 1 && s.ends_with(b"\0") && &s[..LOADER.len()] == LOADER.as_bytes());
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
        let l = plan(ld.ok_or("its dynamic loader /lib/ld-musl-armhf.so.1 is not in the system image")?)?;
        if !l.loader_ok() {
            return Err("/lib/ld-musl-armhf.so.1 is not a dynamic loader");
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
