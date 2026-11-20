#!/bin/sh
set -eu
: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to your NDK directory}"
for spec in 'aarch64 aarch64-linux-android' 'armv7 armv7a-linux-androideabi'; do
 set -- $spec
 compiler=$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/"${2}24-clang" | head -1)
 "$compiler" -Os -Wall -Wextra -static -s src/screen.c src/framebuffer.c -o "assets/violet-$1"
done
