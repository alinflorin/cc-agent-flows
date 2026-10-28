#!/bin/sh
# PID 1 of an agent VM (kernel command line `init=/sbin/ccaf-init`): the few things the guest needs
# before agentd, without OpenRC. Builder VMs boot the stock init and never run this. Keep it small and
# quick: every line here is on the boot path of every run.
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin

mount -t proc -o hidepid=2,nosuid,nodev,noexec proc /proc
mount -t sysfs -o nosuid,nodev,noexec sysfs /sys
mountpoint -q /dev || mount -t devtmpfs -o nosuid,noexec devtmpfs /dev
mkdir -p /dev/pts /dev/shm /run /tmp
mount -t devpts -o gid=5,mode=620,nosuid,noexec devpts /dev/pts
mount -t tmpfs -o nosuid,nodev,mode=1777 tmpfs /dev/shm
mount -t tmpfs -o nosuid,nodev,mode=755 tmpfs /run
mount -t tmpfs -o nosuid,nodev,mode=1777 tmpfs /tmp
# the agent must not read or trace agentd's process
echo 2 >/proc/sys/kernel/yama/ptrace_scope 2>/dev/null

# the initramfs loaded the drivers of the devices there were at boot; the nodes and the
# /dev/virtio-ports/agent link come from mdev
echo /sbin/mdev >/proc/sys/kernel/hotplug
mdev -s

# QEMU's user-mode network is fixed, so no DHCP round trip
ip link set lo up
ip addr add 10.0.2.15/24 dev eth0
ip link set eth0 up
ip route add default via 10.0.2.2

hostname alpine
# variables baked into the image (`export NAME='value'` lines), for agentd and the claude it runs
[ -r /etc/conf.d/agentd ] && . /etc/conf.d/agentd
export USE_BUILTIN_RIPGREP=0
export PATH=/home/agent/.local/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin

# agentd powers the VM off when the task is done; if it dies, so does the VM
/usr/local/bin/agentd --run-as agent --poweroff
sync
poweroff -f
