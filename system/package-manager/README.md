# Package manager

To build and install the example fourth app, run the following from the repository root.

```sh
make app APP=targets/qemu/test-app
VIOLET_INBOX=build/sel4/qemu/packages/app-11.vpk make emulator
```

Open **Apps** and tap **Install Package**. The inbox supplies the distribution bytes. The guest package manager asks the supervisor to validate the VPK, commit the new catalog, and restart the managed processes. Select **Next** to find Notebook and **Open** to launch it. Subsequent boots read installed packages from the QEMU package disk. Use **Uninstall** on an optional component to remove it. The user-data disk retains its files.

The same path updates services. For example, use `make component NAME=radio` and supply `build/sel4/qemu/packages/radio.vpk` as the inbox. The [catalog volume](volume.rs) preserves the prior version across an interrupted write. A fault or unanswered service request can trigger rollback and a group restart. The launcher, Runtime, display and touch accept replacements and are required for a bootable catalog.

The loader, kernel, supervisor and package manager ship in the recovery image. The host [staging tool](manage.py) can prepare a base catalog before boot. Valid guest-installed catalogs take precedence. Package inputs and the development host are trusted. CRCs detect corruption. This format has no publisher signature.
