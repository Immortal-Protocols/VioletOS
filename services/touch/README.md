# Touch service

The touch process turns hardware reports into finger coordinates for the UI. The [current driver](../../platform/sel4/services/touch.rs) handles controller initialization, firmware upload and contact decoding. The proposal adds typed subscriptions through Runtime. Run `make test-emulator` to exercise normal input and transfer failures.
