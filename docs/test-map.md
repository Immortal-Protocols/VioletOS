# Where the QEMU assertions went

Waves 1 and 2 of the security work added eleven QEMU suites. Each built its own image and booted it, one after another: about 15 minutes for the seven slowest alone (887 s measured). They are gone. This page maps every assertion they made to where it is checked now, so nothing was dropped without a line here.

- **Host**: a test in [tests/integration](../tests/integration/README.md) (`make test-integration`, seconds, parallel, part of `make test-unit` and so of CI). It runs the shipped sources against in-memory fakes of the kernel and hardware.
- **QEMU**: [targets/qemu/test_system.py](../targets/qemu/test_system.py) (`make test-system`): one image, two boots.
- **Stand-in only**: a host test, but the property rests on code in `tests/integration/open/` or `tests/integration/world.rs` that copies shipped code which cannot be built for a host. The test checks the copy; the shipped code is not checked by the loop for this property unless a **QEMU** entry is in the same row.
- **Dropped**: no longer checked by the local loop, with the reason and what still covers it.

A host test proves a decision and how decisions combine. It does not prove that `syscall.rs`, `device.rs`, `guard.rs`, `trusted_ui.rs` or `mod.rs` call them, because those files cannot be built for a host (they use fixed 32-bit addresses). The stand-ins in `tests/integration/open/` call the same shipped decisions in the same order, and some of them copy shipped glue outright: guard.rs `subject`, `shield_owner`, `gate_open`, `fd_write_error`, `gate_rename`; syscall.rs `open`, `kill`, `sigaction`, `mmap`, `mprotect`, `ftruncate` and `fault_signal`'s classification; trusted_ui.rs `caller`, `ui_may`, `ui_sys`, `ui_touch`, `ui_tick`, `request_consent`, `LOCK_FILE` and the drawing; device.rs's band-row touch filter (in `World::finger`); mod.rs `reap`. A host row about what those do is **Host (stand-in)**: it checks how the shipped decisions combine, and a **QEMU** entry in the same row is what checks the shipped glue. A row with no QEMU entry says so.

What the host does run unchanged: policy.rs, manifest.rs, fs.rs, shield.rs, scorer.rs, sysui.rs, lifecycle.rs, signal.rs, vma.rs, elf.rs, entropy.rs, procmgr.rs (with 4 KiB pages as shipped and again with 64 KiB pages), loopback.rs, sockaddr.rs, slots.rs, names.rs, sntp.rs, powerctl.rs, Power's policy.rs, ota.rs, verify.rs's `ota_verify` (built as the image builds it, with a rollback floor), Storage's slotwrite.rs and writeback.rs `key` (what a restart keeps of /data).

`make test-local` runs the whole loop: `test-unit` (with the integration tests), `test-system`, `test-ci`.

## test_policy.py (`make test-policy`)

| Assertion | Now |
| --- | --- |
| An app writes and reads its own `/data/NAME` and `/data/NAME.x` | Host `policy::an_app_reaches_only_its_own_data`; QEMU (probe) |
| Another app's files, Settings' and the runtime's dot files are ENOENT to read and stat | Host, same test; QEMU (probe, `/data/.lock` included). `/data/.lock` on the host: stand-in only (front.rs `at` with the stand-in's own `LOCK_FILE`; the shipped `trusted_ui::private_path` is checked by the QEMU probe) |
| Writing, making, removing or renaming into another app's name is refused | Host, same test; QEMU (probe) |
| An `O_CREAT` open does not tell an existing hidden file from a missing one | Host `policy::an_o_creat_open_does_not_tell_whether_a_hidden_file_exists`; QEMU (probe) |
| `/tmp` is shared; its dot names only for the image apps that name them | Host `policy::tmp_is_shared_but_its_dot_names_are_not`; QEMU (probe) |
| `..` does not climb out of an app's folder | Host `policy::an_app_reaches_only_its_own_data`; QEMU (probe) |
| Listing `/data` shows only the app's own names | Host `policy::listing_data_shows_only_the_apps_own_names`; QEMU (probe) |
| `/apps`, `/bin`, `/lib` and `/` are read-only; programs can be read | Host `policy::the_images_trees_are_read_only`; QEMU (probe) |
| `/dev/violet/{mic,camera,hci,audio}` EACCES, an unknown node ENOENT, TCP and UDP sockets EACCES | Host `policy::devices_and_sockets_need_a_capability_and_refusals_are_logged`; QEMU (probe) |
| The system's sys commands answer `error` | Host `policy::the_systems_sys_commands_are_not_an_apps`; QEMU (probe) |
| `set`/`get` work; `consent net` and `consent hci` answer `deny` with no prompt | Host, same test; QEMU (probe) |
| The runtime logs each refused capability (net, mic, system, wifi-admin) | Host (both tests above); QEMU |
| New on the host: image manifests grant exactly what they declare; another app's program started by an app runs as the caller; an installed app's declared power is asked of the user, kept in `/data/.grants` across a restart (the restart keeps what writeback.rs `key` keeps) and forgotten on reinstall; uninstall removes `/data` and answers | Host `policy::image_apps_get_what_their_manifests_declare`, `another_apps_program_started_by_an_app_is_not_that_app`, `an_installed_apps_declared_power_is_the_users_to_grant`, `uninstalling_an_app_removes_its_data_and_answers` |

## test_wx.py (`make test-wx`)

| Assertion | Now |
| --- | --- |
| `mmap` RWX and RX refused; RW→RX and R→RX `mprotect` refused; code never made writable | Host `memory::no_writable_and_executable_memory_and_no_data_turned_into_code`; QEMU (wxprobe "checks ok") |
| Nothing of the app's is mapped into a 64 KiB page that holds code | Host `memory::nothing_of_the_apps_goes_into_a_page_that_holds_code`; QEMU |
| New on the host (4 KiB pages, as shipped): a one-page `PROT_NONE` guard inside a mapping is enforced; procmgr.rs read-ahead stays inside the region and keeps its rights | Host `memory::a_one_page_guard_inside_a_mapping_is_enforced`, `read_ahead_stays_inside_the_region_and_keeps_its_rights`. Every memory scenario also runs with 64 KiB pages (`--cfg page64`) |
| The trampoline page cannot be unmapped, replaced, changed or read into; threads still start through it | Host `memory::the_trampoline_page_is_the_runtimes`; QEMU (a real `pthread_create`) |
| `read()` and `getrandom()` into code, constants, read-only and `PROT_NONE` memory are EFAULT with nothing written; `PROT_NONE` keeps its contents | Host `memory::system_calls_do_not_write_where_the_app_may_not`; QEMU |
| `getrandom` is seeded and never repeats; `/dev/urandom` | Host `memory::getrandom_is_seeded_and_never_repeats`, `no_app_starts_until_the_generator_is_seeded`; QEMU. The real seeding from timer jitter: QEMU (the "entropy: seeded" line) |
| A write to code, a write to a constant, running the stack, running the heap, reading `PROT_NONE`, writing memory made read-only after it was mapped: each ends the app with status 139 and the access in the log | **QEMU** (seven launches, real page tables). Host `memory::a_write_to_code_ends_the_app` and five more check the fault handler's decision against the fake MMU |
| The harmless seventh launch exits 0 | QEMU; Host `memory::a_harmless_run_exits_cleanly` |
| Stack, heap, mmap area and AT_RANDOM move between launches; AT_RANDOM is not zero | Host `memory::the_layout_moves_between_launches_and_boots` (21 launches over 3 boots); QEMU (7 launches) |
| New on the host: a program with a writable+executable segment, or code sharing a page with data, is not loaded | Host `memory::a_program_with_writable_code_is_not_loaded` |

## test_abi.py (`make test-abi`)

| Assertion | Now |
| --- | --- |
| csig: handlers with siginfo on an alternate stack, masks and pending signals, SIGILL, a thread interrupted in user code with VFP and Thumb IT/GE state kept, SA_RESTART against EINTR in ARM and Thumb, alarm, interval and POSIX timers, sigtimedwait | **QEMU** (csig, every line). The rules also in `runtime/open/tests/abi_host.rs` |
| csig: SIGSEGV caught for unmapped (code 1) and read-only (code 2) memory | QEMU (csig, the shipped `fault_signal`); Host (stand-in) `abi::sigsegv_reaches_a_handler_with_maperr_or_accerr`, `a_fault_whose_signal_the_thread_blocks_ends_the_app_despite_a_handler` (front.rs `touch` copies `fault_signal`'s classification, SIGBUS and the blocked mask included) |
| cproc: fork, fork+execve, posix_spawn with a pipe, an environment and `O_CLOEXEC`, a missing program, vfork, SIGCHLD, ECHILD | **QEMU** (cproc) |
| cproc: a child killed by a signal and its wait status; a child may not signal its parent | QEMU; Host `abi::an_app_signals_itself_and_its_descendants_only` (also: not another app, not Home, `kill(-1)`) |
| cfault: a fault with no handler ends the program with status 139 | QEMU; Host `abi::a_fault_with_no_handler_ends_the_program_with_status_139` |
| charden: fork keeps d8 and FPSCR; unaligned LDRD is SIGBUS BUS_ADRALN; SIGKILL ends a sleeping or spinning vfork child; 400 vforks | **QEMU** (charden) |
| charden: a 1 µs timer fires at most once a millisecond; 64 zombies at most | QEMU; Host `abi::limits_from_signal_rs_hold_where_the_runtime_applies_them` |
| charden: a hidden `#!` interpreter answers ENOENT like a missing one | QEMU; Host `abi::a_hidden_interpreter_answers_like_a_missing_one` |
| charden: an image the loader refuses fails execve with the caller alive | QEMU; Host `abi::an_image_the_loader_refuses_does_not_start_and_the_caller_lives` |
| cdyn: a dynamically linked program run by `/lib/ld-musl-armhf.so.1` | **QEMU** (cdyn). The shipped loader's checks: `runtime/open/tests/abi_host.rs` |
| rsig: Rust std, a handler, `std::process::Command` | **QEMU** (rsig) |
| goabi: Go with asynchronous preemption, `os/signal`, a recovered nil dereference, `os/exec`, three rounds | **QEMU** (goabi), **one round**. Dropped: the two more rounds (they looked for a rare preemption race; run `make test-system` again to repeat) |
| The shipped loader passes `abi_host.rs the_shipped_loader` with `VIOLET_ABI_REQUIRE_LIBC=1` | Kept: `make test-system` runs it after its image build, before QEMU |
| New on the host: SIGKILL and SIGSTOP cannot be caught; a handler that never fixes its fault is stopped after `FAULT_LIMIT` | Host `abi::sigkill_and_sigstop_cannot_be_caught`; Host (stand-in) `a_handler_that_never_fixes_the_fault_is_stopped` |

## test_lifecycle.py and test_idle.py (`make test-lifecycle`, `make test-idle`)

lifecycle.rs and procmgr.rs run unchanged; ending an app is the stand-in for mod.rs `reap` (front.rs `end`). No QEMU row: the real `reap` and `tcb_suspend` are booted only by `make test-memory`, outside the loop.

| Assertion | Now |
| --- | --- |
| Ticker behind Home runs through its grace and cached time, freezes after about 15 s, stops ticking, runs again in front | Host `lifecycle::an_app_left_behind_home_runs_through_its_grace_then_freezes` (exact times on the fake clock). Dropped: that the frozen app's threads really stop (`tcb_suspend` on the real kernel); `make test-memory` still boots these paths |
| The child of the app in front runs past 15 s and is never frozen | Host `lifecycle::the_child_of_the_app_in_front_is_never_frozen` |
| Under pool pressure background apps hear LOW_MEMORY before anything is closed; the frozen Cache is closed with status 137; the app in front and Home survive | Host `lifecycle::low_memory_closes_the_frozen_app_not_the_one_in_front_or_home` (a real `procmgr::Space` and a 16 MiB pool) |
| A closed app opens again from scratch | Host, same test |
| New on the host: an app with audio open stays visible; an app behind that runs out of memory is closed itself; a closed app's pages and sockets come back | Host `lifecycle::an_app_with_a_reason_to_run_stays_visible`, `an_app_behind_that_runs_out_of_memory_is_closed_itself`, `a_closed_apps_memory_and_sockets_come_back` |
| test_idle.py: QEMU's CPU share at Home over 20 s; the Open Runtime asleep at least 50% and at most 620 loops a second | **Dropped**: a performance measurement of the emulator on this Mac, 30 s of waiting, no security property. The script is in git history (`git show 628b358:targets/qemu/test_idle.py`) |

## test_net.py (`make test-net`)

These are unit level only: slots.rs, names.rs, loopback.rs and sockaddr.rs run unchanged, but the loopback scenarios join listeners and connects in the test file itself (`scenarios/net.rs` `Lo`), not through sockets.rs or the system calls (front.rs has no bind, listen, connect or accept). Who may reach whom (sockets.rs's identity check on a loopback connect) is checked by no test in the loop. No QEMU row.

| Assertion | Now |
| --- | --- |
| 300 names get 300 addresses in 198.18.0.0/15; the table is bounded | Host `net::the_resolver_is_bounded_and_a_stale_address_goes_nowhere` |
| An address whose name was dropped is no host (EHOSTUNREACH); a bad name is refused | Host, same test (`Lookup::Stale`, NXDOMAIN). Dropped: the errno `connect` gives for it (sockets.rs) |
| One app holds its share of the slots (9 of 32) and then gets EMFILE | Host `net::one_app_holds_its_share_of_the_slots_and_no_more` (also: every app keeps its floor, the system its reserve) |
| netpeer fetches a page at once beside a connect that never answers; the hanging connect ends in ETIMEDOUT with its slot returned | Host, same test, for the slot table (no call of another app's waits). The timeout itself: `make test-net-lwip` (Radio's sockets.c against lwIP, host). **Dropped**: the timing measured through the relay |
| A server on the phone's loopback over IPv4, `::1`, `::ffff:127.0.0.1` and the dual-stack wildcard; a refused port; ENETUNREACH for a real IPv6 address | Host, unit level `net::an_app_serves_on_loopback_over_ipv4_and_ipv6` |
| Only IPv4 answers for a name (AAAA has none); localhost has both | Host `net::only_ipv4_answers_for_a_name` |
| UDP on loopback with the sender's port | Host `net::udp_on_loopback_goes_only_to_the_socket_bound_to_the_port` |
| Python's http.server with urllib, asyncio, socketpair | **Dropped**: CPython end to end over loopback sockets. The pipes they ride on: Host `net::loopback_pipes_are_budgeted_per_app`; Python's TCP client is still `make test-open` (pynet) |
| A TCP listener and a bound UDP socket on the wildcard that this Mac reaches through the relay | **Dropped**: needs Radio's relay and servers on the Mac. Who may be reached: Host, unit level `net::another_app_reaches_a_loopback_server_only_if_its_manifest_serves_apps` (`serve: lan`, `serve: apps`: loopback.rs's rule, with the owner comparison written in the test, not sockets.rs's). That build_initfs.py packs `serve:` lines: `tools/test_manifests.py` |

## test_power.py and test_time.py (`make test-power`, `make test-time`)

| Assertion | Now |
| --- | --- |
| The app takes a wake lock, renews it, a bad argument is refused; the lock is in Power's table and accounted to the app | Host `power::wake_locks_and_governors` (the shipped powerctl.rs and Power policy over the fake endpoint) |
| No sensor is reported as absent, never as cool; without a load figure the top operating point | Host, same test |
| The operating point follows the load (600 MHz idle, 1.5 GHz at half, top at full) | Host, same test (also: every step keeps voltage above what the frequency needs) |
| Warm and hot cap the operating point; the level falls only after two dwells of 5 s | Host, same test |
| A failing sensor counts as hot, is reported, powers nothing off, and recovers | Host, same test |
| The lock ends with its app | Host, same test |
| A critical temperature powers the machine off | Host, same test (the board's power switch is asked once). Dropped: that QEMU really exits |
| Dropped: the text of the app's `power` and `wakelocks` reports | They are formatted from Power's replies in `services/power/process.rs`, which is not host-buildable; `services/power/policy.rs`'s own tests cover the numbers |
| test_time.py: the clock starts at the build time and arrives within seconds of real time from NTP; an app's wall clock follows | Host `time::the_phone_sets_its_clock_and_an_apps_wall_clock_follows` and three more (a lying server, no agreement, an unplugged link), with time servers on the fake link. **Dropped**: real public NTP servers over the Mac's UDP (the test failed whenever the Mac was offline) |

## test_trusted_ui.py (`make test-trusted-ui`)

sysui.rs (every rule: the passcode, its back-off, consent, the pad, the band's rows) runs unchanged. Everything trusted_ui.rs and device.rs do around it is the stand-in in `tests/integration/open/trusted_ui.rs` and `World::finger`; the second boot of test_system.py runs the real ones for the passcode lock.

| Assertion | Now |
| --- | --- |
| No lock screen without a passcode | Host `trusted_ui::the_band_stays_the_runtimes_over_a_full_screen_app` |
| The band's rows stay the runtime's while an app presents red over its whole surface | Host, same test, for the rule (sysui.rs `app_rows`). Pixels: stand-in only (`ui_draw` is the harness's painter). **Dropped**: the real pixels (device.rs's present copy); `make test-open-apps` in CI still draws through it |
| `bench`, `set relay`, `notifications`, `passcode off` refused to an app | Host `trusted_ui::an_app_may_not_give_the_systems_sys_commands` |
| `consent mic`, `consent hci` and opening the camera are refused at once to an image app with no manifest, and no prompt comes up | Host `trusted_ui::consent_is_policys_answer_for_an_image_app_and_no_prompt_comes_up` |
| Touches reach the app; none from the band | Stand-in only `trusted_ui::an_app_gets_its_own_touches_and_none_from_the_band`: the band-row filter is `World::finger`'s copy of device.rs's. Not in QEMU now |
| A passcode set in Settings on the runtime's keypad; the pad's touches are not the app's | **QEMU** boot 1 (Settings, the real pad); Host (stand-in) `trusted_ui::a_passcode_locks_the_next_boot_with_a_back_off` |
| Second boot: locked at once, a tap on the lock screen opens nothing | **QEMU** boot 2 (the real trusted_ui.rs, `ui_save`'s fsync and Storage's write-back); Host (stand-in), same test: the restart keeps what writeback.rs `key` keeps |
| A wrong passcode is counted; the right one unlocks | **QEMU** boot 2 (one wrong, then the right one); Host (stand-in), same test |
| Five wrong passcodes counted; the right one not checked during the back-off; unlocks after it | Host (stand-in), same test (sysui.rs's back-off unchanged; the 30 s pass on the fake clock; the real 100,000-iteration PBKDF2 runs). The real back-off: not in QEMU (30 s of waiting) |
| Home's Lock locks; the passcode unlocks | Host (stand-in), same test. Not in QEMU now |
| A notification shows on the lock screen only as a count | sysui.rs `summary`: Host. On screen: stand-in only (`ui_draw` paints a block when the summary is not empty). Not in QEMU now |
| New on the host: a prompt takes every touch and the whole screen, an app cannot answer it, a prompt for an app that is not in front is never raised; an unreadable `/data` stays locked; the idle lock; the failure count survives a restart | Host (stand-in) `trusted_ui::a_prompt_takes_the_touches_and_the_screen_and_only_the_user_answers_it`, `a_data_volume_that_cannot_be_read_stays_locked`, `the_phone_locks_itself_when_left_alone`. The failure count across a restart rests on the real `ui_save` waiting for `data::fsync`, which no test in the loop checks (QEMU boot 2 checks the record itself survives) |

## test_ota.py (`make test-ota`)

| Assertion | Now |
| --- | --- |
| An older version is refused before anything is switched | **QEMU** boot 1 (old.vota through Settings, supervisor.c and verify.rs); Host `ota::the_supervisors_own_check_applies_the_builds_floor_and_the_read_back_digest` (verify.rs `ota_verify` as the image builds it, floor 4) and `older_forged_and_foreign_bundles_are_refused_and_nothing_changes` (real P-256) |
| A bundle whose image is not the one its manifest signed is refused | **QEMU** boot 1 (forged.vota); Host, both tests above (also: a forged signature, an untrusted key, another device's bundle, an image too large, a raised version field). That supervisor.c passes the read-back digest on apply: QEMU only |
| A signed version 2 is installed into slot B by Settings over HTTPS; B is staged as a trial; slot B holds the image that was sent | **QEMU** boot 1; Host `ota::a_signed_update_is_staged_boots_as_a_trial_and_is_marked` |
| The trial boots from B and the supervisor marks it successful | **QEMU** boot 2; Host, same test |
| B stays the boot slot once proven | Host, same test |
| A signed release that does not start spends A's six tries and the phone falls back to B; A is unbootable; the broken slot is never marked | Host `ota::a_release_that_does_not_start_falls_back_after_six_tries` (slot.rs `choose`). **Dropped** from QEMU: six 45 s boots of a zeroed kernel |
| New on the host: nothing before the switch changes which slot boots; out-of-order hashing and writes outside the inactive slot are refused; a trial slot accepts no update | Host `ota::a_power_cut_before_the_switch_leaves_the_running_system` |

## test_shield_enforce.py (`make test-shield-enforce`)

| Assertion | Now |
| --- | --- |
| notes runs to the end untouched | Host `shield::an_ordinary_app_is_not_touched` |
| photosync reads notes' data, goes to the background, uploads 4 MiB: quarantined, most of it never leaves | Host `shield::an_upload_after_reading_another_apps_data_is_quarantined` (also: cut off from new connections and other apps' data, released after 30 s) |
| themes' write to `/apps/home` is refused (shield 1), and so are a write and a truncate through a read-only descriptor and a read-only open with `O_TRUNC` | **QEMU** boot 1 (the policy probe: write, `ftruncate` and `truncate` of `/apps/home`, `O_RDONLY\|O_TRUNC`, and Home's size unchanged, through the real syscall.rs and guard.rs); Host (stand-in) `shield::an_app_cannot_change_another_apps_program_by_any_door` (ftruncate, ftruncate64 and truncate included) |
| `/apps/home` is unchanged; Home was not contained and still answers | Host, same test |
| The image starts in strict mode | Host `shield::image_apps_are_contained_only_in_strict_mode_and_nothing_in_monitor`, `the_saved_mode_can_only_make_a_build_stricter`. **Dropped**: a strict-mode image build and the demo apps' own code in QEMU. `make test-shield` (older, local) still runs the demo apps end to end with the companion; the recorded traces replay in `make test-unit` |

## test_lock.py (`make test-lock`, from main's power-key branch)

Main's app-drawn lock was replaced by the trusted UI's when the branches merged; its QEMU suite went with it.

| Assertion | Now |
| --- | --- |
| A short press of the power key turns the screen off; the next turns it on | **QEMU** `make test-system` trial boot (`power key: screen off locks, the next press turns it on`); Host `lock::short_press_acts_on_release_while_on`, `press_wakes_at_once_while_off`, `counted_press_between_samples`, `key_held_since_start_is_ignored` |
| The screen is black while off; a tap does not wake it | **Dropped** (QEMU): power.rs fills the frame black and the viewer drops touches while paused; not asserted |
| Without a passcode nothing locks | Host `sysui::record_round_trip_and_damage` (`Pass::lock` is false with no record) |
| A passcode set in Settings; a mismatched second entry starts over | The trusted UI's own keypad: Host sysui tests (`set_lock_unlock_with_backoff`); **QEMU** `make test-system` boot 1 sets one |
| With a passcode the power key locks; the lock screen refuses a wrong passcode and an app icon, opens with the right one | **QEMU** `make test-system` trial boot (screen off locks); locked-at-boot, a refused tap and wrong/right passcodes are already there; Host sysui tests |
| The phone starts locked on the next boot | **QEMU** `make test-system` trial boot |
| Holding the key shows the power sheet; Cancel closes it; Power off stops the machine | Host `lock::hold_is_long_once` and Home's `sheet::tests::sheet_buttons`. **Dropped** (QEMU): the sheet and an actual power off, which would end the consolidated run's boot |
| New: a passcode saved by main's lock (`1 SALT HASH [WRONG]`) still unlocks and is rewritten as a VPL1 record | Host `sysui::an_earlier_locks_passcode_still_unlocks_and_is_upgraded` |

## Kept as they were

These came before the waves, or are host-only, and stay as separate local targets outside the loop: `test-emulator`, `test-isolation`, `test-audio`, `test-open`, `test-memory`, `test-ladybird`, `test-wifi-app`, `test-agent`, `test-locomo`, `test-open-apps`, `test-shield`, `test-shield-proxy`, `test-shield-unit`, the handset targets, and from wave 1 the host-only `test-net-lwip`, `test-radio-tls`, `test-storage-long`, `test-wpa`, `test-sae`.

`test-packages` and `test-packages-floor` (wave 1, QEMU: a patched, unsigned or foreign-signed Radio must not boot; one below the rollback floor must not boot) are boot-path tests, each with its own staged catalog and image build. They stay local targets **outside the loop**: folding them into test_system.py would add an image build each. What the loop does cover: the refusals themselves, decided by `system/package-manager/verify.rs`, which `make test-unit` tests with the real BearSSL (`test_verify.sh`). What it does not: that supervisor.c's `valid_package` calls the verifier when it starts a component, acts on its answer and passes the floor. Run `make test-packages test-packages-floor` after changing supervisor.c's package path.

## The AArch64 build (`--arch aarch64`)

No QEMU assertion moved or was dropped for the AArch64 port: the AArch32 rows above are unchanged, and `make test-system`, `make test-ci` and `make test-local` still boot only the AArch32 image. The AArch64 system has its checks in the same scripts, run by `make qemu64 QEMU64_STACK=1` and by hand, outside the loop and CI ([targets/qemu64](../targets/qemu64/README.md#the-violetos-system-on-aarch64)): `test_system.py --arch aarch64` (one boot: the layers in order, then a 64-bit program's signal handlers and posix_spawn), `test_open_apps.py --arch aarch64` and `test_ladybird.py --arch aarch64`. The host side is in `make test-unit`: `runtime/arch/tests/host.rs` (`enter64`, `widen`, `memory_args`, `signal64`, `compat`), `runtime/open/tests/abi_host.rs` (AArch64 programs below 4 GiB or refused, sigreturn's alternate stack) and the integration scenario `an_aarch64_program_meets_the_same_policy_through_its_own_numbers` (through the shipped `enter64::translate` and `widen`).

What `test_system.py` checks on AArch32 and has **no AArch64 counterpart yet**:

| AArch32 check | On AArch64 |
| --- | --- |
| Linux ABI apps (cdyn, cfault, charden, cproc, csig, goabi, rsig) | Not built: they need an AArch64 musl C toolchain. A 64-bit Rust program (`targets/qemu64/test-apps/abi64`) covers signals, siginfo, a fault's handler and posix_spawn. fork without `CLONE_VM` answers ENOSYS. |
| W^X and layout randomization on real page tables (wxprobe) | Not probed end to end. The same `sel4.c` `open_page_map` sets ExecuteNever; host `memory::*` covers the decisions. |
| The policy probe (boot 1) | Not run. Host `policy::*` and the integration scenario above cover the decisions. |
| Verified boot, the timekeeper | Absent on AArch64. Printed as absent layers from `differences.json`, never as passes. |
| The trusted UI's lock kept across a restart (boot 2) | Not run on AArch64. |
