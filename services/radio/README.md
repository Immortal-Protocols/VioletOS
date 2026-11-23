# Radio service

Wi-Fi Setup scans the QEMU network profiles, selects one by stable ID and remembers the choice on disk. Set `VIOLET_NETWORKS='["VIOLET LAB","GUEST WIFI"]'` to change the available names. These are virtual open access points sharing one Ethernet uplink. On the Moto, Radio controls the physical Wi-Fi chip and joins a provisioned WPA2 network. [USB Diagnostics](../usb-diagnostics/README.md) exposes its status and forwards scan and join requests from the Mac.

**Test Radio** sends an ARP request to the QEMU gateway. It then opens a TCP stream through the emulator’s [relay](../../tools/emulator_endpoint.py). Inside the guest, the [TLS client](tls.c) draws entropy from virtio-rng, checks the certificate chain, hostname and QEMU clock, and fetches the response over HTTP/1.1. The guest accepts a UUIDv4 from the fixed endpoint and shows it on screen. The image ships two trust anchors in [trust_anchors.h](trust_anchors.h): ISRG Root X1 (the UUID endpoint and wttr.in) and GTS Root R1 (the music library). Changes to an endpoint’s trust chain can require a radio package update.

**Music stream.** Radio also fills Audio's music ring from its event loop ([stream.rs](stream.rs)): HTTPS range requests of up to 128 KiB for `/tracks/NN.mp3` on the music library host, retried while the library sleeps. See the [Audio service](../audio/README.md#music). [Endpoint](https://www.uuidgenerator.net/api/version4)

Run `make test-wifi` for network selection, persistence, link failures, rejection of plaintext, and app switching during a delayed request. Run `make test-wifi-live` for the public endpoint.
