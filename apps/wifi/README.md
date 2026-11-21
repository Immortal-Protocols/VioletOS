# Wi-Fi Setup

The [app](process.rs) connects to or disconnects from **Violet Lab**, the QEMU target's modeled open Wi-Fi network. It saves the choice in a private checkpoint and restores it when the app opens. The launcher controls airplane mode.

**Test Radio** checks the local gateway, then asks the emulator's HTTPS helper for a fresh response from [UUID Generator](https://www.uuidgenerator.net/api/version4). The guest shows the returned UUID across two lines. The helper verifies TLS using the host certificate store and accepts only a UUIDv4. The radio service sends its request and receives the result over the guest's virtio network device.

Run `make test-wifi` for repeatable tests with a local fixture or `make test-wifi-live` to contact the real endpoint and compare its response with the guest framebuffer. The Moto still needs a physical Wi-Fi driver, discovery and authentication.
