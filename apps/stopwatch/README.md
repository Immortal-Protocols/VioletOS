# Stopwatch

The [Moto and QEMU app](process.rs) owns its timer and drawing surface. Home preserves a running stopwatch. Quit discards the running instance, and opening it again loads the last durable checkpoint when storage is available.

The [state machine](state.rs) tracks elapsed time and handles start, pause and reset on both targets. Run `make test-stack` for lifecycle checks and `make test-storage` for reboot recovery.
