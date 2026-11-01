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
# no access-time writes on the root disk (the kernel cannot take noatime as a root mount option;
# the disk has no journal, so there is no data= mode or barrier to tune)
mount -o remount,noatime / 2>/dev/null
# the agent must not read or trace agentd's process
echo 2 >/proc/sys/kernel/yama/ptrace_scope 2>/dev/null

# the drivers are built into the kernel; the nodes and the /dev/virtio-ports/agent link come
# from mdev
echo /sbin/mdev >/proc/sys/kernel/hotplug
mdev -s

# agentd waits for the app's bootstrap anyway: read what it will run into the page cache meanwhile
# (the disk is a cold overlay, and Claude Code is a big binary)
{ cat "$(readlink -f /usr/local/bin/claude)" /usr/bin/jq /usr/bin/rg /usr/lib/lib*.so* >/dev/null 2>&1; } &

# QEMU's user-mode network is fixed, so no DHCP round trip
ip link set lo up
ip addr add 10.0.2.15/24 dev eth0
ip link set eth0 up
ip route add default via 10.0.2.2

# The only way out of the VM is one connection to the app (QEMU forwards 10.0.2.100:22 to it), and
# ssh carries the traffic of `claude` to the app's proxy as channels (HTTPS_PROXY points at the local
# end, set by agentd). The connection needs no key: it belongs to this VM alone, like the agent
# channel, and the server accepts nothing but the proxy channel. It starts now, in the background, so
# the handshake overlaps with the rest of the boot; agentd reads the outcome from the fifo before it
# starts claude. The options are the cheapest to set up under software emulation: no compression, one
# cipher, plain curve25519 (not the post-quantum hybrid OpenSSH proposes first), ed25519 only.
mkdir -m 700 /run/ccaf
mkfifo /run/ccaf/tunnel
(
	timeout 90 ssh -F /dev/null -f -N -T -o BatchMode=yes -o PreferredAuthentications=none \
		-o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no -o LogLevel=ERROR -o ExitOnForwardFailure=yes \
		-o ConnectTimeout=30 -o ServerAliveInterval=30 -o ServerAliveCountMax=4 \
		-o Compression=no -o IPQoS=none -o Ciphers=chacha20-poly1305@openssh.com,aes128-gcm@openssh.com \
		-o KexAlgorithms=curve25519-sha256 -o HostKeyAlgorithms=ssh-ed25519 \
		-L 127.0.0.1:3128:egress:3128 -p 22 agent@10.0.2.100 2>/run/ccaf/tunnel.err
	rc=$?
	read -r up _ </proc/uptime
	echo "$rc ${up:-0}" >/run/ccaf/tunnel
) &

hostname alpine
# variables baked into the image (`export NAME='value'` lines), for agentd and the claude it runs
[ -r /etc/conf.d/agentd ] && . /etc/conf.d/agentd
export USE_BUILTIN_RIPGREP=0
export PATH=/home/agent/.local/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin

# agentd powers the VM off when the task is done; if it dies, so does the VM
/usr/local/bin/agentd --run-as agent --poweroff
sync
poweroff -f
