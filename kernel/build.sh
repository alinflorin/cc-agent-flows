#!/usr/bin/env bash
# Builds the agent VM kernel for this machine's architecture, in a container (docker), from the
# kernel.org source named in version.env and the config in config-<arch>, and writes
# ../vendor/images/alpine-<arch>.vmlinuz (the plain image's kernel, see ../scripts/images).
#
#   kernel/build.sh [x86_64|aarch64]        build
#   kernel/build.sh <arch> menuconfig       edit config-<arch> (saved as a minimal defconfig)
#   kernel/build.sh <arch> derive           (re)make config-<arch> from Alpine's linux-virt config
#
# The kernel has virtio, ext4 and everything else it needs built in: no modules, no initramfs.
set -euo pipefail
cd "$(dirname "$0")"
here=$PWD
host=$(uname -m); [ "$host" = arm64 ] && host=aarch64
arch=${1:-$host}; mode=${2:-build}
[ "$arch" = "$host" ] || { echo "build on a $arch machine (native build only)" >&2; exit 1; }
. ./version.env
case $arch in
  x86_64) karch=x86 image=arch/x86/boot/bzImage ;;
  aarch64) karch=arm64 image=arch/arm64/boot/Image ;;
esac
docker build -q -t ccaf-kernel-build -f Dockerfile . >/dev/null
mkdir -p .cache out
tarball=.cache/linux-$KERNEL_VERSION.tar.xz
if [ ! -f "$tarball" ]; then
  curl -fL "https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_VERSION%%.*}.x/linux-$KERNEL_VERSION.tar.xz" -o "$tarball.part"
  mv "$tarball.part" "$tarball"
fi
echo "$KERNEL_SHA256  $tarball" | sha256sum -c - >/dev/null || { echo "bad hash for $tarball" >&2; rm -f "$tarball"; exit 1; }

# runs in the container: /src is this folder, /work the throwaway build tree
inner=$(cat <<'EOS'
set -euo pipefail
mkdir /work && cd /work
tar -xf "/src/.cache/linux-$KERNEL_VERSION.tar.xz" --strip-components=1
export ARCH=$KARCH
case $MODE in
  derive)
    cp /src/alpine-$TARGET.config .config
    make olddefconfig >/dev/null
    scripts/config --disable MODULES
    make olddefconfig >/dev/null
    cat /src/ccaf.fragment >> .config
    make olddefconfig >/dev/null
    make savedefconfig >/dev/null
    cp defconfig /src/config-$TARGET
    ;;
  menuconfig)
    cp /src/config-$TARGET .config; make olddefconfig >/dev/null
    make menuconfig; make savedefconfig >/dev/null; cp defconfig /src/config-$TARGET
    ;;
  build)
    cp /src/config-$TARGET .config
    make olddefconfig >/dev/null
    make -j"$(nproc)" "$(basename "$IMAGE")" >/dev/null
    cp "$IMAGE" /src/out/vmlinuz-$TARGET
    cp .config /src/out/config-$TARGET.full
    ;;
esac
EOS
)
tty=(); [ "$mode" = menuconfig ] && tty=(-it)
docker run --rm "${tty[@]}" -v "$here":/src -e KERNEL_VERSION="$KERNEL_VERSION" -e KARCH=$karch -e TARGET=$arch -e IMAGE=$image -e MODE=$mode \
  ccaf-kernel-build bash -c "$inner"
if [ "$mode" = build ]; then
  cp "out/vmlinuz-$arch" "../vendor/images/alpine-$arch.vmlinuz"
  ls -l "out/vmlinuz-$arch"
fi
