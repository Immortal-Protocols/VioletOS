# Audio service

The Audio process owns sound output, the shared volume, the volume buttons and the headphone jack. Only Runtime calls it. Apps reach it through two Runtime keys:

| Key | Subscribe / Get | Execute |
| --- | --- | --- |
| `VOLUME` | `value` is the level, 0 to 15. `arg` flags: `AUDIO_HEADPHONES` (a plug is in the jack, so output goes there), `AUDIO_SPEAKER_OUT` and `AUDIO_HEADPHONE_OUT` (that path can play), `AUDIO_BUTTONS` and `AUDIO_JACK` (the buttons or the jack are being read). | `value` is a level 0 to 15, `VOLUME_UP` or `VOLUME_DOWN`. |
| `PLAYBACK` | `value` is the sound now playing, or 0. The low 16 bits of `arg` are the serial of the latest request; `PLAYBACK_OUTCOME` bits say whether it played, was stopped or found no output. | `value` is `SOUND_CLICK`, `SOUND_CHIME`, `SOUND_ALERT`, `SOUND_TONE` or `SOUND_STOP`. For a tone, `arg` holds the frequency in hertz (20 to 20,000) in its low 16 bits and the duration in milliseconds (1 to 5,000) in its high 16. Use `sdk::request_with` to pass it. |

Any app can subscribe or read. Only the app on screen can change the volume or start a sound, and not while the phone sleeps. A new sound replaces the one playing. The volume buttons work on every screen: one step per press, then repeated steps after half a second. Pressing both does nothing.

The service synthesizes the built-in sounds at 48 kHz ([synth.rs](synth.rs)) and scales them by the volume, 3 dB per step. Its loop serves Runtime without blocking, reads the buttons and jack, and refills the output. Each board supplies a backend with the same functions:

- [QEMU](../../targets/qemu/audio.rs): an emulated sound card in shared memory at guest `0x49204000`, with an eight-page PCM ring. The Mac viewer plays it through SDL. **Up** and **Down** (or **+** and **-**) hold the volume buttons, and **H** plugs or unplugs headphones.
- [Moto G Pure](../../targets/moto-g-pure/audio.rs): see the handset section below.

Audio is component 22, beside USB Diagnostics (21), so existing app and service IDs keep their values. It is optional: without it, Runtime reports `OFFLINE` for both keys.

## Try it

Run `make emulator`, open **Apps → Next → Next → Open** for **Sounds**, and tap **CHIME**. Press the arrow keys to change the volume and **H** to switch to headphones. Home shows the level and output in its top-right corner.

`make test-audio` checks the buttons (single steps, repeats, both together), jack routing during playback, sound lengths, a 440 Hz tone's pitch and level, Stop, and the Sounds volume bar.

## Handset

On the Moto G Pure, Audio reports all four capabilities: speaker, headphones, buttons and jack. The [backend](../../targets/moto-g-pure/audio.rs) follows Motorola's ellis kernel. Audio has no direct access to the PMIC, clocks or pins:

- **Power** performs PMIC accesses for it. Reads and writes are limited to the MT6357 audio block (`0x2080`–`0x233e`) and the named audio bits: TOPSTATUS (read only), the 26 MHz audio clock buffer, and the audio pad modes and their Schmitt triggers.
- **The supervisor** ungates the audio and I2C clocks and sets the audio pin modes.
- **Mapped into Audio:** the keypad (read only), I2C0, the AFE and its SRAM.

| Part | Path | Checked on the handset (November 22, 2026) |
| --- | --- | --- |
| Volume Up | MT6357 TOPSTATUS `0x0024` bit 3 (0 = pressed) | Idle reads released. **Pressing it does not change the volume yet**, so this bit is probably not the right input. |
| Volume Down | Keypad `KP_MEM1` bit 1 (0 = pressed) | Works. |
| Jack | MT6357 ACCDET with PMIC EINT0, polled | Works: plugging in moves output to headphones. |
| Headphones | AFE DL1 in SRAM → ADDA → MT6357 DAC and HP amplifier, at −2 dB analog | DMA runs at 48 kHz. The codec completes the vendor power-up and power-down. Plays. |
| Speaker | AFE DL1 → I2S3 → FourSemi FS1603S on I2C0 `0x34` | The amp locks to the bit clock, and its boost, amplifier and DAC run with no fault flags. Plays. |

The speaker runs without its vendor preset (`fs1603s.fsm`), which only root can read on Android. So the amp uses its hardcoded register init, its DSP is bypassed, and it has no speaker-protection tuning. Its gain is held at about −12 dB below full scale; the service's volume applies on top.

### Developer requests over USB

`build/moto-package audio-status` prints the 64-word status page: capabilities, raw button and jack registers, the supervisor's clock and pin survey, AFE and codec registers, and FS1603 status. `build/moto-package audio=HEX` sends a request, with the command in the high byte:

| Request | Effect |
| --- | --- |
| `010000SS` | Play sound `SS` (1 click, 2 chime, 3 alert). |
| `02` + 24 bits | Tone: frequency in the low 14 bits, duration in tens of milliseconds above. For example, `023e83e8` is 1 kHz for 2.5 s. |
| `030000LL` | Set the volume level. |
| `0400000R` | Route: 0 follows the jack, 1 forces the speaker, 2 forces headphones. |
| `05000000` | Stop. |
| `10`–`16` | Board debugging: amplifier volume, DSP word, re-init, refresh, codec readback, amplifier register write (`15RRVVVV`) and read (`160000RR`, into word 57). |

These requests bypass Runtime's rule that only the app on screen may play sound. They exist for testing on the bench.
