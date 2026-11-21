//! SoC1.0 Connac format-2 download table for the Ellis WLAN processor.
//! The image ends with 40 bytes per region and a 36-byte common trailer.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Destination {
    Chip(u32),
    SharedMemory(usize),
}
pub struct Region<'a> {
    pub destination: Destination,
    pub bytes: &'a [u8],
    pub flags: u8,
}
pub fn regions(image: &[u8]) -> Option<[Region<'_>; 5]> {
    let tail = image.len().checked_sub(36)?;
    if image[tail..tail + 4] != [7, 0, 5, 2] {
        return None;
    }
    let table = tail.checked_sub(5 * 40)?;
    let mut descriptors = image[table..tail].chunks_exact(40);
    let mut body = &image[..table];
    let mut next = || {
        let descriptor = descriptors.next()?;
        let address = u32::from_le_bytes(descriptor[16..20].try_into().ok()?);
        let size = u32::from_le_bytes(descriptor[20..24].try_into().ok()?);
        let flags = descriptor[24];
        let end = address.checked_add(size)?;
        if size == 0 {
            return None;
        }
        let destination = match (flags, address) {
            (7, 0x57000 | 0x02010000 | 0x40100200) => Destination::Chip(address),
            (0x80, 0xf0177000) if end <= 0xf01fe000 => Destination::SharedMemory(0x177000),
            (0x80, 0xf01fe000) if end <= 0xf0400000 => {
                Destination::SharedMemory((address & 0xffffff) as usize)
            }
            _ => return None,
        };
        let (bytes, rest) = body.split_at_checked(size as usize)?;
        body = rest;
        Some(Region {
            destination,
            bytes,
            flags,
        })
    };
    Some([next()?, next()?, next()?, next()?, next()?])
}

/// Copy and read back only regions marked DL_TO_EMI in the factory image.
/// Validate the entire transfer before changing the first destination byte.
/// The remaining encrypted segments must be uploaded over the WLAN transport.
pub fn stage_shared(image: &[u8], emi: &mut [u8]) -> Option<usize> {
    let regions = regions(image)?;
    let mut ranges = [(0, 0); 2];
    let mut count = 0;
    for region in &regions {
        if let Destination::SharedMemory(start) = region.destination {
            let end = start.checked_add(region.bytes.len())?;
            if end > emi.len()
                || count == ranges.len()
                || ranges[..count]
                    .iter()
                    .any(|&(lo, hi)| start < hi && lo < end)
            {
                return None;
            }
            ranges[count] = (start, end);
            count += 1;
        }
    }
    if count != ranges.len() {
        return None;
    }
    let mut bytes = 0;
    for region in regions {
        if let Destination::SharedMemory(start) = region.destination {
            let dest = &mut emi[start..start + region.bytes.len()];
            // This region is mapped uncached into Radio. Volatile accesses
            // ensure readback comes from the mapped memory.
            for (cell, value) in dest.iter_mut().zip(region.bytes) {
                unsafe {
                    core::ptr::write_volatile(cell, *value);
                }
            }
            for (cell, value) in dest.iter().zip(region.bytes) {
                if unsafe { core::ptr::read_volatile(cell) } != *value {
                    return None;
                }
            }
            bytes += dest.len();
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn factory_segments() {
        let data = include_bytes!(env!("VIOLET_TEST_WLAN_IMAGE"));
        let parsed = regions(data).unwrap();
        assert_eq!(
            parsed.map(|r| (r.destination, r.bytes.len(), r.flags)),
            [
                (Destination::Chip(0x57000), 4016, 7),
                (Destination::Chip(0x02010000), 336, 7),
                (Destination::Chip(0x40100200), 9616, 7),
                (Destination::SharedMemory(0x177000), 477408, 128),
                (Destination::SharedMemory(0x1fe000), 142416, 128),
            ]
        );
        let mut emi = vec![0xa5; 4 << 20];
        assert_eq!(stage_shared(data, &mut emi), Some(619824));
        for r in regions(data).unwrap() {
            if let Destination::SharedMemory(start) = r.destination {
                assert_eq!(&emi[start..start + r.bytes.len()], r.bytes);
                assert_eq!(emi[start - 1], 0xa5);
                assert_eq!(emi[start + r.bytes.len()], 0xa5);
            }
        }
        let mut short = vec![0x5a; 0x200000];
        assert_eq!(stage_shared(data, &mut short), None);
        assert!(short.iter().all(|&b| b == 0x5a));
        let mut damaged = data.to_vec();
        let size = damaged.len() - 36 - 5 * 40 + 20;
        damaged[size..size + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(regions(&damaged).is_none());
        assert!(regions(&data[..100]).is_none());
    }
}
