//! STP over the Ellis BTIF programmed-I/O port: WMT transactions (channel 4)
//! and, once the chip runs, Bluetooth HCI (channel 0) beside them.
//!
//! The link state (sequence numbers and a half-received frame) lives in one
//! static, so Bluetooth traffic can keep using the link after `enable`.
use super::radio_io::{byte, expired, now, put, rd, wr};
const PORT: usize = 0x09010000;
/// STP channels.
pub const BT: u8 = 0;
const WMT: u8 = 4;
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
fn ready() -> bool {
    read(0x14) & 1 != 0
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

/// Counters for diagnostics: frames each way and frames rejected.
#[derive(Clone, Copy)]
pub struct Counts {
    pub rx_frames: u32,
    pub tx_frames: u32,
    pub bad: u32,
}

/// Where channel-0 (Bluetooth) payloads go as they arrive.
pub type Sink = fn(&[u8]);
fn discard(_: &[u8]) {}

struct Link {
    full: bool,
    tx: u8,
    ack: u8,
    pos: usize,
    sink: Sink,
    counts: Counts,
}
static mut LINK: Link = Link {
    full: false,
    tx: 0,
    ack: 7,
    pos: 0,
    sink: discard,
    counts: Counts { rx_frames: 0, tx_frames: 0, bad: 0 },
};
fn link() -> &'static mut Link {
    unsafe { &mut *core::ptr::addr_of_mut!(LINK) }
}
pub fn counts() -> Counts {
    link().counts
}
pub fn set_sink(sink: Sink) {
    link().sink = sink;
}

/// The frame being received (4 header bytes, the body, 2 CRC bytes), in a
/// page of Radio's own beside the HCI pages: Radio's RAM has no 2 KiB to spare.
const FRAME_SIZE: usize = 2048 + 6;
impl Link {
    #[cfg(not(test))]
    fn frame(&self) -> &'static mut [u8; FRAME_SIZE] {
        unsafe { &mut *(0x0b04_2000 as *mut [u8; FRAME_SIZE]) }
    }
    /// Host tests have no page at that address: one buffer for each test thread.
    #[cfg(test)]
    fn frame(&self) -> &'static mut [u8; FRAME_SIZE] {
        std::thread_local!(static FRAME: *mut [u8; FRAME_SIZE] = Box::leak(Box::new([0; FRAME_SIZE])));
        FRAME.with(|frame| unsafe { &mut **frame })
    }
    fn send(&mut self, channel: u8, payload: &[u8]) -> Result<(), u32> {
        let n = payload.len();
        if n == 0 || n > 2048 {
            return Err(26);
        }
        let mut h = [0x80, (channel << 4) | ((n >> 8) as u8 & 15), n as u8, 0];
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
        self.counts.tx_frames += 1;
        Ok(())
    }
    /// Takes one received byte. Returns the channel and body length of a
    /// complete, valid data frame (the body is at `frame[4..]`).
    fn feed(&mut self, b: u8) -> Option<(u8, usize)> {
        if self.pos == 0 && b & 0xc0 != 0x80 {
            return None;
        }
        self.frame()[self.pos] = b;
        self.pos += 1;
        if self.pos < 4 {
            return None;
        }
        let h = &self.frame()[..4];
        let len = ((h[1] as usize & 15) << 8) | h[2] as usize;
        if self.pos == 4 {
            if self.full && h[0].wrapping_add(h[1]).wrapping_add(h[2]) != h[3] {
                // Not a header after all: look for the next start byte.
                self.counts.bad += 1;
                self.pos = 0;
                return None;
            }
            if len == 0 {
                // An acknowledgement only. Nothing to answer.
                self.pos = 0;
                return None;
            }
            if len + 6 > FRAME_SIZE {
                self.counts.bad += 1;
                self.pos = 0;
                return None;
            }
        }
        if self.pos < len + 6 {
            return None;
        }
        self.pos = 0;
        let first = self.frame()[0];
        let channel = self.frame()[1] >> 4;
        if self.full {
            let check = u16::from_le_bytes([self.frame()[4 + len], self.frame()[5 + len]]);
            let seq = (first >> 3) & 7;
            if check != crc16(&self.frame()[4..4 + len]) {
                self.counts.bad += 1;
                return None;
            }
            if seq == self.ack {
                // A retransmission of a frame already taken: acknowledge it again.
                let ack = 0x80 | self.ack;
                let _ = transmit(&[ack, 0, 0, ack]);
                return None;
            }
            if seq != (self.ack + 1) & 7 {
                self.counts.bad += 1;
                return None;
            }
            self.ack = seq;
            let ack = 0x80 | self.ack;
            let _ = transmit(&[ack, 0, 0, ack]);
        }
        self.counts.rx_frames += 1;
        Some((channel, len))
    }
    /// Hands a complete frame on: Bluetooth to the sink, the rest is WMT.
    fn deliver(&mut self, channel: u8, len: usize) -> Option<usize> {
        if channel == BT {
            (self.sink)(&self.frame()[4..4 + len]);
            return None;
        }
        (channel == WMT).then_some(len)
    }
    /// Waits up to 1.8 s for the next WMT frame, passing others on.
    fn wmt_reply(&mut self) -> Result<usize, u32> {
        let start = now();
        loop {
            if expired(start, 1800) {
                return Err(22);
            }
            if !ready() {
                continue;
            }
            if let Some((channel, len)) = self.feed(byte(PORT)) {
                if let Some(len) = self.deliver(channel, len) {
                    return Ok(len);
                }
            }
        }
    }
    fn exchange(&mut self, payload: &[u8], expected: &[u8]) -> Result<(), u32> {
        self.exchange_with(payload, expected, false)
    }
    /// An exchange whose answer only has to begin with `expected`.
    fn exchange_prefix(&mut self, payload: &[u8], expected: &[u8]) -> Result<(), u32> {
        self.exchange_with(payload, expected, true)
    }
    fn exchange_with(&mut self, payload: &[u8], expected: &[u8], prefix: bool) -> Result<(), u32> {
        super::radio_io::trace(
            0,
            (u32::from(payload[1]) << 16) | (u32::from(payload[4]) << 8) | self.tx as u32,
        );
        super::radio_io::trace(1, 0);
        super::radio_io::trace(2, 0);
        self.send(WMT, payload)?;
        let len = self.wmt_reply()?;
        let h = &self.frame()[..4];
        super::radio_io::trace(3, u32::from_le_bytes([h[0], h[1], h[2], h[3]]));
        let body = &self.frame()[4..4 + len];
        let mut words = [0u8; 8];
        let n = len.min(8);
        words[..n].copy_from_slice(&body[..n]);
        super::radio_io::trace(1, u32::from_le_bytes(words[..4].try_into().unwrap()));
        super::radio_io::trace(2, u32::from_le_bytes(words[4..].try_into().unwrap()));
        // Ellis includes calibration data after the success/subcommand bytes.
        let calibration = payload == [1, 0x14, 1, 0, 1]
            && len >= 6
            && body[..2] == [2, 0x14]
            && usize::from(u16::from_le_bytes([body[2], body[3]])) == len - 4
            && body[4..6] == [0, 1];
        if !calibration && body != expected && !(prefix && body.starts_with(expected)) {
            return Err(24);
        }
        Ok(())
    }
}

/// Moves every byte the chip has sent so far, without waiting.
/// Bluetooth frames reach the sink; stray WMT frames are dropped.
pub fn poll() {
    let link = link();
    let mut budget = 4096;
    while budget > 0 && ready() {
        budget -= 1;
        if let Some((channel, len)) = link.feed(byte(PORT)) {
            link.deliver(channel, len);
        }
    }
}
/// Sends one H4 packet (type byte first) to the Bluetooth controller.
pub fn send_bt(packet: &[u8]) -> Result<(), u32> {
    link().send(BT, packet)
}
/// WMT function control: Bluetooth on or off. The chip must be running.
pub fn bluetooth(on: bool) -> Result<(), u32> {
    let link = link();
    if !link.full {
        return Err(27);
    }
    link.exchange(&[1, 6, 2, 0, 0, on as u8], &[2, 6, 1, 0, 0])
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
    let link = link();
    link.full = false;
    link.tx = 0;
    link.ack = 7;
    link.pos = 0;
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
    // The rest follows MediaTek's mtk_wcn_soc_sw_init for MT6765 (wmt_ic_soc.c)
    // with the board's WMT_SOC_ELLIS(_EPA).cfg: the analog die query, then
    // Bluetooth's transmit-power reference from Wi-Fi (bt_tssi_from_wifi=3,
    // bt_tssi_target=3776), then the coexistence table before calibration.
    link.exchange_prefix(&[1, 2, 1, 0, 0x12], &[2, 2, 5, 0, 0])?;
    link.exchange(&[1, 2, 4, 0, 0x10, 3, 0xc0, 0x0e], &[2, 2, 1, 0, 0])?;
    let coex = &[2, 0x10, 1, 0, 0];
    if profile == 1 {
        link.exchange(&[1, 0x10, 2, 0, 1, 1], coex)?;
    }
    if profile == 2 {
        // Antenna mode 13 (sub-command 6: 1 is another setting, and Bluetooth
        // was nearly deaf with it), the Wi-Fi path, the eLNA gains, the ePA mode.
        link.exchange(&[1, 0x10, 2, 0, 6, 13], coex)?;
        link.exchange(&[1, 0x10, 3, 0, 0x1a, 0, 0], coex)?;
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
        link.exchange(&[1, 0x10, 2, 0, 0x1d, 0], coex)?;
    }
    stage(29);
    link.exchange(&[1, 0x14, 1, 0, 1], &[2, 0x14, 2, 0, 0, 1])?;
    if profile == 2 {
        // Coexistence adjustments after calibration, all zero for this board.
        for sub in [0x1e, 0x1f, 0x20] {
            link.exchange(&[1, 0x10, 4, 0, sub, 0, 0, 0], coex)?;
        }
    }
    stage(30);
    link.exchange(&[1, 6, 2, 0, 3, 1], &[2, 6, 1, 0, 0])
}
