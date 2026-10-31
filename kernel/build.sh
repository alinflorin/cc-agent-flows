#!/usr/bin/env bash
# Builds the agent VM kernel for this machine's architecture from the kernel.org source named in
# version.env: `make tinyconfig` plus common.config and <arch>.config, everything built in (no
# modules, no initramfs). Writes out/vmlinuz-<arch>, out/config-<arch>. Native build only; runs on
# a CI runner or any Linux with build-essential bc bison flex libelf-dev libssl-dev lz4 curl.
#
#   kernel/build.sh [x86_64|aarch64]        build
#   kernel/build.sh <arch> menuconfig       tweak interactively (the result is not saved)
set -euo pipefail
cd "$(dirname "$0")"
here=$PWD
host=$(uname -m); [ "$host" = arm64 ] && host=aarch64
arch=${1:-$host}; mode=${2:-build}
[ "$arch" = "$host" ] || { echo "build on a $arch machine (native build only)" >&2; exit 1; }
. ./version.env
case $arch in
  x86_64) export ARCH=x86 image=arch/x86/boot/bzImage ;;
  aarch64) export ARCH=arm64 image=arch/arm64/boot/Image ;;
esac
mkdir -p .cache out
tarball=.cache/linux-$KERNEL_VERSION.tar.xz
if [ ! -f "$tarball" ]; then
  curl -fL --retry 5 "https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_VERSION%%.*}.x/linux-$KERNEL_VERSION.tar.xz" -o "$tarball.part"
  mv "$tarball.part" "$tarball"
fi
echo "$KERNEL_SHA256  $tarball" | sha256sum -c - >/dev/null || { echo "bad hash for $tarball" >&2; rm -f "$tarball"; exit 1; }
src=${KERNEL_SRC:-$here/.cache/src-$KERNEL_VERSION}
if [ ! -f "$src/Makefile" ]; then mkdir -p "$src"; tar -xf "$tarball" -C "$src" --strip-components=1; fi
cd "$src"
make tinyconfig >/dev/null
cat "$here/common.config" "$here/$arch.config" >"$here/.cache/fragment-$arch"
scripts/kconfig/merge_config.sh -m -O . .config "$here/.cache/fragment-$arch" >/dev/null 2>&1
make olddefconfig >/dev/null
# every option asked for must have survived (a missing dependency silently drops it)
missing=0
while read -r line; do
  case $line in
    CONFIG_*=*) name=${line%%=*}; want=${line#*=}; got=$(grep -m1 "^$name=" .config || true)
      [ "${got#*=}" = "$want" ] && [ -n "$got" ] || { echo "not set as asked: $line (got: ${got:-nothing})" >&2; missing=1; } ;;
    "# CONFIG_"*" is not set") name=${line#\# }; name=${name% is not set}
      ! grep -q "^$name=" .config || { echo "should be off: $name" >&2; missing=1; } ;;
  esac
done <"$here/.cache/fragment-$arch"
[ $missing = 0 ] || exit 1
[ "$mode" = menuconfig ] && make menuconfig
make -j"$(nproc)" "$(basename "$image")" >/dev/null
cp "$image" "$here/out/vmlinuz-$arch"
cp .config "$here/out/config-$arch"
ls -l "$here/out/vmlinuz-$arch"
