# Handset camera bring-up notes

These notes record how the [camera backend](camera.rs) was derived and what the XT2163DL showed on November 24, 2026. The [Camera service README](../../services/camera/README.md) describes the current behavior.

## Sources

- **Motorola's ellis kernel** (`MotorolaMobilityLLC/kernel-mtk`, branch `android-12-release-s3rhs32.20-42-10-4-11`, commit `fbd7d0d5c865`):
  - `drivers/misc/mediatek/imgsensor/src/mt6765/camera_hw/imgsensor_cfg_table.c`: the Hi-1336's power sequence (reset low, MCLK, DOVDD 1.8 V, AVDD, DVDD 1.1 V, reset high).
  - `arch/arm/boot/dts/mt6762-ellis-camera-sensor-overlay.dtsi`: the pins (MCLK0 on GPIO99, reset on GPIO101, the AVDD switch on GPIO92, the torch on GPIO154) and the regulators (VCAMIO, VCAMD).
  - `drivers/misc/mediatek/dws/mt6765/ellis_evt.dws`: CAMERA_MAIN on I2C channel 2 at address `0x40` (7-bit `0x20`).
  - `drivers/misc/mediatek/imgsensor/src/common/v1/mot_ellis_hi1336_mipi_raw/`: the ID registers (`0x0716`/`0x0717` = `0x1336`), the init and 1280 × 720 "slim video" tables, stream on (`0x0b00`), coarse integration (`0x020a`) and analog gain (`0x0213`).
  - `drivers/clk/mediatek/clk-mt6765*.c`: the CAM power domain sequence, `camtg_sel`, `seninf_sel`, `camtm_sel`, the camsys and MMSYS gates and `mipi0a_csr_0a`.
  - `drivers/i2c/busses/i2c-mtk.c`: the multi-channel I2C (`ch_offset`, shadow registers, arbitration).
  - `drivers/misc/mediatek/cameraisp/src/mt6765/camera_isp.c`: the CAMSV register offsets and interrupt bits, and the SENINF debug dump (which confirms the SENINF register layout).
- **MediaTek's MT8183 seninf driver** (Louis Kuo, RFC v7, July 2020): the SENINF top, CSI-2 receiver and mux registers and the D-PHY sequence.
- **BayLibre's MT8365 ISP 3.0 drivers** (v8, January 2025, and the 2023 version with a test pattern) and **mainline's MIPI CSI 0.5 PHY driver**: CAMSV programming, `MUX_LP_MODE`, the mux SPARE value, `CSI2_SPARE0` and the test model's clock divider.

The MT6765's SENINF configuration otherwise lives in MediaTek's closed camera HAL, so the remaining details were found on the handset.

## What the handset showed

Each step below was needed; the earlier ones looked fine in registers before the later ones were found.

1. **Power and MCLK.** The supervisor powers the CAM domain (PWR_STATUS bit 25 set, `CAM_PWR_CON = 0x0c0d`), sets `camtg_sel` to 24 MHz and GPIO99 to CMMCLK0. Sampling GPIO99's input level showed it toggling. Power turns on VCAMIO and VCAMD at 1.1 V (`VCAMD_ANA_CON0` selector 5).
2. **I2C2 is a shared, arbitrated bus.** The AP's channel is `0x100` into the block (the device tree's `ch_offset_default`), behind a set of "shadow" registers that the vendor driver resets first. The arbiter and IMM clocks (`ifr_i2c2a`, `ifr_i2c2_imm`, infra CG4 bits 23 and 24) must run too. Until the data FIFO was also written at the channel offset, a sensor that ACKed its address hung the transfer waiting for bytes that had gone to the wrong FIFO. With the sensor in reset, the same read ended in a clean ACK error, which separated the two problems. The sensor then answered: **ID `0x1336` at `0x20`**.
3. **Clocks for the receivers.** `seninf_sel` (univpll1_d4, 312 MHz), `camtm_sel` (univpll2_d2, 208 MHz), the CSI0 and CSI1 26 MHz gates in APMIXED and `mipi0a_csr_0a` (bit 1 of the receiver's register `0x80`).
4. **The muxes start in low-power mode.** `SENINF_TOP_CTRL` reads `0x80000c00` at power-up; bit 31 (`MUX_LP_MODE`) must be cleared or nothing passes the muxes. The test model also needs a non-zero clock divider (`TM_CLK = 8`). With both, SENINF's colour bars reached CAMSV: 1280 × 728 frames with SOF and pass-1-done interrupts.
5. **CAMSV's memory port.** CAMSV first reported IMGO overruns: the camera's bridge to the MM bus (`mm_smi_cam_ck`, MMSYS gate bit 25, the device tree's `gals-cam2mm`) was gated. After that, frames completed, but the window still held Android's leftover camera HAL strings: SMI larb3 comes out of reset with the IOMMU on for all 21 ports (`SMI_LARB_NON_SEC_CON` bit 0). Camera now clears it for every port before CAMSV starts, so CAMSV writes physical addresses.
6. **The rear camera is on CSI1, not CSI0.** With CSI0, every lane state machine sat in its idle state and no packet arrived. Selecting CSI1 (feeding SENINF3, `PHY_SENINF_CTL_CSI1`) gave 721 packets per frame, `CSI2_INT_STATUS = 0x9044`, and CAMSV frames of 1280 × 721 at about 27 frames per second with no errors. CSI2 stayed idle as well.
7. **CAMSV's packing is reversed from the MT8365's.** With PAK mode 3 the frame data repeated every seven bytes (four 14-bit pixels). Trying each mode while streaming showed 0 = RAW8 (the high eight bits), 1 = RAW10, 2 = RAW12, 3 = RAW14.
8. **The picture.** The phone lay on its back on a desk, so frames read the sensor's black level (about 16 of 255) even at the longest exposure. With the torch on, the mean rose to 55 and the 160 × 90 preview showed the lit desk surface, out of focus, brightest under the torch.
9. **Cached reads.** Uncached, copying one 921,600-byte frame into the app took 146 ms (about 6 MB/s), which held the viewfinder to 5 frames per second. With the window cacheable and Camera invalidating each slot's lines once CAMSV has written it, the copy takes 2 ms. Developing the picture takes 22 ms and presenting it 8 ms, so the app shows about 19 frames per second.

Two Hi-1336 settings differ from Motorola's: the frame length is 3,331 lines (30 frames per second instead of the slim-video table's 120), and exposure is set directly from Camera's auto-exposure level.

## Still open

- **Orientation and colour on a real scene.** On the handset, only the desk lit by the torch has been seen. The app turns the picture upright assuming the sensor is mounted with its top to the phone's right, and develops it as GBRG (the driver's `SENSOR_OUTPUT_FORMAT_RAW_Gb`). A picture of a real scene would confirm both.
- **The front and macro cameras**, focus (the lens actuator on I2C2 at `0x0c`), flash sequencing, lens shading and the vendor's colour tuning.
