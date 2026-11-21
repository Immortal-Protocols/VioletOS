# App and service packages

The [builder](build.py) links each QEMU component into its own ARM executable. Each VPK records the component ID, ABI version, code and image lengths, and payload checksum. Code pages are executable and read-only. Writable data is private to each process.

Run `make component NAME=stopwatch` for one package or `make qemu-image` for a complete factory catalog. The [package format](package_format.py) checks each record before staging it.
