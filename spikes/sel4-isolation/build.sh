#!/bin/sh
set -eu
src=$(cd "$(dirname "$0")" && pwd)
: "${MICROKIT_SDK:?Point MICROKIT_SDK at the extracted 2.4.0 SDK}"
out=${1:?Supply a build directory}
mkdir -p "$out"
out=$(cd "$out" && pwd)
lib=$MICROKIT_SDK/board/qemu_virt_aarch64/debug/lib
host=$(rustc -vV | sed -n 's/^host: //p')
lld="$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld"
for pd in notes network; do
    rustc --edition=2021 --target=aarch64-unknown-none --emit=obj -C panic=abort -C opt-level=s \
        --cfg "$pd" "$src/service.rs" -o "$out/$pd.o"
    "$lld" -flavor gnu "$out/$pd.o" -L"$lib" -lmicrokit -T"$lib/microkit.ld" -o "$out/$pd.elf"
done
sed '/<map mr="packet_page"/a\
        <map mr="note_page" vaddr="0x40000000" perms="r" />
' "$src/isolated.system" > "$out/granted.system"
for case_name in isolated granted; do
    system="$out/granted.system"
    if [ "$case_name" = isolated ]; then system="$src/isolated.system"; fi
    "$MICROKIT_SDK/bin/microkit" "$system" --search-path "$out" --board qemu_virt_aarch64 --config debug \
        --capdl-json "$out/$case_name.json" -r "$out/$case_name.report" -o "$out/$case_name.img"
done
