# Power service

Power owns the battery reading, the power key, sleep and power off. On the handset it also owns the PMIC, and performs the few PMIC accesses [Audio](../audio/README.md), Camera and Radio need.

| Request (from Runtime) | Answer |
| --- | --- |
| `GET POWER` | The battery: a percentage in QEMU, millivolts on the handset (`arg` has `POWER_MILLIVOLTS`). |
| `GET POWER_KEY` | Bit 0 of `value` is set while the power key is held. In QEMU `arg` counts presses, so a press shorter than a poll is still seen. |
| `EXECUTE POWER 1` / `0` | Sleep and wake. Runtime has Display blank the screen first and reports no app in front until wake. |
| `EXECUTE POWER 2` | Power off. |

The [Open Runtime](../../runtime/open/README.md) polls the key every 40 ms and decides what a press means ([power.rs](../../runtime/open/power.rs), [lock.rs](../../runtime/open/lock.rs)):

- **A short press** turns the screen off; the next one turns it on. Touches do not reach a screen that is off. With a passcode set in Settings › Passcode, turning the screen off also locks the phone, and the phone starts locked.
- **Holding the key** (0.8 s) brings Home's power sheet on any screen, the lock screen included: **Power off** or **Cancel**. Settings › Power has **Power off** too.

Before power off, the Open Runtime writes every changed `/data` file back to Storage.

## QEMU

The [fixture](../../targets/qemu/power.rs) is one page shared with the Mac: the battery level, the command word, the key's level and its press count. In `make emulator`, **P** is the power button. While the screen is off, [run.py](../../tools/run.py) pauses the guest and resumes it on the next press of **P**; power off ends the emulator.

`make test-lock` covers all of it across two boots: the key turns the screen off and on, Settings sets a passcode, the key locks, a wrong passcode and an app's icon are refused, the right passcode unlocks, the phone starts locked after a restart, and the power sheet powers the machine off.

## Handset

The [backend](../../targets/moto-g-pure/power.rs) reads the key from the MT6357's TOPSTATUS register (`0x0024` bit 1, `PWRKEY_DEB`, 0 while pressed, as in Linux's `mtk-pmic-keys`) and powers off by releasing the PMIC's power-hold bit.

On November 27, 2026 the owner checked all of it on the XT2163DL: a short press turned the screen off and on, the passcode locked the phone, and power off worked. The screen going "off" is a black frame with touch ignored: the backlight stays lit, since nothing drives the panel's power or backlight yet.
