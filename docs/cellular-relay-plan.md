# Phone cellular relay — plan

**Goal:** VioletOS gets internet through a phone's cellular data, with no carrier tethering.
1. **Phone app.** Paired once, it relays VioletOS traffic over the phone's cellular connection.
2. **VioletOS hotspot.** VioletOS runs its own Wi-Fi network, the phone joins it, and VioletOS traffic goes through the app. VioletOS apps see an ordinary connection and need no extra settings.

**Proof:** VioletOS on the Moto fetches an HTTPS page over the iPhone's cellular data. The run is driven entirely from the Mac.

## Design

```
VioletOS app ─▶ Radio (lwIP TCP + BearSSL TLS) ─▶ VioletOS hotspot (WPA2) ─▶ iPhone app ─▶ cellular ─▶ Internet
```

**VioletOS (Radio).**
- The MT6765 Wi-Fi chip runs as an access point: beacons, open authentication, association, then WPA2-PSK with VioletOS as the authenticator.
- A one-lease DHCP server gives the phone an address with **no default gateway**, so the phone keeps its own traffic on cellular.
- When the phone is the uplink, Radio's TCP connect opens a SOCKS5 connection to the phone with the hostname and port. Name lookup happens on the phone. TLS still runs end to end from VioletOS, and the phone only relays encrypted bytes. Callers of Radio's connect don't change.

**iPhone app.**
- Joins the hotspot with `NEHotspotConfiguration`.
- Listens for SOCKS5 connections on its Wi-Fi address, authenticated by username and password with the pairing secret.
- Opens each outgoing connection with `requiredInterfaceType = .cellular`.

## Order of work
1. iPhone relay app, tested from the Mac against the iPhone's Wi-Fi address on the current network.
2. VioletOS access point, checked by the iPhone seeing and joining it.
3. DHCP server and SOCKS5 uplink in Radio.
4. End to end: the Wi-Fi app's HTTPS UUID test over the iPhone's cellular connection, then throughput.

## Hands-free control
- **Moto:** `moto-flash` flashes and reboots, and `moto-package` / `moto-debug` read status over USB.
- **iPhone:** Xcode 27.2 (`devicectl`, XCUITest). The test phone has no passcode, and Auto-Lock is set to Never.
