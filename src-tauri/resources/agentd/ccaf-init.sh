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
# a second virtio disk is the agent's work disk (the app attaches it for agents of a session)
WORKDEV=
[ -b /dev/vdb ] && WORKDEV=/dev/vdb
# a virtio port `shared` is the session's shared folder (9P, served by the app)
SHAREDDEV=
[ -e /dev/virtio-ports/shared ] && SHAREDDEV=/dev/virtio-ports/shared

# agentd waits for the app's bootstrap anyway: read what it will run into the page cache meanwhile
# (the disk is a cold overlay, and Claude Code is a big binary)
{ cat "$(readlink -f /usr/local/bin/claude)" /usr/bin/jq /usr/bin/rg /usr/lib/lib*.so* >/dev/null 2>&1; } &

# There is no network card. `claude` reaches the app's proxy through ccaf-relay, which listens on
# 127.0.0.1:3128 (HTTPS_PROXY points there, set by agentd) and carries each connection over the
# virtio-serial port `net` to the app (yamux, see relay/). It needs no key: the port belongs to
# this VM alone, like the agent channel, and the app's end hands every stream to the egress proxy
# and to nothing else. It starts now, in the background, and says through the fifo when it is
# listening (`0 <guest uptime>`, or `1 0`); agentd reads that just before it starts claude.
ip link set lo up
mkdir -m 700 /run/ccaf
mkfifo /run/ccaf/tunnel
ccaf-relay --port /dev/virtio-ports/net --listen 127.0.0.1:3128 --ready /run/ccaf/tunnel 2>/run/ccaf/tunnel.err &

hostname alpine
# variables baked into the image (`export NAME='value'` lines), for agentd and the claude it runs
[ -r /etc/conf.d/agentd ] && . /etc/conf.d/agentd
export USE_BUILTIN_RIPGREP=0
export PATH=/home/agent/.local/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin

# agentd powers the VM off when the task is done; if it dies, so does the VM
/usr/local/bin/agentd --run-as agent --poweroff ${WORKDEV:+--work-dev $WORKDEV} ${SHAREDDEV:+--shared-dev $SHAREDDEV}
sync
poweroff -f
