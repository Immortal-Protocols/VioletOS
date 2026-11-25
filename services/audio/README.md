# Audio service

The Audio process owns sound output, the shared volume, the volume buttons, the headphone jack and the microphone. Only Runtime calls it. Apps reach it through three Runtime keys:

| Key | Subscribe / Get | Execute |
| --- | --- | --- |
| `VOLUME` | `value` is the level, 0 to 15. `arg` flags: `AUDIO_HEADPHONES` (a plug is in the jack, so output goes there), `AUDIO_SPEAKER_OUT` and `AUDIO_HEADPHONE_OUT` (that path can play), `AUDIO_BUTTONS` and `AUDIO_JACK` (the buttons or the jack are being read). | `value` is a level 0 to 15, `VOLUME_UP` or `VOLUME_DOWN`. |
| `PLAYBACK` | `value` is the sound now playing, or 0. The low 16 bits of `arg` are the serial of the latest request; `PLAYBACK_OUTCOME` bits say whether it played, was stopped or found no output. | `value` is `SOUND_CLICK`, `SOUND_CHIME`, `SOUND_ALERT`, `SOUND_TONE`, `SOUND_RECORDING` or `SOUND_STOP`. For a tone, `arg` holds the frequency in hertz (20 to 20,000) in its low 16 bits and the duration in milliseconds (1 to 5,000) in its high 16. Use `sdk::request_with` to pass it. `SOUND_RECORDING` returns `EMPTY` if nothing has been recorded. |
| `MICROPHONE` | `value` is the input level while the microphone is on: 0 is −60 dBFS or quieter, 100 is full scale. The low byte of `arg` is the mode, `MIC_HAS_RECORDING` says a recording exists, and the high 16 bits are its length in tens of milliseconds. `VOLUME` reports `AUDIO_MIC` when the board can record. | `value` is `MIC_OFF`, `MIC_LISTEN` (meter only) or `MIC_RECORD`. A recording replaces the previous one and stops by itself after 4 seconds. |

Any app can subscribe or read. Only the app on screen can change the volume, start a sound or turn on the microphone, and not while the phone sleeps. Runtime turns the microphone off when that app leaves the screen or the phone sleeps. A new sound replaces the one playing. The volume buttons work on every screen: one step per press, then repeated steps after half a second. Pressing both does nothing.

The service synthesizes the built-in sounds at 48 kHz ([synth.rs](synth.rs)) and scales them by the volume, 3 dB per step. Microphone input arrives at 16 kHz mono on every board. [mic.rs](mic.rs) removes its DC offset and meters it. A recording is kept in 128 KiB of private RAM that the supervisor maps for Audio. It is played back at 48 kHz, normalized to the level of the built-in sounds with at most 24 dB of make-up gain. Its loop serves Runtime without blocking, reads the buttons and jack, and refills the output. Each board supplies a backend with the same functions:

- [QEMU](../../targets/qemu/audio.rs): an emulated sound card in shared memory at guest `0x49204000`, with an eight-page PCM ring and a two-page microphone ring. The Mac viewer plays output through SDL and, only while the guest is recording, captures the Mac's microphone (macOS asks for permission the first time). Without a microphone or permission, the guest hears silence. **Up** and **Down** (or **+** and **-**) hold the volume buttons, and **H** plugs or unplugs headphones.
- [Moto G Pure](../../targets/moto-g-pure/audio.rs): see the handset section below.

Audio is component 22, beside USB Diagnostics (21), so existing app and service IDs keep their values. It is optional: without it, Runtime reports `OFFLINE` for both keys.

## Try it

Run `make emulator`, open **Apps → Next → Next → Open** for **Sounds**, and tap **CHIME**. Press the arrow keys to change the volume and **H** to switch to headphones. Tap **RECORD**, speak, then tap **FINISH** (or wait 4 seconds) and **PLAY**. Home shows the level and output in its top-right corner.

`make test-audio` checks the buttons (single steps, repeats, both together), jack routing during playback, sound lengths, a 440 Hz tone's pitch and level, Stop, and the Sounds volume bar. It also feeds a 1 kHz tone into the microphone, then checks the meter, the 4-second recording, the recording's pitch and level on playback, and that leaving the app turns the microphone off. `services/audio/mic.rs` has host unit tests (`rustc --edition=2021 --test services/audio/mic.rs`).

## Handset

On the Moto G Pure, Audio reports all five capabilities: speaker, headphones, buttons, jack and microphone. The [backend](../../targets/moto-g-pure/audio.rs) follows Motorola's ellis kernel. Audio has no direct access to the PMIC, clocks or pins:

- **Power** performs PMIC accesses for it. Reads and writes are limited to the MT6357 audio block (`0x2080`–`0x233e`) and the named audio bits: TOPSTATUS (read only), the 26 MHz audio clock buffer, and the audio pad modes, directions and Schmitt triggers (MOSI for output, MISO for the microphone).
- **The supervisor** ungates the audio and I2C clocks and sets the audio pin modes, including the microphone link (GPIO140/142/143).
- **Mapped into Audio:** the keypad (read only), I2C0, the AFE and its SRAM.

| Part | Path | Checked on the handset (November 22, 2026) |
| --- | --- | --- |
| Volume Up | MT6357 TOPSTATUS `0x0024` bit 3 (0 = pressed) | Idle reads released. **Pressing it does not change the volume yet**, so this bit is probably not the right input. |
| Volume Down | Keypad `KP_MEM1` bit 1 (0 = pressed) | Works. |
| Jack | MT6357 ACCDET with PMIC EINT0, polled | Works: plugging in moves output to headphones. |
| Headphones | AFE DL1 in SRAM → ADDA → MT6357 DAC and HP amplifier, at −2 dB analog | DMA runs at 48 kHz. The codec completes the vendor power-up and power-down. Plays. |
| Speaker | AFE DL1 → I2S3 → FourSemi FS1603S on I2C0 `0x34` | The amp locks to the bit clock, and its boost, amplifier and DAC run with no fault flags. Plays. |
| Microphone | Main mic (AIN0, MICBIAS0 1.9 V) → MT6357 ADC1, 18 dB PGA → MTKAIF → AFE ADDA UL → VUL in AFE SRAM, 16 kHz mono | Captures 16,000 samples per second without overruns. A 1 kHz speaker tone reads back at 1000 Hz, about 25 dB over the room. A 4-second recording plays back at the right pitch. See the bring-up notes. |

The speaker runs without its vendor preset (`fs1603s.fsm`), which only root can read on Android. So the amp uses its hardcoded register init, its DSP is bypassed, and it has no speaker-protection tuning. Its gain is held at about −12 dB below full scale; the service's volume applies on top.

### Developer requests over USB

`build/moto-package audio-status` prints the 96-word status page: capabilities, raw button and jack registers, the supervisor's clock and pin survey, AFE and codec registers, FS1603 status, and from word 64 the microphone: samples read, overruns, peak, mean level, DC and zero crossings per 100 ms, and capture registers. `build/moto-package audio=HEX` sends a request, with the command in the high byte:

| Request | Effect |
| --- | --- |
| `010000SS` | Play sound `SS` (1 click, 2 chime, 3 alert). |
| `02` + 24 bits | Tone: frequency in the low 14 bits, duration in tens of milliseconds above. For example, `023e83e8` is 1 kHz for 2.5 s. |
| `030000LL` | Set the volume level. |
| `0400000R` | Route: 0 follows the jack, 1 forces the speaker, 2 forces headphones. |
| `05000000` | Stop. |
| `0600000M` | Microphone mode: 0 off, 1 listen, 2 record. |
| `07000000` | Play the recording. |
| `10`–`17` | Board debugging: amplifier volume, DSP word, re-init, refresh, codec readback, amplifier register write (`15RRVVVV`) and read (`160000RR`, into word 57), and the microphone PGA gain (`170000GG`, 0 to 5 in 6 dB steps). |

These requests bypass Runtime's rule that only the app on screen may play sound. They exist for testing on the bench.

**Microphone stream.** While an open app has `/dev/violet/mic` open, the Open Runtime sets word 16 of the PCM header page and Audio appends every settled 16 kHz sample to an 8-page ring after the PCM ring, counting them in word 17; the Open Runtime reads from there.
