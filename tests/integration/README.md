# Host integration tests

`make test-integration` (also the first step of `make test-unit`, so CI runs it). It builds the test binary twice in parallel, with the 4 KiB pages `runtime/open/sys.rs` ships and with 64 KiB pages (`--cfg page64`, the other configuration `procmgr.rs` supports), and runs each (209 and 207 tests, in parallel): about 6 seconds on an Apple Silicon Mac including both compiles, 1.2 seconds per run.

These tests replace the QEMU suites that waves 1 and 2 of the security work added. [docs/test-map.md](../../docs/test-map.md) says where each of their assertions went.

## What they are

The **shipped sources**, pulled in by `#[path]` and compiled unchanged for this machine, joined by **in-memory fakes of the kernel and hardware edges**. When `policy.rs` or `lifecycle.rs` changes, these tests run the new code. The exception is the stand-ins below: where a shipped file cannot be built for a host, its glue is copied, and a test of what that glue does checks the copy (docs/test-map.md marks those rows **stand-in**).

| Shipped, unchanged | From |
| --- | --- |
| `policy`, `manifest`, `fs`, `shield`, `scorer`, `sysui`, `lifecycle`, `signal`, `vma`, `elf`, `entropy`, `loopback`, `sockaddr`, `slots`, `names`, `sntp`, `inflate`, `hcimux`, `frames`, `powerctl`, `abi` | [runtime/open](../../runtime/open) (list: [open/mod.rs](open/mod.rs)) |
| `procmgr` (address spaces, the ELF loader, copies to and from app memory) | [system/supervisor/procmgr.rs](../../system/supervisor/procmgr.rs) |
| `protocol` (the IPC message) | [runtime/protocol.rs](../../runtime/protocol.rs) |
| Power's wake locks and governors | [services/power/policy.rs](../../services/power/policy.rs) |
| The boot slot writer, its SHA-256, the slot metadata and the bootloader's choice | [services/storage/slotwrite.rs](../../services/storage/slotwrite.rs), [targets/moto-g-pure/slot.rs](../../targets/moto-g-pure/slot.rs) |
| The update manifest check, with BearSSL's real P-256 verification | [system/package-manager/ota.rs](../../system/package-manager/ota.rs), [verify_crypto.c](../../system/package-manager/verify_crypto.c) |
| The supervisor's own entry to it, `ota_verify` (built as the image builds it, not as a test, with `VIOLET_SYSTEM_VERSION=4` as its floor) | [system/package-manager/verify.rs](../../system/package-manager/verify.rs) |
| What a restart keeps of `/data` (`writeback::key`: everything under `/data`, nested folders included, but not `*.ota-staging`) | [services/storage/writeback.rs](../../services/storage/writeback.rs) |
| Home's, Settings' and the Agent's manifests | `apps/open/*/manifest.txt` |

Compiling those modules with `--test` also runs the unit tests inside them, which is why `make test-unit` no longer builds `inflate.rs`, `slots.rs`, `names.rs`, `loopback.rs`, `sockaddr.rs`, `sntp.rs` and `lifecycle.rs` one by one.

| Fake ([kernel/mod.rs](kernel/mod.rs)) | Stands in for |
| --- | --- |
| `clock` | The timer: `sys::now_ns`, `sys::pause`, `ticks`. Time moves only when a test says so. |
| `ipc` | seL4 endpoints (`ipc_call`, the symbol `runtime/protocol.rs` declares) and notification badges. A service is a closure behind a capability slot. |
| `sys`, `pool` | The frame and page-table calls and the page pool `procmgr.rs` maps app memory with. The fake MMU records every mapping and panics on one that is writable and executable. |
| `Disk` | A block device that can lose power (Storage's own `tests/testdisk.rs`). |
| `Screen`, `Touch`, `Link` | The 720 × 1600 framebuffer, the touch controller's report queue, the network link with a scripted peer. |
| `serial` | The log page: every `log!` line the shipped code writes. |

Every fake is a `thread_local!`. The test runner gives each test its own thread, so each test has its own phone and they all run in parallel. Nothing is a file or a port. Two shipped pieces keep state of their own: `procmgr.rs`'s frame ledger is per thread on the host and `World::boot_with` empties it, so a new boot starts with every page free; `powerctl.rs` keeps a process-wide static of wake-lock holders, so only one scenario may use it (below).

A restart (`w.reboot()`, `w.shutdown()`) is a clean shutdown: it keeps what the write-back sends (`writeback::key`). A power cut, and whether a file was committed by `fsync`, is not modelled here; Storage's `tests/writeback_tests.rs` covers it.

## What they are not

Ten files of the Open Runtime are **not** built here: `mod.rs`, `syscall.rs`, `device.rs`, `sockets.rs`, `net.rs`, `trusted_ui.rs`, `data.rs`, `appstore.rs`, `update.rs` and `guard.rs`. They reach memory at fixed 32-bit addresses (the pool at `0x2000_0000`, the framebuffer, the shared pages) or seL4 directly, and a 64-bit host cannot map those addresses (macOS reserves everything below 4 GiB).

Stand-ins take their place: the same decisions called in the same order, each function naming the one it follows. Some of them are copies of shipped glue, not just calls (guard.rs `subject`, `gate_open`, `fd_write_error`; syscall.rs `fault_signal`'s classification; trusted_ui.rs `caller`, `ui_touch`, `LOCK_FILE` and its drawing; device.rs's band-row touch filter in `World::finger`), so a change to the shipped copy is not seen here. docs/test-map.md lists each property that rests on one.

- [open/mod.rs](open/mod.rs): `Runtime`, with the fields the shipped `impl Runtime` blocks use. `manifest.rs` is built unchanged against it.
- [open/front.rs](open/front.rs): the system call dispatch. A scripted app issues real Linux ARM call numbers with pointers into its own memory (a real `procmgr::Space`).
- [open/trusted_ui.rs](open/trusted_ui.rs) and [open/life.rs](open/life.rs): one `sysui::Ui` per phone joined to the screen, touch and `/data/.lock`; `lifecycle.rs` applied to running apps.

So a host test proves the decisions and how they combine. It cannot prove that `syscall.rs` calls them. That is what the one QEMU test is for: [targets/qemu/test_system.py](../../targets/qemu/test_system.py) (`make test-system`) runs the real dispatch, real page tables, real signal delivery and one real slot switch across a reboot.

When a file leaves that list (its fixed addresses go behind a function), add it to `shipped!` in `open/mod.rs` and delete its stand-in.

## Add a test

Put it in the file for its area under [scenarios/](scenarios/) (or add a file and name it in `scenarios/mod.rs`). A test builds a phone, acts and asserts:

```rust
use crate::world::*;

#[test]
fn an_app_cannot_read_settings_networks() {
    // An image with Home, Settings and the Agent (their shipped manifests) and one more app.
    let mut w = World::boot(Image::new().app("notes", ""));
    let notes = w.open_app("notes");                        // as a tap on its Home icon
    assert_eq!(w.app(notes).read_file("/data/networks"), Err(ENOENT));
    assert!(w.app(notes).sys("wifi join 0 00").starts_with("error not allowed"));
    assert_eq!(w.logged("policy: notes"), 1);               // the shipped code's own log line
}
```

| Call | Does |
| --- | --- |
| `Image::new().app(name, manifest).file(path, bytes)` | The boot image. `Image::bare()` has no Home. |
| `World::boot(image)`, `w.reboot()`, `w.shutdown()` + `World::boot_with(image, kept, seed)` | Power on; restart on the same `/data`; a boot with a chosen seed for the random generator. |
| `w.open_app(name)`, `w.start(path, caller)`, `w.fork(parent)`, `w.front(Some(i))`, `w.install(name, manifest)`, `w.uninstall(name)` | Run apps: from Home, by another app, a child process, an Agent-installed app. |
| `w.app(i).open / read_file / write_file / stat / unlink / rename / mkdir / listdir / socket / mmap / mprotect / munmap / brk / sigaction / kill` | Linux system calls by app `i`. `Ok(value)` or `Err(errno)`. |
| `w.app(i).syscall(number, [r0..r5])` | A raw call. `w.app(i).string(offset, "text")` puts an argument in the app's memory. |
| `w.app(i).sys("consent mic")` | A `/dev/violet/sys` command and its answer. |
| `w.app(i).touch(address, Access::Write)` | The app reads, writes or runs memory: `Ok`, a signal to its handler, or the status it was ended with. |
| `w.advance_secs(n)`, `w.advance_ms(n)` | Time passes; the runtime's loop runs throughout. |
| `w.tap(x, y)`, `w.finger(x, y, down)`, `w.enter("1357")`, `w.answer_prompt(true)` | The touch controller. `tap` returns the app that got the touch. |
| `w.present(i, colour)`, `w.screen.pixel(x, y)` | What the app draws and what the panel shows. |
| `w.log()`, `w.logged("text")`, `w.mark()` + `w.since(mark)` | The log. |
| `w.rt` | The runtime itself, for anything else (`w.rt.shield`, `w.rt.fs`, `w.rt.pool`, ...). |
| `serve_through_runtime(handler)` | Put a service's request handler behind Runtime's tickets, as an open app reaches one. |
| `kernel::ipc::serve(slot, handler)`, `kernel::Link`, `kernel::Disk` | The fakes directly, for a component that has no `World` (see `scenarios/ota.rs` and `scenarios/time.rs`; no scenario uses `Disk` yet). |

Rules for a new test:

- Keep it parallel: no files outside what `run.sh` prepares, no ports, no statics. One shipped module, `powerctl.rs`, keeps a static of its own; only `scenarios/power.rs` may use it, in one test.
- Assert on what the shipped code decided or logged, not on a log line a stand-in wrote.
- To join a new component, give its hardware or kernel access a narrow function or trait, add the source to `shipped!` (or a `#[path]` in `main.rs`) and fake only that edge.
- If the property needs the real kernel, page tables or the boot path, it belongs in `targets/qemu/test_system.py`, within its two boots.

## Run one test

```sh
tests/integration/run.sh trusted_ui            # a filter on the test name
tests/integration/run.sh --nocapture           # any option of the test binary
```

`run.sh` needs BearSSL's sources (`make setup` or any image build fetches them), `cc`, `python3` and `openssl`, the same as `system/package-manager/test_ota.sh`.
