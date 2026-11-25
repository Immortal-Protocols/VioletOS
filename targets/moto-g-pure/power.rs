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

// MT6357 RTC, from Motorola's drivers/misc/mediatek/rtc/mt6357 and
// include/linux/mfd/mt6357/registers.h. The bootloader enters fastboot when
// RTC_PDN1 bit 13 is set, which is how rtc_mark_fast() requests it.
const RTC_BBPU: u16 = 0x0588;
const RTC_PDN1: u16 = 0x05b4;
const RTC_PROT: u16 = 0x05bc;
const RTC_WRTGR: u16 = 0x05c2;
const RTC_FAST_BOOT: u16 = 1 << 13;
fn rtc_commit() -> Option<()> {
    pmic_write(RTC_WRTGR, 1)?;
    for _ in 0..500 {
        if pmic_read(RTC_BBPU)? & (1 << 6) == 0 {
            return Some(());
        }
        unsafe { delay_ms(1) };
    }
    None
}
fn set_fastboot(enable: bool) -> Option<()> {
    if !ready() {
        return None;
    }
    for key in [0x586a, 0x9136] {
        pmic_write(RTC_PROT, key)?;
        rtc_commit()?;
    }
    let pdn1 = pmic_read(RTC_PDN1)?;
    let wanted = if enable { pdn1 | RTC_FAST_BOOT } else { pdn1 & !RTC_FAST_BOOT };
    pmic_write(RTC_PDN1, wanted)?;
    rtc_commit()?;
    (pmic_read(RTC_PDN1)? == wanted).then_some(())
}
pub fn fastboot_mark(enable: bool) -> Message {
    match set_fastboot(enable) {
        Some(()) => Message::result(u32::from(enable)),
        None => Message::error(OFFLINE),
    }
}

// Audio reaches the PMIC through Power, which serializes every wrapper access.
// Audio may read and write the MT6357 audio block (AUD_TOP, AUDDEC, AUDENC,
// ACCDET and ZCD at 0x2080-0x233e) and only the named bits elsewhere: the
// 26 MHz audio clock buffer, the audio link pad modes and directions, and their Schmitt
// triggers. Everything else, including every regulator, is refused.
const AUDIO_BLOCK: core::ops::RangeInclusive<u16> = 0x2080..=0x233e;
const AUDIO_FIELDS: [(u16, u16); 10] = [
    (0x0024, 0x0000), // TOPSTATUS: read only (power and home key)
    (0x07ac, 0x2000), // DCXO_CW14 XO_AUDIO_EN_M
    (0x00b6, 0xffff), // GPIO_MODE2: audio MOSI pads
    (0x00b8, 0xffff), // GPIO_MODE2_SET
    (0x00ba, 0xffff), // GPIO_MODE2_CLR
    (0x00bc, 0xffff), // GPIO_MODE3: audio MISO pads (microphone)
    (0x00be, 0xffff), // GPIO_MODE3_SET
    (0x00c0, 0xffff), // GPIO_MODE3_CLR
    (0x0088, 0xff00), // GPIO_DIR0: audio MOSI and MISO pad directions
    (0x002c, 0x0ff0), // SMT_CON1: audio pad Schmitt triggers
];
// Camera may switch the MT6357 camera LDOs and nothing else: VCAMIO (1.8 V,
// the sensors' I/O), VCAMD (the rear and front sensors' core, 1.0-1.8 V) and
// VCAMA (2.8 V analog for the third sensor), with the voltage selectors of the
// last two. Their status registers are read only.
const CAMERA_FIELDS: [(u16, u16); 8] = [
    (0x1a9c, 0x0001), // LDO_VCAMIO_CON0: enable
    (0x1aaa, 0x0000), // LDO_VCAMIO_CON1: status
    (0x1a88, 0x0001), // LDO_VCAMD_CON0: enable
    (0x1a96, 0x0000), // LDO_VCAMD_CON1: status
    (0x1c94, 0x0f00), // VCAMD_ANA_CON0: voltage
    (0x1a6c, 0x0001), // LDO_VCAMA_CON0: enable
    (0x1a7a, 0x0000), // LDO_VCAMA_CON1: status
    (0x1c8c, 0x0f00), // VCAMA_ANA_CON0: voltage
];
pub fn camera_pmic(op: u32, addr: u32, arg: u32) -> Message {
    let Ok(addr) = u16::try_from(addr) else { return Message::error(DENIED) };
    let Some(&(_, mask)) = CAMERA_FIELDS.iter().find(|f| f.0 == addr) else {
        return Message::error(DENIED);
    };
    pmic_access(op, addr, arg, mask)
}
/// GET reads `addr`. EXECUTE writes `arg & 0xffff` under the mask in its high half.
pub fn audio_pmic(op: u32, addr: u32, arg: u32) -> Message {
    let Ok(addr) = u16::try_from(addr) else { return Message::error(DENIED) };
    let allowed = if AUDIO_BLOCK.contains(&addr) && addr & 1 == 0 {
        0xffff
    } else if let Some(&(_, mask)) = AUDIO_FIELDS.iter().find(|f| f.0 == addr) {
        mask
    } else {
        return Message::error(DENIED);
    };
    pmic_access(op, addr, arg, allowed)
}
fn pmic_access(op: u32, addr: u16, arg: u32, allowed: u16) -> Message {
    if !ready() {
        return Message::error(OFFLINE);
    }
    if op == EXECUTE {
        let mask = (arg >> 16) as u16;
        if mask & !allowed != 0 || mask == 0 {
            return Message::error(DENIED);
        }
        let value = arg as u16 & mask;
        // Set and clear registers act on written ones, so write them directly.
        let write = if matches!(addr, 0x00b8 | 0x00ba | 0x00be | 0x00c0) {
            Some(value)
        } else {
            pmic_read(addr).map(|old| (old & !mask) | value)
        };
        if write.and_then(|v| pmic_write(addr, v)).is_none() {
            return Message::error(OFFLINE);
        }
    }
    match pmic_read(addr) {
        Some(v) => Message::result(u32::from(v)),
        None => Message::error(OFFLINE),
    }
}
