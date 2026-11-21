# Launcher

The [QEMU launcher process](process.rs) asks Runtime for the installed app, draws Home into its private surface, and opens the stopwatch on demand. It also displays virtual radio and battery status and issues radio and sleep commands. The first touch after sleep wakes the display.

Run `make emulator` to use it or `make test-stack` to exercise navigation and controls through guest input.
