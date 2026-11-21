# App and service packages

The [builder](build.py) links each QEMU component into its own ARM executable. Each VPK records the component ID, ABI version, code and image lengths, and payload checksum. Code pages are executable and read-only. Writable data is private to each process.

Run `make component NAME=stopwatch` for one package or `make qemu-image` for a complete factory catalog. The [package format](package_format.py) checks each record before staging it.

An external app directory contains `process.rs` and an `app.json` manifest with an ID from 11 to 14 and a title of up to 16 uppercase display characters. Run `make app APP=path/to/app` to link it against the Rust SDK. The launcher discovers the title from the installed package. [Example app](../../targets/qemu/test-app/) · [Install an app or service](../../system/package-manager/README.md)

Runtime package ABI 2 includes its service workers and mailboxes. The app wire ABI stays at version 1. Boot validation falls back if a catalog supplies an incompatible Runtime.
