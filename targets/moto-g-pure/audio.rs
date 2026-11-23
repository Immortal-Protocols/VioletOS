//! Moto G Pure audio backend.
//!
//! Sequences come from Motorola's ellis kernel (MotorolaMobilityLLC/kernel-mtk,
//! android-12-release-s3rhs32.20-42-10-4-11). See audio-bringup.md for sources.
//!
//! - Volume Up: MT6357 TOPSTATUS (0x0024) bit 3, read through Power; 0 = pressed.
//! - Volume Down: keypad KP_MEM1 bit 1 (0x10010004), mapped read-only; 0 = pressed.
//! - Jack: MT6357 ACCDET EINT0, polled the way mt6357-accdet.c handles its IRQ.
//! - Headphones: AFE DL1 (in AFE SRAM) -> ADDA -> MT6357 DAC and HP amplifier
//!   (TurnOnDacPower and Audio_Amp_Change in mtk-soc-codec-6357.c).
//! - Speaker: AFE DL1 -> I2S3 -> FourSemi FS1603 on I2C0 at 0x34. The vendor
//!   preset (fs1603.fsm) is not available, so the amplifier runs on its
//!   hardcoded register init with its DSP bypassed.
//! - Microphone: main mic (AIN0, MICBIAS0) -> MT6357 ADC1 -> MTKAIF -> AFE ADDA
//!   UL -> VUL in AFE SRAM, mono at 16 kHz (TurnOnADcPowerACC in
//!   mtk-soc-codec-6357.c, set_chip_adc_in in mt6765-sound.c).
//!
//! The Audio process cannot touch the PMIC, clocks or pins directly. Power
//! performs allowlisted PMIC accesses and the supervisor ungates the audio
//! clocks and sets the audio pin modes. A status page at 0x09203000 is readable
//! over USB (`moto-package audio-status`); developer requests arrive at 0x09204000.
use crate::protocol::*;

pub struct Inputs {
    pub up: bool,
    pub down: bool,
    pub headphones: bool,
}
const POWER_SLOT: u32 = 6;
const LIFE_SLOT: u32 = 7;
const AUDIO_PLATFORM: u32 = 0x4071;
const KEYPAD: *const u32 = 0x09000000 as *const u32;
const I2C: usize = 0x09004000;
const AFE: usize = 0x09010000;
const SRAM: usize = 0x09011000;
const SRAM_PHYS: u32 = 0x11221000;
const STATUS: *mut u32 = 0x09203000 as *mut u32;
const REQUEST: *const u32 = 0x09204000 as *const u32;
const POLL_MS: u32 = 5;
// 16 KiB of SRAM: 4096 stereo frames, 85 ms. Keep about 40 ms queued.
const RING_BYTES: u32 = 0x4000;
const RING_FRAMES: u32 = RING_BYTES / 4;
const LEAD_FRAMES: u32 = 48 * 40;

// Status page words.
const S_MAGIC: usize = 0;
const S_CAPS: usize = 1;
const S_STATE: usize = 2;
const S_PLAY: usize = 3;
const S_TOPSTATUS: usize = 4;
const S_KEYPAD: usize = 5; // 5..=7: KP_MEM1, KP_MEM2, KP_MEM5
const S_PLATFORM: usize = 8; // 8: result, 9..=18: supervisor survey
const S_JACK: usize = 19; // plugged | polarity << 1 | events << 8
const S_ACCDET: usize = 20; // 20..=24: 0x2320, 0x2324, 0x232c, 0x208c, 0x230a
const S_PMIC_FAIL: usize = 25; // count | last address << 16
const S_AFE: usize = 27; // 27..=35
const S_CODEC: usize = 36; // 36..=41
const S_AMP: usize = 42; // 42..=50
const S_STAGE: usize = 51;
const S_REQUESTS: usize = 52; // 52: count, 53: last
const S_I2C_OK: usize = 54;
const S_I2C_FAIL: usize = 55; // count | last INTR_STAT << 16
const S_CONSUMED: usize = 56;
/// Microphone, words 64..=87: 64 on | PGA << 8, 65 samples read, 66 overruns,
/// 67 CONN9 | CONN10 << 16, 68 the service's MICROPHONE arg, 69 its level, 70 peak, 71 mean level and 72 DC over the last
/// 100 ms, 73..=78 AFE (VUL_CUR, UL_SRC_CON0, DAC_CON0, DAC_CON1, UL_SRC_MON0,
/// MTKAIF_MON0), 79 zero crossings in the last 100 ms (20 per 100 Hz of a
/// tone louder than 64), 80..=86 codec registers (value | address << 16).
const S_MIC: usize = 64;

fn note(i: usize, v: u32) {
    unsafe { STATUS.add(i).write_volatile(v) }
}
fn noted(i: usize) -> u32 {
    unsafe { STATUS.add(i).read_volatile() }
}
fn stage(n: u32) {
    note(S_STAGE, n);
}
fn udelay(us: u32) {
    let start = unsafe { ticks() };
    let wait = (unsafe { ticks_per_ms } * us).div_ceil(1000);
    while unsafe { ticks() }.wrapping_sub(start) < wait {}
}

// ---- PMIC, through Power ----
fn pmic(addr: u16) -> Option<u16> {
    let r = Message::new(GET, PMIC_ACCESS, u32::from(addr)).call(POWER_SLOT);
    (r.op == OK).then_some(r.value as u16)
}
/// Ana_Set_Reg(addr, value, mask): read-modify-write of the masked bits.
fn ana(addr: u16, value: u16, mask: u16) -> bool {
    let r = Message::new(EXECUTE, PMIC_ACCESS, u32::from(addr))
        .with_arg(u32::from(mask) << 16 | u32::from(value & mask))
        .call(POWER_SLOT);
    if r.op != OK {
        let fails = (noted(S_PMIC_FAIL) & 0xffff) + 1;
        note(S_PMIC_FAIL, fails.min(0xffff) | u32::from(addr) << 16);
    }
    r.op == OK
}
#[derive(Clone, Copy)]
enum Step {
    W(u16, u16, u16),
    Us(u32),
}
use Step::*;
fn run(steps: &[Step]) -> bool {
    let mut ok = true;
    for s in steps {
        match *s {
            W(a, v, m) => ok &= ana(a, v, m),
            Us(us) => udelay(us),
        }
    }
    ok
}

// MT6357 audio registers (mtk-auddrv-ana.h, base 0x2080).
const AUD_TOP_CKPDN_CON0: u16 = 0x208c;
const AUDNCP_CLKDIV_CON0: u16 = 0x20b4;
const AUDNCP_CLKDIV_CON1: u16 = 0x20b6;
const AUDNCP_CLKDIV_CON2: u16 = 0x20b8;
const AUDNCP_CLKDIV_CON3: u16 = 0x20ba;
const AUDNCP_CLKDIV_CON4: u16 = 0x20bc;
const AFE_UL_DL_CON0: u16 = 0x2108;
const AFE_DL_SRC2_CON0_L: u16 = 0x210a;
const PMIC_AFE_TOP_CON0: u16 = 0x2110;
const PMIC_AUDIO_TOP_CON0: u16 = 0x2112;
const AFUNC_AUD_CON0: u16 = 0x2116;
const AFUNC_AUD_CON2: u16 = 0x211a;
const AFE_UL_SRC_CON0_H: u16 = 0x210c;
const AFE_UL_SRC_CON0_L: u16 = 0x210e;
const PMIC_AFE_ADDA_MTKAIF_CFG0: u16 = 0x2134;
const AFE_AUD_PAD_TOP: u16 = 0x214c;
const AUDENC_ANA_CON0: u16 = 0x2188;
const AUDENC_ANA_CON1: u16 = 0x218a;
const AUDENC_ANA_CON3: u16 = 0x218e;
const AUDENC_ANA_CON6: u16 = 0x2194;
const AUDENC_ANA_CON8: u16 = 0x2198;
const AUDENC_ANA_CON9: u16 = 0x219a;
const AUDENC_ANA_CON10: u16 = 0x219c;
const AUDDEC_ANA_CON0: u16 = 0x2208;
const AUDDEC_ANA_CON1: u16 = 0x220a;
const AUDDEC_ANA_CON2: u16 = 0x220c;
const AUDDEC_ANA_CON3: u16 = 0x220e;
const AUDDEC_ANA_CON4: u16 = 0x2210;
const AUDDEC_ANA_CON6: u16 = 0x2214;
const AUDDEC_ANA_CON7: u16 = 0x2216;
const AUDDEC_ANA_CON8: u16 = 0x2218;
const AUDDEC_ANA_CON9: u16 = 0x221a;
const AUDDEC_ANA_CON10: u16 = 0x221c;
const AUDDEC_ANA_CON11: u16 = 0x221e;
const AUDDEC_ANA_CON12: u16 = 0x2220;
const AUDDEC_ANA_CON13: u16 = 0x2222;
const AUDDEC_ELR_0: u16 = 0x2226;
const ZCD_CON0: u16 = 0x2288;
const ZCD_CON2: u16 = 0x228c;
const DCXO_CW14: u16 = 0x07ac;
const GPIO_MODE2: u16 = 0x00b6;
const GPIO_MODE2_SET: u16 = 0x00b8;
const GPIO_MODE2_CLR: u16 = 0x00ba;
const GPIO_MODE3: u16 = 0x00bc;
const GPIO_MODE3_SET: u16 = 0x00be;
const GPIO_MODE3_CLR: u16 = 0x00c0;
const GPIO_DIR0: u16 = 0x0088;
const SMT_CON1: u16 = 0x002c;
// ACCDET (mt6357.h, mt6357-accdet.c).
const AUD_TOP_RST_CON0: u16 = 0x209a;
const AUD_TOP_INT_STATUS0: u16 = 0x20ae;
const ACCDET_CON1: u16 = 0x230a;
const ACCDET_CON2: u16 = 0x230c;
const ACCDET_CON3: u16 = 0x230e;
const ACCDET_CON4: u16 = 0x2310;
const ACCDET_CON5: u16 = 0x2312;
const ACCDET_CON6: u16 = 0x2314;
const ACCDET_CON7: u16 = 0x2316;
const ACCDET_CON9: u16 = 0x231a;
const ACCDET_CON10: u16 = 0x231c;
const ACCDET_CON12: u16 = 0x2320;
const ACCDET_CON14: u16 = 0x2324;
const ACCDET_CON15: u16 = 0x2326;
const ACCDET_CON18: u16 = 0x232c;
const ACCDET_CON24: u16 = 0x2338;

// Headphone gain: ZCD_CON2 index 0 = +8 dB, 1 dB steps, 0x1f = -40 dB.
const HP_GAIN_MUTE: u16 = 0x1f;
const HP_GAIN: u16 = 10; // -2 dB; the service scales samples for volume.
fn hp_gain(index: u16) -> Step {
    W(ZCD_CON2, index << 7 | index, 0x0f9f)
}

/// TurnOnDacPower(HEADSETL).
const DAC_ON: &[Step] = &[
    W(DCXO_CW14, 1 << 13, 1 << 13), // audckbufEnable: 26 MHz to audio
    W(GPIO_MODE2_CLR, 0xffff, 0xffff), // set_playback_gpio: MOSI pad mode
    W(GPIO_MODE2_SET, 0x0249, 0xffff),
    W(GPIO_MODE2, 0x0249, 0xffff),
    W(SMT_CON1, 0x0ff0, 0x0ff0),
    W(AUDDEC_ANA_CON6, 0x0280, 0x0fff), // HP main CMFB switch
    W(AUDDEC_ANA_CON7, 0x00a8, 0xffff), // pull down HPL/R, HS, LO
    W(AUDDEC_ANA_CON6, 0x0000, 0x0fff), // headset: disable main CMFB switch
    W(AUDDEC_ANA_CON7, 0x00a0, 0x0fff), // release HP CMFB pull-down
    W(AUDDEC_ANA_CON11, 0, 1 << 4),     // NvregEnable: AUDGLB on
    W(AUDDEC_ANA_CON2, 0x400, 0x400),   // hp_pull_down(true)
    W(AUDENC_ANA_CON6, 1, 1),           // ClsqEnable: CLKSQ 26 MHz
    W(AUD_TOP_CKPDN_CON0, 0, 0x66),     // Topck_Enable
    Us(250),
    W(PMIC_AUDIO_TOP_CON0, 0, 0x00c4),
    Us(250),
    W(AFUNC_AUD_CON2, 0x0006, 0xffff),
    W(AFUNC_AUD_CON0, 0xcba1, 0xffff),
    W(AFUNC_AUD_CON2, 0x0003, 0xffff),
    W(AFUNC_AUD_CON2, 0x000b, 0xffff),
    W(AFE_UL_DL_CON0, 1, 1),
    W(AFE_DL_SRC2_CON0_L, 1, 0xffff), // setDlMtkifSrc(true), then normal path
];
const DAC_ON_TAIL: &[Step] = &[W(PMIC_AFE_TOP_CON0, 0, 0xffff)];
/// Audio_Amp_Change(on), up to the main output ramp.
const AMP_ON: &[Step] = &[
    W(AUDDEC_ELR_0, 0x1000, 0xffff), // HP trim enabled; no efuse offset applied
    W(AUDDEC_ANA_CON0, 0x3000, 0xffff), // disable short-circuit protection
    W(AUDDEC_ANA_CON3, 0x0010, 0xffff),
    W(AUDDEC_ANA_CON4, 0x0010, 0xffff),
    W(AUDDEC_ANA_CON2, 0x200, 0x200),
    W(ZCD_CON2, 0x0f9f, 0xffff), // -40 dB
    W(AUDNCP_CLKDIV_CON1, 1, 0xffff),
    W(AUDNCP_CLKDIV_CON2, 0x2c, 0xffff),
    W(AUDNCP_CLKDIV_CON0, 1, 0xffff),
    W(AUDNCP_CLKDIV_CON4, 2, 0xffff),
    W(AUDNCP_CLKDIV_CON3, 0, 0xffff), // NCP on
    Us(250),
    W(AUDDEC_ANA_CON12, 0x1055, 0x1055), // cap-less LDOs
    W(AUDDEC_ANA_CON13, 1, 0xffff),      // NV regulator
    Us(100),
    W(AUDDEC_ANA_CON8, 1, 7), // Zcd_Enable(HEADSET)
    W(ZCD_CON0, 1 << 8, 7 << 8),
    W(ZCD_CON0, 0, 1 << 7),
    W(ZCD_CON0, 0, 1 << 6),
    W(ZCD_CON0, 0, 3 << 4),
    W(ZCD_CON0, 5 << 1, 7 << 1),
    W(ZCD_CON0, 1, 1),
    W(AUDDEC_ANA_CON10, 0x0055, 0xffff), // IBIST
    W(AUDDEC_ANA_CON9, 0x92, 0xffff),
    W(AUDDEC_ANA_CON10, 0x0055, 0xffff),
    W(AUDDEC_ANA_CON2, 0x0033, 0x00ff),
    W(AUDDEC_ANA_CON1, 0x000c, 0xffff), // HP aux output stage
    W(AUDDEC_ANA_CON1, 0x003c, 0xffff), // aux feedback loop
    W(AUDDEC_ANA_CON6, 0x0c80, 0x0fff), // aux CMFB loop
    W(AUDDEC_ANA_CON0, 0x30c0, 0xffff), // driver bias
    W(AUDDEC_ANA_CON0, 0x30f0, 0xffff), // driver core
    W(AUDDEC_ANA_CON1, 0x00fc, 0x00ff), // short main output to aux
    W(AUDDEC_ANA_CON6, 0x0e80, 0x0fff),
    W(AUDDEC_ANA_CON6, 0x0280, 0x0fff),
    W(AUDDEC_ANA_CON1, 0x00ff, 0xffff), // main output stage
];
/// Audio_Amp_Change(on), after the volume ramp.
const AMP_ON_TAIL: &[Step] = &[
    W(AUDDEC_ANA_CON1, 0x77c3, 0xffff), // aux output stage off
    W(AUDDEC_ANA_CON1, 0x7703, 0xffff), // unshort main from aux
    Us(100),
    W(AUDDEC_ANA_CON11, 1, 1),          // AUD_CLK
    W(AUDDEC_ANA_CON0, 0x30ff, 0xffff), // DAC on
    W(AUDDEC_ANA_CON6, 0x0281, 0x0fff), // DAC low-noise mode
    Us(100),
    W(AUDDEC_ANA_CON0, 0x32ff, 0xffff), // HPL mux to DAC
    W(AUDDEC_ANA_CON0, 0x3aff, 0xffff), // HPR mux to DAC
    W(AUDDEC_ANA_CON2, 0, 0x400),       // release pull-down
];
/// Audio_Amp_Change(off) before and after its ramps, then TurnOffDacPower.
const AMP_OFF: &[Step] = &[
    W(AUDDEC_ANA_CON2, 0x400, 0x400), // pull down
    W(AUDDEC_ANA_CON0, 0, 0x0f00),    // HP mux open
    W(AUDDEC_ANA_CON6, 0, 0x0001),
    W(AUDDEC_ANA_CON0, 0, 0x000f), // DAC off
    W(AUDDEC_ANA_CON11, 0, 1),
    W(AUDDEC_ANA_CON1, 0x77c3, 0xffff),
    W(AUDDEC_ANA_CON1, 0x77cf, 0xffff),
];
const AMP_OFF_TAIL: &[Step] = &[
    W(AUDDEC_ANA_CON1, 0, 3),
    W(AUDDEC_ANA_CON6, 0x0e00, 0x0fff),
    W(AUDDEC_ANA_CON6, 0x0c00, 0x0fff),
    W(AUDDEC_ANA_CON1, 0, 3 << 6),
    W(AUDDEC_ANA_CON0, 0, 3 << 4),
    W(AUDDEC_ANA_CON0, 0, 3 << 6),
    W(AUDDEC_ANA_CON6, 0x0200, 0xffff),
    W(AUDDEC_ANA_CON1, 0, 3 << 4),
    W(AUDDEC_ANA_CON1, 0, 3 << 2),
    W(AUDDEC_ANA_CON7, 0xa8, 0xff),
    W(AUDDEC_ANA_CON10, 1 << 8, 1 << 8), // IBIST off
    W(AUDDEC_ANA_CON8, 4, 7),            // ZCD off
    W(ZCD_CON0, 0, 0xffff),
    W(AUDDEC_ANA_CON13, 0, 1),
    W(AUDDEC_ANA_CON12, 0, 0x1055),
    W(AUDNCP_CLKDIV_CON3, 1, 1), // NCP off
    // TurnOffDacPower
    W(AFE_DL_SRC2_CON0_L, 0, 0xffff),
    W(AFUNC_AUD_CON2, 0, 0xffff),
    W(AFUNC_AUD_CON0, 0xcba0, 0xffff),
];
/// The rest of TurnOffDacPower. The uplink shares the AFE, clocks and
/// regulators, so while the microphone runs only the downlink parts go.
fn dac_off() {
    let shared = unsafe { !CAPTURING };
    if shared {
        run(&[W(AFE_UL_DL_CON0, 0, 1), W(PMIC_AUDIO_TOP_CON0, 0x00df, 0x00df)]);
    } else {
        ana(PMIC_AUDIO_TOP_CON0, 0x0040, 0x0040); // downlink power down
    }
    run(&[
        W(AFE_AUD_PAD_TOP, 0, 0x00ff), // RX FIFOs
        Us(250),
        W(GPIO_MODE2_CLR, 0xffff, 0xffff), // set_playback_gpio(false)
        W(GPIO_MODE2, 0, 0xffff),
        W(GPIO_DIR0, 0, 0xf << 8),
        W(AUDDEC_ANA_CON2, 0, 0x400),
    ]);
    if shared {
        codec_common_off();
    }
}
/// Topck, CLKSQ, AUDGLB and the 26 MHz buffer, once neither direction needs them.
fn codec_common_off() {
    run(&[
        W(SMT_CON1, 0, 0x0ff0),
        W(AUD_TOP_CKPDN_CON0, 0x66, 0x66),
        W(AUDENC_ANA_CON6, 0, 1),
        W(AUDDEC_ANA_CON11, 1 << 4, 1 << 4),
        W(DCXO_CW14, 0, 1 << 13),
    ]);
}

// ---- Microphone (TurnOnADcPowerACC for ADC1 with the phone mic) ----
// ellis has AC-coupled analog mics. ADC1 takes the main mic on AIN0 through
// the left PGA, biased by MICBIAS0 at 1.9 V. The PMIC sends the ADC stream over
// the MISO pads (MTKAIF) to the AP, whose UL source filters and decimates it.
/// PGA gain index, 6 dB steps from 0 dB. The vendor default is 3 (18 dB).
static mut MIC_PGA: u16 = 3;
fn mic_codec_on() -> bool {
    let gain = unsafe { MIC_PGA }.min(5);
    run(&[
        W(DCXO_CW14, 1 << 13, 1 << 13), // audckbufEnable
        W(AUDDEC_ANA_CON11, 0, 1 << 4), // NvregEnable
        W(AUDENC_ANA_CON6, 1, 1),       // ClsqEnable
        W(GPIO_MODE3_CLR, 0xffff, 0xffff), // set_capture_gpio: MISO pad mode
        W(GPIO_MODE3_SET, 0x0249, 0xffff),
        W(GPIO_MODE3, 0x0249, 0xffff),
        W(SMT_CON1, 0x0ff0, 0x0ff0),
        W(AUDDEC_ANA_CON11, 1 << 5, 1 << 5), // ADC CLKGEN
        W(AUDENC_ANA_CON3, 0, 0xffff),       // ADC clock from CLKGEN (13 MHz)
        W(AUDDEC_ANA_CON12, 0x0100, 0x2500), // LCLDO_ENC 1.8 V
        W(AUDDEC_ANA_CON12, 0x2500, 0x2500), // and its remote sense
        W(AUDENC_ANA_CON8, 0x0021, 0xffff),  // MICBIAS0 on, 1.9 V
        W(AUDENC_ANA_CON0, gain << 8, 0x0700), // SetMicPGAGain
        W(AUDENC_ANA_CON1, gain << 8, 0x0700),
        W(AUDENC_ANA_CON0, 0x0041, 0xf0ff), // L PGA from AIN0, enabled
        W(AUDENC_ANA_CON0, 0x5041, 0xf000), // L ADC from the L PGA, enabled
        W(AUD_TOP_CKPDN_CON0, 0, 0x66),     // Topck_Enable
        W(PMIC_AUDIO_TOP_CON0, 0x8000, 0xdfbf), // clocks on
        W(PMIC_AFE_TOP_CON0, 0, 0xffff),
        W(AFE_UL_DL_CON0, 1, 1),
        W(PMIC_AFE_ADDA_MTKAIF_CFG0, 0, 0xffff), // MTKAIF TX format
        W(AFE_AUD_PAD_TOP, 0x3100, 0xff00),      // TX FIFOs
        W(AFE_UL_SRC_CON0_H, 0, 0xffff),         // analog mic, not DMIC
        W(AFE_UL_SRC_CON0_L, 1, 0xffff),         // uplink on
    ])
}
fn mic_codec_off() {
    // The headphone path shares the PMIC's AFE and its clocks.
    let shared = unsafe { OUTPUT != 2 };
    run(&[W(AFE_UL_SRC_CON0_L, 0, 1), W(AFE_AUD_PAD_TOP, 0x3000, 0xff00)]);
    if shared {
        run(&[W(AFE_UL_DL_CON0, 0, 1), W(PMIC_AUDIO_TOP_CON0, 0x00bf, 0x00bf)]);
    }
    run(&[
        W(PMIC_AUDIO_TOP_CON0, 0x0020, 0x0020), // uplink power down
        W(AUDENC_ANA_CON0, 0, 0xf000),
        W(AUDENC_ANA_CON0, 0, 0x0fff),
        W(AUDENC_ANA_CON8, 0, 0xffff), // MICBIAS0 off
        W(AUDDEC_ANA_CON12, 0x0100, 0x2500),
        W(AUDDEC_ANA_CON12, 0, 0x2500),
        W(AUDENC_ANA_CON3, 0, 0xffff),
        W(AUDDEC_ANA_CON11, 0, 1 << 5),
        W(GPIO_MODE3_CLR, 0xffff, 0xffff), // MISO pads back to GPIO inputs
        W(GPIO_MODE3, 0, 0xffff),
        W(GPIO_DIR0, 0, 0xf << 12),
    ]);
    if shared {
        codec_common_off();
    }
}

fn headphones_on() -> bool {
    stage(0x30);
    let mut ok = run(DAC_ON) && run(DAC_ON_TAIL);
    stage(0x31);
    ok &= run(AMP_ON);
    for i in 0..=7u16 {
        ok &= ana(AUDDEC_ANA_CON1, i << 8, 7 << 8) && ana(AUDDEC_ANA_CON1, i << 12, 7 << 12);
        udelay(600);
    }
    udelay(1000);
    for i in 0..=15u16 {
        ok &= ana(AUDDEC_ANA_CON6, i << 12, 0xf << 12);
        udelay(600);
    }
    ok &= ana(AUDDEC_ANA_CON1, 0x77cf, 0xffff);
    for i in (HP_GAIN..=20).rev() {
        ok &= run(&[hp_gain(i)]);
        udelay(250);
    }
    ok &= run(AMP_ON_TAIL);
    stage(if ok { 0x32 } else { 0x3f });
    ok
}
fn headphones_off() {
    stage(0x38);
    run(AMP_OFF);
    for i in HP_GAIN..=20 {
        run(&[hp_gain(i)]);
        udelay(250);
    }
    run(&[hp_gain(HP_GAIN_MUTE)]);
    ana(AUDDEC_ANA_CON1, 0x77ff, 0xffff);
    for i in (0..=15u16).rev() {
        ana(AUDDEC_ANA_CON6, i << 12, 0xf << 12);
        udelay(600);
    }
    for i in (0..=7u16).rev() {
        ana(AUDDEC_ANA_CON1, i << 8, 7 << 8);
        ana(AUDDEC_ANA_CON1, i << 12, 7 << 12);
        udelay(600);
    }
    run(AMP_OFF_TAIL);
    dac_off();
    stage(0x39);
}

// ---- AFE (mtk-auddrv-afe.h, mt6765-sound.c) ----
const AUDIO_TOP_CON0: usize = 0x000;
const AUDIO_TOP_CON1: usize = 0x004;
const AFE_DAC_CON0: usize = 0x010;
const AFE_DAC_CON1: usize = 0x014;
const AFE_CONN0: usize = 0x020;
const AFE_CONN1: usize = 0x024;
const AFE_CONN3: usize = 0x02c;
const AFE_CONN4: usize = 0x030;
const AFE_I2S_CON1: usize = 0x034;
const AFE_DL1_BASE: usize = 0x040;
const AFE_DL1_CUR: usize = 0x044;
const AFE_DL1_END: usize = 0x048;
const AFE_I2S_CON3: usize = 0x04c;
const AFE_CONN_24BIT: usize = 0x06c;
const AFE_MEMIF_MSB: usize = 0x0cc;
const AFE_ADDA_DL_SRC2_CON0: usize = 0x108;
const AFE_ADDA_DL_SRC2_CON1: usize = 0x10c;
const AFE_ADDA_UL_DL_CON0: usize = 0x124;
const AFE_ADDA_PREDIS_CON0: usize = 0x260;
const AFE_ADDA_PREDIS_CON1: usize = 0x264;
const AFE_MEMIF_HD_MODE: usize = 0x3f8;
const AFE_MEMIF_HDALIGN: usize = 0x3fc;
const AFE_CONN28: usize = 0x4bc;
const AFE_CONN29: usize = 0x4c0;
const AFE_ADDA_DL_SDM_DCCOMP_CON: usize = 0xc50;

fn afe(reg: usize) -> u32 {
    unsafe { ((AFE + reg) as *const u32).read_volatile() }
}
fn afe_set(reg: usize, value: u32, mask: u32) {
    unsafe {
        let p = (AFE + reg) as *mut u32;
        p.write_volatile((p.read_volatile() & !mask) | (value & mask));
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
    }
}
fn afe_report() {
    if !unsafe { CLOCKS } {
        return;
    }
    for (i, reg) in [AUDIO_TOP_CON0, AFE_DAC_CON0, AFE_DL1_CUR, AFE_DAC_CON1, AFE_ADDA_DL_SRC2_CON0,
        AFE_I2S_CON1, AFE_I2S_CON3, AUDIO_TOP_CON1].iter().enumerate()
    {
        note(S_AFE + if i < 3 { i } else { i + 1 }, afe(*reg));
    }
}
fn afe_prepare() {
    // AudDrv_Clk_On without the clock framework: ungate AFE, DAC, DAC predistortion.
    afe_set(AUDIO_TOP_CON0, 0x4000, 0x0600_4044);
    for i in 0..RING_BYTES as usize / 4 {
        unsafe { ((SRAM + i * 4) as *mut u32).write_volatile(0) };
    }
    afe_set(AFE_MEMIF_HDALIGN, 0x7fff << 16, 0x7fff << 16); // SRAM normal mode
    afe_set(AFE_MEMIF_MSB, 0, 1 << 29);
    afe_set(AFE_MEMIF_HD_MODE, 0, 3); // DL1 16-bit
    afe_set(AFE_MEMIF_HDALIGN, 0, 1);
    afe_set(AFE_DL1_BASE, SRAM_PHYS, u32::MAX);
    afe_set(AFE_DL1_END, SRAM_PHYS + RING_BYTES - 1, u32::MAX);
    afe_set(AFE_DAC_CON1, 0xa, 0xf); // DL1 48 kHz
    afe_set(AFE_DAC_CON1, 0, 1 << 21); // stereo
    afe_set(AFE_CONN_24BIT, 0, (1 << 0) | (1 << 1) | (1 << 3) | (1 << 4) | (1 << 28) | (1 << 29));
}
fn afe_route(headphones: bool) {
    // Interconnect: I05/I06 (DL1 L/R) to O03/O04 and O28/O29 (ADDA) or O00/O01 (I2S3).
    afe_set(AFE_CONN3, (headphones as u32) << 5, 1 << 5);
    afe_set(AFE_CONN4, (headphones as u32) << 6, 1 << 6);
    afe_set(AFE_CONN28, (headphones as u32) << 5, 1 << 5);
    afe_set(AFE_CONN29, (headphones as u32) << 6, 1 << 6);
    afe_set(AFE_CONN0, (!headphones as u32) << 5, 1 << 5);
    afe_set(AFE_CONN1, (!headphones as u32) << 6, 1 << 6);
}
fn afe_start(headphones: bool) {
    afe_route(headphones);
    afe_set(AFE_DAC_CON0, 1, 1); // AFE_ON
    if headphones {
        afe_set(AFE_ADDA_PREDIS_CON0, 0, u32::MAX);
        afe_set(AFE_ADDA_PREDIS_CON1, 0, u32::MAX);
        afe_set(AFE_ADDA_DL_SRC2_CON0, 0x8300_1802, u32::MAX); // 48 kHz
        afe_set(AFE_ADDA_DL_SRC2_CON1, 0xf74f_0000, u32::MAX); // -0.3 dB
        afe_set(AFE_ADDA_DL_SDM_DCCOMP_CON, 0x1d, 0x3f);
        afe_set(AFE_I2S_CON1, 0x0a0a, u32::MAX); // 48 kHz, I2S, 32-bit, from O28/O29
        afe_set(AFE_ADDA_UL_DL_CON0, 1, 1);
        afe_set(AFE_ADDA_DL_SRC2_CON0, 1, 1);
        afe_set(AFE_I2S_CON1, 1, 1);
    } else {
        afe_set(AUDIO_TOP_CON1, 0, 1 << 7); // ungate the I2S3 bit clock
        afe_set(AFE_I2S_CON3, 0x0a09, u32::MAX); // 48 kHz, I2S, 16-bit, enabled
    }
    afe_set(AFE_DAC_CON0, 1 << 1, 1 << 1); // DL1_ON
}
fn afe_stop() {
    let capturing = unsafe { CAPTURING };
    afe_set(AFE_DAC_CON0, 0, 1 << 1);
    afe_set(AFE_I2S_CON1, 0, 1);
    afe_set(AFE_I2S_CON3, 0, 1);
    afe_set(AFE_ADDA_DL_SRC2_CON0, 0, 1);
    if !capturing {
        afe_set(AFE_ADDA_UL_DL_CON0, 0, 1);
    }
    afe_set(AUDIO_TOP_CON1, 1 << 7, 1 << 7);
    if !capturing {
        afe_set(AFE_DAC_CON0, 0, 1);
    }
    afe_route(false);
    afe_set(AFE_CONN0, 0, 1 << 5);
    afe_set(AFE_CONN1, 0, 1 << 6);
}

// Capture: ADDA UL (I03/I04) -> VUL, mono from the left channel, 16-bit at
// 16 kHz, into AFE SRAM after the playback ring. In normal SRAM mode the
// usable SRAM is 27 KiB, so the capture ring takes 8 KiB at offset 16 KiB.
const AFE_VUL_BASE: usize = 0x080;
const AFE_VUL_END: usize = 0x088;
const AFE_VUL_CUR: usize = 0x08c;
const AFE_ADDA_UL_SRC_CON0: usize = 0x114;
const AFE_ADDA_TOP_CON0: usize = 0x120;
const AFE_ADDA_UL_SRC_MON0: usize = 0x148;
const AFE_ADDA_IIR_COEF: usize = 0x290; // five words through 0x2a0
const AFE_CONN9: usize = 0x440;
const AFE_CONN10: usize = 0x444;
const AFE_ADDA_MTKAIF_CFG0: usize = 0xe00;
const AFE_ADDA_MTKAIF_RX_CFG0: usize = 0xe20;
const AFE_ADDA_MTKAIF_MON0: usize = 0xe34;
const AFE_AUD_PAD_TOP_CFG: usize = 0xe40;
const MIC: usize = SRAM + RING_BYTES as usize;
const MIC_PHYS: u32 = SRAM_PHYS + RING_BYTES;
const MIC_BYTES: u32 = 0x2000;
const MIC_SAMPLES: u32 = MIC_BYTES / 2;
const MIC_RATE: u32 = 16000;

fn afe_mic_start() {
    afe_set(AUDIO_TOP_CON0, 0x4000, 0x4044); // AFE clock (AudDrv_Clk_On)
    afe_set(AUDIO_TOP_CON0, 0, 1 << 24); // ADC clock (AudDrv_ADC_Clk_On)
    for i in 0..MIC_BYTES as usize / 4 {
        unsafe { ((MIC + i * 4) as *mut u32).write_volatile(0) };
    }
    afe_set(AFE_MEMIF_HDALIGN, 0x7fff << 16, 0x7fff << 16); // SRAM normal mode
    afe_set(AFE_MEMIF_MSB, 0, 1 << 29);
    afe_set(AFE_MEMIF_HD_MODE, 0, 3 << 10); // VUL 16-bit
    afe_set(AFE_MEMIF_HDALIGN, 0, 1 << 5);
    afe_set(AFE_VUL_BASE, MIC_PHYS, u32::MAX);
    afe_set(AFE_VUL_END, MIC_PHYS + MIC_BYTES - 1, u32::MAX);
    afe_set(AFE_DAC_CON1, 4 << 16, 0xf << 16); // VUL 16 kHz
    afe_set(AFE_DAC_CON1, 1 << 27, 3 << 27); // mono, left channel
    afe_set(AFE_CONN_24BIT, 0, 3 << 9); // O09/O10 16-bit
    afe_set(AFE_CONN9, 1 << 3, 1 << 3); // I03 -> O09
    afe_set(AFE_CONN10, 1 << 4, 1 << 4); // I04 -> O10
    // set_chip_adc_in(16000), after the PMIC's MISO clock is running.
    afe_set(AFE_AUD_PAD_TOP_CFG, 0x31, u32::MAX);
    afe_set(AFE_ADDA_MTKAIF_CFG0, 0, u32::MAX);
    afe_set(AFE_ADDA_TOP_CON0, 0, 1); // internal ADC
    for (i, c) in [0, 0x3fb8, 0x3fb8_0000, 0x3fb8_0000, 0xc048].iter().enumerate() {
        afe_set(AFE_ADDA_IIR_COEF + i * 4, *c, u32::MAX); // 35 Hz high-pass (at 48 kHz)
    }
    afe_set(AFE_ADDA_UL_SRC_CON0, 1 << 17 | 1 << 10, u32::MAX); // 16 kHz, IIR on
    afe_set(AFE_ADDA_MTKAIF_RX_CFG0, 0, 1); // analog mic data
    // set_adc_enable, then the memory interface and the AFE.
    afe_set(AFE_ADDA_UL_DL_CON0, 1, 1);
    afe_set(AFE_ADDA_UL_SRC_CON0, 1, 1);
    afe_set(AFE_DAC_CON0, 1 << 3, 1 << 3); // VUL_ON
    afe_set(AFE_DAC_CON0, 1, 1); // AFE_ON
}
fn afe_mic_stop() {
    let output = unsafe { OUTPUT };
    afe_set(AFE_DAC_CON0, 0, 1 << 3);
    afe_set(AFE_ADDA_UL_SRC_CON0, 0, 1);
    if output != 2 {
        afe_set(AFE_ADDA_UL_DL_CON0, 0, 1);
    }
    udelay(150);
    afe_set(AUDIO_TOP_CON0, 1 << 24, 1 << 24);
    afe_set(AFE_CONN9, 0, 1 << 3);
    afe_set(AFE_CONN10, 0, 1 << 4);
    if output == 0 {
        afe_set(AFE_DAC_CON0, 0, 1);
    }
}

// ---- I2C0 in FIFO mode, polled (i2c-mtk.c, register map version 2) ----
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
const AMP: u32 = 0x34;

fn i2c_w(reg: usize, v: u32) {
    unsafe {
        ((I2C + reg) as *mut u32).write_volatile(v);
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
    }
}
fn i2c_r(reg: usize) -> u32 {
    unsafe { ((I2C + reg) as *const u32).read_volatile() }
}
/// One transaction: write `out`, then (with a repeated start) read `input.len()` bytes.
fn i2c_transfer(out: &[u8], input: &mut [u8]) -> bool {
    let read = !input.is_empty();
    i2c_w(I2C_DEBUGCTRL, 0x28);
    i2c_w(I2C_CLOCK_DIV, 0x0404); // clock-div 5 from the device tree
    // The slowest divider pair (8 x 64): under 30 kHz for any i2c_ck up to 136 MHz.
    i2c_w(I2C_CONTROL, 0x20 | 0x08 | if read { 0x10 | 0x02 } else { 0 });
    i2c_w(I2C_EXT_CONF, 0x8001);
    i2c_w(I2C_DELAY_LEN, 0x0a);
    i2c_w(I2C_IO_CONFIG, 0x0003); // open drain
    i2c_w(I2C_TIMING, (7 << 8) | 63);
    i2c_w(I2C_LTIMING, (7 << 6) | 63);
    i2c_w(I2C_HS, 0);
    i2c_w(I2C_SLAVE_ADDR, AMP << 1);
    i2c_w(I2C_INTR_STAT, 0x1ff);
    i2c_w(I2C_FIFO_ADDR_CLR, 1);
    i2c_w(I2C_INTR_MASK, 0x2f | 0x100 | 0x20);
    i2c_w(I2C_TRANSFER_LEN, out.len() as u32);
    if read {
        i2c_w(I2C_TRANSFER_LEN_AUX, input.len() as u32);
        i2c_w(I2C_TRANSAC_LEN, 2);
    } else {
        i2c_w(I2C_TRANSAC_LEN, 1);
    }
    for &b in out {
        unsafe { ((I2C + I2C_DATA) as *mut u8).write_volatile(b) };
    }
    i2c_w(I2C_MCU_INTR, 1);
    i2c_w(I2C_START, 1);
    let start = unsafe { ticks() };
    let mut stat;
    loop {
        stat = i2c_r(I2C_INTR_STAT) & 0x1ff;
        if stat != 0 || unsafe { ticks().wrapping_sub(start) >= ticks_per_ms * 20 } {
            break;
        }
    }
    i2c_w(I2C_INTR_STAT, 0x1ff);
    // Completion alone; any of ACKERR, HS NACK, arbitration, timeout or bus error fails.
    if stat & 0x16e != 0 || stat & 1 == 0 {
        i2c_w(I2C_FIFO_ADDR_CLR, 1);
        let fails = (noted(S_I2C_FAIL) & 0xffff) + 1;
        note(S_I2C_FAIL, fails.min(0xffff) | (stat | 0x8000) << 16);
        return false;
    }
    for b in input.iter_mut() {
        *b = unsafe { ((I2C + I2C_DATA) as *const u8).read_volatile() };
    }
    note(S_I2C_OK, noted(S_I2C_OK).wrapping_add(1));
    true
}
fn amp_w(reg: u8, v: u16) -> bool {
    i2c_transfer(&[reg, (v >> 8) as u8, v as u8], &mut [])
}
fn amp_r(reg: u8) -> Option<u16> {
    let mut b = [0; 2];
    i2c_transfer(&[reg], &mut b).then(|| u16::from(b[0]) << 8 | u16::from(b[1]))
}
fn amp_set(reg: u8, value: u16, mask: u16) -> bool {
    amp_r(reg).is_some_and(|old| amp_w(reg, (old & !mask) | (value & mask)))
}
const AMP_KEY: u16 = 0xca91;
fn amp_report() {
    for (i, reg) in [0x03u8, 0x00, 0x05, 0x09, 0x90, 0x04, 0xa0, 0x06, 0xa1].iter().enumerate() {
        note(S_AMP + i, amp_r(*reg).map_or(0xdead_0000 | u32::from(*reg), u32::from));
    }
}
/// fs1603_i2c_reset and fs1603_reg_init, then leave the amplifier powered down.
fn amp_init() -> bool {
    let id = amp_r(0x03);
    note(S_AMP, id.map_or(0xdead_0003, u32::from));
    if id.map(|v| v >> 8) != Some(0x05) {
        return false;
    }
    let mut ready = false;
    for _ in 0..3 {
        amp_w(0x09, 0x0002);
        let _ = amp_r(0x09);
        unsafe { delay_ms(15) };
        amp_w(0x09, 0x0001);
        if matches!(amp_r(0x90), Some(0x0003 | 0x0300)) {
            ready = true;
            break;
        }
    }
    let mut ok = ready && amp_w(0xc4, 0x000a);
    unsafe { delay_ms(5) };
    ok &= amp_w(0x06, 0x0000)
        && amp_w(0x0b, AMP_KEY)
        && amp_w(0xc0, 0x29c0)
        && amp_w(0xc4, 0x000f)
        && amp_w(0xae, 0x0210)
        && amp_w(0xb9, 0xffff)
        && amp_w(0x09, 0x0009)
        && amp_w(0xcd, 0x2004)
        // No preset: run the DSP bypassed (DSPEN and POSTEQEN clear).
        && amp_w(0xa1, unsafe { AMP_DSP });
    // fs1603s_reg_init for this revision (0x05b4 is not FS1603S_V1), with the
    // trims it applies when the preset is older than version 3.
    if id.is_some_and(|v| v & 0xfff0 == 0x05b0) {
        ok &= amp_w(0xc7, 0x504b)
            && amp_set(0xe1, 0xa << 2, 0xf << 2)
            && amp_w(0x46, 0xca8b)
            && amp_w(0x47, 0xb4be)
            && amp_w(0x48, 0x1008)
            && amp_w(0x49, 0x1311)
            && amp_w(0xce, 0x8000)
            && amp_w(0xcf, 0x0010);
    }
    ok &= amp_w(0x0b, 0)
        && amp_w(0x09, 0x0001)
        && amp_w(0xc4, 0x0000);
    amp_report();
    ok
}
/// fsm_stub_start_up with I2S at 48 kHz and a 1.536 MHz bit clock, then unmute.
fn amp_start() -> bool {
    stage(0x40);
    let mut ok = amp_set(0x04, 8 << 12 | 3, 0xf007) // I2SSR 48 kHz, I2S format
        && amp_w(0xc4, 0);
    unsafe { delay_ms(1) };
    ok &= amp_w(0x0b, AMP_KEY)
        && amp_w(0xd0, 0x0100)
        && amp_w(0xc1, 0x0260)
        && amp_w(0xc2, 0x0540)
        && amp_w(0xc3, 0x0006)
        && amp_w(0xc4, 0x000f)
        && amp_w(0x0b, 0)
        && amp_set(0x09, 0, 1) // PWDN off
        // Boost on (BSTEN). The preset would set it; its voltage and current
        // limit fields keep the values from fs1603_reg_init.
        && amp_set(0xc0, 1 << 3, 1 << 3);
    // Wait for PLL lock and no over-voltage, over-temperature or clock faults.
    for _ in 0..20 {
        unsafe { delay_ms(5) };
        if amp_r(0x00).is_some_and(|s| s & 0x0003 == 0x0003) {
            break;
        }
    }
    ok &= amp_w(0x06, unsafe { AMP_VOLUME } << 8) && amp_set(0x09, 1 << 3, 1 << 3); // AMPE
    // fsm_speaker_onn: wait for the boost soft start (BSTCTRL SSEND), then
    // fsm_stub_set_tsignal(true): TSEN and OFF_AUTOEN, and on the FS1603S
    // release the DAC hold (0xAE bit 9) 20 ms later.
    for _ in 0..20 {
        if amp_r(0xc0).is_some_and(|v| v & (1 << 15) != 0) {
            break;
        }
        unsafe { delay_ms(5) };
    }
    unsafe { delay_ms(10) };
    ok &= amp_set(0xaf, 1 << 3 | 1 << 13, 1 << 3 | 1 << 13);
    unsafe { delay_ms(20) };
    ok &= amp_set(0xae, 0, 1 << 9);
    unsafe { delay_ms(10) };
    amp_report();
    stage(if ok { 0x41 } else { 0x4f });
    ok
}
fn amp_stop() {
    amp_set(0xae, 1 << 9, 1 << 9); // hold the DAC again (set_tsignal(false))
    amp_set(0xc0, 0, 1 << 3);
    amp_set(0xaf, 0, 1 << 3);
    amp_set(0x09, 0, 1 << 3);
    amp_set(0x09, 1, 1);
    amp_w(0xc4, 0);
    amp_report();
}

// ---- Jack (ACCDET EINT0) ----
fn accdet_report() {
    for (i, reg) in [ACCDET_CON12, ACCDET_CON14, ACCDET_CON18, AUD_TOP_CKPDN_CON0, ACCDET_CON1]
        .iter()
        .enumerate()
    {
        note(S_ACCDET + i, pmic(*reg).map_or(0xdead_0000, u32::from));
    }
}
/// accdet_init and accdet_init_once with PMIC EINT0, as the ellis device tree selects.
fn accdet_init() -> bool {
    // The driver leaves the ACCDET clock to the bootloader; make sure it runs.
    let mut ok = ana(AUD_TOP_CKPDN_CON0, 0, 1);
    ok &= ana(ACCDET_CON1, 2, 2);
    unsafe { delay_ms(3) };
    ok &= ana(ACCDET_CON1, 0, 2);
    unsafe { delay_ms(1) };
    ok &= run(&[
        W(ACCDET_CON6, 0x0800, 0xffff),
        W(ACCDET_CON7, 0x0800, 0xffff),
        W(ACCDET_CON9, 0x0020, 0xffff),
        W(ACCDET_CON10, 0x0044, 0xffff),
        W(AUD_TOP_RST_CON0, 2, 2),
        W(AUD_TOP_RST_CON0, 0, 2),
        W(ACCDET_CON3, 0x04ff, 0xffff),
        W(ACCDET_CON4, 0x04ff, 0xffff),
        W(ACCDET_CON5, 0x81f0, 0xffff),
        W(AUDENC_ANA_CON9, 6 << 4, 6 << 4),
        W(AUDENC_ANA_CON10, 0x0807, 0x0807),
        W(AUDENC_ANA_CON6, 0x00a0, 0x00a0),
        W(ACCDET_CON15, 6 << 8 | 2 << 12, 7 << 8 | 3 << 12),
        W(ACCDET_CON15, 0xe << 3, 0xf << 3), // debounce for a plug-in
        W(ACCDET_CON2, 0x0808, 0x0808),
        W(ACCDET_CON1, 0x0004, 0x0004),
        W(ACCDET_CON24, 0x10, 0x14),
        W(ACCDET_CON12, 0, 1 << 14), // start unplugged: the next event is a plug-in
    ]);
    accdet_report();
    ok
}
static mut PLUGGED: bool = false;
static mut EVENTS: u32 = 0;
fn accdet_poll() {
    let Some(stat) = pmic(ACCDET_CON12) else { return };
    if stat & (1 << 2) == 0 {
        return;
    }
    unsafe {
        PLUGGED = !PLUGGED;
        EVENTS += 1;
        // eint_polarity_reverse: wait for the opposite edge next.
        ana(ACCDET_CON12, (PLUGGED as u16) << 14, 1 << 14);
        ana(ACCDET_CON12, 1 << 10, 1 << 10);
        for _ in 0..20 {
            if pmic(ACCDET_CON12).is_some_and(|v| v & (1 << 2) == 0) {
                break;
            }
            udelay(100);
        }
        ana(ACCDET_CON12, 0, 1 << 10);
        ana(AUD_TOP_INT_STATUS0, 1 << 6, 1 << 6);
        ana(ACCDET_CON15, if PLUGGED { 0x1 } else { 0xe } << 3, 0xf << 3);
        note(S_JACK, PLUGGED as u32 | (PLUGGED as u32) << 1 | EVENTS << 8);
        // A wrong polarity guess would flip on every poll. Stop trusting the jack.
        if EVENTS > 50 {
            JACK_OK = false;
            PLUGGED = false;
            note(S_JACK, noted(S_JACK) | 1 << 31);
        }
    }
    accdet_report();
}

// ---- Backend interface ----
static mut PMIC_KEYS: bool = false;
static mut KEYPAD_OK: bool = false;
static mut CLOCKS: bool = false;
static mut I2C_ON: bool = false;
static mut JACK_OK: bool = false;
static mut AMP_OK: bool = false;
static mut AMP_VOLUME: u16 = 0xe0; // About -12 dB: no preset means no speaker protection.
static mut AMP_DSP: u16 = 0x0092;
static mut LAST: (u32, bool, bool) = (0, false, false);
/// The output now running: 0 none, 1 speaker, 2 headphones.
static mut OUTPUT: u32 = 0;
static mut CAPTURING: bool = false;
static mut MIC_PINS: bool = false;
static mut MIC_RD: u32 = 0;
static mut MIC_TOTAL: u32 = 0;
static mut MIC_OVERRUNS: u32 = 0;
static mut MIC_PEAK: u32 = 0;
static mut MIC_SUM: u32 = 0;
static mut MIC_DC: i32 = 0;
static mut MIC_WINDOW: u32 = 0;
static mut MIC_CROSSINGS: u32 = 0;
static mut MIC_POSITIVE: bool = false;
static mut WR: u32 = 0;
static mut QUIET: u32 = 0;
static mut HW_LAST: u32 = 0;
static mut SEEN: u32 = 0;

fn platform(want: u32) -> u32 {
    let m = Message::new(EXECUTE, AUDIO_PLATFORM, want).call(LIFE_SLOT);
    // MR4..MR13 carry the survey: data[0..32] then tail[0..8].
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

pub fn init() -> u32 {
    note(S_MAGIC, 0x3244_5541); // "AUD2"
    stage(1);
    unsafe {
        PMIC_KEYS = pmic(0x0024).is_some();
        // Unused keypad inputs read as released (1). All zeros means the block
        // is unclocked, which would otherwise look like Volume Down held forever.
        KEYPAD_OK = keypad(2) == 0xffff && keypad(5) & 0xff == 0xff;
        stage(2);
        let done = platform(1 | 2 | 4 | 8);
        CLOCKS = done & 1 != 0;
        stage(3);
        if PMIC_KEYS {
            JACK_OK = accdet_init();
        }
        stage(4);
        I2C_ON = done & 4 != 0;
        if I2C_ON {
            AMP_OK = amp_init();
        }
        stage(5);
        if CLOCKS {
            afe_report();
        }
        let mut caps = 0;
        if PMIC_KEYS || KEYPAD_OK {
            caps |= AUDIO_BUTTONS;
        }
        if JACK_OK {
            caps |= AUDIO_JACK;
        }
        if CLOCKS && PMIC_KEYS {
            caps |= AUDIO_HEADPHONE_OUT;
        }
        if CLOCKS && AMP_OK {
            caps |= AUDIO_SPEAKER_OUT;
        }
        MIC_PINS = done & 8 != 0;
        if CLOCKS && PMIC_KEYS && MIC_PINS {
            caps |= AUDIO_MIC;
        }
        note(S_CAPS, caps);
        stage(6);
        caps
    }
}
fn keypad(word: usize) -> u32 {
    unsafe { KEYPAD.add(word).read_volatile() & 0xffff }
}
pub fn inputs() -> Inputs {
    let now = unsafe { ticks() };
    unsafe {
        if now.wrapping_sub(LAST.0) >= POLL_MS * ticks_per_ms || LAST.0 == 0 {
            let top = if PMIC_KEYS { pmic(0x0024) } else { None };
            let up = top.is_some_and(|v| v & (1 << 3) == 0);
            let down = KEYPAD_OK && keypad(1) & (1 << 1) == 0;
            LAST = (now | 1, up, down);
            note(S_TOPSTATUS, top.map_or(0xdead_0000, u32::from));
            note(S_KEYPAD, keypad(1));
            note(S_KEYPAD + 1, keypad(2));
            note(S_KEYPAD + 2, keypad(5));
            if JACK_OK {
                accdet_poll();
            }
        }
        Inputs { up: LAST.1, down: LAST.2, headphones: JACK_OK && PLUGGED }
    }
}
pub fn set_volume(_level: u32, _headphones: bool) {}
pub fn start(headphones: bool) -> bool {
    unsafe {
        if !CLOCKS || OUTPUT != 0 {
            return false;
        }
        afe_prepare();
        WR = 0;
        QUIET = 0;
        HW_LAST = 0;
        // The headphone codec powers up before the stream; the speaker amplifier
        // locks its PLL to the I2S3 bit clock, so the stream starts first.
        let ok = if headphones {
            headphones_on() && { afe_start(true); true }
        } else {
            AMP_OK && { afe_start(false); amp_start() }
        };
        if !ok {
            afe_stop();
            if headphones {
                headphones_off();
            } else {
                amp_stop();
            }
            afe_report();
            return false;
        }
        OUTPUT = 1 + headphones as u32;
        afe_report();
        true
    }
}
pub fn route(headphones: bool) -> bool {
    stop();
    start(headphones)
}
pub fn stop() {
    unsafe {
        if OUTPUT == 0 {
            return;
        }
        let headphones = OUTPUT == 2;
        OUTPUT = 0;
        afe_stop();
        if headphones {
            headphones_off();
        } else {
            amp_stop();
        }
        afe_report();
    }
}
/// Frames the DMA has not yet reached, measured from its read pointer.
fn buffered() -> u32 {
    let cur = afe(AFE_DL1_CUR);
    let hw = if (SRAM_PHYS..SRAM_PHYS + RING_BYTES).contains(&cur) { (cur - SRAM_PHYS) / 4 } else { 0 };
    unsafe {
        let moved = (hw + RING_FRAMES - HW_LAST) % RING_FRAMES;
        HW_LAST = hw;
        SEEN = SEEN.wrapping_add(moved);
        note(S_CONSUMED, SEEN);
        (WR + RING_FRAMES - hw) % RING_FRAMES
    }
}
pub fn writable() -> usize {
    if unsafe { OUTPUT == 0 } {
        return 0;
    }
    LEAD_FRAMES.saturating_sub(buffered()) as usize
}
pub fn write(frames: &[[i16; 2]]) {
    unsafe {
        for f in frames {
            let word = (f[0] as u16 as u32) | (f[1] as u16 as u32) << 16;
            ((SRAM + WR as usize * 4) as *mut u32).write_volatile(word);
            WR = (WR + 1) % RING_FRAMES;
        }
    }
}
/// Fill with silence until a whole ring of it has gone out, so no stale samples replay.
pub fn drained() -> bool {
    unsafe {
        if OUTPUT == 0 {
            return true;
        }
        let n = writable();
        for _ in 0..n {
            ((SRAM + WR as usize * 4) as *mut u32).write_volatile(0);
            WR = (WR + 1) % RING_FRAMES;
        }
        QUIET += n as u32;
        QUIET >= RING_FRAMES
    }
}
/// Power the microphone and start the capture DMA. Samples arrive at MIC_RATE.
pub fn mic_start() -> bool {
    unsafe {
        if !CLOCKS || !PMIC_KEYS || !MIC_PINS || CAPTURING {
            return CAPTURING;
        }
        stage(0x50);
        CAPTURING = true;
        // The PMIC's MISO clock must run before the AP's pad FIFO is enabled.
        if !mic_codec_on() {
            mic_codec_off();
            CAPTURING = false;
            stage(0x5f);
            mic_report();
            return false;
        }
        afe_mic_start();
        MIC_RD = 0;
        MIC_WINDOW = 0;
        MIC_SUM = 0;
        MIC_PEAK = 0;
        stage(0x51);
        mic_codec_report();
        mic_report();
        true
    }
}
pub fn mic_stop() {
    unsafe {
        if !CAPTURING {
            return;
        }
        afe_mic_stop();
        mic_codec_off();
        CAPTURING = false;
        stage(0x52);
        mic_report();
    }
}
/// Copies the samples captured since the last call into `out`; returns the count.
pub fn mic_read(out: &mut [i16]) -> usize {
    unsafe {
        if !CAPTURING {
            return 0;
        }
        let cur = afe(AFE_VUL_CUR);
        let hw = if (MIC_PHYS..MIC_PHYS + MIC_BYTES).contains(&cur) { (cur - MIC_PHYS) / 2 } else { MIC_RD };
        let mut ready = (hw + MIC_SAMPLES - MIC_RD) % MIC_SAMPLES;
        // Fell a whole ring behind: drop what was overwritten.
        if ready > MIC_SAMPLES - 256 {
            MIC_OVERRUNS += 1;
            MIC_RD = hw;
            ready = 0;
        }
        let n = (ready as usize).min(out.len());
        for s in out[..n].iter_mut() {
            *s = ((MIC + MIC_RD as usize * 2) as *const i16).read_volatile();
            MIC_RD = (MIC_RD + 1) % MIC_SAMPLES;
            // Peak and mean level over 100 ms windows, around a slow DC estimate.
            let v = i32::from(*s);
            MIC_DC += (v * 256 - MIC_DC) >> 10;
            let a = (v - (MIC_DC >> 8)).unsigned_abs();
            MIC_PEAK = MIC_PEAK.max(a);
            MIC_SUM += a;
            // Zero crossings with a little hysteresis: twice the pitch of a tone.
            let ac = v - (MIC_DC >> 8);
            if (MIC_POSITIVE && ac < -64) || (!MIC_POSITIVE && ac > 64) {
                MIC_POSITIVE = !MIC_POSITIVE;
                MIC_CROSSINGS += 1;
            }
            MIC_WINDOW += 1;
            if MIC_WINDOW == MIC_RATE / 10 {
                note(S_MIC + 6, MIC_PEAK);
                note(S_MIC + 7, MIC_SUM / MIC_WINDOW);
                note(S_MIC + 8, (MIC_DC >> 8) as u32);
                note(S_MIC + 15, MIC_CROSSINGS);
                MIC_WINDOW = 0;
                MIC_SUM = 0;
                MIC_PEAK = 0;
                MIC_CROSSINGS = 0;
            }
        }
        MIC_TOTAL = MIC_TOTAL.wrapping_add(n as u32);
        n
    }
}
/// The service's microphone state for the status page: MICROPHONE arg and level.
pub fn publish_mic(state: u32, level: u32) {
    note(S_MIC + 4, state);
    note(S_MIC + 5, level);
}
fn mic_report() {
    unsafe {
        note(S_MIC, CAPTURING as u32 | u32::from(MIC_PGA) << 8);
        note(S_MIC + 1, MIC_TOTAL);
        note(S_MIC + 2, MIC_OVERRUNS);
        if CLOCKS {
            for (i, reg) in [AFE_VUL_CUR, AFE_ADDA_UL_SRC_CON0, AFE_DAC_CON0, AFE_DAC_CON1,
                AFE_ADDA_UL_SRC_MON0, AFE_ADDA_MTKAIF_MON0].iter().enumerate()
            {
                note(S_MIC + 9 + i, afe(*reg));
            }
            note(S_MIC + 3, afe(AFE_CONN9) | afe(AFE_CONN10) << 16);
        }
    }
}
fn mic_codec_report() {
    for (i, reg) in [AUDENC_ANA_CON0, AUDENC_ANA_CON8, PMIC_AUDIO_TOP_CON0, AFE_UL_SRC_CON0_L,
        AFE_AUD_PAD_TOP, GPIO_MODE3, AUDDEC_ANA_CON12].iter().enumerate()
    {
        note(S_MIC + 16 + i, pmic(*reg).map_or(0xdead_0000, u32::from) | u32::from(*reg) << 16);
    }
}
pub fn publish(level: u32, flags: u32, serial: u32, sound: u32) {
    note(S_STATE, level | flags << 8 | unsafe { OUTPUT } << 24);
    note(S_PLAY, serial | sound << 16);
    if unsafe { OUTPUT != 0 } {
        let _ = buffered();
        afe_report();
    }
    if unsafe { CAPTURING } {
        mic_report();
    }
}
static mut REQUEST_SEEN: u32 = 0;
pub fn request() -> Option<u32> {
    unsafe {
        let serial = REQUEST.read_volatile();
        if serial == REQUEST_SEEN {
            return None;
        }
        REQUEST_SEEN = serial;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let request = REQUEST.add(1).read_volatile();
        note(S_REQUESTS, serial);
        note(S_REQUESTS + 1, request);
        Some(request)
    }
}
/// Board commands from the developer link: 0x10 amplifier volume (8 bits),
/// 0x11 amplifier DSP control word, 0x12 re-run the amplifier init, 0x13 refresh
/// the status page, 0x14 codec register readback, 0x15 amplifier register
/// write (register << 16 | value), 0x16 amplifier register read into word 57,
/// 0x17 microphone PGA gain index.
pub fn debug(command: u32, param: u32) {
    unsafe {
        match command {
            0x10 => AMP_VOLUME = (param & 0xff) as u16,
            0x11 => AMP_DSP = param as u16,
            0x12 if I2C_ON => AMP_OK = amp_init(),
            0x13 => {
                accdet_report();
                afe_report();
                mic_report();
                mic_codec_report();
                if I2C_ON {
                    amp_report();
                }
            }
            0x14 => {
                for (i, reg) in [AUDDEC_ANA_CON0, AUDDEC_ANA_CON1, ZCD_CON2, PMIC_AUDIO_TOP_CON0,
                    AFUNC_AUD_CON2, DCXO_CW14].iter().enumerate()
                {
                    note(S_CODEC + i, pmic(*reg).map_or(0xdead_0000, u32::from));
                }
            }
            0x15 if I2C_ON => {
                amp_w((param >> 16) as u8, param as u16);
                amp_report();
            }
            0x16 if I2C_ON => {
                let reg = param as u8;
                note(57, amp_r(reg).map_or(0xdead_0000, u32::from) | u32::from(reg) << 24);
            }
            // Microphone PGA gain index, 0 dB to 30 dB in 6 dB steps. It applies
            // at once while capturing.
            0x17 if param <= 5 => {
                MIC_PGA = param as u16;
                if CAPTURING {
                    ana(AUDENC_ANA_CON0, MIC_PGA << 8, 0x0700);
                    ana(AUDENC_ANA_CON1, MIC_PGA << 8, 0x0700);
                }
                mic_report();
            }
            _ => {}
        }
    }
}
