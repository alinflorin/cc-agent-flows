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

# alpine-make-vm-image downloads a static apk-tools from gitlab.alpinelinux.org when the host has no
# apk, and that host is often unreachable from CI. Take it from the Alpine CDN instead. It must be
# apk-tools 2.x (3.x, shipped from v3.24 on, is not what the vendored script expects).
apk_env=()
if ! command -v apk >/dev/null; then
  cdn="https://dl-cdn.alpinelinux.org/alpine/v3.22/main/$(uname -m)"
  pkg=$(curl -fsSL --retry 5 "$cdn/" | grep -o 'apk-tools-static-[0-9][^"]*\.apk' | head -n 1)
  curl -fsSL --retry 5 -o "$work/apk-tools-static.apk" "$cdn/$pkg"
  tar -xzf "$work/apk-tools-static.apk" -C "$work" sbin/apk.static
  apk_env=(APK="$work/sbin/apk.static")
fi

sudo modprobe nbd max_part=8 || true
sudo env "${apk_env[@]}" "$here/vendor/alpine-make-vm-image" \
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
