# Stopwatch

The [QEMU app](process.rs) owns its timer and drawing surface. Home preserves a running stopwatch. Quit discards the running instance, and opening it again loads the last durable checkpoint when storage is available.

The [state machine](state.rs) is shared with the [handset UI](../../platform/sel4/stopwatch/ui.c). Run `make test-stack` for lifecycle checks and `make test-storage` for reboot recovery.
