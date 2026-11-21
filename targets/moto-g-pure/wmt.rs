//! One outstanding WMT transaction over the Ellis BTIF programmed-I/O port.
use super::radio_io::{byte, expired, now, put, rd, wr};
const PORT: usize = 0x09010000;
fn read(off: usize) -> u32 {
    rd(PORT + off)
}
fn set(off: usize, v: u32) {
    wr(PORT + off, v)
}
fn transmit(bytes: &[u8]) -> Result<(), u32> {
    let start = now();
    for byte in bytes {
        while read(0x14) & 0x60 == 0 {
            if expired(start, 1500) {
                return Err(21);
            }
        }
        put(PORT, *byte);
    }
    Ok(())
}
fn receive(start: u32) -> Result<u8, u32> {
    while read(0x14) & 1 == 0 {
        if expired(start, 1800) {
            return Err(22);
        }
    }
    Ok(byte(PORT))
}
fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for b in bytes {
        crc ^= *b as u16;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xa001 } else { 0 };
        }
    }
    crc
}
struct Link {
    full: bool,
    tx: u8,
    ack: u8,
}
impl Link {
    fn exchange(&mut self, payload: &[u8], expected: &[u8]) -> Result<(), u32> {
        let n = payload.len();
        super::radio_io::trace(
            0,
            (u32::from(payload[1]) << 16) | (u32::from(payload[4]) << 8) | self.tx as u32,
        );
        super::radio_io::trace(1, 0);
        super::radio_io::trace(2, 0);
        let mut h = [0x80, 0x40 | ((n >> 8) as u8), n as u8, 0];
        let mut crc = 0;
        if self.full {
            h[0] |= self.tx << 3 | self.ack;
            h[3] = h[0].wrapping_add(h[1]).wrapping_add(h[2]);
            crc = crc16(payload);
            self.tx = (self.tx + 1) & 7;
        }
        transmit(&h)?;
        transmit(payload)?;
        transmit(&crc.to_le_bytes())?;
        let start = now();
        loop {
            if expired(start, 1800) {
                return Err(22);
            }
            let first = receive(start)?;
            if first & 0xc0 != 0x80 {
                if expired(start, 1800) {
                    return Err(22);
                }
                continue;
            }
            let kind = receive(start)?;
            let lo = receive(start)?;
            let sum = receive(start)?;
            super::radio_io::trace(3, u32::from_le_bytes([first, kind, lo, sum]));
            if self.full && first.wrapping_add(kind).wrapping_add(lo) != sum {
                return Err(23);
            }
            let len = ((kind as usize & 15) << 8) | lo as usize;
            if len == 0 {
                if expired(start, 1800) {
                    return Err(22);
                }
                continue;
            }
            let mut body = [0; 2048];
            if len > body.len() {
                return Err(23);
            }
            for b in &mut body[..len] {
                *b = receive(start)?;
            }
            let check = u16::from_le_bytes([receive(start)?, receive(start)?]);
            if self.full {
                if check != crc16(&body[..len]) || (first >> 3) & 7 != (self.ack + 1) & 7 {
                    return Err(23);
                }
                self.ack = (first >> 3) & 7;
                let ack = 0x80 | self.ack;
                transmit(&[ack, 0, 0, ack])?;
            }
            if kind >> 4 != 4 {
                continue;
            }
            super::radio_io::trace(1, u32::from_le_bytes(body[..4].try_into().unwrap()));
            super::radio_io::trace(2, u32::from_le_bytes(body[4..8].try_into().unwrap()));
            // Ellis includes calibration data after the success/subcommand bytes.
            let calibration = payload == [1, 0x14, 1, 0, 1]
                && len >= 6
                && body[..2] == [2, 0x14]
                && usize::from(u16::from_le_bytes([body[2], body[3]])) == len - 4
                && body[4..6] == [0, 1];
            if !calibration && &body[..len] != expected {
                return Err(24);
            }
            return Ok(());
        }
    }
}
pub fn enable(patch: &[u8], profile: u32, stage: fn(u32)) -> Result<(), u32> {
    if !(profile == 1 || profile == 2) {
        return Err(16);
    }
    // WMT_PATCH is a 28-byte header followed by the two-byte checksum.
    if patch.get(16..28) != Some(&b"ALPS\x8a\0\x8a\0\x11\xc0\x01\0"[..]) || patch.len() <= 30 {
        return Err(25);
    }
    let data = &patch[30..];
    stage(20);
    set(0x0c, 0);
    set(0x6c, 1);
    set(8, 2);
    set(8, 0);
    set(8, 4);
    set(8, 0);
    set(0x60, 0x18);
    set(0x4c, 4);
    set(4, 1);
    let mut link = Link {
        full: false,
        tx: 0,
        ack: 7,
    };
    link.exchange(&[1, 4, 1, 0, 4], &[2, 4, 6, 0, 0, 4, 0x11, 0, 0, 0])?;
    stage(26);
    let mut config = [
        1, 1, 17, 0, 0, 0, 0xc0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x2f, 2, 2, 0,
    ];
    config[9..13].copy_from_slice(&(data.len() as u32).to_le_bytes());
    link.exchange(&config, &[2, 1, 1, 0, 0])?;
    let mut hdr = [0; 12];
    hdr[..4].copy_from_slice(&((data.len() + 12) as u32).to_le_bytes());
    transmit(&hdr)?;
    transmit(data)?;
    link.exchange(&[1, 7, 1, 0, 4], &[2, 7, 1, 0, 0])?;
    stage(27);
    link.exchange(&[1, 4, 1, 0, 4], &[2, 4, 6, 0, 0, 4, 0x11, 0, 0, 0])?;
    link.exchange(&[1, 4, 5, 0, 3, 0xdb, 0x0e, 0x68, 1], &[2, 4, 2, 0, 0, 3])?;
    // The controller acknowledges the mode change before resetting its sequence window.
    // MediaTek's BTIF setup waits 10 ms before the first full-mode frame.
    let switching = now();
    while !expired(switching, 10) {
        core::hint::spin_loop();
    }
    link.full = true;
    link.exchange(&[1, 4, 1, 0, 4], &[2, 4, 6, 0, 0, 4, 0xdb, 0x0e, 0x68, 1])?;
    stage(28);
    // MediaTek set_chipid_script sends the MT6765 SoC ID after enabling full STP.
    link.exchange(&[1, 2, 5, 0, 0x11, 0x65, 0x67, 0, 0], &[2, 2, 1, 0, 0])?;
    let coex = &[2, 0x10, 1, 0, 0];
    link.exchange(&[1, 0x10, 2, 0, 1, if profile == 2 { 13 } else { 1 }], coex)?;
    if profile == 2 {
        let mut gain = [0; 22];
        gain[..6].copy_from_slice(&[1, 0x10, 18, 0, 0x1b, 1]);
        for (out, word) in
            gain[6..]
                .chunks_exact_mut(4)
                .zip([0x070e0f06u32, 0x080c0e05, 0x080c0e05, 0x080c0e05])
        {
            out.copy_from_slice(&word.to_le_bytes());
        }
        link.exchange(&gain, coex)?;
    }
    stage(29);
    link.exchange(&[1, 0x14, 1, 0, 1], &[2, 0x14, 2, 0, 0, 1])?;
    stage(30);
    link.exchange(&[1, 6, 2, 0, 3, 1], &[2, 6, 1, 0, 0])
}
