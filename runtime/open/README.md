# Open Runtime

The Open Runtime runs ordinary Linux ARM programs on VioletOS, with no Linux kernel anywhere. A static binary built for `armv7-unknown-linux-musleabihf`, a Go program built with `GOOS=linux GOARCH=arm GOARM=7`, or a Python script with a `#!/bin/python3` line goes into `/apps` and runs as it is. It implements the [open app model RFC](../../docs/open-app-model-rfc.md).

## Try it

Run `make emulator` and tap **/APPS** on Home. The list shows every program in `/apps`; tap one to run it. Programs that print go to the Open Runtime's log. In QEMU the log is a page of guest memory at `0x49220000`; on the handset, `build/moto-package open-log` prints it, and `build/moto-package open-run=/apps/NAME` starts a program over USB.

`make test-open` starts each test program and checks its output, the frames drawn through `/dev/violet/screen`, taps, the PCM it plays and the trip to Home and back.

| Program | Language | Shows |
| --- | --- | --- |
| [hello](../../apps/open/hello/main.rs) | Rust `std` | threads, mutex and condition variable, files, directories, sleep, environment, an 8 MiB allocation |
| [canvas](../../apps/open/canvas/main.rs) | Rust `std` | `ioctl` and `mmap` of the screen, presenting rows, touch events, going Home through `/dev/violet/sys` |
| [chello](../../apps/open/chello/main.c) | C (musl) | `pthreads`, `libm`, `nanosleep`, stdio files, a 16 MiB `malloc` |
| [gohello](../../apps/open/gohello/main.go) | Go 1.27 | goroutines, channels, the garbage collector, timers and tickers, files |
| [pyhello](../../apps/open/pyhello/main.py) | Python 3.12 | threads, `json`, `re`, `zlib`, files, `time`, `datetime` |
| [pypaint](../../apps/open/pypaint/main.py) | Python 3.12 | drawing through an `mmap` of the screen, touch events |
| [pytone](../../apps/open/pytone/main.py) | Python 3.12 | synthesizing PCM and streaming it to `/dev/violet/audio` |
| [pynet](../../apps/open/pynet/main.py) | Python 3.12 | `urllib` over TCP: name lookup, connect, HTTP |
| [gonet](../../apps/open/gonet/main.go) | Go 1.27 | `net/http` over TCP and HTTPS with Go's TLS: Go's own resolver, non-blocking connect, `epoll` |

On the Moto G Pure (November 23, 2026), hello, chello, gohello, pyhello and pytone ran over USB with the same output as in QEMU, and Audio reported pytone's stream playing. Joined to a WPA2 network, pynet and gonet fetched the real example.com over HTTP (HTTP 200, "Example Domain") through Radio. Go's HTTPS fetch was checked in QEMU against the real site; the handset image that ran it predated the certificate bundle fix. The screen programs were checked only in QEMU: on the handset an app reaches the screen when you tap **/APPS** and its row.

## Building programs for /apps

[tools/build_initfs.py](../../tools/build_initfs.py) builds the image from `apps/open`. Each folder becomes `/apps/NAME`:

- `main.rs`: built as a static Rust `std` program. Needs `rustup target add armv7-unknown-linux-musleabihf`.
- `main.go`: built with Go for `linux/arm`, `GOARM=7`, `CGO_ENABLED=0`.
- `main.c`: built against musl with the NDK's clang. Build the musl sysroot once with `python3 tools/build_python.py musl`.
- Anything else is copied as it is. A file starting with `#!` runs under the named interpreter; a `.py` file without one runs under `/bin/python3`; a folder runs its `main` or `main.py`.

`python3 tools/build_python.py python` cross-builds CPython 3.12 statically against musl (with zlib) and the image picks it up as `/bin/python3` and `/lib/python3.12`. It needs Homebrew's `python@3.12` on the Mac. Downloads are pinned by SHA-256.

QEMU loads the image uncompressed at `0x4a000000`. The handset carries it gzip-compressed as its boot image's ramdisk, where the stock Android ramdisk used to be; the loader copies it to `0xc0000000` and the Open Runtime unpacks it at start (about 3 seconds for 20 MB on the phone).

## How it works

```
 app thread ── svc #0 (Linux syscall in r7) ──► seL4: unknown syscall ──► fault endpoint
                                                                             │
 Open Runtime (component 24) ◄──────────────────────────────────────────────┘
   syscall.rs: Linux ARM EABI calls       device.rs: /dev/violet/{screen,events,sys,audio}
   procmgr.rs: address spaces, ELF        fs.rs: /apps image + writable in-memory layer
   pool.rs: physical pages for apps       Runtime client 9: focus, touch, Home
```

- **Processes.** The supervisor gives the Open Runtime its own CSpace (16K slots), page directory, the ASID pool, 4 MiB of kernel memory and a pool of physical memory (384 MiB in QEMU, 256 MiB on the handset). The Open Runtime makes each app's page directory, CNode and threads itself and is their fault handler, so an app's crash ends that app and nothing else.
- **System calls.** seL4's system calls use negative numbers in `r7`; Linux's are positive, so a Linux `svc #0` reaches the Open Runtime as an unknown-syscall fault carrying `r0`–`r7`. The reply sets `r0` and moves the restart address past the `svc`. Calls that must wait (futexes, `read` of events, `poll`, `epoll_wait`, `nanosleep`, a full audio ring) keep the thread's reply capability and answer later. About 150 calls are handled; [syscall.rs](syscall.rs) lists them.
- **Memory.** Pages are 64 KiB, mapped on first touch. Regions still start on any 4 KiB boundary, as Linux allows. `mmap`, `munmap`, `mprotect` and `brk` maintain the regions; every pool page stays mapped in the Open Runtime as well, so reading app memory is a plain load.
- **Threads.** `clone` makes an seL4 thread in the same address space. The thread pointer is `TPIDRURO`, as on Linux; the kernel is built with `KernelArmTLSReg=tpidruro` and the Open Runtime sets it with `seL4_TCB_SetTLSBase`. seL4 clears the Thumb bit whenever registers are written, so new threads start at a one-instruction ARM trampoline (`ldr pc, [pc, #-4]`) that jumps to the Thumb or ARM target.
- **Screen and touch.** The Open Runtime is Runtime's client 9. Home's **/APPS** button gives it focus; it then draws the app list or the app in front directly to the framebuffer, and receives every touch change (down, move, up). An app's surface is two 720 × 1600 BGRA8888 buffers that the app maps with `mmap`; `write` of a `PresentRect` copies rows to the screen. Holding the top edge for a second, or writing `home` to `/dev/violet/sys`, leaves the app running in the background.
- **Sound.** `/dev/violet/audio` takes 48 kHz s16 PCM (stereo, or mono with `AUDIO_IOC_CONFIG`). It fills a 64 KiB ring shared with the [Audio service](../../services/audio/README.md), which plays it at the shared volume through the speaker or headphones. Built-in sounds take precedence while they play.
- **Network.** TCP sockets use eight slots in pages shared with Radio, which runs them on its lwIP stack from its event loop ([sockets.c](../../targets/moto-g-pure/ip/sockets.c)): directly on a joined network, or through the phone's SOCKS5 relay in Phone Internet mode. DNS queries (UDP to port 53) never leave the phone: the Open Runtime answers each name with an address in `198.18.0.0/15`, and a connect to one hands Radio the name, which it resolves (or the relay does). `/etc/resolv.conf`, `/etc/hosts` and the build machine's root certificates are in the image. In QEMU, [tools/net_relay.py](../../tools/net_relay.py) serves the same pages with the Mac's sockets; `VIOLET_NET_HOSTS` can point names elsewhere for tests.
- **Files.** `/apps`, `/bin` and `/lib` come from the image. Writes go to an in-memory layer over it (`/tmp`, `/data` or anywhere else) and are lost at restart.
- **Policy.** Every `open` passes through [policy.rs](policy.rs); the default `OpenPolicy` allows everything.

## Not done yet

- Listening sockets, general UDP and IPv6. TCP clients work; `listen`/`accept` return `EOPNOTSUPP`, and UDP serves only DNS.
- Persistent files. `/data` lives in memory; Storage still offers only its small per-app files.
- Signals are accepted but never delivered, so Go runs with `GODEBUG=asyncpreemptoff=1` and a fault ends the app.
- `fork` and `execve`. Programs can start threads but not other processes; `/dev/violet/sys` can `launch` another app.
- Dynamic linking. Programs must be static (static PIE works).
- One CPU, and up to three apps at once.
