# Radio service

The [QEMU radio process](process.rs) reads a virtual carrier register and controls an airplane-mode register. Runtime exposes state queries and subscriptions and authorizes the launcher to toggle the radio. Run `make test-stack` to toggle it in both directions and verify the guest device state.

Cellular data, calls, Wi-Fi and a physical modem driver remain future work.
