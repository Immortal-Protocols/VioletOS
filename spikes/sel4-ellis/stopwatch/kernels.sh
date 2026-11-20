#!/bin/sh
set -eu
repo=$(git rev-parse --show-toplevel)
src="$repo/build/spike2/seL4-17.0.0"
# Reuse the prepared Ellis source and the toolchain from trial/build.py.
for variant in fixture measured; do
    overlay=
    if [ "$variant" = fixture ]; then overlay="$repo/spikes/sel4-ellis/trial/qemu-overlay.dts"; fi
    out="$repo/build/spike2/watch-kernel-$variant"
    cmake -GNinja -S "$src" -B "$out" -DCMAKE_TOOLCHAIN_FILE="$src/llvm.cmake" \
        -DTRIPLE=arm-none-eabi -DPYTHON3="$(command -v python3)" \
        -DKernelPlatform=ellis -DKernelSel4Arch=aarch32 -DKernelIsMCS=OFF \
        -DKernelPrinting=OFF -DKernelDebugBuild=OFF -DKernelVerificationBuild=OFF \
        -DKernelArmExportVCNTUser=ON -DKernelCustomDTSOverlay="$overlay"
    cmake --build "$out" --target kernel.elf
done
