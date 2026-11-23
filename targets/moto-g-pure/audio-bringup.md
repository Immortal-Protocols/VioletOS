# Handset audio bring-up notes

These notes record how the [Audio backend](audio.rs) was derived and what the XT2163DL showed on November 22, 2026. The [Audio service README](../../services/audio/README.md) describes the current behavior.

**Sources.** Motorola's ellis kernel is `MotorolaMobilityLLC/kernel-mtk`, branch `android-12-release-s3rhs32.20-42-10-4-11`. It is the closest release to the shipped S3RHS32.20-42-13-2-3-2. The backend follows these files:

- `sound/soc/mediatek/codec/mt6357/mtk-soc-codec-6357.c`: `TurnOnDacPower`, `Audio_Amp_Change`
- `sound/soc/mediatek/mt6765/` (AFE) and `drivers/clk/mediatek/clk-mt6765.c` (clocks)
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

- **AFE SRAM size.** Markers written from `0x11221000` read back through offset `0x6b00` and read zero from `0x6c00`. The usable SRAM is 27 KiB, which bounds the playback ring.
- **Hands-on check.** With the image installed, the speaker and headphones both play, Volume Down changes the volume and plugging in headphones moves output to them. **Volume Up does not respond.**

## Still open

- **Speaker preset.** `fs1603s.fsm` is in `/vendor/firmware` on Android slot A, but SELinux denies the adb shell read, stat and copy access. It could come from the factory image's vendor partition, like the Wi-Fi firmware. With it, the backend could load the vendor EQ and protection settings instead of bypassing the DSP and holding the amplifier at about −12 dB.
- **HP trim.** The driver adds an efuse offset to `AUDDEC_ELR_0`. The backend writes the trim enable with a zero offset, which may leave a small DC offset.
- **Volume Up.** Pressing it does not change TOPSTATUS bit 3 as read, so the key is likely wired elsewhere (another TOPSTATUS bit, a PMIC key status register, or a keypad bit). Holding it while `build/moto-package audio-status` runs will show which raw word changes (words 4–7).
