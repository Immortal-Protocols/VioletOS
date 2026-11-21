# Package manager

The host [staging tool](manage.py) installs, replaces and removes packages for the next QEMU boot. It checks identities, ABI version, lengths and checksums before switching the active catalog. The guest [catalog process](process.rs) serves app discovery from the supervisor's active inventory. The supervisor and catalog process ship in the recovery image.

From the repository root:

```sh
make qemu-image
make component NAME=radio
python3 system/package-manager/manage.py install build/sel4/qemu/packages/radio.vpk
python3 system/package-manager/manage.py list
python3 system/package-manager/manage.py uninstall stopwatch
make emulator
```

Close the emulator before staging the next change. Use `rollback` to undo the last transaction, or `install build/sel4/qemu/packages/stopwatch.vpk` to restore the stopwatch. The package format targets the eight declared component identities. Package inputs and the host are trusted. Checksums detect corruption and provide no publisher authentication.
