# Radio service

Wi-Fi Setup scans the QEMU network profiles, selects one by stable ID and remembers the choice on disk. Set `VIOLET_NETWORKS='["VIOLET LAB","GUEST WIFI"]'` to change the available names. These are virtual open access points sharing one Ethernet uplink. On the Moto, Radio controls the physical Wi-Fi chip and joins a provisioned WPA2 network. [USB Diagnostics](../usb-diagnostics/README.md) exposes its status and forwards scan and join requests from the Mac.

**Test Radio** sends an ARP request to the QEMU gateway. It then opens a TCP stream through the emulator’s [relay](../../tools/emulator_endpoint.py). Inside the guest, the [TLS client](tls.c) draws entropy from virtio-rng, checks the certificate chain, hostname and QEMU clock, and fetches the response over HTTP/1.1. The guest accepts a UUIDv4 from the fixed endpoint and shows it on screen. The image ships the ISRG Root X1 trust anchor. Changes to the endpoint’s trust chain can require a radio package update. [Endpoint](https://www.uuidgenerator.net/api/version4)

Run `make test-wifi` for network selection, persistence, link failures, rejection of plaintext, and app switching during a delayed request. Run `make test-wifi-live` for the public endpoint.
