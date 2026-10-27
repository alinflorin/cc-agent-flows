#!/usr/bin/env bash
# Builds the Alpine image for an agent profile: a compressed qcow2 with Claude Code installed plus
# the profile's extra packages (profiles/<profile>.json).
# Linux only; needs root (loop/nbd mounts) plus qemu-img, qemu-nbd, rsync, sfdisk, dosfstools, jq.
#
#   build.sh <x86_64|aarch64> <out-dir> [profile]   (profile defaults to base)
#
# aarch64 must be built on an aarch64 host. x86_64 boots with BIOS, aarch64 with UEFI.
set -euo pipefail

arch=$1
out=$2
profile=${3:-base}
here=$(cd "$(dirname "$0")" && pwd)
cfg="$here/version.json"
pcfg="$here/profiles/$profile.json"
[ -f "$pcfg" ] || { echo "unknown profile '$profile' (no $pcfg)" >&2; exit 1; }

branch=$(jq -r .alpine "$cfg")
size=$(jq -r '.image_size // "4G"' "$pcfg")
# word-split on purpose: one argument per package
read -ra packages <<<"$(jq -r '.packages | join(" ")' "$pcfg")"
image="agent-$profile-$arch.qcow2"

work=$(mktemp -d)
trap 'sudo rm -rf "$work"' EXIT
mkdir -p "$out"

# alpine-make-vm-image copies a single script into the chroot, so splice the common provisioning,
# the profile's optional hook and the cleanup into one
script="$work/provision.sh"
{
  cat "$here/provision.sh"
  hook="$here/profiles/$profile.sh"
  if [ -f "$hook" ]; then printf '\n'; cat "$hook"; fi
  printf '\n'
  cat "$here/finalize.sh"
} >"$script"

sudo modprobe nbd max_part=8 || true
sudo "$here/vendor/alpine-make-vm-image" \
  --arch "$arch" \
  --branch "$branch" \
  --image-format qcow2 \
  --image-size "$size" \
  --serial-console \
  --packages "alpine-base" \
  --script-chroot \
  "$work/raw.qcow2" -- "$script" "${packages[@]}"

# compress; zeroed free space (see provision.sh) is dropped
sudo qemu-img convert -O qcow2 -c "$work/raw.qcow2" "$out/$image"
sudo chown "$(id -u):$(id -g)" "$out/$image"
