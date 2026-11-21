# Process supervisor

The [QEMU first task](qemu.c) constructs address spaces, threads and endpoint permissions. It starts the launcher, Runtime and services, opens apps on demand, and reinitializes private execution state on Quit. Each Runtime worker can contact its assigned service. Only display holds the scanout mapping.

The supervisor receives kernel fault messages and monitors in-flight service calls. Runtime and the foreground app also have progress deadlines. After a fault or a 20-second stall it restarts the managed group and attempts the previous catalog. A failed guest-installed catalog is retired on disk. Repeated failures during startup stop the recovery loop. Run `make test-stack` and `make test-apps` for fault, hang and cold-boot rollback checks.
