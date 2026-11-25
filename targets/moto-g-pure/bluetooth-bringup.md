# Handset Bluetooth bring-up notes

How Bluetooth was brought up on the XT2163DL on November 25, 2026, and what the phone showed. The code is [bt.rs](bt.rs) (the controller and the HCI pages) and [wmt.rs](wmt.rs) (STP and WMT).

## Where Bluetooth lives

Bluetooth is part of the MT6765 connectivity subsystem that Radio already starts for Wi-Fi.

- The image already staged the Bluetooth RAM code (`soc1_0_ram_bt_1_1_hdr.bin`) in connectivity EMI. The chip's ROM loads it from there.
- There is **no separate Bluetooth patch step**. WMT function control turns the core on: `01 06 02 00 00 01` answered by `02 06 01 00 00`. MediaTek's driver (`wmt_func.c`) does the same, after a BT_PALDO switch that is a no-op on the MT6765.
- HCI then travels in **STP channel 0** on the same BTIF port as WMT (channel 4). Each H4 packet is one STP frame, type byte included, in full mode: sequence numbers, acknowledgement and CRC.
- Radio owns BTIF, so Radio carries HCI.
  - It hands H4 packets to the Open Runtime through two shared pages. `/dev/violet/hci` exposes them ([runtime/open/hci.rs](../../runtime/open/hci.rs)).
  - A third page, Radio's alone, holds the STP frame being received. Radio's own RAM had only about 1.8 KiB to spare.
- Radio turns Bluetooth on right after WMT, and before the WLAN download, whenever it starts the chip. It sets the radio with MediaTek's HAL defaults (`FC79`, `FC93`, `FC7A`, from `stBtDefault_connac_soc_1_0` via `radiomod.c`), then resets the controller and reads its address and version.
- Bluetooth goes off when Radio stops the chip. So for now **Bluetooth runs while Wi-Fi is joined** (or Phone Internet is hosted).

## What the phone showed

- **First answers.**
  - Reset, Read BD_ADDR and Read Local Version all completed.
  - Manufacturer 0x0046 (MediaTek), HCI version 0x0b, subversion 0x7036.
  - The address is the unprogrammed `00:00:46:76:63:01`, so apps advertise from a random static address.
  - No STP frame was lost or rejected, and Wi-Fi kept working beside it.
- **Nearly deaf and mute at first.**
  - Advertising and scanning commands all succeeded, but nothing heard the phone.
  - The phone heard an iPhone running bitchat a metre away at **-96 dBm**, while a Mac next to it heard the same iPhone at -46.
  - Radio's WMT sequence had been enough for Wi-Fi but not for Bluetooth.
- **The fix: MediaTek's own WMT sequence.**
  - This is `mtk_wcn_soc_sw_init` in `wmt_ic_soc.c`, from realme's MT6765 kernel release of MediaTek's connectivity driver, with the board's `WMT_SOC_ELLIS_EPA.cfg`.
  - Radio had sent the antenna mode with sub-command 1 (`01 10 02 00 01 0D`). The driver uses sub-command **6** (`01 10 02 00 06 0D`); 1 is a different setting.
  - Now, after the chip ID, Radio sends:

  | Step | Bytes |
  | --- | --- |
  | A-die query | `01 02 01 00 12` |
  | Bluetooth TSSI from Wi-Fi, `bt_tssi_from_wifi=3`, `bt_tssi_target=3776` | `01 02 04 00 10 03 C0 0E` |
  | Antenna mode 13 | `01 10 02 00 06 0D` |
  | Wi-Fi path 0 | `01 10 03 00 1A 00 00` |
  | eLNA gains | `01 10 12 00 1B 01 …` |
  | ePA mode 0 | `01 10 02 00 1D 00` |
  | Calibration | `01 14 01 00 01` |
  | Coexistence adjustments, all zero for this board | `01 10 04 00 1E/1F/20 00 00 00` |
  | Wi-Fi function on | `01 06 02 00 03 01` |

  - After that, the phone heard the iPhone at **-40 dBm**, and the Mac heard the phone at **-42 dBm**.
  - Wi-Fi still joined and fetched over HTTPS.
  - Units on the other RF profile (1, `WMT_SOC_ELLIS.cfg`) keep the old coexistence command, apart from the new A-die and TSSI steps. MediaTek sends no coexistence table for that profile at all. None has been tried.
- **bitchat.**
  - With the app ([apps/open/bitchat](../../apps/open/bitchat/README.md)) running, bitchat for iOS on an iPhone connected on its own.
  - It subscribed and announced itself, and the two phones exchanged signed public messages both ways.
  - The Mac test peer ([tools/bitchat-peer](../../tools/bitchat-peer/main.swift)) checked the phone's announce with CryptoKit and found 80–120 ms of clock skew. The iPhone negotiated a 517-byte ATT MTU.

## Sources

- MediaTek's connectivity driver as released for the MT6765: `modules/connectivity/common/common_main/` in `realme-kernel-opensource/realmeC12_realmeC15_AndroidR-kernel-source`. The relevant files are `core/wmt_ic_soc.c`, `core/wmt_func.c` and `platform/mt6765.c`.
- MediaTek's Bluetooth HAL: `hardware/connectivity/bluetooth/driver/mt66xx/bluedroid/radiomod.c` in `StyxProject/vendor_mediatek_opensource`, with `CFG_BT_Default.h` for the defaults.
