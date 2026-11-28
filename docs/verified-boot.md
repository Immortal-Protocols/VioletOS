# Verified boot

What checks the software before it runs, on each target, and what does not.

| | QEMU | Handset (XT2163DL) |
| --- | --- | --- |
| Who starts the VioletOS loader | `tools/run.py`, with `tools/ota_boot.py` as the bootloader model for updates | Motorola's bootloader, **unlocked** |
| Is the loader itself checked | Through `tools/ota_boot.py`: yes, the whole slot (loader, entry, device tree, `/apps`) against the slot's signed update manifest and keys the model keeps itself. Plain `make emulator`: no, the loader is the root of trust (QEMU has no boot ROM) | **No.** The bootloader starts any boot image written to the slot |
| Kernel and root task (supervisor, pinned package keys) | Checked by the loader against a signed manifest; a mismatch boots nothing | **Not checked** |
| Open apps' image (`/apps`) | Checked by the loader in a release image or with `VIOLET_BOOT_BIND_APPS=1`; **not** in a developer image | **Not checked** |
| Rollback to an older signed system | Refused by the bootloader model before the slot runs (the slot is marked unbootable and the other one boots), and again by the loader, below the stored floor | **Not refused at boot** (updates are still refused by the running system) |
| Packages (services and system apps) | Each checked by the supervisor: signature, target, version floors ([package manager](../system/package-manager/README.md)) | The same |

**The handset stays unlocked and unverified.** Anyone holding the phone and a USB cable can write a different boot image, and it will run. Everything VioletOS enforces on the handset (package signatures, app policy, the lock screen) protects against software that arrives through the system, not against that person. The [last section](#the-handset) says what a locked boot with a root of trust of the owner's would need on this phone, from the factory files alone. None of it has been tried.

## The chain on QEMU

1. **The bootloader model** ([tools/ota_boot.py](../tools/ota_boot.py)) picks a boot slot from the A/B record and spends a try for a slot on trial. Before loading it, it checks the update manifest in the slot's last sector (`VOT1`, what Storage writes when it switches slots and what `provision` writes for the factory image): signed for QEMU by a key the model recorded at provisioning (misc + 6, never read from the slot), a SHA-256 over the whole slot image, a version at least the stored floor, and the same version stated by the boot manifest the loader carries. A slot that fails is marked unbootable and the other slot is tried in the same power-on. It then hands the loader the floor. Plain `make emulator` runs without it: no slots, no stored floor, and the loader is the first check.
2. **The loader** ([boot/loader/loader.c](../boot/loader/loader.c), [verified.rs](../boot/loader/verified.rs)) carries one boot manifest and the keys the image pins: the release root alone in a release image, the build's developer key (and the release root, if its public half is in the repository) otherwise. Before it copies anything it checks, in this order:
   - the manifest's shape, target (`QEMU`) and key ID;
   - its ECDSA P-256 signature (BearSSL, the code the supervisor uses for packages);
   - that its system version is at least the rollback floor;
   - in a release loader, that it binds the `/apps` image;
   - SHA-256 and length of the kernel image and of the root task image;
   - when bound, SHA-256 of the `/apps` image where QEMU loaded it.

   On any failure it prints `VioletOS loader: boot refused, reason N`, paints the status band red and stops: the kernel and root task are never copied to where they run. On success it prints `VioletOS loader: verified boot, system version N` and continues as before.
3. **seL4** starts the root task.
4. **The supervisor** runs a package only if a key pinned in the root task signed it for this target and its version meets the floors. The pinned keys are inside the root task image, so they are under the loader's check.

| Reason | Meaning |
| --- | --- |
| 1 | No manifest, or not one this loader understands |
| 2 | Signed for another target |
| 3 | Signed by a key this loader does not pin (a developer-signed system on a release loader, for one) |
| 4 | Bad signature (any changed manifest byte) |
| 5 | Version below the rollback floor, version 0, or a damaged floor record |
| 6 | A release loader, and the manifest does not bind the `/apps` image |
| 7, 8, 9 | The kernel, the root task or the `/apps` image is not the one that was signed |

The manifest is 192 bytes, built and signed by [tools/boot_manifest.py](../tools/boot_manifest.py) at the end of the image build with the same signer as packages (`python3 tools/boot_manifest.py show build/sel4/qemu/zImage` prints it). Its magic (`VBM1`) keeps a package or update signature from passing as one.

### Rollback floor

The system's security version is `VIOLET_SYSTEM_VERSION` at build time (1 by default). The loader build signs the version note compiled into the root task (the number the running system and `tools/ota_bundle.py` use), and fails if `VIOLET_SYSTEM_VERSION` is set to anything else; a release build fails without it. The bootloader model keeps the floor in the sector after the A/B record: the highest version of a slot that passed the model's own check and has been **marked successful**. A system on trial does not raise it, so a failed trial can still fall back; once the new system has run healthy and been marked, the next power-on raises the floor. From then on an older system, an image with no update manifest, or one signed by a key the model does not hold is refused by the model and the other slot boots, whatever the A/B record says. The loader refuses it again from the floor it is handed. A damaged floor or key record boots nothing and spends no try.

This closes the gap the update path had: the running system already refused a bundle older than itself, but nothing stopped an older, validly signed image put straight into a slot.

A real bootloader keeps this number and its keys where the operating system cannot rewrite them (RPMB or fuses). The model keeps them in misc sectors that Storage's slot interface refuses (`slot_sector` in services/storage/block.rs accepts only the A/B record and the slots), so they hold against a compromised package manager or app using slot writes, not against code that drives the disk itself (Storage, or the host).

### What QEMU's check does not cover

- **The loader, `entry.bin` and the device tree handed to it, outside the bootloader model.** Through `tools/ota_boot.py` they are covered by the slot's signed manifest; with plain `make emulator` they are what QEMU is started with, and if the loader is replaced nothing is checked. A boot ROM that verified the loader is the missing first link, and QEMU has none.
- **A developer image's `/apps` image.** The local tests rebuild that image without relinking the loader, so a developer manifest leaves it unbound and says so in the build log. A release build always binds it, and a release loader refuses a manifest that does not.
- **Anyone with the developer key.** It is in the build directory.
- **The data disk** (app files, installed apps, the catalog banks). Packages in the banks are checked by signature; the rest is data.

### Tests

`make test-unit` runs [boot/loader/test_verified.sh](../boot/loader/test_verified.sh): the loader's check compiled for the host against real BearSSL and manifests signed through openssl (every manifest byte, every image, wrong and unknown keys, the other target, a release loader with a developer or unbound manifest, package and update signatures offered as boot manifests, the floor), [tools/test_boot_manifest.py](../tools/test_boot_manifest.py) (the tool; the bootloader model's floor across trial, success and fallback; an old successful slot put in front falling back to the newer one; older, unmanifested and foreign-key images written into a slot; a forged `VBM1` appended to a zImage; flipped slot and manifest bytes; damaged floor and key records; the build's version and loader checks). The loader build itself fails if the QEMU zImage does not carry its signed manifest exactly once, or lacks the verified-boot code (`boot_manifest.check_loader`). The loader itself was booted in QEMU by hand while this was written: an untouched image starts and draws the same first screen as before; a flipped byte in the kernel, the root task, the manifest or a bound `/apps` image, and a floor above the image's version, each stop at the refusal line. That is not yet in an automated QEMU test.

## The handset

Read from the factory package for firmware S3RHS32.20-42-13-2-3-2 (`boot.img`, `vbmeta.img`, `vbmeta_system.img`, `flashfile.xml`, `signing-info.txt`) and the bring-up notes kept beside it. The bootloader, preloader and trusted-execution images are named by that package but are not among the files held, so nothing below is a statement about their code.

### What the factory files show

- **Android Verified Boot 1.0, one Motorola key.** `vbmeta.img` is signed SHA256-RSA2048; its key's SHA-256 is `c2224571c9cd5c89200a7311b1e37aa9cf751e2e19753e8d3702bca00be1d42c`. It chains `vbmeta_system` to the same key. [tools/phone_image.py](../tools/phone_image.py) `avb_verify` reads both, and the signatures verify under the key they carry.
- **The boot image is bound by a hash descriptor in `vbmeta`**, not by a chain: salted SHA-256 over the first 22,202,368 bytes of the boot partition. The boot image's own footer carries the same descriptor unsigned. A locked bootloader therefore accepts a boot image only together with a `vbmeta` it trusts.
- **Rollback index 18 at location 0**, the same number `signing-info.txt` gives as `HAB_SECURITY_VERSION`. A locked bootloader refuses a `vbmeta` whose index is below the one it has stored.
- **The earlier stages have their own signing and counters.** `signing-info.txt` lists anti-rollback versions for the preloader, LK, ATF, TEE and others, and says the build is "customer signed" for customer ID 15, to be matched against the phone's own (`HAB_META = ellis_15` in `vbmeta`). On a MediaTek phone these stages are checked from the boot ROM down against a key whose hash is in fuses. No file here lets an owner change that key.
- **No sign of an owner-installable key.** The flash list writes `vbmeta_a`, `vbmeta_system_a`, `boot_a`, the bootloader stages and `super`; it writes no `avb_custom_key` partition, and nothing in the files mentions one. That is absence of evidence only.
- **The phone in use is unlocked**: it runs a boot image no Motorola key signed. Its installed bootloader (`MBM-2.1-ellis_vzw-…-231214`, from the bring-up notes) is newer than this package's, so its stored rollback index may be above 18.

### How a locked boot with the owner's key would work

Android Verified Boot defines one way for an owner to be the root of trust: the bootloader holds a second, owner-set public key (`avb_custom_key`), settable only while unlocked. Locked, it accepts a `vbmeta` signed by either the manufacturer's key or that one, and shows a warning with the key's fingerprint when it is the owner's. If this phone's bootloader implements that, VioletOS would boot locked like this:

1. Build the boot image as now. What the bootloader reads of `boot_b` is the Android v2 image: the VioletOS loader, seL4, the root task (supervisor, pinned package keys, factory catalog) and the ramdisk, which is the `/apps` image (`phone_image.v2_payload_length`).
2. Make a `vbmeta` whose hash descriptor covers exactly those bytes, signed with the owner's RSA key (`phone_image.avb_vbmeta`), with a rollback index at least the one the phone has stored.
3. With the bootloader unlocked: write `boot_b`, write that `vbmeta` to `vbmeta_b`, set the owner's key, then lock.
4. From then on the bootloader starts slot B only if `vbmeta_b` verifies under the owner's key and `boot_b` hashes to its descriptor. The supervisor inside that image checks every package. The chain is: boot ROM and Motorola's stages (Motorola's keys), then the owner's key from `vbmeta` down.

Three properties of VioletOS's layout matter here:

- **`boot_b` also holds data that changes** after the first 16 MiB: the storage volume, boot history, catalog banks and, until `userdata` is claimed, installed apps. A hash descriptor in `vbmeta` covers only its stated length from the start of the partition, so those sectors stay outside it, as they must. The packages in the banks are covered by their own signatures. An AVB footer at the end of the partition would collide with them and is not needed.
- **The rollback index would be `VIOLET_SYSTEM_VERSION` plus whatever base clears the stored index.** The index location is shared with the Android images this phone ran before.
- **The device tree the loader receives is assembled by the bootloader** from the boot image and `dtbo_b`. The factory `vbmeta` has no descriptor for `dtbo`, so that input would stay outside the owner's check; the loader validates the layout it reads from it.

### Why it has not been done, and what is not known

- **Whether the bootloader has an owner key at all.** Unknown from these files; many phones' do not. Without it there is no owner root of trust on this phone short of replacing stages the boot ROM verifies, and locking would mean booting only Motorola-signed systems.
- **Whether a locked phone could be unlocked again.** Unlocking is normally gated by a setting inside Android and Motorola's unlock code. This phone no longer has Android after `moto-flash --take-userdata`. A locked phone whose VioletOS image fails to verify might not be recoverable.
- **The stored rollback index**, and what locking does to `userdata`, which now holds VioletOS's data.
- Even done perfectly, it would **not** cover Motorola's bootloader, the secure monitor and trusted-execution environment (closed, and able to change memory protection: see [the kernel's assurance statement](../kernel/sel4/README.md)), or the modem.

Finding out the first item needs only the bootloader image from the same factory package, read on a computer. The rest needs the phone. Until someone decides to take that risk with a phone they can afford to lose, the handset loader carries no boot manifest: a check inside an image that the bootloader will replace on request proves nothing about replacement, and the handset loader runs with caches off under an armed watchdog, where hashing the image would add seconds to every start.
