# App and service packages

The [builder](build.py) links each QEMU component into its own ARM executable. Each package is a VPK2 record: a header with the component ID, ABI version, code and image lengths and payload checksum, the image, and an 80-byte signature trailer. The trailer holds the signing key's ID, the package's security version, the target (QEMU or the handset) and an ECDSA P-256 signature over everything before it ([sign.py](sign.py), [keys](../keys/README.md)). The build signs every package; the supervisor runs none without a valid signature from a key pinned in its boot image, and the old unsigned VPK1 format is refused. Code pages are executable and read-only. Writable data is private to each process.

A catalog is the packages one after another and, when the supervisor wrote it, a floor record after them: the highest version seen of each component ([versions](../../system/package-manager/README.md#signatures-and-versions)). The catalog as a whole has a CRC and no signature.

Run `make component NAME=stopwatch` for one package or `make qemu-image` for a complete factory catalog. The [package format](package_format.py) checks each record's shape before staging it, and its signature when given the pinned keys (`python3 image/packages/sign.py verify build/sel4/qemu/package-keys.json FILE.vpk`). `python3 image/packages/test_package_format.py` tests the format and the key configuration.

An external app directory contains `process.rs` and an `app.json` manifest with an ID from 11 to 14, a title of up to 16 uppercase display characters and, optionally, a `version` (1 if absent). Run `make app APP=path/to/app` to link it against the Rust SDK. The launcher discovers the title from the installed package. [Example app](../../targets/qemu/test-app/) · [Install an app or service](../../system/package-manager/README.md)

Runtime package ABI 2 includes its service workers and mailboxes. The app wire ABI stays at version 1. Boot validation falls back if a catalog supplies an incompatible Runtime.
