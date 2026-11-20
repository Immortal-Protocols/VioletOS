#!/bin/sh
# SDK 2.4.0 qemu_virt_aarch64/debug settings. Use a fresh output directory for timing.
set -eu
src=${1:?supply seL4 17.0.0 source directory}
out=${2:?supply kernel build directory}
src=$(cd "$src" && pwd)
cmake -GNinja -S "$src" -B "$out" -DCMAKE_TOOLCHAIN_FILE="$src/llvm.cmake" \
    -DTRIPLE=aarch64-none-elf -DPYTHON3="$(command -v python3)" \
    -DKernelPlatform=qemu-arm-virt -DKernelSel4Arch=aarch64 -DQEMU_MEMORY=2048 \
    -DKernelIsMCS=ON -DKernelRootCNodeSizeBits=17 -DLibSel4UseThreadLocals=OFF \
    -DKernelNumDomains=16 -DKernelNumDomainSchedules=100 -DKernelArmExportPCNTUser=ON \
    -DKernelArmHypervisorSupport=ON -DKernelArmVtimerUpdateVOffset=OFF -DKernelAllowSMCCalls=ON \
    -DKernelDebugBuild=ON -DKernelPrinting=ON -DKernelVerificationBuild=OFF \
    -DHardwareDebugAPI=ON -DKernelArmExportPTMRUser=ON
cmake --build "$out" --parallel 4 --target kernel.elf
