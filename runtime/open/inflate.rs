//! DEFLATE decoding (RFC 1951) inside a gzip member (RFC 1952), for the
//! compressed file system image the handset's bootloader loads. It decodes into
//! a caller's buffer without allocating; tables are canonical Huffman codes
//! decoded a bit at a time.

/// The output buffer and how much of it is filled.
pub struct Out<'a> {
    buf: &'a mut [u8],
    len: usize,
}
impl<'a> Out<'a> {
    fn push(&mut self, b: u8) -> Result<(), &'static str> {
        *self.buf.get_mut(self.len).ok_or("output larger than expected")? = b;
        self.len += 1;
        Ok(())
    }
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    bit: u32,
    count: u32,
}
impl<'a> Bits<'a> {
    fn need(&mut self, n: u32) -> Result<(), &'static str> {
        while self.count < n {
            let b = *self.data.get(self.at).ok_or("compressed data ends early")?;
            self.at += 1;
            self.bit |= u32::from(b) << self.count;
            self.count += 8;
        }
        Ok(())
    }
    fn take(&mut self, n: u32) -> Result<u32, &'static str> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1 << n) - 1);
        self.bit >>= n;
        self.count -= n;
        Ok(v)
    }
    fn align(&mut self) {
        self.bit = 0;
        self.count = 0;
    }
}

/// counts[len] = codes of that length; symbols sorted by code.
struct Huffman {
    counts: [u16; 16],
    symbols: [u16; 320],
}
impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, &'static str> {
        let mut h = Huffman { counts: [0; 16], symbols: [0; 320] };
        for &l in lengths {
            h.counts[l as usize] += 1;
        }
        h.counts[0] = 0;
        let mut offsets = [0u16; 16];
        for i in 1..16 {
            offsets[i] = offsets[i - 1] + h.counts[i - 1];
        }
        for (symbol, &l) in lengths.iter().enumerate() {
            if l != 0 {
                h.symbols[offsets[l as usize] as usize] = symbol as u16;
                offsets[l as usize] += 1;
            }
        }
        Ok(h)
    }
    fn decode(&self, bits: &mut Bits) -> Result<u16, &'static str> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return Ok(self.symbols[(index + code - first) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("bad Huffman code")
    }
}

const LENGTH_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn block(bits: &mut Bits, out: &mut Out, lit: &Huffman, dist: &Huffman) -> Result<(), &'static str> {
    loop {
        let sym = lit.decode(bits)?;
        if sym < 256 {
            out.push(sym as u8)?;
        } else if sym == 256 {
            return Ok(());
        } else {
            let i = (sym - 257) as usize;
            if i >= 29 {
                return Err("bad length symbol");
            }
            let len = LENGTH_BASE[i] as usize + bits.take(u32::from(LENGTH_EXTRA[i]))? as usize;
            let d = dist.decode(bits)? as usize;
            if d >= 30 {
                return Err("bad distance symbol");
            }
            let back = DIST_BASE[d] as usize + bits.take(u32::from(DIST_EXTRA[d]))? as usize;
            if back > out.len {
                return Err("distance before start of output");
            }
            let start = out.len - back;
            for k in 0..len {
                let b = out.buf[start + k];
                out.push(b)?;
            }
        }
    }
}

/// Decode a raw DEFLATE stream, appending to `out`.
pub fn inflate(data: &[u8], out: &mut Out) -> Result<usize, &'static str> {
    let mut bits = Bits { data, at: 0, bit: 0, count: 0 };
    loop {
        let last = bits.take(1)?;
        match bits.take(2)? {
            0 => {
                bits.align();
                let at = bits.at;
                let header = data.get(at..at + 4).ok_or("stored block ends early")?;
                let len = usize::from(u16::from_le_bytes([header[0], header[1]]));
                let nlen = u16::from_le_bytes([header[2], header[3]]);
                if nlen != !(len as u16) {
                    return Err("bad stored block length");
                }
                for &b in data.get(at + 4..at + 4 + len).ok_or("stored block ends early")? {
                    out.push(b)?;
                }
                bits.at = at + 4 + len;
            }
            1 => {
                let mut lengths = [0u8; 288];
                for (i, l) in lengths.iter_mut().enumerate() {
                    *l = match i { 0..=143 => 8, 144..=255 => 9, 256..=279 => 7, _ => 8 };
                }
                let lit = Huffman::new(&lengths)?;
                let dist = Huffman::new(&[5u8; 30])?;
                block(&mut bits, out, &lit, &dist)?;
            }
            2 => {
                let hlit = bits.take(5)? as usize + 257;
                let hdist = bits.take(5)? as usize + 1;
                let hclen = bits.take(4)? as usize + 4;
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut code_lengths = [0u8; 19];
                for &o in ORDER.iter().take(hclen) {
                    code_lengths[o] = bits.take(3)? as u8;
                }
                let codes = Huffman::new(&code_lengths)?;
                let mut lengths = [0u8; 320];
                let mut n = 0;
                while n < hlit + hdist {
                    let sym = codes.decode(&mut bits)?;
                    let (value, repeat) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => {
                            let prev = *lengths.get(n.wrapping_sub(1)).ok_or("repeat with no previous length")?;
                            (prev, 3 + bits.take(2)? as usize)
                        }
                        17 => (0, 3 + bits.take(3)? as usize),
                        _ => (0, 11 + bits.take(7)? as usize),
                    };
                    if n + repeat > hlit + hdist {
                        return Err("too many code lengths");
                    }
                    for l in &mut lengths[n..n + repeat] {
                        *l = value;
                    }
                    n += repeat;
                }
                let lit = Huffman::new(&lengths[..hlit])?;
                let dist = Huffman::new(&lengths[hlit..hlit + hdist])?;
                block(&mut bits, out, &lit, &dist)?;
            }
            _ => return Err("bad block type"),
        }
        if last == 1 {
            return Ok(bits.at);
        }
    }
}

/// The decoded size a gzip file records in its trailer.
/// Decode a raw DEFLATE stream into `dest`, which must hold all of it.
pub fn inflate_into(data: &[u8], dest: &mut [u8]) -> Result<usize, &'static str> {
    let mut out = Out { buf: dest, len: 0 };
    inflate(data, &mut out)?;
    Ok(out.len)
}

pub fn gzip_size(data: &[u8]) -> Option<usize> {
    let t = data.get(data.len().checked_sub(4)?..)?;
    Some(u32::from_le_bytes(t.try_into().ok()?) as usize)
}

/// Decode a gzip file (a single member) into `dest`, which must hold
/// gzip_size() bytes. Returns the decoded length.
pub fn gunzip(data: &[u8], dest: &mut [u8]) -> Result<usize, &'static str> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return Err("not gzip");
    }
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        at += 2 + usize::from(u16::from_le_bytes([data[at], data[at + 1]]));
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            at += data[at..].iter().position(|&b| b == 0).ok_or("bad gzip header")? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    let size = gzip_size(data).ok_or("bad gzip trailer")?;
    let mut out = Out { buf: dest, len: 0 };
    inflate(&data[at..], &mut out)?;
    if out.len != size {
        return Err("gzip length mismatch");
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::process::Command;

    #[test]
    fn matches_gzip() {
        // Text, repetition and random bytes exercise stored, fixed and dynamic blocks.
        let mut input = alloc::vec::Vec::new();
        for i in 0..20000u32 {
            input.extend_from_slice(alloc::format!("line {} of the violet file system\n", i % 37).as_bytes());
        }
        let mut x = 12345u32;
        for _ in 0..70000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            input.push(x as u8);
        }
        let dir = std::env::temp_dir().join(alloc::format!("violet-inflate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data");
        for level in ["-1", "-9"] {
            std::fs::write(&path, &input).unwrap();
            let gz = Command::new("gzip").args([level, "-c"]).arg(&path).output().unwrap().stdout;
            let mut out = alloc::vec![0u8; gzip_size(&gz).unwrap()];
            assert_eq!(gunzip(&gz, &mut out).unwrap(), input.len());
            assert_eq!(out, input, "gzip {}", level);
            let mut short = alloc::vec![0u8; input.len() - 1];
            assert!(gunzip(&gz, &mut short).is_err(), "a too-small buffer is an error");
        }
        assert!(gunzip(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0], &mut []).is_ok());
        assert!(gunzip(b"not gzip at all, sorry", &mut []).is_err());
    }
}
