#!/usr/bin/env bash
# Builds the base Alpine image: a compressed qcow2 with Claude Code and agentd installed. Profiles
# (src-tauri/resources/profiles/, user-made ones too) are applied on top of it by the app, in a VM.
# Linux only; needs root (loop/nbd mounts) plus qemu-img, qemu-nbd, rsync, sfdisk, dosfstools, jq.
#
#   build.sh <x86_64|aarch64> <out-dir> <agentd-binary>
#
# <agentd-binary> is the static musl build of crates/agentd for that arch.
#
# aarch64 must be built on an aarch64 host. x86_64 boots with BIOS, aarch64 with UEFI.
set -euo pipefail

arch=$1
out=$2
agentd=$3
[ -x "$agentd" ] || { echo "agentd binary $agentd not found or not executable" >&2; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
cfg="$here/version.json"

branch=$(jq -r .alpine "$cfg")
size=4G
image="agent-base-$arch.qcow2"

work=$(mktemp -d)
trap 'sudo rm -rf "$work"' EXIT
mkdir -p "$out"

# alpine-make-vm-image copies a single script into the chroot, so splice provisioning and cleanup
# into one. Its directory is /mnt in the chroot: agentd and the protocol it must speak go in there.
script="$work/provision.sh"
cat "$here/provision.sh" >"$script"
printf '\n' >>"$script"
cat "$here/finalize.sh" >>"$script"
chmod +x "$script"
cp "$agentd" "$work/agentd"
jq -r .protocol "$cfg" >"$work/protocol"

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
  "$work/raw.qcow2" -- "$script"

# alpine-make-vm-image exits 0 even when the script fails; finalize.sh drops this marker only at
# the end of a complete run (the script's directory is bind-mounted at /mnt in the chroot)
[ -e "$work/.provisioned" ] || { echo "provisioning failed, see the log above" >&2; exit 1; }

# compress; zeroed free space (see provision.sh) is dropped
sudo qemu-img convert -O qcow2 -c "$work/raw.qcow2" "$out/$image"
sudo chown "$(id -u):$(id -g)" "$out/$image"
