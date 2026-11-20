# Moto G Pure kernel build

The tested 32-bit ARM kernel uses Motorola's Android 12 Ellis release MMI-S3RHS32.20-42-10-4-11 at commit fbd7d0d5c865e48fcd7e3e7483d1e1c0d34c789d. The handset ran firmware S3RHS32.20-42-13-2-3-2. These are related releases. The saved stock.config comes from the running factory kernel. The patch and kernel configuration are derived from the Motorola Linux tree and retain the upstream licensing terms.

The build used Debian bookworm on 64-bit ARM, Clang and LLD 14, GNU ARM cross-binutils and four parallel jobs. Install the Linux build tools, including build-essential, gcc-arm-linux-gnueabi, clang, lld, llvm, bc, flex, bison, libssl-dev, libelf-dev, cpio, xz-utils and Python 3. Use a Linux filesystem for the source and outputs.

From the VioletOS checkout, download the pinned source archive and build into a fresh directory. Requires Bash.

```bash
set -eu
repo=$PWD
revision=fbd7d0d5c865e48fcd7e3e7483d1e1c0d34c789d
work=$(mktemp -d)
mkdir "$work/src" "$work/out"
curl -fL --retry 2 "https://codeload.github.com/MotorolaMobilityLLC/kernel-mtk/tar.gz/$revision" -o "$work/source.tar.gz"
tar xzf "$work/source.tar.gz" -C "$work/src" --strip-components=1
cd "$work/src"
patch -p1 < "$repo/kernel/ellis/build-compat.patch"
cp "$repo/kernel/ellis/stock.config" "$work/out/.config"
args=(O="$work/out" ARCH=arm CROSS_COMPILE=arm-linux-gnueabi- CC=clang HOSTCC=clang LD=ld.lld CLANG_TRIPLE=arm-linux-gnueabi- LOCALVERSION=+ KBUILD_BUILD_USER=violet KBUILD_BUILD_HOST=ellis)
make "${args[@]}" olddefconfig
make "${args[@]}" -j4 zImage modules
```

The handset kernel is at `$work/out/arch/arm/boot/zImage`. The factory touch modules' versioned imports matched the tested kernel. The build timestamp can change the binary hash. Provide the factory boot image, device-specific vendor files and the new kernel to the candidate builder described in the main README.

## Measured build time

One run on the development Apple Silicon Mac used four parallel jobs in an ARM64 Debian container. With sources extracted and dependencies installed, configuring a fresh output directory and building the kernel image plus modules took **3 minutes 31.6 seconds**. Immediately rerunning the same image and module targets with unchanged sources took **9.5 seconds**. These are wall-clock measurements. Downloading sources, installing packages, building the VioletOS ramdisk and flashing the handset were outside the timed kernel build.
