//! A pair of sectors holds one app checkpoint. The value field retains its
//! original stopwatch wire layout. Each update replaces the older sector.
pub type Sector = [u8; 512];
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub generation: u64,
    pub tenths: u32,
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ ((0u32.wrapping_sub(crc & 1)) & 0xedb88320);
        }
    }
    !crc
}
impl Record {
    pub fn encode(self) -> Sector {
        let mut data = [0; 512];
        data[..8].copy_from_slice(b"VIOLET01");
        data[8..16].copy_from_slice(&self.generation.to_le_bytes());
        data[16..20].copy_from_slice(&self.tenths.to_le_bytes());
        let sum = crc32(&data[..508]);
        data[508..].copy_from_slice(&sum.to_le_bytes());
        data
    }
    fn decode(data: &Sector) -> Option<Self> {
        if &data[..8] != b"VIOLET01" || data[508..] != crc32(&data[..508]).to_le_bytes() {
            return None;
        }
        Some(Self {
            generation: u64::from_le_bytes(data[8..16].try_into().ok()?),
            tenths: u32::from_le_bytes(data[16..20].try_into().ok()?),
        })
    }
}
// Distinguish a new volume from loss of both committed records.
pub fn recover(a: &Sector, b: &Sector) -> Result<(usize, Record), ()> {
    match (Record::decode(a), Record::decode(b)) {
        (Some(x), Some(y)) if y.generation > x.generation => Ok((1, y)),
        (Some(x), _) => Ok((0, x)),
        (_, Some(y)) => Ok((1, y)),
        (None, None) if a.iter().chain(b).all(|&v| v == 0) => Ok((
            1,
            Record {
                generation: 0,
                tenths: 0,
            },
        )),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn torn_replacement_preserves_last_checkpoint() {
        let old = Record {
            generation: 8,
            tenths: 92,
        };
        let stale = Record {
            generation: 7,
            tenths: 11,
        }
        .encode();
        let next = Record {
            generation: 9,
            tenths: 0,
        }
        .encode();
        for split in 0..512 {
            let mut torn = stale;
            torn[..split].copy_from_slice(&next[..split]);
            assert_eq!(recover(&old.encode(), &torn).unwrap().1, old);
        }
        assert_eq!(recover(&old.encode(), &next).unwrap().1.tenths, 0);
        assert!(recover(&[1; 512], &[2; 512]).is_err());
    }
}
