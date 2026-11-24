# RFC: Sticky, Reliable Wi-Fi & Persistent Credentials on Pure seL4

**Status:** Phase 1 and parts of phases 2 and 3 implemented (see [Implementation status](#implementation-status)). The handset changes build but have not run on the XT2163DL yet.  
**Targets:** Moto G Pure (`XT2163DL`, MT6765 Gen4m/Connac WLAN + lwIP) and QEMU.

## Motivation

Today, Wi-Fi on the handset regularly drops connections, refuses to reconnect, loses saved passwords, and fails to join common home and public routers. Auditing the stack reveals ten concrete root causes:

### Password & persistence bugs
1. **Transient timeouts wipe saved passwords:** `apps/open/wifi/src/main.rs` deletes the saved PMK from `/data/networks` on `code == 52`, while `targets/moto-g-pure/wpa.rs` returns `Err(52)` for *any* 8 s handshake timeout or dropped EAPOL frame—not just an actual wrong password.
2. **No boot auto-connect:** `/data/networks` is only read when the user opens `/apps/wifi`; nothing in Open Runtime or Radio auto-joins saved networks at startup.
3. **4-file storage cap + silent write drops:** `services/storage/files.rs` hard-caps all open apps combined to `SLOTS = 4` files. Once 4 files exist, `runtime/open/data.rs` silently fails lazy 24-byte-chunk flushes even after `std::fs::write` returned `Ok(())`. In addition, `moto-flash` re-initializes the 1 MiB storage volume in `boot_b`, wiping `/data/networks`, and `moto-wifi-profile` only lives in supervisor RAM.
4. **Single RAM key slot:** `targets/moto-g-pure/wpa.rs::APP_KEY` retains only one SSID/PMK pair in RAM.

### Connection-drop & router-compatibility bugs
5. **`relink()` always fails (stale `pairwise_ready`):** `targets/moto-g-pure/radio.rs::relink()` never resets `axi.pairwise_ready = false` or clears old firmware WTBL keys. `wlan_axi.rs::transmit_eapol()` encrypts EAPOL Message 2 with the stale key, so the AP drops every reconnect attempt.
6. **Home's 2 s status poll aborts reconnects:** In `targets/moto-g-pure/radio.rs`, *any* IPC (`GET PROFILE` / `GET RADIO`) arriving while `LINK_ERROR` is set immediately executes `wpa::close()`, `dev.shutdown()`, and `connected = 0`—racing and permanently killing the `relink()` loop.
7. **Hair-trigger drops, no keepalive, blocking sockets, and 5.5-min timer wrap:**
   - Any single TX error (`wpa.rs::radio_l2_send`) or unauthenticated broadcast deauth (`wlan_axi.rs::poll_data`) immediately trips `LINK_ERROR`.
   - No 802.11 Null-Data keepalives exist, so idle routers deauth the station after 60–300 s.
   - `targets/moto-g-pure/ip/sockets.c` blocks synchronously up to 5–8 s inside `open_sockets_poll()` without calling `station_wpa_poll()`, starving WPA2 GTK group rekeys.
   - `targets/moto-g-pure/radio_io.rs::now()` uses 32-bit 13 MHz ticks, wrapping every ~5.5 minutes.
8. **8-AP scan cap & passive-only scan:** `targets/moto-g-pure/passive_scan.rs` caps `aps` at 8 entries (dropping networks in busy buildings) and sends no active Probe Requests (missing hidden SSIDs and collided beacons).
9. **Common routers rejected:** `passive_scan.rs::psk_ccmp()` rejects WPA/WPA2-mixed routers using TKIP group cipher (`00-0f-ac-2`) and `MFPR=1` routers; `radio.rs` rejects Open networks (`!ap.wpa2_psk_ccmp`) and `scan_entry()` never sets `SCAN_OPEN`; `station.rs` advertises legacy 802.11a/b/g rates only (no 802.11n HT/WMM).
10. **Cold chip reboot on every scan/join + random MAC:** `radio.rs` powers down the connectivity domain after every scan or disconnect (3 full firmware uploads to switch networks), cannot scan while connected, and invents a fresh random MAC on every cold boot (`wlan_axi.rs`), slowing DHCP and breaking router reservations.

## Design Goals

- **Connections stick:** clean `relink()` with WTBL/key reset, decoupled `RadioState` FSM (polls never kill reconnects), Null-Data keepalives, non-blocking sockets that never starve GTK rekeys, and 64-bit monotonic timers.
- **Passwords stay saved & phone auto-connects:** dedicated persistent `VWIF1` profile bank + stable device MAC, boot auto-connect in Open Runtime, and strict separation between definite `BadPassword` vs. `HandshakeTimeout`.
- **Fast, warm Wi-Fi:** keep WMT/WLAN firmware warm across scans and disconnects; support cached-channel + DHCP `INIT-REBOOT` fast resume (< 1 s) and multi-BSSID mesh roaming.
- **Broad router compatibility:** 64-entry active+passive scanner with RSSI sorting, Open networks on handset, WPA/WPA2-mixed TKIP group cipher + CCMP pairwise, optional PMF, and 802.11n HT + WMM IEs.

## Architecture

The target design. Its file names are the draft's: the auto-connector lives in `runtime/open/wifi.rs` and the radio changes in `radio.rs`, `wlan_axi.rs`, `passive_scan.rs` and `station.rs` (see [Implementation status](#implementation-status)).

```text
  /apps/wifi (UI)          Home / Other Open Apps (read-only state & diagnostics)
        │ join / forget / list                 │ state / RSSI / reason
        ▼                                      ▼
  ┌───────────────────────────────────────────────────────────────────────────┐
  │  Open Runtime WifiManager (runtime/open/wifi_store.rs)                    │
  │   • Dedicated VWIF1 ProfileStore (32+ profiles, stable MAC, cached IP)    │
  │   • AutoConnector: boots -> scans -> joins highest-priority saved SSID    │
  │   • Never deletes PMK on HandshakeTimeout (re-prompts only on 2x BadPass) │
  └──────────────────────────────────┬────────────────────────────────────────┘
                                     │ async Runtime tickets
                                     ▼
  ┌───────────────────────────────────────────────────────────────────────────┐
  │  Handset Radio Service (targets/moto-g-pure/radio_fsm.rs)                 │
  │   • Explicit FSM: Off -> WarmIdle -> Scanning -> Joining -> Connected     │
  │                          ^------------ Reconnecting (backoff + roam) <--┘ │
  │   • Warm WMT/WLAN firmware (no cold reboot per scan/switch)               │
  │   • Clean relink: resets pairwise_ready + WTBL + tries all BSSIDs by RSSI │
  │   • 64-bit CNTVCT ms clock, 30 s Null-Data keepalive, broadcast-deauth    │
  │     verification, TX backpressure (WouldBlock != fatal LINK_ERROR)        │
  ├───────────────────────────────────┬───────────────────────────────────────┤
  │  scan_compat.rs                   │  ip/async_sockets.rs                  │
  │   • 64-AP table sorted by RSSI    │   • Per-slot non-blocking state mach. │
  │   • Active + passive (incl. DFS)  │   • 1 ms tick -> station_wpa_poll()   │
  │   • Open, WPA2-CCMP, TKIP-group   │     always runs (GTK rekeys succeed)  │
  │   • 802.11n HT + WMM IEs          │   • DHCP INIT-REBOOT fast-path        │
  └───────────────────────────────────┴───────────────────────────────────────┘
```

## Implementation status

The draft's code skeletons (`radio_fsm.rs`, `scan_compat.rs`, `wifi_store.rs`, `ip/async_sockets.rs`) were never built; they are replaced by changes to the real code, listed here by root cause.

| # | Root cause | Status | Where |
| --- | --- | --- | --- |
| 1 | Timeouts wipe saved passwords | **Done.** Radio reports `JOIN_AUTH` (52) only when the access point repeated message 1 after our message 2 (or the supplicant rejected a frame); silence or a single lost frame is the new `JOIN_HANDSHAKE_TIMEOUT` (57). The Wi-Fi app never deletes a saved key on a failure; after a 52 it asks for the password and replaces the key only once the new one works. | `wpa.rs::authorize`, `protocol.rs`, `apps/open/wifi` |
| 2 | No boot auto-connect | **Done.** The Open Runtime's auto-connector scans and joins the most recently joined saved network in range 8 s after boot, tries the next one in range when one fails, backs off from 30 s to 10 min while none is in range, rechecks every 30 s, skips a network whose key was refused, and pauses after the user disconnects. Open networks are saved too. | `runtime/open/wifi.rs` |
| 3 | 4-file `/data` cap; `moto-flash` wipes storage | **Done.** Open apps get 32 files (the extra 28 in the volume's unused sectors 608–1112; the first four stay where they were, so existing data is kept). `moto-flash` sends a sparse image that leaves the storage window alone (`--wipe-storage` for the old behavior). | `services/storage/files.rs`, `runtime/open/data.rs`, `moto-flash` |
| 4 | One RAM key slot in Radio | Not needed now: the app sends the key with every join, and the auto-connector does the same. | |
| 5 | `relink()` reuses the stale pairwise state | **Done.** `associate()` clears `pairwise_ready`, so the new handshake's message 2 goes out unencrypted; the firmware's station record is rewritten by `commit_peer` as before. Explicit WTBL key removal was not added (unverified firmware command). | `wlan_axi.rs` |
| 6 | Home's status poll kills reconnects | **Done.** Only an explicit leave tears the link down. A failed link stays joined and reports `WIFI RECONNECTING`; the event loop makes one rejoin attempt per pass (500 ms apart three times, then 2 s doubling to 60 s) for as long as the network stays joined, answering requests in between. | `radio.rs` |
| 7 | Hair-trigger drops, no keepalive, blocking sockets, timer wrap | **Partly done.** 32 failed transmits in a row (or any while unkeyed) end the link, not one. `station_net_poll()` now runs the supplicant's timers, so every blocking wait in `client.c` and `sockets.c` keeps them going. EAPOL frames, including group rekeys, were already handled inside those waits, so rekeys were not actually starved. The timer wrap is not a bug: `radio_monotonic_ms()` comes from a 64-bit millisecond count and `radio_io::expired()` is only used for windows well under the 5.5-minute wrap. **Deferred:** Null-Data keepalives and non-blocking sockets. | `wpa.rs`, `ip/client.c` |
| 8 | 8-AP passive-only scan | **Partly done.** 32 access points per scan (kept in a static, not on Radio's stack). **Deferred:** active probes, DFS channels, RSSI ordering (the RX descriptor's RSSI field is not decoded yet). | `passive_scan.rs` |
| 9 | Common routers rejected | **Partly done.** Open networks are recognised (no RSN or WPA element, privacy clear), reported with `SCAN_OPEN` and joined without a handshake: association without RSN, gen4m BSS auth mode open with encryption disabled, unprotected data frames. **Not yet tested on a real open network.** **Deferred:** TKIP group cipher, optional PMF, 802.11n HT/WMM. | `passive_scan.rs`, `station.rs`, `wlan_axi.rs`, `radio.rs` |
| 10 | Cold chip per scan; random MAC | **Deferred.** Warm firmware across scans and a stable MAC need hardware sessions to develop. | |

**Verified in QEMU:** `make test-wifi-app` (now four boots: wrong and right password, switching to an open network, rejoining by itself after a restart, one-tap rejoin with the saved key, and a handshake timeout that keeps the key, where the QEMU radio's `"stalls": true` network answers 57), `make test-wifi`, `make test-storage`, `make test-open-apps`, `make test-open`.
**Built for the handset, not yet run on it:** everything in `targets/moto-g-pure` and the sparse `moto-flash`. The Open Runtime package is close to its 0x3e000-byte limit (about 750 bytes of code room left).

## Phased Milestones

1. **Phase 1 — Immediate stability fixes (done):** Reset `pairwise_ready` & WTBL in `associate()`/`relink()`, stop `dispatch()` from tearing down the chip on `!alive`, split `BadPassword` vs. `HandshakeTimeout` so saved PMKs aren't wiped, and add boot auto-connect in Open Runtime.
2. **Phase 2 — Durable credential & MAC storage:** Give Wi-Fi profiles a dedicated journaled bank (or raise `services/storage/files.rs::SLOTS` from 4 → 32+ with 512-byte transfers), persist a stable device MAC, and preserve storage across `moto-flash` unless `--wipe` is passed.
3. **Phase 3 — Warm chip, non-blocking sockets, and router compatibility:** Keep firmware warm in `WarmIdle`, replace blocking loops in `ip/sockets.c` with `async_sockets`, move `radio_io` to 64-bit ms + 30 s Null-Data keepalives, expand scan table to 64 APs with active probes, and enable Open networks, TKIP group cipher, optional PMF, and 802.11n HT/WMM.
4. **Phase 4 — Mesh roaming, sleep/wake, & diagnostics:** Multi-BSSID RSSI roaming, firmware PS mode across screen sleep with cached-BSSID + DHCP `INIT-REBOOT` resume, and per-network diagnostics (deauth reason, RSSI, channel, GTK rekey count) in `/apps/wifi` and `moto-debug`.

## Open Questions

1. **Dedicated storage bank vs. general `/data` expansion:** Should `VWIF1` live in its own reserved 8 KiB bank in `services/storage` (immune to app file pressure and readable even before Open Runtime mounts `/data`), or should we expand `SLOTS` to 32+ globally first?
2. **TKIP group cipher in Gen4m firmware:** Does `wlan_axi::install_ccmp` accept cipher ID `2` (TKIP, 32-byte key) for group WTBL 2 on XT2163DL firmware, or should WPA/WPA2-mixed routers also be supported via a separate `install_gtk_tkip()` helper?
3. **Preserving storage on `moto-flash`:** Should `moto-flash` read back and splice the existing 1 MiB VioletOS volume from `boot_b` (or move the persistent volume to a dedicated untouched offset/partition) so flashing updates keeps saved Wi-Fi networks?
