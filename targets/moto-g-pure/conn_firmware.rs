//! ROM images run in connectivity EMI. WLAN RAM code has a separate directory
//! and mixes EMI payloads with encrypted sections for the firmware downloader.
use super::wlan_image;
const COUNT: usize = 11;
pub(super) fn files(bundle: &[u8]) -> Option<[&[u8]; COUNT]> {
    if bundle.get(..8)? != b"VRF2\x0b\0\0\0" {
        return None;
    }
    let end = 8 + COUNT * 8;
    let mut pos = end;
    let mut out = [&[][..]; COUNT];
    for (slot, entry) in out.iter_mut().zip(bundle.get(8..end)?.chunks_exact(8)) {
        let start = u32::from_le_bytes(entry[..4].try_into().ok()?) as usize;
        let len = u32::from_le_bytes(entry[4..].try_into().ok()?) as usize;
        if start != pos || len == 0 {
            return None;
        }
        let stop = start.checked_add(len)?;
        *slot = bundle.get(start..stop)?;
        pos = stop.checked_add(3)? & !3;
    }
    Some(out)
}
pub fn stage(bundle: &[u8], emi: &mut [u8]) -> Option<usize> {
    let fw = files(bundle)?;
    // Fixed ranges for this board. The low address byte is a packaging
    // sequence flag. The vendor driver consumes the body after its 48 B header.
    let roms = [
        (0, 4, 0, 0x68320),
        (1, 3, 0x140000, 0x68310),
        (6, 0, 0x80000, 0x68300),
    ];
    for &(id, kind, start, _) in &roms {
        let h = fw[id].get(..48)?;
        if &h[16..24] != b"ALPS\x8a\0\x8a\0"
            || h[31] != kind
            || u32::from_le_bytes(h[24..28].try_into().ok()?) != (0xf0000011 | start as u32)
            || !h[..14].iter().all(u8::is_ascii_digit)
            || !(h[14..16].iter().all(u8::is_ascii_digit) || h[14..16] == [0, 0])
        {
            return None;
        }
        let limit = match kind {
            4 => 0x68000,
            0 => 0x140000,
            _ => 0x177000,
        };
        if fw[id].len() == 48 || fw[id].len() - 48 > limit - start || emi.len() < limit {
            return None;
        }
    }
    let mut bytes = wlan_image::stage_shared(fw[3], emi)?;
    for (id, _, start, stamp) in roms {
        let src = &fw[id][48..];
        let dst = &mut emi[start..start + src.len()];
        for (p, b) in dst.iter_mut().zip(src) {
            unsafe { core::ptr::write_volatile(p, *b) };
        }
        if !dst
            .iter()
            .zip(src)
            .all(|(p, b)| unsafe { core::ptr::read_volatile(p) } == *b)
        {
            return None;
        }
        emi[stamp..stamp + 15].copy_from_slice(&fw[id][..15]);
        emi[stamp + 15] = 0;
        bytes += src.len();
    }
    Some(bytes)
}
