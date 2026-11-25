//! Moto G Pure camera: the rear SK Hynix Hi-1336 (13 MP) on CSI0, four lanes.
//!
//! Sources (see targets/moto-g-pure/camera-bringup.md):
//! - Motorola's ellis kernel (`MotorolaMobilityLLC/kernel-mtk`,
//!   android-12-release-s3rhs32.20-42-10-4-11): the power sequence
//!   (imgsensor_cfg_table.c), pins (mt6762-ellis-camera-sensor-overlay.dtsi),
//!   the I2C bus (ellis_evt.dws: CAMERA_MAIN on I2C2 at 0x40 >> 1) and the
//!   Hi-1336 driver (ID at 0x0716, 1280 x 720 "slim video" table, shutter and
//!   gain registers). The register tables are fetched at build time
//!   (tools/sensor_tables.py), not kept here.
//! - The sensor interface (SENINF) and MIPI receiver follow MediaTek's MT8183
//!   seninf driver (Louis Kuo, v7 2020) and mainline's MIPI CSI 0.5 PHY driver:
//!   the MT6765 has the same blocks at the same offsets.
//! - CAMSV, which writes what SENINF receives straight to memory, follows the
//!   MT8365 ISP 3.0 CAMSV driver (BayLibre, v8 2025) at the MT6765's register
//!   offsets (cameraisp/src/mt6765/camera_isp.c).
//!
//! The supervisor powers the camera domain and sets the clocks and pins on
//! request; Power switches the MT6357 camera LDOs. Camera owns camsys, SMI
//! larb3, SENINF, CAMSV1, the MIPI receivers and I2C2.
use crate::process::{Format, BAYER_GBRG, FORMAT_RAW8, WINDOW};
use crate::protocol::*;
use crate::sensor_tables as tables;

const POWER_SLOT: u32 = 6;
const LIFE_SLOT: u32 = 7;
const CAMSYS: usize = 0x0900_0000;
const LARB3: usize = 0x0900_1000;
const I2C: usize = 0x0900_2000;
const SENINF: usize = 0x0901_0000;
const CAMSV: usize = 0x0901_8000;
const MIPI: usize = 0x0902_0000;
const STATUS: *mut u32 = 0x0920_3000 as *mut u32;
const REQUEST: *const u32 = 0x0920_4000 as *const u32;
const PREVIEW: *mut u8 = 0x0934_0000 as *mut u8;
/// Physical address of the frame window (supervisor camera_frames).
const WINDOW_PHYS: u32 = 0xe000_0000;
const SENSOR_ADDR: u32 = 0x20;
const HI1336_ID: u32 = 0x1336;

// Status words, read with `moto-package camera-status`.
const S_MAGIC: usize = 0;
const S_STAGE: usize = 1;
const S_STATE: usize = 2;
const S_FRAMES: usize = 3;
const S_PLATFORM: usize = 4; // 4: result | op << 16, 5..=14: supervisor survey
const S_LDO: usize = 15; // 15..=19: VCAMIO, VCAMIO status, VCAMD, VCAMD status, VCAMD voltage
const S_SENSOR_ID: usize = 20;
const S_I2C_OK: usize = 21;
const S_I2C_FAIL: usize = 22; // count | last INTR_STAT << 16
const S_REQUEST: usize = 23; // 23: last request, 24: serial
const S_PEEK: usize = 25; // 25: value, 26: address
const S_CAMSYS: usize = 27;
const S_SENINF: usize = 28; // 28..=39
const S_CAMSV: usize = 40; // 40..=47
const S_IRQ: usize = 48; // 48: accumulated INT_STATUS, 49: SOF, 50: done, 51: errors
const S_IMGO: usize = 52; // 52: base, 53: error status
const S_FIRST: usize = 54; // first word of the last frame
const S_EXPOSURE: usize = 56; // 56: lines, 57: gain register, 58: level
const S_STREAM_REG: usize = 59;
const S_MIPI: usize = 60; // 60, 61: ANA00 of CSI0A and CSI0B
const S_ELAPSED: usize = 62; // milliseconds the last start took
const S_TABLES: usize = 63;
const S_LARB: usize = 64; // 64..=67
const S_MCLK: usize = 70;
const S_I2C_DEBUG: usize = 68; // 68: channel START | DEBUGSTAT << 16, 69: shadow START | INTR_STAT << 16

fn note(i: usize, v: u32) {
    unsafe { STATUS.add(i).write_volatile(v) }
}
fn noted(i: usize) -> u32 {
    unsafe { STATUS.add(i).read_volatile() }
}
fn stage(n: u32) {
    note(S_STAGE, n);
}
fn now() -> u32 {
    unsafe { ticks() }
}
fn udelay(us: u32) {
    let start = now();
    let wait = (unsafe { ticks_per_ms } * us).div_ceil(1000);
    while now().wrapping_sub(start) < wait {}
}
fn mdelay(ms: u32) {
    unsafe { delay_ms(ms) }
}
fn rd(base: usize, off: usize) -> u32 {
    unsafe { ((base + off) as *const u32).read_volatile() }
}
fn wr(base: usize, off: usize, v: u32) {
    unsafe {
        ((base + off) as *mut u32).write_volatile(v);
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
    }
}
/// Replace `width` bits at `shift`.
fn field(base: usize, off: usize, shift: u32, width: u32, v: u32) {
    let mask = ((1u32 << width) - 1) << shift;
    wr(base, off, (rd(base, off) & !mask) | ((v << shift) & mask));
}

// ---- Supervisor: power domain, clocks and pins ----
const DOMAIN: u32 = 1;
const MCLK: u32 = 2;
const AVDD: u32 = 4;
const RESET: u32 = 8;
const FRONT_RESET: u32 = 16;
const I2C_PINS: u32 = 32;
const MIPI_GATE: u32 = 64;
const TORCH: u32 = 128;
/// Change the items in `which` to the state in `on` (both bit sets above).
fn platform(which: u32, on: u32) -> u32 {
    let m = Message::new(EXECUTE, CAMERA_PLATFORM, which | (on & which) << 8).call(LIFE_SLOT);
    for i in 0..10 {
        let raw = if i < 8 {
            u32::from_le_bytes(m.data[i * 4..i * 4 + 4].try_into().unwrap())
        } else {
            u32::from_le_bytes(m.tail[(i - 8) * 4..(i - 8) * 4 + 4].try_into().unwrap())
        };
        note(S_PLATFORM + 1 + i, raw);
    }
    note(S_PLATFORM, m.value | (m.op << 16));
    if m.op == OK { m.value } else { 0 }
}

// ---- PMIC, through Power ----
const LDO_VCAMIO: u16 = 0x1a9c;
const LDO_VCAMIO_STATUS: u16 = 0x1aaa;
const LDO_VCAMD: u16 = 0x1a88;
const LDO_VCAMD_STATUS: u16 = 0x1a96;
const VCAMD_VOLTAGE: u16 = 0x1c94;
fn pmic(addr: u16) -> u32 {
    let r = Message::new(GET, PMIC_ACCESS, u32::from(addr)).call(POWER_SLOT);
    if r.op == OK { r.value } else { 0xdead_0000 | r.op }
}
fn pmic_set(addr: u16, value: u16, mask: u16) -> bool {
    let r = Message::new(EXECUTE, PMIC_ACCESS, u32::from(addr))
        .with_arg(u32::from(mask) << 16 | u32::from(value & mask))
        .call(POWER_SLOT);
    r.op == OK
}
fn ldo_report() {
    for (i, a) in [LDO_VCAMIO, LDO_VCAMIO_STATUS, LDO_VCAMD, LDO_VCAMD_STATUS, VCAMD_VOLTAGE].iter().enumerate() {
        note(S_LDO + i, pmic(*a));
    }
}

// ---- I2C2 in FIFO mode, polled (i2c-mtk.c, register map version 2) ----
const I2C_DATA: usize = 0x00;
const I2C_SLAVE_ADDR: usize = 0x04;
const I2C_INTR_MASK: usize = 0x08;
const I2C_INTR_STAT: usize = 0x0c;
const I2C_CONTROL: usize = 0x10;
const I2C_TRANSFER_LEN: usize = 0x14;
const I2C_TRANSAC_LEN: usize = 0x18;
const I2C_DELAY_LEN: usize = 0x1c;
const I2C_TIMING: usize = 0x20;
const I2C_START: usize = 0x24;
const I2C_EXT_CONF: usize = 0x28;
const I2C_LTIMING: usize = 0x2c;
const I2C_HS: usize = 0x30;
const I2C_IO_CONFIG: usize = 0x34;
const I2C_FIFO_ADDR_CLR: usize = 0x38;
const I2C_MCU_INTR: usize = 0x40;
const I2C_TRANSFER_LEN_AUX: usize = 0x44;
const I2C_CLOCK_DIV: usize = 0x48;
const I2C_DEBUGCTRL: usize = 0xe8;
/// Bus speed: the slow divider pair proven with Audio's amplifier (under
/// 30 kHz), or about 380 kHz for the sensor's long tables (i2c_ck / 5 =
/// 27.3 MHz, divided by 2 x 2 x 18).
static mut FAST: bool = false;

/// I2C2 is shared with the camera's CCU: the AP's channel is 0x100 into the
/// block (the device tree's ch_offset_default), and the registers at its
/// start are the controller's "shadow" set, which the vendor driver
/// (mt_i2c_init_hw) resets and configures before using the channel.
const CH: usize = 0x100;
const I2C_SOFTRESET: usize = 0x50;
const I2C_DEBUGSTAT: usize = 0xe4;
static mut SHADOW_READY: bool = false;
fn i2c_init_shadow() {
    wr(I2C, I2C_INTR_MASK, 0);
    wr(I2C, I2C_INTR_STAT, rd(I2C, I2C_INTR_STAT));
    wr(I2C, I2C_SOFTRESET, 1);
    wr(I2C, I2C_IO_CONFIG, 0x0003);
    wr(I2C, I2C_TIMING, (7 << 8) | 63);
    wr(I2C, I2C_LTIMING, (7 << 6) | 63);
    wr(I2C, I2C_HS, 0);
    unsafe { SHADOW_READY = true };
}
/// One transaction: write `out`, then (with a repeated start) read `input.len()` bytes.
fn i2c_transfer(out: &[u8], input: &mut [u8]) -> bool {
    if !unsafe { SHADOW_READY } {
        i2c_init_shadow();
    }
    let read = !input.is_empty();
    i2c_w(I2C_DEBUGCTRL, 0x28);
    i2c_w(I2C_CLOCK_DIV, 0x0404);
    i2c_w(I2C_CONTROL, 0x20 | 0x08 | if read { 0x10 | 0x02 } else { 0 });
    i2c_w(I2C_EXT_CONF, 0x8001);
    i2c_w(I2C_DELAY_LEN, 0x0a);
    i2c_w(I2C_IO_CONFIG, 0x0003);
    if unsafe { FAST } {
        i2c_w(I2C_TIMING, (1 << 8) | 17);
        i2c_w(I2C_LTIMING, (1 << 6) | 17);
    } else {
        i2c_w(I2C_TIMING, (7 << 8) | 63);
        i2c_w(I2C_LTIMING, (7 << 6) | 63);
    }
    i2c_w(I2C_HS, 0);
    i2c_w(I2C_SLAVE_ADDR, SENSOR_ADDR << 1);
    i2c_w(I2C_INTR_STAT, 0x1ff);
    i2c_w(I2C_FIFO_ADDR_CLR, 4 | 1);
    i2c_w(I2C_INTR_MASK, 0x2f | 0x100 | 0x20);
    i2c_w(I2C_TRANSFER_LEN, out.len() as u32);
    if read {
        i2c_w(I2C_TRANSFER_LEN_AUX, input.len() as u32);
        i2c_w(I2C_TRANSAC_LEN, 2);
    } else {
        i2c_w(I2C_TRANSAC_LEN, 1);
    }
    for &b in out {
        unsafe { ((I2C + CH + I2C_DATA) as *mut u8).write_volatile(b) };
    }
    i2c_w(I2C_MCU_INTR, 1);
    i2c_w(I2C_START, 1);
    let start = now();
    let mut stat;
    loop {
        stat = i2c_r(I2C_INTR_STAT) & 0x1ff;
        if stat != 0 || now().wrapping_sub(start) >= unsafe { ticks_per_ms } * 20 {
            break;
        }
    }
    i2c_w(I2C_INTR_STAT, 0x1ff);
    if stat & 0x16e != 0 || stat & 1 == 0 {
        let start_reg = i2c_r(I2C_START);
        note(S_I2C_DEBUG, start_reg | i2c_r(I2C_DEBUGSTAT) << 16);
        note(S_I2C_DEBUG + 1, rd(I2C, I2C_START) | rd(I2C, I2C_INTR_STAT) << 16);
        i2c_w(I2C_FIFO_ADDR_CLR, 4 | 1);
        // The vendor driver re-initializes and hands the bus back to this
        // channel when the arbiter kept it.
        unsafe { SHADOW_READY = false };
        if start_reg & 2 != 0 {
            i2c_init_shadow();
            wr(I2C, I2C_START, 2);
        }
        let fails = (noted(S_I2C_FAIL) & 0xffff) + 1;
        note(S_I2C_FAIL, fails.min(0xffff) | (stat | 0x8000) << 16);
        return false;
    }
    for b in input.iter_mut() {
        *b = unsafe { ((I2C + CH + I2C_DATA) as *const u8).read_volatile() };
    }
    note(S_I2C_OK, noted(S_I2C_OK).wrapping_add(1));
    true
}
fn i2c_w(reg: usize, v: u32) {
    wr(I2C + CH, reg, v);
}
fn i2c_r(reg: usize) -> u32 {
    rd(I2C + CH, reg)
}
fn sensor_r(reg: u16) -> Option<u8> {
    let mut b = [0];
    i2c_transfer(&reg.to_be_bytes(), &mut b).then_some(b[0])
}
fn sensor_w8(reg: u16, v: u8) -> bool {
    let [a, b] = reg.to_be_bytes();
    i2c_transfer(&[a, b, v], &mut [])
}
fn sensor_w16(reg: u16, v: u16) -> bool {
    let [a, b] = reg.to_be_bytes();
    let [c, d] = v.to_be_bytes();
    i2c_transfer(&[a, b, c, d], &mut [])
}
fn sensor_table(pairs: &[(u16, u16)]) -> bool {
    pairs.iter().all(|&(a, v)| sensor_w16(a, v))
}
fn sensor_id() -> u32 {
    match (sensor_r(0x0716), sensor_r(0x0717)) {
        (Some(h), Some(l)) => u32::from(h) << 8 | u32::from(l),
        _ => 0xdead_0000,
    }
}

// ---- Exposure (Hi-1336: coarse integration lines at 0x020a, analog gain
// at 0x0213 in sixteenths above 1x) ----
const LINE_NS: u32 = 10_007; // 6004 pixel clocks at 600 MHz
const FRAME_LINES: u32 = 3331; // 30 frames per second
const MARGIN: u32 = 7;
/// 2^(i/20) in 1/256ths.
const STEPS: [u32; 20] = [256, 265, 274, 284, 294, 305, 315, 327, 338, 350, 362, 375, 388, 402, 416, 431, 446, 461, 478, 494];
/// Level 0..=255 is exposure on a log scale, from 8 lines at 1x (level 0)
/// doubling every 20 levels to the longest frame at 16x.
fn exposure(level: u32) -> (u32, u32) {
    let total = (8u64 * u64::from(STEPS[(level % 20) as usize]) << (level / 20)) >> 8;
    let max_lines = FRAME_LINES - MARGIN;
    let lines = (total as u32).clamp(MARGIN, max_lines);
    let gain16 = ((total * 16) / u64::from(lines)).clamp(16, 256) as u32;
    (lines, gain16 - 16)
}
pub fn set_exposure(level: u32) {
    let (lines, gain) = exposure(level);
    sensor_w8(0x020d, 0);
    sensor_w16(0x020a, lines as u16);
    sensor_w8(0x0213, gain as u8);
    note(S_EXPOSURE, lines);
    note(S_EXPOSURE + 1, gain);
    note(S_EXPOSURE + 2, level);
    let _ = LINE_NS;
}

// ---- MIPI CSI receiver, 4 data lanes + clock (D-PHY) ----
/// The CSI port the rear sensor is wired to: CSI1 on the XT2163DL (found on
/// the handset: CSI0 and CSI2 stay idle while it streams). Each port's analog
/// front end is two 4 KiB halves (A and B); CSI0 feeds SENINF1, CSI1 SENINF3
/// and CSI2 SENINF4 (MT8183's seninf mapping, with no SENINF5 here).
static mut PORT: usize = 1;
fn port() -> usize {
    unsafe { PORT }
}
fn mipi_a() -> usize {
    MIPI + port() * 0x2000
}
fn si() -> usize {
    SENINF + [0, 0x2000, 0x3000][port()]
}
fn mipi_on() {
    for half in [0, 0x1000] {
        let b = mipi_a() + half;
        field(b, 0x00, 0, 1, 0); // RG_CSI0A_CPHY_EN: D-PHY
        // Lane roles: CSIxA L0/L1 data, L2 clock; CSIxB L0/L1 data.
        field(b, 0x00, 5, 1, 0);
        field(b, 0x00, 6, 1, 1);
        field(b, 0x00, 8, 1, 0);
        field(b, 0x00, 9, 1, 1);
        field(b, 0x00, 11, 1, if half == 0 { 1 } else { 0 });
        field(b, 0x00, 12, 1, 1);
        // Byte clock invert.
        field(b, 0xa8, 0, 3, 7);
        // Analog EQ.
        field(b, 0x18, 4, 2, 1);
        field(b, 0x18, 6, 2, 1);
        field(b, 0x1c, 20, 2, 1);
        field(b, 0x1c, 22, 2, 1);
        field(b, 0x20, 20, 2, 1);
        field(b, 0x20, 22, 2, 1);
        field(b, 0x24, 24, 8, 0x40);
        field(b, 0x80, 16, 2, 0); // CSR_CSI_RST_MODE
    }
    wr(MIPI, 0x40, rd(MIPI, 0x40) | 0x90);
    // mipi0a_csr_0a (clk-mt6765-mipi0a.c): the receiver's clock enable.
    field(mipi_a(), 0x80, 1, 1, 1);
    for half in [0, 0x1000] {
        field(mipi_a() + half, 0x00, 3, 1, 1); // BG_CORE_EN
    }
    udelay(40);
    for half in [0, 0x1000] {
        field(mipi_a() + half, 0x00, 2, 1, 1); // BG_LPF_EN
    }
    note(S_MIPI, rd(mipi_a(), 0));
    note(S_MIPI + 1, rd(mipi_a() + 0x1000, 0));
}
fn mipi_off() {
    for half in [0, 0x1000] {
        field(mipi_a() + half, 0x00, 2, 2, 0);
    }
}

// ---- SENINFn (the port's receiver) -> mux 1 -> CAMSV1 ----
const S1: usize = SENINF; // SENINF1's page, which also holds the top registers and mux 1
fn seninf_csi() {
    let si = si();
    // Top: the port as one 4D1C D-PHY. The muxes leave low-power mode
    // (MUX_LP_MODE, set at reset here), or nothing flows.
    field(SENINF, 0x00, 31, 1, 0);
    field(SENINF, 0x00, 10, 2, 3); // SENINF and SENINF2 pixel clocks
    let phy = 0x1c + 4 * port();
    field(SENINF, phy, 0, 1, 0); // DPHY_MODE: 4D1C
    field(SENINF, phy, 8, 2, 2); // CK_SEL_1
    field(SENINF, phy, 31, 1, 1); // PHY_SENINF_LANE_MUX_CSIx_EN
    // Lane order inside the receiver for 4D1C.
    field(si, 0x824, 24, 2, 1);
    field(si, 0x824, 26, 2, 2);
    field(si, 0x824, 28, 2, 0);
    field(si, 0x824, 30, 2, 3);
    field(si, 0x200, 0, 1, 1); // SENINF_EN
    field(si, 0x200, 28, 1, 0); // PAD2CAM_DATA_SEL: 10 bit
    field(si, 0x200, 12, 4, 0); // SENINF_SRC_SEL: CSI2
    field(si, 0x204, 6, 1, 1); // CSI2_IP_EN
    field(si, 0x204, 5, 1, 0); // NCSI2_IP_EN
    field(si, 0x204, 1, 1, 0); // TESTMDL_IP_EN
    wr(si, 0xa0c, 1 << 7); // no DPCM (DI_30_DPCM_EN, as MT8365's driver writes)
    field(si, 0xa08, 8, 8, unsafe { SETTLE }); // data settle
    wr(si, 0xa00, rd(si, 0xa00) | (1 << 16) | 0x10 | 0xf);
    field(si, 0xa74, 10, 1, 0);
    field(si, 0xa74, 11, 1, 0);
    field(si, 0xa74, 0, 4, 3);
    field(si, 0xae8, 0, 8, 0);
    field(si, 0xae8, 8, 8, 0);
    field(si, 0xb20, 0, 16, 0xff00);
    field(si, 0xb20, 16, 16, 0x001d);
    field(si, 0xa00, 27, 1, 0); // CLOCK_HS_OPTION
    field(si, 0xa00, 7, 1, 0); // HSRX_DET_EN
    field(si, 0xa00, 25, 1, 1); // HS_TRAIL_EN
    field(si, 0xa40, 0, 8, 8);
    field(si, 0xa18, 31, 1, 1); // debug port: packet count
    field(si, 0xa18, 0, 8, 0x1a);
    wr(si, 0xa20, 0xffff_fffe); // CSI2_SPARE0 ("HQ" in MediaTek's drivers)
    wr(si, 0xa10, 0xffff_ffff);
    wr(si, 0xa14, 0xffff_ffff);
    field(si, 0x200, 7, 1, 1); // CSI2_SW_RST
    udelay(1);
    field(si, 0x200, 7, 1, 0);
    seninf_mux(0);
}
static mut SETTLE: u32 = 0x15;
/// Mux 1 (in SENINF1's page) takes SENINF1, in one-pixel mode; `test` picks
/// the test pattern generator instead of the CSI2 receiver.
fn seninf_mux(test: u32) {
    field(S1, 0xd00, 31, 1, 1); // MUX_EN
    field(S1, 0xd00, 12, 4, if test != 0 { 1 } else { 8 }); // SRC_SEL: test model or MIPI
    field(S1, 0xd3c, 0, 4, if test != 0 { 0 } else { 1 }); // SRC_SEL_EXT: normal model
    field(S1, 0xd3c, 4, 1, 0);
    field(S1, 0xd00, 8, 1, 0); // one pixel per clock
    field(S1, 0xd00, 7, 1, test & 1);
    field(S1, 0xd00, 28, 2, if test != 0 { 1 } else { 2 });
    field(S1, 0xd00, 22, 6, 0x1b);
    field(S1, 0xd00, 16, 6, 0x1f);
    field(S1, 0xd00, 9, 2, 0); // sync polarities
    wr(S1, 0xd04, 0x8000_007f);
    wr(S1, 0xd2c, (1 << 13) | 0xc0000); // MUX_SPARE: FIFO_FULL_SEL
    wr(S1, 0xd00, (rd(S1, 0xd00) | 3) & !3); // pulse the mux and IRQ resets
    let source = if test != 0 { 0 } else { [0, 2, 3][port()] as u32 };
    field(SENINF, 0x08, 0, 4, source); // top mux 1 <- the port's SENINF
    wr(SENINF, 0x10, 0); // every cam input <- mux 1
}
/// The test pattern generator in SENINF1: colour bars at `w` x `h`.
fn seninf_test(w: u32, h: u32) {
    field(SENINF, 0x00, 31, 1, 0); // MUX_LP_MODE off
    field(SENINF, 0x00, 10, 2, 3);
    field(S1, 0x200, 0, 1, 1);
    field(S1, 0x200, 12, 4, 1);
    seninf_mux(1);
    // TM_EN, colour bars, 4-line VSYNC, 0x28 dummy pixels (BayLibre's 2023
    // MT8365 driver); the clock divider must not be zero or nothing runs.
    wr(S1, 0x608, 1 | (0xc << 4) | (4 << 8) | (0x28 << 16));
    wr(S1, 0x60c, ((h + 8) << 16) | w);
    wr(S1, 0x610, 8);
    wr(S1, 0x614, 1);
    wr(S1, 0x204, rd(S1, 0x204) | 2);
}
fn seninf_off() {
    let si = si();
    wr(si, 0xa00, rd(si, 0xa00) & !0x1f);
    field(si, 0x200, 0, 1, 0);
    field(S1, 0x608, 0, 1, 0);
    field(S1, 0xd00, 31, 1, 0);
    field(S1, 0x200, 0, 1, 0);
}
fn seninf_report() {
    for (i, (b, o)) in [
        (SENINF, 0x00), (SENINF, 0x08), (SENINF, 0x10), (SENINF, 0x1c),
        (si(), 0x200), (si(), 0xa00), (si(), 0xa14), (S1, 0xd00),
        (S1, 0xd08), (S1, 0xd0c), (si(), 0xa1c), (si(), 0xa28),
    ].iter().enumerate() {
        note(S_SENINF + i, rd(*b, *o));
    }
}

// ---- CAMSV1 (MT6765 offsets) ----
const SV_MODULE_EN: usize = 0x010;
const SV_FMT_SEL: usize = 0x014;
const SV_INT_EN: usize = 0x018;
const SV_INT_STATUS: usize = 0x01c;
const SV_SW_CTL: usize = 0x020;
const SV_IMGO_FBC: usize = 0x02c;
const SV_CLK_EN: usize = 0x030;
const SV_PAK: usize = 0x04c;
const SV_IMGO_BASE: usize = 0x220;
const SV_IMGO_OFST: usize = 0x228;
const SV_IMGO_XSIZE: usize = 0x230;
const SV_IMGO_YSIZE: usize = 0x234;
const SV_IMGO_STRIDE: usize = 0x238;
const SV_IMGO_ERR: usize = 0x444;
const SV_TG_SEN_MODE: usize = 0x500;
const SV_TG_VF_CON: usize = 0x504;
const SV_TG_GRAB_PXL: usize = 0x508;
const SV_TG_GRAB_LIN: usize = 0x50c;
const SV_TG_PATH_CFG: usize = 0x510;
const SV_TG_FRMSIZE: usize = 0x538;
const SV_TG_INTER: usize = 0x53c;
const IRQ_SOF: u32 = 1 << 7;
const IRQ_DONE: u32 = (1 << 10) | (1 << 20);
const IRQ_ERRORS: u32 = (1 << 4) | (1 << 5) | (1 << 16) | (1 << 17);
/// Output packing (CAMSV_PAK): 0 RAW8, 1 RAW10, 2 RAW12, 3 RAW14, the reverse
/// of MT8365's order (measured on the handset by the byte period of frames:
/// 3 gave seven-byte groups of four pixels). RAW8 keeps each pixel's high
/// eight bits.
static mut PAK_MODE: u32 = 0;
static mut TG_FMT: u32 = 1; // RAW10 from the sensor

/// SMI larb3, the camera's memory port, comes out of reset with the IOMMU
/// on for all 21 of its ports (SMI_LARB_NON_SEC_CON bit 0), which would
/// translate CAMSV's addresses through whatever page table the M4U holds.
/// Camera gives CAMSV physical addresses, so every port bypasses it.
const LARB_PORTS: usize = 21;
fn larb_physical() {
    for i in 0..LARB_PORTS {
        wr(LARB3, 0x380 + 4 * i, 0);
    }
}
fn camsv_start(w: u32, h: u32, stride: u32, offset: usize) {
    larb_physical();
    wr(CAMSV, SV_SW_CTL, 4);
    wr(CAMSV, SV_SW_CTL, 0);
    wr(CAMSV, SV_SW_CTL, 1);
    let start = now();
    while rd(CAMSV, SV_SW_CTL) != 3 && now().wrapping_sub(start) < unsafe { ticks_per_ms } {}
    wr(CAMSV, SV_SW_CTL, 0);
    wr(CAMSV, SV_CLK_EN, 1 | 4 | (1 << 15));
    wr(CAMSV, SV_TG_SEN_MODE, 0);
    wr(CAMSV, SV_TG_GRAB_PXL, w << 16);
    wr(CAMSV, SV_TG_GRAB_LIN, h << 16);
    wr(CAMSV, SV_TG_PATH_CFG, 0x1000);
    wr(CAMSV, SV_IMGO_FBC, 0);
    wr(CAMSV, SV_INT_EN, IRQ_SOF | IRQ_DONE | IRQ_ERRORS);
    wr(CAMSV, SV_MODULE_EN, 1 | 4 | (1 << 30));
    wr(CAMSV, SV_FMT_SEL, unsafe { TG_FMT });
    wr(CAMSV, SV_PAK, unsafe { PAK_MODE });
    wr(CAMSV, SV_IMGO_XSIZE, stride - 1);
    wr(CAMSV, SV_IMGO_YSIZE, h - 1);
    wr(CAMSV, SV_IMGO_STRIDE, (1 << 19) | (3 << 16) | stride);
    wr(CAMSV, SV_IMGO_OFST, 0);
    arm(offset);
    let _ = rd(CAMSV, SV_INT_STATUS);
    wr(CAMSV, SV_TG_SEN_MODE, rd(CAMSV, SV_TG_SEN_MODE) | 1); // CMOS_EN
    wr(CAMSV, SV_MODULE_EN, rd(CAMSV, SV_MODULE_EN) | (1 << 4)); // IMGO_EN
    wr(CAMSV, SV_TG_VF_CON, rd(CAMSV, SV_TG_VF_CON) | 1); // VFDATA_EN
}
fn camsv_stop() {
    wr(CAMSV, SV_TG_VF_CON, rd(CAMSV, SV_TG_VF_CON) & !1);
    wr(CAMSV, SV_TG_SEN_MODE, rd(CAMSV, SV_TG_SEN_MODE) & !1);
    wr(CAMSV, SV_MODULE_EN, 0);
    wr(CAMSV, SV_SW_CTL, 4);
    wr(CAMSV, SV_SW_CTL, 0);
}
fn camsv_report() {
    for (i, o) in [SV_MODULE_EN, SV_FMT_SEL, SV_IMGO_STRIDE, SV_SW_CTL, SV_TG_SEN_MODE, SV_TG_VF_CON, SV_TG_FRMSIZE, SV_TG_INTER]
        .iter()
        .enumerate()
    {
        note(S_CAMSV + i, rd(CAMSV, *o));
    }
    note(S_IMGO, rd(CAMSV, SV_IMGO_BASE));
    note(S_IMGO + 1, rd(CAMSV, SV_IMGO_ERR));
    for i in 0..4 {
        note(S_LARB + i, rd(LARB3, [0x000, 0x008, 0x380, 0x380 + 4 * (LARB_PORTS - 1)][i]));
    }
}

// ---- The camera subsystem's clock gates ----
const CAM_GATES: u32 = 1 | (1 << 6) | (1 << 7) | (1 << 8) | (1 << 9) | (1 << 10) | (1 << 11);

static mut POWERED: bool = false;
static mut STREAMING: bool = false;
static mut TEST: bool = false;
static mut REQUEST_SEEN: u32 = 0;

/// Power the rear sensor the way imgsensor_cfg_table.c orders it for the
/// Hi-1336, then read its ID.
fn sensor_power_on() -> Result<(), u32> {
    stage(10);
    let ok = platform(DOMAIN | I2C_PINS | MIPI_GATE, DOMAIN | I2C_PINS | MIPI_GATE);
    if ok & DOMAIN == 0 {
        return Err(10);
    }
    stage(11);
    wr(CAMSYS, 0x08, CAM_GATES);
    note(S_CAMSYS, rd(CAMSYS, 0));
    platform(RESET | FRONT_RESET, 0);
    mdelay(2);
    platform(MCLK, MCLK);
    mclk_on();
    mdelay(2);
    stage(12);
    let io = pmic_set(LDO_VCAMIO, 1, 1);
    platform(AVDD, AVDD);
    let core = pmic_set(VCAMD_VOLTAGE, 5 << 8, 0x0f00) && pmic_set(LDO_VCAMD, 1, 1); // 1.1 V
    mdelay(1);
    ldo_report();
    if !io || !core {
        return Err(12);
    }
    stage(13);
    platform(RESET, RESET);
    // Motorola's ellis kernel pulses the front sensor's reset as the rear
    // one comes out of reset (gpio.c, CONFIG_MOTO_ELLIS_PROJECT_CAMERA).
    platform(FRONT_RESET, FRONT_RESET);
    mdelay(1);
    platform(FRONT_RESET, 0);
    mdelay(2);
    unsafe { POWERED = true };
    stage(14);
    unsafe { FAST = false };
    let id = sensor_id();
    note(S_SENSOR_ID, id | SENSOR_ADDR << 16);
    if id != HI1336_ID {
        return Err(14);
    }
    Ok(())
}
/// MCLK0 leaves the SoC through SENINF's TG1 clock generator, fed by
/// camtg_sel (24 MHz): divide by one, ADCLK and the pad clock on.
fn mclk_on() {
    wr(SENINF, 0x604, 0); // TG1_SEN_CK: CLKFL, CLKRS, CLKCNT = 0
    wr(SENINF, 0x600, (1 << 31) | (1 << 29) | (1 << 2) | 1); // PCEN, ADCLK_EN, CLKFL_POL, TGCLK_SEL
    note(S_MCLK, rd(SENINF, 0x600));
}
fn sensor_power_off() {
    if !unsafe { POWERED } {
        return;
    }
    unsafe { SHADOW_READY = false };
    platform(RESET, 0);
    mdelay(1);
    pmic_set(LDO_VCAMD, 0, 1);
    platform(AVDD, 0);
    pmic_set(LDO_VCAMIO, 0, 1);
    platform(MCLK, 0);
    wr(SENINF, 0x600, 0);
    ldo_report();
    // Gate the camera clocks before the domain goes down.
    wr(CAMSYS, 0x04, CAM_GATES);
    platform(DOMAIN | MIPI_GATE, 0);
    unsafe { POWERED = false };
}

pub fn init() {
    note(S_MAGIC, 0x4843_4d41); // "AMCH"
    note(S_TABLES, u32::from(tables::PRESENT) | (tables::HI1336_INIT.len() as u32) << 8);
    stage(1);
}
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
pub fn start(level: u32) -> Result<Format, u32> {
    let began = now();
    if unsafe { TEST } {
        return test_start();
    }
    if !tables::PRESENT {
        return Err(2);
    }
    if let Err(e) = sensor_power_on() {
        sensor_power_off();
        return Err(e);
    }
    stage(20);
    unsafe { FAST = true };
    if !sensor_table(tables::HI1336_INIT) || !sensor_table(tables::HI1336_SLIM) {
        sensor_power_off();
        return Err(20);
    }
    // 30 frames per second rather than the table's 120.
    sensor_w16(0x020e, FRAME_LINES as u16);
    set_exposure(level);
    stage(21);
    mipi_on();
    seninf_csi();
    stage(22);
    let f = format();
    camsv_start(f.width, f.height, f.stride, 0);
    stage(23);
    sensor_w16(0x0b00, 0x0100); // stream on
    note(S_STREAM_REG, sensor_r(0x0b00).map_or(0xdead, u32::from));
    unsafe { STREAMING = true };
    note(S_ELAPSED, now().wrapping_sub(began) / unsafe { ticks_per_ms });
    stage(24);
    report();
    Ok(f)
}
fn format() -> Format {
    // PAK RAW8 writes one byte per pixel. The other packings (bench testing
    // only) are MediaTek's, which apps do not decode: format 0.
    let pak = unsafe { PAK_MODE } as usize;
    Format {
        width: WIDTH,
        height: HEIGHT,
        stride: WIDTH * [8, 10, 12, 14][pak & 3] / 8,
        format: if pak == 0 { FORMAT_RAW8 | BAYER_GBRG } else { 0 },
        sensor: HI1336_ID,
    }
}
/// CAMSV fed by SENINF's test pattern, with the sensor left off.
fn test_start() -> Result<Format, u32> {
    stage(30);
    let ok = platform(DOMAIN | MIPI_GATE, DOMAIN | MIPI_GATE);
    if ok & DOMAIN == 0 {
        return Err(30);
    }
    wr(CAMSYS, 0x08, CAM_GATES);
    note(S_CAMSYS, rd(CAMSYS, 0));
    // The test model runs from TG1's clock, as the sensor's MCLK does.
    platform(MCLK, MCLK);
    mclk_on();
    unsafe { POWERED = true };
    let f = format();
    seninf_test(f.width, f.height);
    camsv_start(f.width, f.height, f.stride, 0);
    unsafe { STREAMING = true };
    stage(31);
    report();
    Ok(Format { sensor: 0x7e57, ..f })
}
pub fn stop() {
    if unsafe { STREAMING } {
        if !unsafe { TEST } {
            sensor_w16(0x0b00, 0);
            mdelay(10);
        }
        camsv_stop();
        seninf_off();
        mipi_off();
        unsafe { STREAMING = false };
    }
    if unsafe { TEST } {
        wr(SENINF, 0x600, 0);
        wr(CAMSYS, 0x04, CAM_GATES);
        platform(DOMAIN | MIPI_GATE | MCLK, 0);
        unsafe { POWERED = false };
    } else {
        sensor_power_off();
    }
    stage(40);
    report();
}
extern "C" {
    fn page_invalidate(cap: u32, start: u32, end: u32) -> i32;
}
/// The frame window's four 1 MiB sections, in Camera's slots 8 to 11.
const FRAME_SLOT: u32 = 8;
const SECTION: usize = 0x10_0000;
/// Drop cached lines of [offset, offset + len) that CAMSV has just written.
pub fn invalidate(offset: usize, len: usize) {
    let end = offset + len;
    let mut at = offset & !(SECTION - 1);
    while at < end {
        let (from, to) = (offset.max(at) - at, end.min(at + SECTION) - at);
        unsafe { page_invalidate(FRAME_SLOT + (at / SECTION) as u32, from as u32, to as u32) };
        at += SECTION;
    }
}
/// Point IMGO at the slot `offset` bytes into the window. CAMSV latches it at
/// the next start of frame.
pub fn arm(offset: usize) {
    wr(CAMSV, SV_IMGO_BASE, WINDOW_PHYS + offset as u32);
}
pub fn frame_done() -> bool {
    if !unsafe { STREAMING } {
        return false;
    }
    let irq = rd(CAMSV, SV_INT_STATUS);
    if irq == 0 {
        return false;
    }
    note(S_IRQ, noted(S_IRQ) | irq);
    if irq & IRQ_SOF != 0 {
        note(S_IRQ + 1, noted(S_IRQ + 1).wrapping_add(1));
    }
    if irq & IRQ_ERRORS != 0 {
        note(S_IRQ + 3, noted(S_IRQ + 3).wrapping_add(1));
    }
    if irq & IRQ_DONE != 0 {
        note(S_IRQ + 2, noted(S_IRQ + 2).wrapping_add(1));
        return true;
    }
    false
}
/// Every 16th frame: a 160 x 90 grey preview (2 x 2 Bayer blocks averaged
/// over 8 x 8 pixels) for `moto-package camera-shot`, and the registers.
pub fn preview(f: &Format, offset: usize) {
    let frames = noted(S_FRAMES).wrapping_add(1);
    note(S_FRAMES, frames);
    let base = (WINDOW + offset) as *const u8;
    note(S_FIRST, unsafe { (base as *const u32).read_volatile() });
    if frames % 16 != 1 {
        return;
    }
    let raw8 = f.format & 0xff == FORMAT_RAW8;
    let (pw, ph) = (f.width / 8, f.height / 8);
    for y in 0..ph {
        for x in 0..pw {
            let mut sum = 0u32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (px, py) = (x * 8 + dx, y * 8 + dy);
                    let i = if raw8 { px } else { px / 4 * 5 + px % 4 };
                    sum += u32::from(unsafe { base.add((py * f.stride + i) as usize).read_volatile() });
                }
            }
            unsafe { PREVIEW.add((y * pw + x) as usize).write(sum.div_ceil(4) as u8) };
        }
    }
    report();
}
fn report() {
    seninf_report();
    camsv_report();
    note(S_CAMSYS, rd(CAMSYS, 0));
}
pub fn request() -> Option<u32> {
    unsafe {
        let serial = REQUEST.read_volatile();
        if serial == REQUEST_SEEN {
            return None;
        }
        REQUEST_SEEN = serial;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let request = REQUEST.add(1).read_volatile();
        note(S_REQUEST, request);
        note(S_REQUEST + 1, serial);
        Some(request)
    }
}
static mut LATCH: u32 = 0;
/// Board requests over USB (`moto-package camera=HEX`, command in the high byte):
/// 10 power the sensor and read its ID, 11 power it off, 12 RRRR read a sensor
/// register into word 25, 13 RRRRVV write one, 14 AAAAAA read the 32-bit
/// register at 0x09000000 + A into word 25, 15 VVVVVV and 16 0000VV latch the
/// low 24 and high 8 bits of a value, 17 AAAAAA write the latch there, 18 0000TT
/// test pattern mode on (1) or off (0), 19 0000PP the CAMSV PAK mode, 1a 0000FF
/// the TG format, 1b 0000PP the torch, 1c refresh the report.
pub fn developer(request: u32) {
    let arg = request & 0x00ff_ffff;
    match request >> 24 {
        0x10 => {
            let r = sensor_power_on();
            note(S_PEEK, r.map_or_else(|e| e, |_| 0));
        }
        0x11 => sensor_power_off(),
        0x12 => note(S_PEEK, sensor_r(arg as u16).map_or(0xdead, u32::from)),
        0x13 => note(S_PEEK, u32::from(sensor_w8((arg >> 8) as u16, arg as u8))),
        0x14 => {
            note(S_PEEK + 1, arg);
            note(S_PEEK, rd(0x0900_0000, arg as usize & !3));
        }
        0x15 => unsafe { LATCH = (LATCH & 0xff00_0000) | arg },
        0x16 => unsafe { LATCH = (LATCH & 0x00ff_ffff) | (arg << 24) },
        0x17 => wr(0x0900_0000, arg as usize & !3, unsafe { LATCH }),
        0x18 => unsafe { TEST = arg != 0 },
        0x19 => unsafe { PAK_MODE = arg & 3 },
        0x1a => unsafe { TG_FMT = arg & 7 },
        0x1b => {
            platform(TORCH, if arg != 0 { TORCH } else { 0 });
        }
        0x1c => {
            ldo_report();
            report();
        }
        // 1d WWOO: platform items WW to state OO; 1e AAAAMV: a camera LDO
        // register AAAA, bit M (0-15) to V; 1f: MCLK generator on.
        0x1d => {
            note(S_PEEK, platform((arg >> 8) & 0xff, arg & 0xff));
        }
        0x1e => {
            let bit = 1u16 << ((arg >> 4) & 0xf);
            note(S_PEEK, u32::from(pmic_set((arg >> 8) as u16, if arg & 1 != 0 { bit } else { 0 }, bit)));
            ldo_report();
        }
        0x1f => mclk_on(),
        // 23 OOOOOO: copy 64 KiB of the frame window from byte OOOOOO x 16
        // into the preview (read it with camera-raw).
        0x23 => {
            let src = (WINDOW + (arg as usize) * 16) as *const u8;
            for i in 0..0x10000 {
                unsafe { PREVIEW.add(i).write(src.add(i).read_volatile()) };
            }
        }
        // 21 0000PP: the CSI port (0-2); 22 0000SS: the data settle count.
        0x21 => unsafe { PORT = (arg as usize).min(2) },
        0x22 => unsafe { SETTLE = arg & 0xff },
        // 20 0000PP: copy the 4 KiB register page at 0x09000000 + PP x 4 KiB
        // into the preview (read it with camera-shot).
        0x20 => {
            let base = 0x0900_0000 + ((arg & 0xff) as usize) * 0x1000;
            for i in 0..1024 {
                let v = rd(base, i * 4);
                for (j, b) in v.to_le_bytes().iter().enumerate() {
                    unsafe { PREVIEW.add(i * 4 + j).write(*b) };
                }
            }
        }
        _ => {}
    }
}
pub fn note_state(state: u32, frames: u32) {
    note(S_STATE, state | frames << 8);
}
