//! A full consistency check of a mounted VioletFS (read-only apart from the
//! commit it starts with). It needs memory the filesystem itself never does,
//! a bit per block and a counter per inode, so the caller lends it.
use super::*;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Report {
    pub files: u32,
    pub dirs: u32,
    pub links: u32,
    pub used_blocks: u32,
    pub free_blocks: u32,
}

impl<D: BlockDev> Fs<D> {
    fn mark(&mut self, map: &mut [u8], b: u32) -> Result<(), Error> {
        if !self.okp(b) || map[(b / 8) as usize] & 1 << (b % 8) != 0 {
            bad!(self); // outside the data area, or used twice
        }
        map[(b / 8) as usize] |= 1 << (b % 8);
        Ok(())
    }
    fn check_tree(&mut self, map: &mut [u8], blk: u32, level: usize, base: u64, keep: u64) -> Result<u32, Error> {
        let span = if level == 1 { 1 } else if level == 2 { P } else { P * P };
        let mut count = 0;
        for k in 0..P as usize {
            let child = self.rd(blk, k * 4)?;
            if child == 0 {
                continue;
            }
            let first = base + k as u64 * span;
            if first >= keep {
                bad!(self); // a block past the end of the file
            }
            self.mark(map, child)?;
            count += 1;
            if level > 1 {
                count += self.check_tree(map, child, level - 1, first, keep)?;
            }
        }
        Ok(count)
    }

    /// Verify that every block is used exactly as the bitmap says, every
    /// inode's block and link counts are right, every directory is well
    /// formed and names live inodes, and the free counts and each owner's
    /// quota usage add up. `map`: a bit per block; `links`: an entry per
    /// inode and one more. `Corrupt` (with `why`) on the first thing wrong.
    /// Not checked: two equal names in one directory, file data.
    pub fn check(&mut self, map: &mut [u8], links: &mut [u16]) -> Result<Report, Error> {
        self.commit()?;
        let r = self.check_inner(map, links);
        self.guard(r)
    }
    fn check_inner(&mut self, map: &mut [u8], links: &mut [u16]) -> Result<Report, Error> {
        let g = self.g;
        if map.len() < (g.blocks as usize).div_ceil(8) || links.len() <= g.inodes as usize {
            return Err(Error::Invalid);
        }
        map.fill(0);
        links.fill(0);
        for b in 0..g.data_start {
            map[(b / 8) as usize] |= 1 << (b % 8);
        }
        if self.st(S_ORPHAN)? != 0 {
            bad!(self);
        }
        let mut tally = [(0u32, 0u32); QUOTAS];
        let mut rep = Report::default();
        let mut used_inodes = 0;
        for ino in 1..=g.inodes {
            let n = self.iget(ino)?;
            if n.mode == 0 {
                continue;
            }
            used_inodes += 1;
            match n.kind() {
                S_FILE => rep.files += 1,
                S_DIR => rep.dirs += 1,
                S_LINK => rep.links += 1,
                _ => bad!(self),
            }
            let keep = n.size.div_ceil(B);
            let mut count = 0u32;
            for k in 0..12 {
                if n.ptr[k] != 0 {
                    if k as u64 >= keep {
                        bad!(self);
                    }
                    self.mark(map, n.ptr[k])?;
                    count += 1;
                }
            }
            let (mut base, mut span) = (12u64, P);
            for k in 12..15 {
                if n.ptr[k] != 0 {
                    if base >= keep {
                        bad!(self);
                    }
                    self.mark(map, n.ptr[k])?;
                    count += 1 + self.check_tree(map, n.ptr[k], k - 11, base, keep)?;
                }
                base += span;
                span *= P;
            }
            if count != n.blocks {
                bad!(self);
            }
            let Some(at) = self.quota_find(n.owner, false)? else { bad!(self) };
            let q = (at - QOFF) / QSIZE;
            tally[q] = (tally[q].0 + count, tally[q].1 + 1);
            if !n.is_dir() {
                continue;
            }
            if n.size % B != 0 || n.nlink != 1 {
                bad!(self);
            }
            if ino != ROOT && (n.parent == 0 || n.parent > g.inodes || !self.iget(n.parent)?.is_dir()) {
                bad!(self);
            }
            let (mut pos, mut prev) = (0, usize::MAX);
            while let Some(r) = self.dnext(&n, &mut pos, &mut prev)? {
                if r.ino == 0 {
                    continue;
                }
                if r.ino > g.inodes || links[r.ino as usize] == u16::MAX {
                    bad!(self);
                }
                links[r.ino as usize] += 1;
                let t = self.iget(r.ino)?;
                if t.mode == 0 || (t.mode >> 12) as u8 != r.kind || (t.is_dir() && t.parent != ino) {
                    bad!(self);
                }
            }
        }
        for ino in 1..=g.inodes {
            let n = self.iget(ino)?;
            let named = links[ino as usize] + (ino == ROOT) as u16;
            if n.nlink != named && n.mode != 0 || n.mode == 0 && named != 0 {
                bad!(self);
            }
        }
        let mut used = 0u32;
        for k in 0..g.bitmap_blocks {
            let i = self.load(g.bitmap_start + k, false)?;
            self.home_blk = 0;
            self.home = self.cache[i].data;
            for byte in 0..BODY {
                let b = k as u64 * BITS as u64 + byte as u64 * 8;
                let mut want = 0u8;
                for bit in 0..8u64 {
                    let at = b + bit;
                    if at >= g.blocks as u64 || map[(at / 8) as usize] & 1 << (at % 8) != 0 {
                        want |= 1 << bit;
                        used += (at < g.blocks as u64) as u32;
                    }
                }
                if self.home[byte] != want {
                    bad!(self); // a leaked block, or one in use that is marked free
                }
            }
        }
        if self.st(S_FREE)? != g.blocks - used || self.st(S_IFREE)? != g.inodes - used_inodes {
            bad!(self);
        }
        for (k, t) in tally.iter().enumerate() {
            let at = QOFF + k * QSIZE;
            let have = (self.st(at + 8)?, self.st(at + 16)?);
            if self.st(at + 4)? & 1 == 0 && *t != (0, 0) || self.st(at + 4)? & 1 != 0 && have != *t {
                bad!(self);
            }
        }
        rep.used_blocks = used - g.data_start;
        rep.free_blocks = g.blocks - used;
        Ok(rep)
    }
}
