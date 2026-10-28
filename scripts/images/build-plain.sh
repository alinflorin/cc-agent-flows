#!/usr/bin/env bash
# Builds the plain Alpine image: alpine-base and the linux-virt kernel on one ext4 root (no partition
# table, no bootloader, no firmware), DHCP on eth0 and a serial login for the builder. The kernel and
# a slim initramfs (virtio + ext4 only) come out next to the disk: QEMU boots them directly
# (-kernel/-initrd, root=/dev/vda). Linux only, needs root (chroot, mounts) plus curl, e2fsprogs,
# qemu-img.
#
#   build-plain.sh <x86_64|aarch64> <out-dir>
#
# Builds natively: run it on a host of the architecture it builds. Outputs, in <out-dir>:
# alpine-<arch>.qcow2, alpine-<arch>.vmlinuz, alpine-<arch>.initramfs.
set -euo pipefail

arch=$1
out=$2
here=$(cd "$(dirname "$0")" && pwd)
branch=$(jq -r .alpine "$here/version.json")
cdn=https://dl-cdn.alpinelinux.org/alpine
case $arch in
  x86_64) console=ttyS0 ;;
  aarch64) console=ttyAMA0 ;;
  *) echo "unknown arch $arch" >&2; exit 1 ;;
esac

work=$(mktemp -d)
root=$work/root
mkdir -p "$root" "$out"
cleanup() {
  for m in dev/pts dev proc sys; do mountpoint -q "$root/$m" && umount -l "$root/$m"; done
  rm -rf "$work"
}
trap cleanup EXIT

# the minimal root filesystem of the newest release of the branch, checked against its published hash
meta=$(curl -fsSL --retry 5 "$cdn/$branch/releases/$arch/latest-releases.yaml")
file=$(printf '%s\n' "$meta" | awk '/^- /{f=""} /^ +file: alpine-minirootfs-/{f=$2; print f; exit}')
sum=$(printf '%s\n' "$meta" | awk -v f="$file" '$2==f{on=1} on && /sha256:/{print $2; exit}')
[ -n "$file" ] && [ -n "$sum" ] || { echo "cannot find the minirootfs in latest-releases.yaml" >&2; exit 1; }
curl -fsSL --retry 5 -o "$work/mini.tar.gz" "$cdn/$branch/releases/$arch/$file"
echo "$sum  $work/mini.tar.gz" | sha256sum -c -
tar -xzf "$work/mini.tar.gz" -C "$root"

mount -t proc proc "$root/proc"
mount --rbind /dev "$root/dev"
mount --rbind /sys "$root/sys"
cp /etc/resolv.conf "$root/etc/resolv.conf"
printf "%s/main\n%s/community\n" "$cdn/$branch" "$cdn/$branch" >"$root/etc/apk/repositories"

# which modules the initramfs carries: the root disk, ext4 and the virtio devices the VM has
mkdir -p "$root/etc/mkinitfs"
cat >"$root/etc/mkinitfs/mkinitfs.conf" <<'CONF'
features="base ext4 virtio"
CONF
cat >"$root/provision.sh" <<PROVISION
#!/bin/sh
set -eu
apk update
apk add --no-cache --no-progress alpine-base linux-virt mkinitfs
# apk keeps the package's own mkinitfs.conf next to ours; build the initramfs from ours
kver=\$(ls /lib/modules | head -n 1)
mkinitfs -c /etc/mkinitfs/mkinitfs.conf -o /boot/initramfs-virt "\$kver"

cat >/etc/network/interfaces <<'NET'
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
NET
echo alpine >/etc/hostname
echo '/dev/vda / ext4 rw,relatime 0 0' >/etc/fstab

# serial login for the builder (empty root password); runs do not use it
cat >/etc/inittab <<'TAB'
::sysinit:/sbin/openrc sysinit
::sysinit:/sbin/openrc boot
::wait:/sbin/openrc default
$console::respawn:/sbin/getty -L 0 $console vt100
::ctrlaltdel:/sbin/reboot
::shutdown:/sbin/openrc shutdown
TAB
echo $console >>/etc/securetty
passwd -d root

for s in devfs dmesg mdev hwdrivers; do rc-update add \$s sysinit; done
for s in modules sysctl hostname bootmisc networking root localmount; do rc-update add \$s boot; done
for s in mount-ro killprocs; do rc-update add \$s shutdown; done
rm -rf /var/cache/apk/* /etc/resolv.conf
PROVISION
chmod +x "$root/provision.sh"
chroot "$root" /provision.sh
rm "$root/provision.sh"

cp "$root/boot/vmlinuz-virt" "$out/alpine-$arch.vmlinuz"
# Alpine's arm64 kernel is an EFI "zboot" wrapper (MZ, "zimg", payload offset and size, compression
# name) around the real Image, which QEMU's -kernel cannot run: unwrap it
kernel=$out/alpine-$arch.vmlinuz
if [ "$(head -c 2 "$kernel")" = MZ ] && [ "$(dd if="$kernel" bs=1 skip=4 count=4 2>/dev/null)" = zimg ]; then
  read -r off size < <(od -An -tu4 -j8 -N8 "$kernel")
  algo=$(dd if="$kernel" bs=1 skip=24 count=8 2>/dev/null | tr -d '\0')
  case $algo in
    gzip) unpack="gzip -dc" ;;
    zstd) unpack="zstd -dc" ;;
    *) echo "unsupported zboot compression: $algo" >&2; exit 1 ;;
  esac
  tail -c +$((off + 1)) "$kernel" | head -c "$size" | $unpack >"$work/Image"
  [ "$(dd if="$work/Image" bs=1 skip=56 count=4 2>/dev/null)" = "ARM$(printf '\144')" ] || { echo "unwrapped kernel is not an arm64 Image" >&2; exit 1; }
  mv "$work/Image" "$kernel"
fi
cp "$root/boot/initramfs-virt" "$out/alpine-$arch.initramfs"
# the kernel and initramfs are shipped beside the disk, /boot is dead weight in it
rm -rf "$root/boot"/*

for m in dev/pts dev proc sys; do umount -l "$root/$m" 2>/dev/null || true; done
truncate -s 8G "$work/disk.raw"
mkfs.ext4 -q -F -L root -O ^has_journal -d "$root" "$work/disk.raw"
qemu-img convert -O qcow2 -c "$work/disk.raw" "$out/alpine-$arch.qcow2"
ls -l "$out"
