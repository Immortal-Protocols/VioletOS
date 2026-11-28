# Cellular

VioletOS has no cellular radio support. It cannot place or receive calls, send or receive SMS, or reach emergency services. This page says what exists, what a standalone cellular phone would need, and why this project cannot build or test most of it.

**VioletOS on the Moto G Pure must not be described as a phone replacement.** It has no way to call an emergency number.

## What exists

- **Phone Internet.** The handset hosts a WPA2 Wi-Fi network. An iPhone running [Violet Relay](../companion/ios/README.md) joins it and relays the handset's TCP connections over its own cellular data through SOCKS5. TLS runs end to end from VioletOS; the iPhone sees only encrypted bytes. See the [relay plan](cellular-relay-plan.md).
- **Wi-Fi** as a station on WPA2-Personal networks.

That is all. Data depends on a second phone with its own SIM and plan.

## What does not exist

| Capability | State |
|---|---|
| Baseband driver | None. No code in this repository talks to the MT6765's modem. |
| Modem firmware loading | None. The modem partitions are never read. |
| SIM | None. No card reader, no PIN handling, no carrier profiles. |
| Mobile data | Only through the iPhone relay. |
| Voice | None. |
| SMS | None. |
| Emergency calls (E911) | None. |
| Emergency alerts | None. |

## What a standalone phone would need

**1. The baseband interface.** The modem is a separate processor inside the MT6765 running MediaTek's firmware. The application processor reaches it over MediaTek's CCCI transport (shared memory and CLDMA queues). The vendor's Android kernel has a driver for it, but bringing the modem up also needs:

- the signed modem firmware images, loaded and verified by the secure world the way the vendor's boot chain does;
- the radio calibration data in the device's NVRAM partitions, served to the modem on request;
- MediaTek's radio interface protocol above the transport, which Android reaches through closed vendor libraries.

None of this is documented publicly. VioletOS replaces the kernel the vendor's driver lives in, so the driver would have to be rewritten as an seL4 component against an undocumented protocol.

**2. Voice.** US carriers have shut down, or are shutting down, their 2G and 3G voice networks. A call is VoLTE: an IMS stack with SIP signalling, AKA authentication against the SIM, RTP with the AMR-WB or EVS codecs, and carrier-specific settings. The audio path runs from the modem's DSP, not the audio codec VioletOS drives today.

**3. Emergency calling.** A device that can make calls must be able to reach emergency services, with location. In the US that means FCC rules for E911, including location accuracy. Carriers will not activate voice on a device they have not accepted.

**4. Certification.** A cellular device needs regulatory approval of the radio with the software it ships (FCC in the US), industry conformance testing (PTCRB or GCF) and each carrier's own acceptance. The Moto G Pure's approvals cover Motorola's software. Replacing the software that controls the radio is outside them.

## Why this project cannot close the gap

- **It cannot be opened.** The modem firmware, the vendor radio libraries and the secure-world code that loads them are closed. There is no source and no specification to build against.
- **It cannot be tested here.** QEMU has no model of this modem. Testing on air needs a SIM, a carrier, and for anything beyond data a network test set or a shielded room. Transmitting on licensed bands with uncertified software is not something to try on a desk.
- **Partial is worse than none.** A dialler that can call most numbers but not an emergency number is a hazard. No call screen will ship without certified emergency calling.

## What software can still do

All of it stays inside the relay design:

- Reconnect and status for the relay link.
- Hardening the pairing between handset and companion ([VSP1](shield-pairing.md) is specified for the Shield's key; the relay's own pairing code is separate).
- A data-only modem over USB would still need a USB host stack VioletOS does not have, and would carry the same certification questions as soon as it is built into a product.

## What must stay true in the product

- No dialler, call screen or SMS app.
- Settings and any description of the device say "data through a paired phone", not "cellular".
- Nothing suggests the device can reach emergency services.
