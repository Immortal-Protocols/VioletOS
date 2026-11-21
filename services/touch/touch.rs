use crate::spi::{progress, wait, Bus};
use core::fmt::{self, Write};

include!(env!("VIOLET_FIRMWARE_RS"));

struct Status<'a> {
    bytes: &'a mut [u8; 40],
    used: usize,
}
impl Write for Status<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.used + text.len();
        if end >= self.bytes.len() {
            return Err(fmt::Error);
        }
        self.bytes[self.used..end].copy_from_slice(text.as_bytes());
        self.bytes[end] = 0;
        self.used = end;
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Page,
    Data,
}
impl Bus {
    fn page(&mut self, a: usize) -> bool {
        self.exchange(&mut [0xff, (a >> 15) as u8, (a >> 7) as u8])
    }
    fn put(&mut self, a: usize, bytes: &[u8]) -> Result<(), Phase> {
        if !self.page(a) {
            return Err(Phase::Page);
        }
        let mut packet = [0; 32];
        packet[0] = 0x80 | (a & 127) as u8;
        packet[1..1 + bytes.len()].copy_from_slice(bytes);
        self.exchange(&mut packet[..1 + bytes.len()])
            .then_some(())
            .ok_or(Phase::Data)
    }
    fn get(&mut self, a: usize, out: &mut [u8]) -> bool {
        let mut packet = [0; 32];
        packet[0] = (a & 127) as u8;
        if !self.page(a) || !self.exchange(&mut packet[..out.len() + 2]) {
            return false;
        }
        out.copy_from_slice(&packet[2..out.len() + 2]);
        true
    }
    fn matches(&mut self, mut addr: usize, data: &[u8]) -> bool {
        let mut buf = [0; 30];
        for chunk in data.chunks(30) {
            let read = &mut buf[..chunk.len()];
            if !self.get(addr, read) || read != chunk {
                return false;
            }
            addr += chunk.len();
        }
        true
    }
    fn upload(&mut self, status: &mut Status) -> Result<(), &'static str> {
        let mut limit = 31;
        for &(addr, offset, size, _) in PARTS {
            let mut i = 0;
            while i < size {
                let a = addr + i;
                let mut n = (size - i).min(128 - (a & 127)).min(limit);
                for attempt in 0..5 {
                    let data = &touch_firmware[offset + i..offset + i + n];
                    let phase = match self.put(a, data) {
                        Ok(()) => break,
                        Err(e) => e,
                    };
                    let state = self.state;
                    if phase == Phase::Data && self.matches(a, data) {
                        break;
                    }
                    progress();
                    wait(1);
                    if attempt == 4 {
                        let stage = if phase == Phase::Page { "PAGE" } else { "DATA" };
                        let _ = write!(status, "FW {stage} {a:06X} S1 {state:08X}");
                        return Err("");
                    }
                    if attempt == 1 && phase == Phase::Data {
                        limit = 3;
                        n = n.min(limit);
                    }
                }
                i += n;
                progress();
            }
        }
        Ok(())
    }
    fn reset(&mut self, pass: u32, status: &mut Status) -> Result<(), &'static str> {
        for (a, value, name) in [(0x3f0fe, 0x69, "RESET"), (0x3f310, 0, "FAST")] {
            for attempt in 0..5 {
                let phase = match self.put(a, &[value]) {
                    Ok(()) => break,
                    Err(e) => e,
                };
                wait(1);
                if attempt == 4 {
                    let stage = if phase == Phase::Page { "PAGE" } else { "DATA" };
                    let _ = write!(
                        status,
                        "R{pass} {name} {stage} S{:08X} C{:08X}",
                        self.state, self.command
                    );
                    return Err("");
                }
            }
            if name == "RESET" {
                wait(5);
            }
        }
        Ok(())
    }
    fn init(&mut self, status: &mut Status) -> Result<(), &'static str> {
        if !self.start() {
            return Err("SPI CLOCK FAILED");
        }
        self.reset(1, status)?;
        let mut id = [0; 6];
        if ![0x3f004, 0x1f64e]
            .iter()
            .any(|&a| self.get(a, &mut id) && id[0] == 11 && id[3..] == [0x25, 0x65, 3])
        {
            return Err("TOUCH ID FAILED");
        }
        self.reset(2, status)?;
        for (bank, &(addr, _, size, crc)) in PARTS[..2].iter().enumerate() {
            let registers = [[0x3f128, 0x3f118, 0x3f100], [0x3f12c, 0x3f130, 0x3f104]][bank];
            let errors = [
                [
                    "ILM ADDRESS TIMEOUT",
                    "ILM LENGTH TIMEOUT",
                    "ILM CRC TIMEOUT",
                ],
                [
                    "DLM ADDRESS TIMEOUT",
                    "DLM LENGTH TIMEOUT",
                    "DLM CRC TIMEOUT",
                ],
            ][bank];
            for (field, (value, width)) in [(addr as u32, 3), ((size - 1) as u32, 2), (crc, 4)]
                .into_iter()
                .enumerate()
            {
                for attempt in 0..5 {
                    if self
                        .put(registers[field], &value.to_le_bytes()[..width])
                        .is_ok()
                    {
                        break;
                    }
                    if attempt == 4 {
                        return Err(errors[field]);
                    }
                    wait(1);
                }
            }
        }
        self.upload(status)?;
        let mut byte = [0];
        if !self.get(0x3f30e, &mut byte) {
            return Err("FW START FAILED");
        }
        for (a, v) in [
            (0x3f30e, byte[0] | 128),
            (0x21c60, 0),
            (0x21c50, 0xae),
            (0x3f10d, 1),
        ] {
            self.put(a, &[v]).map_err(|_| "FW START FAILED")?;
        }
        for _ in 0..100 {
            wait(10);
            if self.get(0x21c60, &mut byte) && (0xa1..=0xaf).contains(&byte[0]) {
                return Ok(());
            }
        }
        Err("FW READY TIMEOUT")
    }
}

#[no_mangle]
pub extern "C" fn touch_initialize(bytes: &mut [u8; 40]) {
    bytes.fill(0);
    let mut status = Status { bytes, used: 0 };
    if let Err(message) = Bus::default().init(&mut status) {
        let _ = status.write_str(message);
    }
}

#[no_mangle]
pub extern "C" fn touch_sample(x: &mut u32, y: &mut u32, down: &mut bool) -> bool {
    let mut b = [0; 6];
    if !Bus::default().get(0x21c00, &mut b) {
        return false;
    }
    *down = (1..=10).contains(&(b[0] >> 3)) && matches!(b[0] & 7, 1 | 2);
    if *down {
        *x = (u32::from(b[1]) << 4) | u32::from(b[3] >> 4);
        *y = (u32::from(b[2]) << 4) | u32::from(b[3] & 15);
        return *x < 720 && *y < 1600;
    }
    true
}
