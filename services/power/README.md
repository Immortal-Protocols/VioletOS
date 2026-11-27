# Power service

Power answers for the battery, sleep and shutdown, keeps the wake locks, runs the thermal and frequency governors, and switches the supplies other services share. [process.rs](process.rs) is the request loop. Every decision is made in [policy.rs](policy.rs), which touches no hardware and is tested on the host by `make test-unit`. The registers belong to the board: [targets/qemu/power.rs](../../targets/qemu/power.rs) and [targets/moto-g-pure/power.rs](../../targets/moto-g-pure/power.rs).

Power has no timer. The governors run when a request arrives (Home asks for the battery every two seconds while it is in front), and every delay in the policy is measured in real time, so uneven requests only make a decision later.

## Wake locks

A holder asks Power to keep the CPU awake after the screen goes off. Open apps write `wakelock 1` and `wakelock 0` to `/dev/violet/sys`; Audio and Camera can take one under their own component ID over their Power endpoint (neither does yet, so that path is unused). Radio cannot: it has no endpoint of its own at Power (badge 7 is Runtime's worker for Radio's requests, which only switches the Wi-Fi rails).

- One lock per owner and kind. Taking it again renews it and is not a second lock.
- Only the owner that took a lock can release it. An app's lock ends when the app exits.
- An app's lock lapses ten minutes after it was last taken, so an app that forgets or hangs cannot keep the phone awake for ever. Lapses are counted against the owner.
- Any open app may take a lock; there is no permission for it. What bounds an app that renews for ever is the screen: once it is off, no app is given a new lock, and a held one ends at most thirty minutes after the screen went off however often it is renewed (`error screen-off`). Turning the screen on starts the allowance again. Services' locks have no such limit.
- A full table (16 locks) is refused as `DENIED` with `POWER_LOCK_FULL` in `arg`, which apps read as `error busy`. It is not sent as `BUSY`: Runtime uses that for a request still running, and the client would ask again and lose the answer.
- Sixteen locks at most. A request beyond that is refused (`error busy`), never dropped silently.
- Power keeps, per owner, how many locks it took, how many lapsed, and the total and longest time held. `wakelocks` in `/dev/violet/sys` prints who holds what now and those totals; any app can read it, and on the handset `build/moto-package "open-run=@sys wakelocks"` puts it in the open log. Power knows an app only by its pid (`pid:N`); the Open Runtime's log has a line with the program's path when a lock is taken, released, lost with the screen off, or left held at exit, which ties a pid to an app. The table is not written to the log by itself, and no Settings page shows it.

When Runtime asks for sleep while a lock is held, the screen stays off and Power postpones the suspend. Its answer names the kinds of lock in the way, and the suspend follows when the last one is released, lapses or its owner exits. Power has no timer, so a lapse is acted on at the next request that reaches it, whoever sends it (Home's battery query every two seconds while it is in front; nothing regular otherwise). With no lock held, sleep behaves as it did before.

What this does not do: nothing on screen asks for sleep today (only the boot launcher may, and it has no screen), and the handset has no suspend at all (its sleep command only blanks the screen), so on the handset a wake lock is accounting, not yet a power saving. Time is counted, not energy. Audio streams and downloads do not take a lock by themselves.

## Idle

Power itself sleeps in its `receive` between requests. The rest of the system does not idle yet: `delay_ms` in [runtime/transport.c](../../runtime/transport.c) is a loop of yields, so any service that waits keeps a core out of the kernel's idle thread (WFI). Fixing that needs a timer and a notification per process from the supervisor, which is not done.

## Thermal and frequency governors

Tested as pure code; functional in QEMU against an emulated sensor; **not active on the handset**.

- Thermal: four levels (normal, warm, hot, critical) from three trip points. A level rises at once. It falls one level at a time, only after five seconds at the level and only once the temperature is below the trip by the hysteresis. A sensor that fails three readings in a row, or reads outside −40 to 150 °C, is treated as hot, never as cool, and never powers the phone off by itself. Critical powers the phone off. Once the board has accepted that power-off, Power refuses sleep and wake and postpones no suspend, so a later command cannot replace it (in QEMU all three are one fixture word).
- Frequency: picks the slowest operating point with a quarter more capacity than the load needs, goes up at once, comes down after the load has stayed low for half a second, and obeys the thermal cap at once. A change raises the supply before the frequency and lowers the frequency before the supply; if a step fails, the governor stays at a point whose supply is at least what the running frequency needs.
- QEMU: the sensor, the load and the operating points are words of the fixture page (the table is at the top of the QEMU backend). The operating points change nothing about how fast QEMU runs. `make test-power` (local, not in CI) drives them.
- Handset: this repository documents no MT6765 thermal sensor, CPU PLL, per-point voltage or load counter, so the handset backend reports no sensor and no operating points and Power neither throttles nor scales there. `power` in `/dev/violet/sys` says so (`sensor=0 dvfs=0`) instead of showing a made-up temperature. There is no charger control and no low-battery shutdown beyond the PMIC's own.

`power` also gives `battery=`: a percentage. In QEMU it is the fixture's. On the handset it is estimated from the cell voltage with a generic lithium-ion curve, uncorrected for load or temperature and not measured on this battery (untested on hardware). `status` is unchanged and still reports millivolts there.

## Shared supplies

`policy::Gates` counts the users of a supply, so it is switched on by the first request and off by the last release. `policy::Sequence` is the ordered power-up of masked PMIC fields: it saves what each field held, verifies each write, and restores in reverse, so a rail the bootloader left on stays on. The handset's Wi-Fi rails now run through both, with the same register accesses in the same order as before (a host test records them); that port is untested on hardware. Radio is still the only user, and a repeated enable from it is refused and the rails restored, as before. Camera and Audio still switch their own PMIC fields through the allow-lists in the handset backend.

When the screen goes off Power calls the board's `screen_peripherals(false)`. QEMU records it; on the handset it does nothing, because no clock gate or supply is known to be safe to switch with the screen off.

## Requests

Everything is under the `POWER` key ([runtime/protocol.rs](../../runtime/protocol.rs)).

| Request | From | Answer |
| --- | --- | --- |
| `GET POWER 0` | any client, through Runtime | The battery: percent in QEMU, millivolts on the handset. |
| `GET POWER POWER_INFO` | any client | Thermal level, flags (sensor present, sensor failed, frequency scaling present, suspend postponed, screen off), temperature, operating point, lock kinds held, lock count, battery percent. |
| `GET POWER POWER_LOCKS\|n` | any client | The nth held lock, or with `arg` 1 the nth owner's totals. |
| `EXECUTE POWER 0\|1\|2` | the boot launcher | Wake, sleep, power off. |
| `EXECUTE POWER POWER_LOCK\|…` | the Open Runtime for its apps; Radio, Audio, Camera directly | Take, release, or drop everything an owner held. |

## Tests

- `make test-unit` (CI) runs policy.rs's tests on the host: lock ownership, renewal, lapse, a full table, an owner's exit, accounting; the postponed suspend; thermal hysteresis, dwell and sensor failure; operating-point choice and step order, including failed steps; user counting; the Wi-Fi rail sequence access by access; the screen-off limit on app locks; a power-off that sleep and wake cannot cancel. It also runs [runtime/open/tests/powerctl_host.rs](../../runtime/open/tests/powerctl_host.rs): the real `wakelock` command against the real policy, joined by a stand-in for Runtime's tickets (a full table reads `error busy`, the log names the program).
- `make test-power` (local) boots QEMU with the Power Test program and checks the same through the running system, ending with a critical temperature powering the machine off. It does not drive sleep (nothing in the image can ask for it), so the postponed suspend is only tested on the host.
