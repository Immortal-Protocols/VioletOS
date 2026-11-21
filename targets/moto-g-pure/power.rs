//! MT6765 WACS2 and MT6357 channel 0 (battery voltage).
use crate::protocol::*;

const WRAPPER: usize = 0x09000000;
const COMMAND: usize = 0xc20;
const STATUS: usize = 0xc24;
const CLEAR: usize = 0xc28;
const ADC0: u16 = 0x1088;
const REQUEST0: u16 = 0x1110;
const POWER_CONTROL: u16 = 0x0a08;

fn read_reg(offset: usize) -> u32 {
    unsafe { ((WRAPPER + offset) as *const u32).read_volatile() }
}
fn write_reg(offset: usize, value: u32) {
    unsafe {
        ((WRAPPER + offset) as *mut u32).write_volatile(value);
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
    }
}
fn wait(state: u32) -> Option<u32> {
    let start = unsafe { ticks() };
    loop {
        let status = read_reg(STATUS);
        if (status >> 16) & 7 == state {
            return Some(status);
        }
        if unsafe { ticks().wrapping_sub(start) >= ticks_per_ms * 2 } {
            return None;
        }
    }
}
fn pmic_read(addr: u16) -> Option<u16> {
    wait(0)?;
    write_reg(COMMAND, (addr as u32) << 15);
    let value = wait(6)? as u16;
    write_reg(CLEAR, 1);
    Some(value)
}
fn pmic_write(addr: u16, value: u16) -> Option<()> {
    wait(0)?;
    write_reg(COMMAND, 1 << 31 | (addr as u32) << 15 | value as u32);
    wait(0).map(|_| ())
}
fn ready() -> bool {
    read_reg(0xa0) & 1 != 0
        && read_reg(STATUS) & (1 << 22) != 0
        && pmic_read(0x0a).map(|id| id >> 8) == Some(0x57)
}
fn millivolts() -> Option<u32> {
    if !ready() {
        return None;
    }
    pmic_write(REQUEST0, 1)?;
    // The factory device tree requests 128 samples at 10 us per sample.
    unsafe { delay_ms(2) };
    for _ in 0..32 {
        let sample = pmic_read(ADC0)?;
        if sample & 0x8000 != 0 {
            return Some(u32::from(sample & 0x7fff) * 5400 / 32768);
        }
        unsafe { delay_ms(1) };
    }
    None
}
// Only the service thread accesses the sample cache.
static mut SAMPLE: Option<(u32, Message)> = None;
pub fn read() -> Message {
    let now = unsafe { ticks() };
    if let Some((when, previous)) = unsafe { SAMPLE } {
        if now.wrapping_sub(when) < unsafe { ticks_per_ms } * 1000 {
            return previous;
        }
    }
    let mut reply = Message::result(0);
    reply.arg = POWER_MILLIVOLTS;
    if let Some(mv) = millivolts() {
        reply.value = mv;
        let mut text = *b"BATTERY 0000 MV";
        for (i, scale) in [1000, 100, 10, 1].iter().enumerate() {
            text[i + 8] += ((mv / scale) % 10) as u8;
        }
        reply.put_text(&text);
    } else {
        reply.put_text(b"BATTERY READ FAILED");
    }
    unsafe {
        (0x09201000 as *mut u32).write_volatile(reply.value);
        SAMPLE = Some((now, reply));
    }
    reply
}
pub fn command(value: u32) -> Message {
    match value {
        // Runtime blanks scanout and consumes the first touch on wake.
        0 | 1 => Message::result(value),
        2 if ready() => {
            if let Some(control) = pmic_read(POWER_CONTROL) {
                if control & 1 != 0 && pmic_write(POWER_CONTROL, control & !1).is_some() {
                    return Message::result(value);
                }
            }
            Message::error(OFFLINE)
        }
        _ => Message::error(OFFLINE),
    }
}

// Radio requests are serialized by the Power process. Preserve the inherited
// state of each masked field, including rails already enabled by firmware.
const RADIO_FIELDS: [(u16,u16,u16);5] = [
    (0x1a58,1,1),       // VCN18
    (0x1c1c,3 << 8,1 << 8), // 3.3 V selector, as in mt6357-regulator.c
    (0x1b1c,1,1),       // VCN33 shared analog supply
    (0x0788,7 << 3,1 << 5), // XO_WCN manual mode, enabled
    (0x1b88,1,1),       // VCN28
];
static mut RAIL_SAVED: [u16;5] = [0;5];
static mut RAIL_COUNT: usize = 0;
fn restore_rails() -> Option<()> {
    loop {
        let count = unsafe { RAIL_COUNT };
        if count == 0 { return Some(()); }
        let i=count-1;
        let (addr,mask,_) = RADIO_FIELDS[i];
        let original = unsafe { RAIL_SAVED[i] };
        let now = pmic_read(addr)?;
        if now & mask != original {
            pmic_write(addr,(now & !mask) | original)?;
            if pmic_read(addr)? & mask != original {return None;}
        }
        unsafe {RAIL_COUNT = i};
    }
}
fn enable_rails() -> Option<()> {
    if !ready() || unsafe {RAIL_COUNT} != 0 {return None;}
    // Never change an active consumer's voltage.
    if pmic_read(0x1b2c)? & 0x8000 != 0 && pmic_read(0x1c1c)? & 0x300 != 0x100 {
        return None;
    }
    for (i,(addr,mask,wanted)) in RADIO_FIELDS.iter().copied().enumerate() {
        let original = pmic_read(addr)?;
        unsafe { RAIL_SAVED[i]=original & mask; RAIL_COUNT=i+1; }
        if original & mask != wanted {
            pmic_write(addr,(original & !mask)|wanted)?;
            if pmic_read(addr)? & mask != wanted {return None;}
        }
        unsafe {delay_ms(1)};
    }
    let mut status=0;
    for (i,addr) in [0x1a66,0x1b2c,0x1b96].into_iter().enumerate() {
        status |= u32::from(pmic_read(addr)? >> 15) << i;
    }
    unsafe { (0x09201004 as *mut u32).write_volatile(status); }
    (status==7).then_some(())
}
pub fn radio_rails(enable: bool) -> Message {
    if enable && enable_rails().is_some() {return Message::result(1);}
    let restored=restore_rails().is_some();
    if !enable && restored {Message::result(0)} else {Message::error(OFFLINE)}
}

pub fn usb_supply() -> Message {
    // MT6357 VUSB33 enable and physical regulator acknowledgement.
    if let Some(original)=pmic_read(0x199c) {
        if original&1 != 0 || pmic_write(0x199c,original|1).is_some() {
            for _ in 0..20 {
                if pmic_read(0x19ac).is_some_and(|v|v&0x8000!=0) {return Message::result(1);}
                unsafe { delay_ms(1) };
            }
        }
    }
    Message::error(OFFLINE)
}
