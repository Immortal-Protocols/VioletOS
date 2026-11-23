# Handset audio bring-up notes

These notes record how the [Audio backend](audio.rs) was derived and what the XT2163DL showed on November 22, 2026. The [Audio service README](../../services/audio/README.md) describes the current behavior.

**Sources.** Motorola's ellis kernel is `MotorolaMobilityLLC/kernel-mtk`, branch `android-12-release-s3rhs32.20-42-10-4-11`. It is the closest release to the shipped S3RHS32.20-42-13-2-3-2. The backend follows these files:

- `sound/soc/mediatek/codec/mt6357/mtk-soc-codec-6357.c`: `TurnOnDacPower`, `Audio_Amp_Change`, `TurnOnADcPowerACC`
- `sound/soc/mediatek/mt6765/` (AFE, including `set_chip_adc_in` and the interconnect table) and `drivers/clk/mediatek/clk-mt6765.c` and `clk-mt6765-audio.c` (clocks)
- `sound/soc/codecs/mt6357-accdet.c` (jack)
- `drivers/i2c/busses/i2c-mtk.c`, register map version 2
- `sound/soc/codecs/foursemi/` (the FS1603 amplifier)

The ellis DWS file and mainline `mtk-pmic-keys.c` give the buttons.

## What the handset showed

- **Clocks and pins.** `ifr_audio`, `ifr_audio26m` and `ifr_i2c_ap` are ungated after the supervisor's request. `audio_sel` runs at 26 MHz and `aud_intbus_sel` at syspll1_d4. The bootloader had already set the audio pins to the vendor modes:

  | Pins | Use | Mode |
  | --- | --- | --- |
  | GPIO17–20 | I2S3 | 6 |
  | GPIO82/83 | I2C0 | 1 |
  | GPIO93/94 | KPCOL0/1 | 1 |
  | GPIO136/138/139 | Audio link to the PMIC | 1 |

- **Buttons.** Idle TOPSTATUS reads `0x000e`, so the power and home (Volume Up) keys read as released. KP_MEM1 reads `0xffff`, so all keypad inputs read as released. Volume Down works through KP_MEM1 bit 1.
- **Jack.** After ACCDET init with the jack empty, `ACCDET_CON1` reads `0x0004` (EINT0 enabled) and `ACCDET_CON14` reads `0x00ff`, whose AB state 3 means nothing is plugged in. No EINT0 event fired, so starting in the "unplugged" polarity is right for the empty jack. Plug-in and removal are untested.
- **Headphones.** With the route forced to headphones and nothing plugged in, DL1 consumed 48,000 frames per second from AFE SRAM. The codec reached `AUDDEC_ANA_CON0 = 0x3aff` (both HP muxes on the DAC), `AUDDEC_ANA_CON1 = 0x7703` and HP gain index 10 on both channels. Power-down returned the gain to −40 dB. No PMIC write failed.
- **Speaker.** The amplifier is an **FS1603S** (ID `0x05b4`). It reset cleanly (CHIPINI `0x0003`) over the polled I2C driver.
  - **Two differences from the stock flow mattered:**
    - The amplifier only locks its PLL once the I2S3 bit clock runs, so the stream must start first.
    - Boost (BSTCTRL bit 3) is normally set by the preset; until it is set, the analog stages never start.
  - **Running state:** with boost on, STATUS reads `0x005f` (no faults, PLL locked, clocks stable) and ANASTAT reads `0x000f` (bandgap, PLL, boost and amplifier running). DIGSTAT shows the DAC running.
  - **Power-down:** afterwards SYSCTRL returns to `0x0001` and ANASTAT to `0x0000`.

- **Hands-on check.** With the image installed, the speaker and headphones both play, Volume Down changes the volume and plugging in headphones moves output to them. **Volume Up does not respond.**

## Microphone (November 23, 2026)

**Path.** The ellis audio HAL picks the mic mode at runtime, and the driver's default is ACC (AC-coupled), which the backend uses. ADC1 takes the main mic on AIN0 through the left PGA, biased by MICBIAS0 at 1.9 V. The PMIC streams the ADC over the MISO pads (MTKAIF) to the AP. There the ADDA UL source (16 kHz, with the driver's 35 Hz high-pass IIR) feeds I03/I04 → O09/O10 → VUL. VUL captures mono from the left channel into AFE SRAM at offset 16 KiB, an 8 KiB ring after the playback ring. Normal SRAM mode leaves 27 KiB usable. The ADC clock is AUDIO_TOP_CON0 bit 24, and the MTKAIF 26 MHz clock is `ifr_audio26m`, which the supervisor already ungates. The bootloader had already set GPIO140/142/143 to mode 1, and the supervisor sets them again.

**Sharing with playback.** The headphone path and the microphone share the PMIC's AFE enable, top clocks, CLKSQ, AUDGLB, the 26 MHz buffer and the pad Schmitt triggers. On the AP side they share AFE_ON and ADDA_UL_DL_CON0. The vendor's TurnOffDacPower checks GetAdcStatus, and the backend does the same in both directions. The speaker path does not use the PMIC codec.

**What the handset showed.**
- Codec state matched the vendor sequence: `AUDENC_ANA_CON0 = 0x5341` (L ADC and PGA on, AIN0, gain index 3), `AUDENC_ANA_CON8 = 0x0021`, `PMIC_AUDIO_TOP_CON0 = 0x0000`, `AFE_UL_SRC_CON0_L = 1`, `AFE_AUD_PAD_TOP = 0x3100` and `GPIO_MODE3 = 0x0249`. No PMIC write failed.
- VUL advanced at 16,000 samples per second with no overruns.
- In a quiet room, the peak was about 40 and the mean about 10 (about −60 dBFS) at 18 dB PGA.
- Acoustic loopback through the speaker, at the forced speaker route and full volume: a 1 kHz tone read a mean of about 590 and a peak of about 950. Zero crossings matched 500, 1000 and 3000 Hz tones exactly.
- With the tone playing, a 4-second recording played back through the speaker, and the mic read 1000 Hz again for the tone's length.
- Playing to headphones while the microphone ran, then stopping, left the capture running.
- The jack reported a plug for the whole session (`S_JACK = 0x103`), so ordinary output went to headphones. The speaker tests used `audio=04000001`.

**Not yet done.** The headset mic (AIN1, MICBIAS1) and the second (reference) mic on ADC2 are not wired up. There is no automatic gain beyond normalizing the recording on playback. The vendor HAL's mic mode and gain tuning live on the vendor partition and were not read.

## Still open

- **Speaker preset.** `fs1603s.fsm` is in `/vendor/firmware` on Android slot A, but SELinux denies the adb shell read, stat and copy access. It could come from the factory image's vendor partition, like the Wi-Fi firmware. With it, the backend could load the vendor EQ and protection settings instead of bypassing the DSP and holding the amplifier at about −12 dB.
- **HP trim.** The driver adds an efuse offset to `AUDDEC_ELR_0`. The backend writes the trim enable with a zero offset, which may leave a small DC offset.
- **Volume Up.** Pressing it does not change TOPSTATUS bit 3 as read, so the key is likely wired elsewhere (another TOPSTATUS bit, a PMIC key status register, or a keypad bit). Holding it while `build/moto-package audio-status` runs will show which raw word changes (words 4–7).
