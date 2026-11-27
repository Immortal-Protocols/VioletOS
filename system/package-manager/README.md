# Package manager

To build and install the example fourth app, run the following from the repository root.

```sh
make app APP=targets/qemu/test-app
VIOLET_INBOX=build/sel4/qemu/packages/app-11.vpk make emulator
```

Open **Apps** and tap **Install Package**. The inbox supplies the distribution bytes. The guest package manager asks the supervisor to validate the VPK, commit the new catalog, and restart the managed processes. Select **Next** to find Notebook and **Open** to launch it. Subsequent boots read installed packages from the QEMU package disk. Use **Uninstall** on an optional component to remove it. The user-data disk retains its files.

The same path updates services. For example, use `make component NAME=radio` and supply `build/sel4/qemu/packages/radio.vpk` as the inbox. The [catalog volume](volume.rs) preserves the prior version across an interrupted write. A fault or unanswered service request restarts the group, and rolls back a catalog that is still on trial (below). The launcher, Runtime, display and touch accept replacements and are required for a bootable catalog.

The loader, kernel, supervisor and package manager ship in the recovery image. The host [staging tool](manage.py) can prepare a base catalog before boot. Valid guest-installed catalogs take precedence. The staging tool checks a package's signature and version as the supervisor does, using `package-keys.json` from the image build.

## Signatures and versions

Every package is signed ([format](../../image/packages/README.md), [keys](../../image/keys/README.md)). The supervisor checks the signature in one place, `valid_package`, which calls [verify.rs](verify.rs) (SHA-256 and P-256 from the pinned BearSSL). Every way a package can come to run goes through it: the factory catalog in the boot image, a catalog bank read from the package disk or eMMC, a staged catalog, and the install inbox (QEMU's inbox window, or a USB upload on the handset). A package that is unsigned, altered, signed by a key the image does not pin, or signed for the other target is refused; a catalog holding one is not booted, and the supervisor falls back to the previous catalog or the factory one. There is no build option that turns the check off. The package manager process itself only forwards requests: the decision is the supervisor's.

Each package carries a security version, under the signature. Two floors use it:

- **Boot.** A catalog holding a package older than the boot image's own copy of that component is not booted. A new boot image therefore retires older signed packages.
- **Install.** A package is not installed if it is older than the boot image's copy, than the version running now, than any version that ran healthy since power-on, or than the version the installed catalog's floor record remembers.

**The floor record** is how the install floor outlives a removal and a reboot. Every catalog the supervisor writes ends with one: the highest version seen of each component, wherever that is above the boot image's version. Removing a component leaves its version in the record, and each later catalog carries the record forward. So removing Audio 5, rebooting and installing a signed Audio 2 is refused. A catalog with a malformed record, or with a package below its own record, is not booted. The host staging tool keeps the same record.

**Rollback after a fault is limited to a trial.** A catalog is on trial from the moment it is committed until it has run without a fault or stall for the supervisor's health period (20 seconds). A fault during the trial rejects that catalog on disk, and the previous catalog (or the factory one) runs again; the rejected catalog's versions leave no floor behind, so the version that ran before it can be installed again. After the trial, and for a catalog found at power-on, a fault only restarts the same catalog (five faults in quick succession stop the system, as they do for the factory catalog). A crash therefore cannot be used to bring back the older packages of the other bank or of the boot image.

Limits, stated plainly:

- **Nothing here is rollback-protected storage.** The floor record lives in the catalog bank, inside its CRC but under no signature. Whoever can rewrite a bank (a compromised Storage process, fastboot, the storage medium) can remove the record, or put back any older signed catalog that still meets the boot image's floor. Closing that needs eMMC RPMB or fuses, which VioletOS does not use. What the record does stop is the ordinary path: remove, reboot, install older.
- **A catalog is not signed as a set.** Each package is signed; the list of packages in a catalog is not, because the phone itself composes the catalog at each install and holds no signing key. Whoever can rewrite a bank can therefore also drop an optional component (USB Diagnostics then runs from the boot image's copy) or combine signed packages of different versions, down to the boot image's floor.
- The trial is kept in supervisor memory. If the phone is powered off before a new catalog's trial ends, the catalog is treated as proven at the next power-on, and a fault no longer rolls it back. Within its trial (20 seconds after an install) a crash still rolls a new catalog back; that is the purpose of the trial.
- If every installed catalog is rejected or unreadable, the boot image's catalog runs and the record is gone with the banks.
- All components ship at version 1 today, so the floors change nothing until a version is raised in [components.json](../../image/catalog/components.json).
- The boot image, and with it the pinned keys and the verifier, is not itself verified while the bootloader is unlocked.
- The recovery package manager is part of the boot image and is not checked separately.
- Open apps in `/apps` are not packages and are not covered by this signature.

Tests: `make test-unit` runs [the verifier's host tests](test_verify.sh) against packages signed with openssl (bit flips, relabelled and re-checksummed packages, wrong and unknown keys, the other target, truncation, VPK1, the floors, the floor record across a removal and a reboot, the trial) and [the format's](../../image/packages/test_package_format.py). `make test-packages` (local) boots QEMU with tampered staged and installed catalogs, including ones below their floor record; `make test-packages-floor` (local) does the same on an image whose Radio is version 2, to exercise the boot image's floor. No test drives an on-device install of an older package: the install decision is covered by the host tests of the floors only.

## Moto developer packages

Build the board-specific Radio package with `make moto-component NAME=radio` and the Mac transfer tool with `make build/moto-package`. With VioletOS running, use `build/moto-package status` to read the active Radio checksum and catalog bank. Send the package with `build/moto-package build/sel4/moto/packages/radio.vpk`, then open **Apps** on the handset and select **Install Package**. The Mac reports the checksum of the staged package. After installation and again after reboot, check that the active checksum matches it.

The installer commits to alternating catalog banks inside slot B. The stopwatch and user files live in a separate volume. A full installer-image flash initializes both areas. To test preservation, pause the stopwatch at a known nonzero time before transferring the package and compare it after rebooting the same image.

Developer images with USB reboot support accept `build/moto-package stopwatch` to read the saved time and `build/moto-package reboot` to request a full handset restart. Run `tools/moto-reboot-check` to record Radio and stopwatch, restart the board, wait for the new boot over USB, and compare both values. It needs the `build/moto-debug` and `build/moto-package` tools. Older handset images reject the checkpoint request before the script sends a reboot.

For a hands-free Moto Radio update, use the Mac that built the developer image:

```sh
tools/moto-reboot-check --install-radio build/sel4/moto/radio-r2.vpk
```

Supply a different board-specific Radio VPK from the one already running. The command records the saved stopwatch value, signs the exact package, transfers it over USB, requests activation, and checks for the new checksum and catalog bank. It then reboots the handset and checks the active Radio and saved stopwatch again. Leave USB connected throughout.

The build creates a local P-256 developer key at `build/sel4/moto/developer-signing.pem` and signs every package with it. Keep it private and retain it: a newly generated key requires a matching boot image. For activation over USB without a tap on the handset, the Mac also sends a detached signature by the same key over the whole staged package; the supervisor checks it in addition to the package's own signature and version, and restricts this to Radio and USB Diagnostics. Release images pin no developer key and have no such path: there, a package is installed only by confirmation in Apps. That path does not install USB Diagnostics, so in a release image Diagnostics changes only with the boot image.

On the Moto baseline with package 21, [USB Diagnostics](../../services/usb-diagnostics/README.md) also accepts a signed replacement for itself. The supervisor runs the replacement as a trial and waits for confirmation over the new USB connection before persisting the catalog.
