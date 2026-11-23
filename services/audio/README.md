# Audio service

The Audio process owns sound output, music playback, the shared volume, the volume buttons and the headphone jack. Only Runtime calls it. Apps reach it through three Runtime keys:

| Key | Subscribe / Get | Execute |
| --- | --- | --- |
| `VOLUME` | `value` is the level, 0 to 15. `arg` flags: `AUDIO_HEADPHONES` (a plug is in the jack, so output goes there), `AUDIO_SPEAKER_OUT` and `AUDIO_HEADPHONE_OUT` (that path can play), `AUDIO_BUTTONS` and `AUDIO_JACK` (the buttons or the jack are being read). | `value` is a level 0 to 15, `VOLUME_UP` or `VOLUME_DOWN`. |
| `PLAYBACK` | `value` is the sound now playing, or 0. The low 16 bits of `arg` are the serial of the latest request; `PLAYBACK_OUTCOME` bits say whether it played, was stopped or found no output. | `value` is `SOUND_CLICK`, `SOUND_CHIME`, `SOUND_ALERT`, `SOUND_TONE` or `SOUND_STOP`. For a tone, `arg` holds the frequency in hertz (20 to 20,000) in its low 16 bits and the duration in milliseconds (1 to 5,000) in its high 16. Use `sdk::request_with` to pass it. |
| `MUSIC` | `value` is the track loaded (1–10), or 0. The low 4 bits of `arg` are a `MUSIC_STATE_*` (stopped, buffering, playing, paused, offline, ended) and the rest is the position in milliseconds. The text holds 16 spectrum levels, the seconds buffered ahead, the output peak and the track length; see `runtime/protocol.rs`. | `MUSIC_PLAY` with `arg` = track \| seconds << 8, `MUSIC_PAUSE`, `MUSIC_RESUME`, `MUSIC_STOP`, or `MUSIC_SEEK` with `arg` = seconds. `MUSIC_MODE` sets `MUSIC_ADVANCE`, `MUSIC_REPEAT` and `MUSIC_SHUFFLE`: with advance on, Audio starts the next track itself when one ends, so the album keeps playing after Music leaves the screen. |

Any app can subscribe or read. Only the app on screen can change the volume, start a sound or control the music, and not while the phone sleeps. A new sound replaces the one playing; music keeps playing underneath it and after its app leaves the screen. The volume buttons work on every screen: one step per press, then repeated steps after half a second. Pressing both does nothing.

The service synthesizes the built-in sounds at 48 kHz ([synth.rs](synth.rs)) and scales them by the volume, 3 dB per step. Its loop serves Runtime without blocking, reads the buttons and jack, and refills the output. Each board supplies a backend with the same functions:

- [QEMU](../../targets/qemu/audio.rs): an emulated sound card in shared memory at guest `0x49204000`, with an eight-page PCM ring. The Mac viewer plays it through SDL. **Up** and **Down** (or **+** and **-**) hold the volume buttons, and **H** plugs or unplugs headphones.
- [Moto G Pure](../../targets/moto-g-pure/audio.rs): see the handset section below.

Audio is component 22, beside USB Diagnostics (21), so existing app and service IDs keep their values. It is optional: without it, Runtime reports `OFFLINE` for both keys.

## Music

Music plays recordings from the Rose Music library, the album *Glass Orchard*, hosted at `MUSIC_HOST` (`runtime/protocol.rs`). The tracks are 48 kHz stereo MP3s at 128 kbps.

- **Fetching.** Radio fetches the file. Audio writes the track and start offset into a stream control page that the supervisor maps only into Audio and Radio. From its event loop, Radio fills a 512 KiB ring with HTTPS range requests of up to 128 KiB each. Radio builds the path itself (`/tracks/NN.mp3`) and fetches only from that host, so no app can name a URL. A library that answers 503 (asleep) is retried every 4 seconds; other failures every 1.5 seconds.
- **Decoding.** Audio decodes with [minimp3](https://github.com/lieff/minimp3) (CC0). The build fetches one pinned header and checks its hash. The decoder runs with hardware floating point; its 16 KiB scratch space lives in `.bss`, because Audio has an 8 KiB stack.
- **Buffering.** Playback starts once 48 KiB (about 3 s) is buffered, and resumes after 24 KiB if the stream runs dry. Seeking assumes a constant bit rate, which the library's files have.
- **Spectrum.** A bank of Goertzel filters measures ten bands on the signal decimated to 12 kHz and six on the full signal, and reports them with a slow fall-off.
- **Mixing.** Sounds mix on top of the music, and the volume scales both.
- **Handset output.** The AFE SRAM is 27 KiB: on the handset, words read back through offset `0x6bxx` and read zero from `0x6c00` (`19006000` probes it in 256-byte steps). The ring uses all of it, 144 ms, and Audio keeps 120 ms queued. Earlier rings of 32 and 28 KiB ran past the end, so the DMA played samples Audio never wrote on every lap: 21 ms that made music choppy, then 5 ms that clicked about seven times a second. On the single core, app drawing and Radio's TLS work compete with Audio. Radio yields while it waits on the network, so it never holds the CPU for a whole time slice. Status word 26 counts underruns, including stalls longer than the ring, which are caught by the clock. After one, Audio resumes just ahead of the DMA. Words 96 and 97 hold the longest gap between output refills in milliseconds and the count over 60 ms. Words 94 and 95 count mid-song dropouts and the silence frames written while music wanted to play. Over a minute of streaming with Music on screen, and across song changes, app launches and volume changes, no refill gap exceeded 60 ms.

`make test-music-live` (needs the internet) connects the emulator's Wi-Fi, opens Music, streams track 1 and checks that the sound card receives 48,000 frames a second, then pauses and resumes. Comparing the emulator's output with ffmpeg's decoding of the same file showed a 29 dB signal-to-noise ratio once aligned, after removing the buffering silence and the MP3 encoder delay.

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

`build/moto-package audio-status` prints the 64-word status page: capabilities, raw button and jack registers, the supervisor's clock and pin survey, AFE and codec registers, and FS1603 status. Words 58–63 show the music: track, state and peak; position in milliseconds; bytes fetched; file size; last HTTP status and fetch count; and failures, Radio's stream state and the last fetch's duration in milliseconds. `build/moto-package audio=HEX` sends a request, with the command in the high byte:

| Request | Effect |
| --- | --- |
| `010000SS` | Play sound `SS` (1 click, 2 chime, 3 alert). |
| `02` + 24 bits | Tone: frequency in the low 14 bits, duration in tens of milliseconds above. For example, `023e83e8` is 1 kHz for 2.5 s. |
| `030000LL` | Set the volume level. |
| `0400000R` | Route: 0 follows the jack, 1 forces the speaker, 2 forces headphones. |
| `05000000` | Stop. |
| `06SSSSTT` | Music: play track `TT` from second `SSSS`. For example, `06007805` plays track 5 from 2:00. |
| `07AAAACC` | Music command `CC` (2 pause, 3 resume, 4 stop, 5 seek) with argument `AAAA`. |
| `10`–`19` | Board debugging: amplifier volume, DSP word, re-init, refresh, codec readback, amplifier register write (`15RRVVVV`) and read (`160000RR`, into word 57), a deliberate stall of `17000MMM` milliseconds to test underrun recovery, and an SRAM size probe (`19000000`, results in words 100–108, output off). |

These requests bypass Runtime's rule that only the app on screen may play sound. They exist for testing on the bench. `build/moto-package open=ID` wakes the phone and opens an app, for example `open=12` for Music. Audio counts Music's status requests, one per frame, in word 98. Word 99 holds the longest gap between two requests in milliseconds, and words 100–111 the first twelve frame times after `18000NNN` clears them, skipping `NNN` frames. Display records its present timings in words 112–119: present count, panel rows copied, the last and longest present in microseconds, the rows in the last one, and total microseconds. A full-screen present takes 8.2 ms.
