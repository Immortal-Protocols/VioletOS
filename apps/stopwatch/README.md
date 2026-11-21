# Stopwatch

The stopwatch owns elapsed time and the Start, Pause and Reset transitions. Its current [Rust state machine](../../platform/sel4/services/stopwatch.rs) runs on the Moto and QEMU with a [C renderer](../../platform/sel4/stopwatch/ui.c). The proposed package would draw into its own surface and request checkpoint storage through Runtime. Run `make test-emulator` from the repository root to exercise the current stopwatch.
