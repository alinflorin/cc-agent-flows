#!/usr/bin/env bash
# Builds the Alpine base image for an agent VM: a compressed qcow2 with Claude Code installed.
# Linux only; needs root (loop/nbd mounts) plus qemu-img, qemu-nbd, rsync, sfdisk, dosfstools, jq.
#
#   build.sh <x86_64|aarch64> <out-dir>
#
# aarch64 must be built on an aarch64 host. x86_64 boots with BIOS, aarch64 with UEFI.
set -euo pipefail

arch=$1
out=$2
here=$(cd "$(dirname "$0")" && pwd)
cfg="$here/version.json"

branch=$(jq -r .alpine "$cfg")

work=$(mktemp -d)
trap 'sudo rm -rf "$work"' EXIT
mkdir -p "$out"

sudo modprobe nbd max_part=8 || true
sudo "$here/vendor/alpine-make-vm-image" \
  --arch "$arch" \
  --branch "$branch" \
  --image-format qcow2 \
  --image-size 4G \
  --serial-console \
  --packages "alpine-base bash git curl ca-certificates ripgrep libgcc libstdc++" \
  --script-chroot \
  "$work/raw.qcow2" -- "$here/provision.sh"

# compress, and drop the free space the build left behind
sudo qemu-img convert -O qcow2 -c "$work/raw.qcow2" "$out/agent-base-$arch.qcow2"
sudo chown "$(id -u):$(id -g)" "$out/agent-base-$arch.qcow2"
