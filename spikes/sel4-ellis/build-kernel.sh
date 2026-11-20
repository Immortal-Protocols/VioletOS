#!/bin/sh
# Prepare a fresh seL4 17.0.0 source copy. Output is a kernel ELF for inspection.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "${1:?supply a disposable pristine seL4 source tree}" && pwd)
stock=${2:?supply the factory boot.img}
out=${3:?supply an output directory}
mkdir -p "$out"
out=$(cd "$out" && pwd)
plat="$src/src/plat/ellis"
abi="$src/libsel4/sel4_plat_include"
mkdir -p "$plat" "$abi/ellis/sel4/plat/api"
python3 - "$stock" "$out/base.dtb" <<'PY'
from pathlib import Path
import hashlib, struct, sys
data = Path(sys.argv[1]).read_bytes()
if hashlib.sha256(data).hexdigest() != "36adc5df15662da6140e14b81dbd2de4b0701164c2198f99608bee710d4dac0a":
    raise ValueError("expected the exact XT2163DL factory image")
page, version = struct.unpack_from("<II", data, 36)
assert version == 2
start = page
for field in (8, 16, 24, 1632):
    size, = struct.unpack_from("<I", data, field)
    start += (size + page - 1) // page * page
magic, total, _, entry_size, count, entries = struct.unpack_from(">6I", data, start)
assert magic == 0xd7b7ab1e and count == 1 and entry_size == 32
size, offset = struct.unpack_from(">II", data, start + entries)
assert offset + size <= total and struct.unpack_from(">II", data, start + offset) == (0xd00dfeed, size)
Path(sys.argv[2]).write_bytes(data[start + offset:start + offset + size])
PY
python3 "$here/platform/gic-single-core.py" "$src"
dtc -q -I dtb -O dts -o "$plat/factory-base.dts" "$out/base.dtb"
cp "$here/platform/config.cmake" "$here/platform/kernel-overlay.dts" "$plat/"
# Same upstream AArch32 Cortex-A53 constants and user/kernel address split.
cp "$abi/imx8mm-evk/sel4/plat/api/constants.h" "$abi/ellis/sel4/plat/api/"
cmake -GNinja -S "$src" -B "$out" -DCMAKE_TOOLCHAIN_FILE="$src/llvm.cmake" \
    -DTRIPLE=arm-none-eabi -DPYTHON3="$(command -v python3)" \
    -DKernelPlatform=ellis -DKernelSel4Arch=aarch32 -DKernelIsMCS=OFF \
    -DKernelPrinting=OFF -DKernelDebugBuild=OFF -DKernelVerificationBuild=OFF
cmake --build "$out" --parallel 4 --target kernel.elf
