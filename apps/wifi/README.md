# Wi-Fi Setup

The [app](process.rs) connects to or disconnects from **Violet Lab**, the QEMU target's modeled open Wi-Fi network. It saves the choice in a private checkpoint and restores it when the app opens. The launcher controls airplane mode.

**Test Radio** asks the isolated radio service to send an ARP request and check the reply from the private QEMU gateway. Run `make test-wifi` to exercise a successful round trip, a dropped link, radio-off behavior and a reboot. The Moto still needs a physical Wi-Fi driver, network discovery and authentication.
