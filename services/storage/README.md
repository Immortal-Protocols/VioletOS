# Storage service

The current [QEMU storage process](../../platform/sel4/services/storage.rs) saves a paused stopwatch value and recovers it after reboot. The proposal extends storage to app-scoped resources accessed through Runtime. Run `make test-storage` to check durable saves, reset and recovery from a damaged record.
