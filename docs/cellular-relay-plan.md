# Cellular relay through a companion phone — plan

Goal: VioletOS on the Moto G Pure uses an iPhone's (later any Android phone's) cellular data without the carrier's hotspot. VioletOS runs a Wi-Fi access point, the phone joins it, and a companion app relays traffic over cellular. Apps on VioletOS see an ordinary network and need no relay-specific code. Once paired, the link survives the phone being locked and reconnects by itself.

Proof target: a real iPhone and the XT2163DL on slot B, with the iPhone locked, relaying live traffic for hours at acceptable throughput and battery cost.

## Architecture

```
VioletOS apps ──socket API──▶ Network service ──▶ relay backend ──Wi-Fi (VioletOS AP, WPA2)──▶ companion app ──cellular──▶ Internet
                                   └─▶ Wi-Fi station backend (existing lwIP path, unchanged)
```

### 1. Flow-level relay, not a packet tunnel

The phone opens real cellular TCP and UDP sockets on VioletOS's behalf. VioletOS sends it requests: *open TCP to host:port*, *send UDP datagram*, *resolve name*. It does not send raw IP packets.

- iOS gives apps no raw IP sockets. A packet tunnel would need a full userspace TCP/IP stack on the phone, and TCP-over-TCP behaves badly under cellular loss.
- The phone's native TCP stack handles cellular congestion, handover and Wi-Fi Assist. That gives the best throughput per mW.
- TLS stays end-to-end from VioletOS. Radio's BearSSL client still validates certificates, so the phone only ever handles ciphertext.
- "No per-app configuration" comes from VioletOS owning the socket API. Today Radio's lwIP clients are internal. The plan adds a **Network** service that gives apps `connect / send / recv / resolve / udp`. That service either drives lwIP (station mode) or the relay, and apps can't tell which.

**Relay protocol (v1).** One authenticated, multiplexed session over TCP on the AP link:
- Noise `NNpsk0` (or TLS-PSK) keyed by the pairing secret. Another client on the AP can't use the relay.
- Stream frames carry an id, a sequence number, credit-based flow control, and open/close/reset. Datagram and DNS frames are added too.
- Sessions can be resumed. If Wi-Fi drops for a few seconds, the phone keeps its cellular sockets open, buffers about 256 KiB per stream, and replays from the last acknowledged sequence on reconnect. Remote TCP connections survive brief Wi-Fi drops.
- Heartbeats are adaptive: fast while traffic is flowing, 30–60 s when idle, so the phone's radios can sleep.

### 2. VioletOS side

- **Radio SoftAP.** This is the biggest new piece and the main risk. Radio already loads the MT6765 connectivity firmware, scans, and runs WPA2 as a station. AP mode needs these firmware commands: BSS activation in AP role, beacon template, station-record add/remove, key install, and buffering for clients in power save (DTIM / TIM). The gen4m driver in Motorola's kernel (`p2p_role_*`, `ap0`) is the reference, as it was for station mode. The WPA2 authenticator (4-way handshake as AP) comes from the same hostap codebase already ported as `supplicant.c`. A single 2.4 GHz channel to start; 5 GHz non-DFS once stable.
- **DHCP server.** Small and fixed: one /29 subnet, one lease per paired phone. **No router option and no DNS option** are handed out. The phone then treats the network as local-only, keeps cellular as its default route, and needs no captive-portal workaround (see the iOS risks below). VioletOS answers only on the relay port.
- **Network service.** This is the socket API described in section 1. It picks a backend in this order: relay if a session is up, then the station link. Its status goes through USB Diagnostics.
- **Pairing UI.** Wi-Fi Setup gets a "Phone Internet" page. It shows a QR code holding the SSID, a random WPA2 passphrase, the relay PSK and the AP address, plus link and throughput status.
- **QEMU first.** A Python stand-in for the companion, next to `tools/emulator_endpoint.py`, implements the relay protocol so `make test-relay` exercises the Network service, resumption, flow control and failure paths headless. The hardware only has to prove the AP and the radio.

### 3. iPhone companion (Swift, sideloaded with the Immortal Company team)

Joining the network: `NEHotspotConfiguration` with `joinOnce = false` saves the network, so iOS rejoins on its own. It needs the Hotspot Configuration entitlement, which this team can use.

Keeping the app running while locked. These are tried in order and measured on the device:

1. **Packet Tunnel extension as a keep-alive host (primary).** A `NEPacketTunnelProvider` whose `includedRoutes` covers only an unused /32. The phone's own traffic is untouched, but iOS keeps the extension process alive while the "VPN" is connected, including when locked. It is relaunched on failure and can be started by on-demand rules. The extension runs the relay: one socket to VioletOS bound to Wi-Fi, and cellular sockets created with `requiredInterfaceType = .cellular`. This is a supported long-running mechanism with no audio-session conflicts. The extension's ~50 MB memory cap fits a relay with bounded buffers.
2. **Silent background audio (fallback / comparison).** An `AVAudioSession` set to `.playback` + `.mixWithOthers`, looping silence. The app handles interruptions (calls, Siri, other apps' audio) and route changes, and restarts the session. It's App Store–unsafe but fine sideloaded. Its battery cost is measured against option 1.
3. Not pursued unless 1 and 2 fail: background location (battery cost), PushKit/VoIP (needs a server and doesn't hold sockets), NEHotspotHelper (restricted entitlement).

Reconnecting:
- Wi-Fi rejoin is handled by iOS. The relay uses `NWPathMonitor` on the Wi-Fi interface plus mDNS/fixed-address probes, with exponential backoff capped at 5 s while associated.
- After a phone reboot, on-demand VPN rules start the extension after first unlock. The pairing secret is stored as `kSecAttrAccessibleAfterFirstUnlock`.

### 4. Android companion (after the iPhone proof)

- A foreground service of type `connectedDevice`, with a battery-optimization exemption.
- `WifiNetworkSpecifier` or a saved suggestion joins the local-only network without changing the default route.
- `ConnectivityManager.requestNetwork(TRANSPORT_CELLULAR)` + `Network.bindSocket` handles the cellular side.
- Same protocol and pairing QR.

## Risks and how we retire them early

| Risk | Retire by |
| --- | --- |
| MT6765 firmware AP mode can't be brought up from VioletOS | Spike S2 in parallel with S1. Fallback: run the S1 experiments against a stand-in AP while AP mode is built. |
| iOS drops or deprioritises a Wi-Fi network with no internet while locked | S1: measure association and relay reachability across 8 h locked, with and without a router option |
| iOS routes its own traffic onto VioletOS Wi-Fi (breaking the phone) | DHCP with no router/DNS. Verify the phone's own browsing stays on cellular in S1 |
| Packet Tunnel trick throttled or killed when locked | S1 compares tunnel vs. audio. Keep both code paths behind one relay core |
| Phone battery cost | Measure %/h locked, idle vs. streaming. Tune heartbeat and phone power save; AP must buffer for PS clients |
| Moto battery cost of AP beaconing | Beacon interval 100–200 ms, DTIM 3. Report Power service mV/h |
| Throughput ceiling of 1×1 802.11n + lwIP on VioletOS | Measure in M3. Target ≥ 20 Mbit/s down, ≤ 15 ms added RTT |

## Milestones

**M0 — Tooling (this session).** The Mac reboots, flashes and reads the Moto, and installs, launches, drives and locks the iPhone, with no hands on either device. Status is below.

**S1 — iPhone background spike (2–3 days).** Companion app v0 with both keep-alive strategies. The stand-in AP is a Wi-Fi network with no internet: Android slot A's SoftAP on the Moto, or a spare AP on the Mac's Ethernet. A Mac-side relay peer pushes and pulls traffic through it. Measurements: hours associated while locked, relay reachability every 30 s, phone battery %/h, cellular throughput through the relay. Exit criterion: 8 h locked with ≥ 99% heartbeat success and < 3%/h battery idle.

**S2 — VioletOS SoftAP spike (in parallel, longest pole).** Radio brings up an open AP, beacons, accepts an association from the iPhone, and passes a ping-equivalent over a fixed IP. Then WPA2-PSK.

**M3 — Network service + relay protocol in QEMU.** Socket API, relay backend, the Python companion stand-in, and `make test-relay` covering resumption, flow control, DNS, UDP, and loss/latency injection. The Wi-Fi app's HTTPS UUID test switches to the socket API, so it then runs over either backend.

**M4 — Integration on hardware.** Moto AP + DHCP + relay ↔ iPhone companion (Packet Tunnel). HTTPS UUID test over iPhone cellular. QR pairing. Then the S1 endurance run repeated against the real VioletOS AP: 8 h locked, out-of-range/return, VioletOS reboot, iPhone reboot, iPhone call interrupting.

**M5 — Performance and battery tuning**, then **M6 — Android companion**.

## Acceptance test (hardware, automated from the Mac)

1. Pair once (QR scanned by the iPhone, driven by XCUITest).
2. Lock the iPhone. Every 60 s for 8 h, VioletOS fetches a UUID over HTTPS through the relay, triggered and read over USB Diagnostics.
3. Mid-run: toggle VioletOS Wi-Fi off and on, reboot VioletOS, take the phone out of range (AP off for 5 min), reboot the iPhone.
4. Pass: ≥ 99% of requests succeed outside the injected outages; recovery ≤ 10 s after each outage ends; iPhone battery ≤ 3%/h idle; throughput and RTT targets met.

## M0 status — device control

**Moto G Pure (XT2163DL, slot B):**
- VioletOS USB Diagnostics answers.
- Verified today: `moto-package reboot` → (slot B retries were exhausted) Android A with adb authorized → `adb reboot bootloader` → fastboot reports B → `set_active b` restores 7 retries → `fastboot reboot` → VioletOS back in 10 s (generation 1, disk ready, previous boot stage 6). No hands on the phone.
- The faster path — a VioletOS `reboot-bootloader` command that sets the MT6357 RTC fast-boot marker (RTC_PDN1 bit 13, per Motorola's `rtc_mark_fast`) before the watchdog reset — is being built as its own PR, along with a `moto-flash` script.
- Note: fastboot reports `securestate: oem_locked`, yet earlier trials flashed `boot_b` successfully. The PR's flash test settles this.

**iPhone 18 Pro Max (iPhone19,3, iOS 27.2):**
- Paired with CoreDevice, Developer Mode on, no passcode (lock/unlock can be scripted).
- The team provisioning profile already includes this UDID.
- **Blocked:** Xcode 26.6 has no developer disk image for this chip, and every devicectl action (launch, processes, XCUITest, screenshots) needs one. Xcode 27.2 (App Store) needs macOS ≥ 26.6. This Mac runs 26.5.1 and is offered macOS Tahoe 26.7.3.
- Legacy lockdown pairing (libimobiledevice/pymobiledevice3) reports "user denied trust". It's not needed once devicectl works, but it's handy for battery readouts via `diagnostics`.
- Installed on the Mac today: `mas`, `libimobiledevice`, `pymobiledevice3` (pipx).
