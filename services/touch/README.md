# Touch service

The touch process turns hardware reports into finger coordinates for the UI. The [current driver](../../services/touch/touch.rs) handles controller initialization, firmware upload and contact decoding. QEMU apps subscribe to contact events through Runtime. Run `make test-emulator` to exercise normal input and transfer failures.
