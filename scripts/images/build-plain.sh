#!/usr/bin/env bash
# Builds a plain Alpine image: alpine-base on an 8G disk, DHCP on eth0 and a serial console, nothing
# else. The app starts from it and installs Claude Code, agentd and profiles in a VM on the user's
# machine. Linux only; needs root (loop/nbd mounts) plus qemu-img, qemu-nbd, rsync, sfdisk,
# dosfstools, jq.
#
#   build-plain.sh <x86_64|aarch64> <out-dir>
#
# aarch64 must be built on an aarch64 host. x86_64 boots with BIOS, aarch64 with UEFI.
set -euo pipefail

arch=$1
out=$2
here=$(cd "$(dirname "$0")" && pwd)
branch=$(jq -r .alpine "$here/version.json")
image="alpine-$arch.qcow2"

work=$(mktemp -d)
trap 'sudo rm -rf "$work"' EXIT
mkdir -p "$out"

cat >"$work/provision.sh" <<'SCRIPT'
#!/bin/sh
set -eu
cat > /etc/network/interfaces <<'NET'
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
NET
echo alpine > /etc/hostname
rc-update add networking boot
touch /mnt/.provisioned
SCRIPT
chmod +x "$work/provision.sh"

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
  --image-size 8G \
  --serial-console \
  --packages "alpine-base" \
  --script-chroot \
  "$work/raw.qcow2" -- "$work/provision.sh"

# alpine-make-vm-image exits 0 even when the script fails
[ -e "$work/.provisioned" ] || { echo "provisioning failed, see the log above" >&2; exit 1; }

sudo qemu-img convert -O qcow2 -c "$work/raw.qcow2" "$out/$image"
sudo chown "$(id -u):$(id -g)" "$out/$image"
