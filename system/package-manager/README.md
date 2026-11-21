# Package manager

To build and install the example fourth app, run the following from the repository root.

```sh
make app APP=targets/qemu/test-app
VIOLET_INBOX=build/sel4/qemu/packages/app-11.vpk make emulator
```

Open **Apps** and tap **Install Package**. The inbox supplies the distribution bytes. The guest package manager asks the supervisor to validate the VPK, commit the new catalog, and restart the managed processes. Select **Next** to find Notebook and **Open** to launch it. Subsequent boots read installed packages from the QEMU package disk. Use **Uninstall** on an optional component to remove it. The user-data disk retains its files.

The same path updates services. For example, use `make component NAME=radio` and supply `build/sel4/qemu/packages/radio.vpk` as the inbox. The [catalog volume](volume.rs) preserves the prior version across an interrupted write. A fault or unanswered service request can trigger rollback and a group restart. The launcher, Runtime, display and touch accept replacements and are required for a bootable catalog.

The loader, kernel, supervisor and package manager ship in the recovery image. The host [staging tool](manage.py) can prepare a base catalog before boot. Valid guest-installed catalogs take precedence. Package inputs and the development host are trusted. CRCs detect corruption. This format has no publisher signature.

## Moto developer packages

Build the board-specific Radio package with `make moto-component NAME=radio` and the Mac transfer tool with `make build/moto-package`. With VioletOS running, use `build/moto-package status` to read the active Radio checksum and catalog bank. Send the package with `build/moto-package build/sel4/moto/packages/radio.vpk`, then open **Apps** on the handset and select **Install Package**. The Mac reports the checksum of the staged package. After installation and again after reboot, check that the active checksum matches it.

The installer commits to alternating catalog banks inside slot B. The stopwatch and user files live in a separate volume. A full installer-image flash initializes both areas. To test preservation, pause the stopwatch at a known nonzero time before transferring the package and compare it after rebooting the same image.

Developer images with USB reboot support accept `build/moto-package stopwatch` to read the saved time and `build/moto-package reboot` to request a full handset restart. Run `tools/moto-reboot-check` to record Radio and stopwatch, restart the board, wait for the new boot over USB, and compare both values. It needs the `build/moto-debug` and `build/moto-package` tools. Older handset images reject the checkpoint request before the script sends a reboot.

For a hands-free Moto Radio update, use the Mac that built the developer image:

```sh
tools/moto-reboot-check --install-radio build/sel4/moto/radio-r2.vpk
```

Supply a different board-specific Radio VPK from the one already running. The command records the saved stopwatch value, signs the exact package, transfers it over USB, requests activation, and checks for the new checksum and catalog bank. It then reboots the handset and checks the active Radio and saved stopwatch again. Leave USB connected throughout.

The Moto build creates a local P-256 signing key at `build/sel4/moto/developer-signing.pem`. Keep it private and retain it to authorize future Radio builds. The boot image embeds the public key. A newly generated key requires a matching boot image. The supervisor verifies the signature over the entire staged VPK and restricts remote activation to Radio. Local confirmation in Apps is still available for other components.
