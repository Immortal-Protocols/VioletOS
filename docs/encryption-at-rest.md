# Encryption at rest

What is on the disk when the phone is off, who can read it, and what is still missing. The code is [services/storage/crypt](../services/storage/crypt/mod.rs); it is pure Rust with no hardware or IPC in it, and `make test-unit` runs it on the host.

## Status

| Piece | State |
|---|---|
| Sector cipher (XTS-AES-256, constant time) | Done, tested on the host against published vectors. |
| `CryptDev<B: BlockDev>`, `crypt::open(dev, &volume_key)` | Done, tested on the host with VioletFS mounted on it, power cuts included. Type-checked and linked for ARM; not yet run on a target. |
| Key hierarchy, key block, passcode rewrap, attempt counter | Done, tested on the host. |
| QEMU root key (provisioning blob) | Tool, emulator option and the page mapping are in; **not run in QEMU yet** (no image was built in this change). A stand-in, see below. |
| Storage mounting VioletFS on the encrypted device | **Not in this change.** [volume.rs](../services/storage/volume.rs) has the call site with a pass-through in its place; until that is switched to this module `/data` is not encrypted. |
| Agent-built app store on the encrypted volume | **Not wired.** `crypt::open_extent(extent, &volume_key, crypt::APPS)` is the confidentiality layer for it; Storage's `APPSTORE` path does not go through it yet. |
| App store and `/data` integrity and anti-rollback | **Not done.** XTS does not authenticate: someone who can write the raw medium can put back an older ciphertext of the app store (an installed app downgraded to a version with a known bug, run under its current name and grants) or of `/data` (an older sealed `.grants`, older pairing secrets). A MAC over the app store directory bound to a counter the disk cannot roll back (RPMB on the handset) is what it takes; none of it is here. |
| Trusted UI passcode feeding the key block | **Not wired.** The lock screen still keeps its own record (see "The passcode"). |
| Handset root key | **Not done.** There is no hardware-bound key on the Moto G Pure under VioletOS. See "The handset". |

So today nothing on a running image is encrypted. What exists is the tested layer and its key handling, ready to sit under the filesystem.

## The cipher

Each 512-byte sector is encrypted with XTS-AES-256 (IEEE 1619-2007, NIST SP 800-38E). The tweak is the sector's number as the filesystem sees it.

- **Constant time.** AES is bitsliced ([aes.rs](../services/storage/crypt/aes.rs), a port of BearSSL's `aes_ct`): the S-box is a Boolean circuit, there is no table indexed by key or data and no branch on either. The tweak's multiplication uses a mask. Tag and seal comparisons do not exit early.
- **Tested against** FIPS-197 C.3 (AES-256), all 256 S-box values against the field arithmetic, IEEE 1619 vectors 10 to 14 (the 512-byte XTS-AES-256 ones), NIST SHA-256 examples and RFC 4231 HMAC cases ([kat.rs](../services/storage/crypt/kat.rs)).
- **What XTS does not do.** It does not authenticate. Someone who can write the raw disk can turn any 16-byte block into noise, or put back an older ciphertext of the same sector, and the cipher will not notice. VioletFS's metadata checksums catch the first for metadata; file data has no checksum. Someone who reads the disk twice learns which 16-byte blocks changed. An authenticated mode needs a tag per sector kept consistent with the data across power cuts, and a counter the attacker cannot roll back; neither is here.
- **Speed.** About 80 MiB/s on the build Mac. Not measured on the handset or in QEMU; a rough guess for the Cortex-A53 in 32-bit mode is 5 to 10 MiB/s.
- **Size.** 9,664 bytes of code and 477 of constants for ARM at `-Copt-level=s` (measured by linking the module alone), and about 5 KiB of RAM per open device. It goes into Storage, never into the Open Runtime.

## Keys

```
device root key (32 bytes, from the platform, never on this disk)
  |-- seal key  = HMAC(root, "violet keyblock seal v1")
  `-- key-encryption key = HMAC(root, "violet kek v1" | flags | salt | passcode secret)
        `-- wraps the volume key (encrypt, then HMAC)
volume key (32 random bytes, made once when the volume is formatted)
  |-- /data filesystem:  XTS key = HMAC(volume key, "violet volume v1" | 1 | 0..1)
  `-- installed apps:    XTS key = HMAC(volume key, "violet volume v1" | 2 | 0..1)
```

The volume key exists on disk only wrapped, in the **key block**: one sector kept twice at the start of the volume (the first 4 KiB are reserved for it). The block also holds the passcode's PBKDF2 salt and iteration count, and the count of wrong attempts. Every change goes over the older copy and is flushed, so a power cut leaves the old block or the new one ([keys.rs](../services/storage/crypt/keys.rs) has the layout; [tools/root_key.py](../tools/root_key.py) `key_block` builds the same bytes in Python, and both sides' tests check the same vectors).

- **The passcode.** The passcode secret is the trusted UI's PBKDF2-HMAC-SHA-256 value of the digits. With no passcode set it is 32 zero bytes and a flag says so: the volume then opens with the root key alone. **Changing the passcode wraps the same volume key again; no sector of the volume is rewritten** (a test compares the whole volume before and after). Both copies of the block are replaced, so the old passcode's block is not left on the disk for the cipher to accept (the flash chip may still hold the old sector internally).
- **Attempts.** With a passcode set, an attempt is counted, sealed and flushed *before* the passcode is checked; if that write fails there is no attempt. After 5 wrong ones the next is refused for 30 s, doubling to an hour (the trusted UI's schedule). A new `Throttle` (Storage restarted, or the device rebooted) waits the whole delay for the stored count, counted from its first use, so neither a service restart with the clock running nor a reboot skips it. The count is sealed with a key from the root key, so it cannot be edited without it. The delay lives in the caller's memory, not on the disk: restarting Storage over and over costs the attacker the full delay each time, it never shortens it.
- **Fail closed.** A block that does not verify (damage, or another device's disk) is `Damaged`, never treated as empty. `Blank` means every byte of the 8 header sectors holds one value (a new or erased disk); a key block with damaged bytes anywhere, magic included, or another filesystem's first sectors, is `Damaged`. Only `Blank` is formatted, and only when the caller supplies random bytes; the volume code should supply them only while it has no volume header of its own. A copy that cannot be read counts as missing: the other copy still unlocks, and `Io` comes back only when no copy verifies.
- **Passcode change.** `set_pin` returning `Ok` means the new passcode is in effect (its first copy is durable and newest). If the second copy could not be written, the older copy is replaced at the next unlock, which writes both copies. `Err(Io)` from the write of the first copy is the one ambiguous case: the caller must try both passcodes.
- **Erase.** `format` discards the 8 header sectors, overwrites them with zeros and flushes before it writes the new block, so a power cut gives the old key, a blank header or the new key, and never the old key once the new block was written (a power-cut sweep checks it). That is a crypto-erase only as far as the medium forgets: an eMMC's flash translation layer may keep the old key block's page until it erases it, and a discard is advice, not a secure trim. Someone who recovers that page by chip-off, and has the root key, gets the old volume key. Making it final needs a secure trim or a nonce the medium cannot keep (RPMB), neither of which is here.
- **Zeroing and logs.** Keys sit in a type that zeroes itself when dropped, has no `Debug`, and is never formatted; working buffers (hash state, key schedules, S-box temporaries, tweaks) are zeroed after use and the key schedule is built in place. Errors carry counters only. This is best effort: the compiler may still leave copies on the stack, and the root key blob stays in its page for Storage's whole life by design (see the QEMU section).
- **Not protected: rollback.** Someone who can write the raw disk can put back an older key block (an older attempt count, or the previous passcode's wrap if they saved it) or damage the newer copy so the older one is used. Stopping that needs a counter the disk cannot roll back (RPMB on the handset).

What still has to change in the trusted UI for the passcode to mean anything here: today [sysui.rs](../runtime/open/sysui.rs) stores the PBKDF2 output itself in the runtime's private file and compares against it. That value must not be stored anywhere once it is the passcode secret; the salt and iteration count move to the key block (`crypt::params`), "right passcode" becomes "the key block unwrapped" (`crypt::unlock`), and the first unlock after boot happens before `/data` is mounted.

## QEMU: the provisioning blob (a stand-in)

On QEMU the root key comes from a 64-byte blob that [tools/root_key.py](../tools/root_key.py) makes (`<disk>.rootkey`, mode 0600, beside the disk image and never inside it; `VIOLET_ROOT_KEY` names another). [tools/run.py](../tools/run.py) loads it into guest memory at `0x49003000`, the supervisor maps that page into Storage only and read-only, and `crypt::provision::from_page` reads it. The page stays mapped (Storage must find the key again after a restart) and the supervisor keeps the frame; on QEMU the page is also in the emulator's guest RAM on the Mac. Deleting the blob makes the volume unreadable for good.

This models *where a hardware key would come from*, not a security boundary: anyone with the Mac's files has both the disk and the key. It lets the whole path (format, unlock, mount, passcode change) be exercised without hardware. With no blob the page is empty, there is no root key, and the volume stays closed.

## The handset: not done

**There is no root key on the Moto G Pure (XT2163DL) under VioletOS, and nothing in this change touches the phone.** Without one, encryption there would protect nothing from someone holding the device, so it is not enabled. What follows is what it would take, from the factory package and the stock kernel configuration only (`build/xt2163dl/S3RHS32.20-42-13-2-3-2`: `flashfile.xml`, `signing-info.txt`, `config.gz`); none of it was tried.

What the factory files show:

- The secure world is Trustonic's TEE: `CONFIG_TRUSTONIC_TEE_SUPPORT=y`, `CONFIG_TRUSTONIC_TEE_VERSION="500"`, `CONFIG_MTK_TEE_GP_SUPPORT=y`. It is flashed as `tee.img` into `tee_a`, beside `gz.img` (MediaTek's hypervisor) and the preloader and `lk`. VioletOS replaces only `boot_b`, so these still run before it.
- `CONFIG_RPMB=y` and `CONFIG_MTK_EMMC_SUPPORT=y`: the eMMC's replay-protected block is in use by the stock system. `CONFIG_MTK_HW_FDE` and `CONFIG_FSCRYPT_WRAPED_KEY_MODE_SUPPORT` are off and `CONFIG_DM_CRYPT=y`, which suggests stock Android encrypts in software with keys its TEE keymaster protects, not with an inline crypto engine.
- `signing-info.txt`: the firmware is customer signed with anti-rollback versions for the preloader, ATF, TEE and `lk`. The TEE and its trusted applications are signed by keys we do not have.

How a root key would have to be obtained (points 1 to 3 rest on how MediaTek and Trustonic systems are generally built, not on anything read from this phone):

1. **The hardware unique key (HUK) is reachable only inside the TEE.** On MT6765 it is derived from eFuses by the crypto engine in the secure world; the normal world never sees it. A device-bound key therefore means asking a trusted application for a key derived from the HUK (what Android's keymaster does), over the Trustonic normal-world protocol: fast calls into the secure monitor, a shared-memory session, and the trusted application's own messages.
2. **That needs three things VioletOS does not have.** (a) A Trustonic client in a VioletOS component, including loading the vendor's trusted application binary, which normally ships in the vendor image inside `super` and is signed for this TEE. (b) Secure-monitor calls from user space: the kernel here allows only two fixed operations for the Wi-Fi windows ([targets/moto-g-pure/README.md](../targets/moto-g-pure/README.md)), deliberately; a TEE client needs a wider, carefully bounded allow-list. (c) The TEE's normal-world services (it expects the normal world to store its files and relay RPMB frames).
3. **The attempt counter and anti-rollback need RPMB.** RPMB accepts only writes authenticated with a key that was programmed once by the stock firmware and is known only inside the TEE. So RPMB is usable only through the TEE as well, with Storage's eMMC driver relaying the frames.
4. **None of it means much while the bootloader is unlocked.** VioletOS boots because the bootloader is unlocked and `boot_b` is flashed over fastboot. Anyone holding the phone can flash an image of their own that asks the TEE for the same key. A hardware-bound key protects data only together with a boot chain that lets the key out to the installed system alone (verified boot with our key, or a TEE that binds keys to the booted image). The USB diagnostics channel is in the same position.

Until then, what the software alone can offer on the handset is the passcode wrap with a root key kept on the same flash chip: that would make the passcode the only secret, and a 6-digit passcode at 100,000 PBKDF2 iterations falls to an offline search on a graphics card in minutes to hours. That is why it is not presented as encryption at rest and is not switched on.

## Tests

`make test-unit` builds [crypt/tests/host.rs](../services/storage/crypt/tests/host.rs) (28 tests, a tenth of a second) and runs [tools/test_root_key.py](../tools/test_root_key.py):

- the published vectors above;
- a blank disk is formatted once; another root key, a damaged block (magic included), a plaintext VioletFS or a wrong passcode never formats and never unlocks; one unreadable copy still unlocks, two give `Io`;
- the passcode: needed, counted before it is checked (the disk failing the counter's write gives no key), delayed after 5, not skipped by a reboot or by a Storage restart long after boot;
- a passcode change whose second copy fails is in effect and repaired at the next unlock;
- erasing (`format`) over a used key block, swept through every power cut: the old key never returns after the new block was written;
- changing and removing it: the volume's sectors are untouched, the old passcode opens neither copy;
- a power cut at every write and flush of a passcode change, with torn sectors: the old or the new passcode opens the volume, with the same volume key;
- the device: only ciphertext below, the key block out of reach, bounds, zeros (not ciphertext) on a failed read, separate keys per region;
- VioletFS on top: no file name or file data in the raw image, mount after a restart with the passcode, no mount with another key, and a power-cut sweep through the cipher.

Nothing here runs in QEMU or on the phone, and no test in tests/integration covers it yet.
