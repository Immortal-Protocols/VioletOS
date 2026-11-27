# Package signing keys

Every VioletOS package (VPK2) carries an ECDSA P-256 signature over its header, image, security version and target ([sign.py](../packages/sign.py)). The supervisor runs a package only if a key pinned in its boot image signed it ([verify.rs](../../system/package-manager/verify.rs)). Two kinds of key exist, with different IDs so one can never stand in for the other.

| Key | ID | Where the private half lives | Pinned in |
| --- | --- | --- | --- |
| Developer | 2 | `build/sel4/<target>/developer-signing.pem`, generated on the first build of that target on this machine, mode 0600, never checked in | Developer images (the default build) only |
| Release root | 1 | Outside this repository (an offline machine or an HSM) | Release images; also developer images, when the public key file exists |

## Developer builds

`make qemu-image` and `make moto` sign every package with the developer key and pin its public half. Nothing needs to be configured. A package built on another machine, or after the key file was deleted, is refused by an older image: rebuild and flash the image, or keep the key file. CI generates a new developer key on every run.

A developer image is not a production image: anyone with that build directory can sign packages for it.

## Release builds

```sh
VIOLET_RELEASE=1 VIOLET_SIGNING_KEY=/secure/release-root.pem make moto
```

A release build pins only the release root's public key, read from `image/keys/release-root.pub.pem` (or the file named by `VIOLET_RELEASE_PUBLIC_KEY`), and signs with `VIOLET_SIGNING_KEY`, a PEM private key, or with `VIOLET_SIGN_COMMAND`, a command that reads the bytes to sign on standard input and writes a DER ECDSA-SHA256 signature to standard output (for a key that stays in an HSM). The build stops if the public key is missing, if there is no way to sign, or if a signature it gets does not verify under the pinned key. A release image contains no developer key, so it refuses developer-signed packages, and it has no USB activation without confirmation on the handset. USB Diagnostics, which is only ever replaced through that USB path, therefore cannot be updated in a release image except by a new boot image.

`VIOLET_RELEASE` must be exactly `1` for a release. Any other value except unset, empty or `0` stops the build (`VIOLET_RELEASE=true` is an error, not a developer build), and so does `VIOLET_SIGNING_KEY` or `VIOLET_SIGN_COMMAND` without `VIOLET_RELEASE=1`. The image build ends with a line that says which it made, for example `Package signing: DEVELOPER image, not for release. Trusts: developer (ID 2); ...`, and `package-keys.json` beside the image records the same.

**No release root key has been created yet**, so `image/keys/release-root.pub.pem` does not exist and `VIOLET_RELEASE=1` fails until someone generates the key pair and commits the public half:

```sh
openssl ecparam -name prime256v1 -genkey -noout -out release-root.pem   # keep offline
openssl ec -in release-root.pem -pubout -out image/keys/release-root.pub.pem
```

Who holds the private key, how releases are signed with it and how it would be replaced are decisions this repository cannot make. There is one root key and no revocation list: retiring a key means shipping a boot image that pins a different one.

## What the signature does not cover

The boot image itself (loader, kernel, supervisor, the pinned keys and the factory catalog) is not verified by anything while the handset's bootloader is unlocked. Whoever can flash the boot partition can replace the keys. Package signatures stop a compromised service, the USB host or a tampered catalog bank from running unsigned code; they do not stop someone who can flash the phone.
