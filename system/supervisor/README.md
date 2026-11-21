# Process supervisor

The [QEMU first task](qemu.c) constructs each component's address space, thread and endpoint permissions. It starts the launcher at boot, resumes the stopwatch on Open, and resets the stopwatch's private execution state on Quit.

On a managed process fault it suspends the group, revokes its resources and reconnects fresh processes. A fault with a staged catalog selects the previous compatible catalog or falls back to the factory image. After five unsuccessful generations it stops recovery. Run `make test-stack` to exercise replacement failure and check that the recovered launcher and stopwatch work.
