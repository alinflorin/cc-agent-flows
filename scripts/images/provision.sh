#!/bin/sh
# Runs inside the image's chroot (alpine-make-vm-image --script-chroot).
#
# build.sh runs this as one script: this file, then finalize.sh.
set -eu

# bash/curl/ca-certificates: the installer below; ripgrep: see the note below; libgcc/libstdc++:
# runtime libs of the Claude Code native binary. git is left out: not every agent needs it
apk add --no-cache bash curl ca-certificates ripgrep libgcc libstdc++

adduser -D -s /bin/bash agent

# native installer, as the agent user so it lands in ~/.local/bin and can update itself
su agent -c 'curl -fsSL https://claude.ai/install.sh | bash'
# musl: the ripgrep binary bundled with Claude Code doesn't run here, use the apk one
cat > /etc/profile.d/claude.sh <<'PROFILE'
export PATH="$HOME/.local/bin:$PATH"
export USE_BUILTIN_RIPGREP=0
PROFILE

cat > /etc/network/interfaces <<'NET'
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
NET
echo agent > /etc/hostname

rc-update add networking boot

# agentd (installed by the app's image setup): root-owned, runs once per boot as an
# OpenRC service and powers the VM off when the task is done. claude runs as the agent user.
install -o root -g root -m 0755 /mnt/agentd /usr/local/bin/agentd
cat > /etc/init.d/agentd <<'SERVICE'
#!/sbin/openrc-run
description="Runs the agent's task and reports to the app"
command=/usr/local/bin/agentd
command_args="--run-as agent --claude /home/agent/.local/bin/claude --workspace /home/agent/work --poweroff"
command_background=true
pidfile=/run/agentd.pid
output_log=/var/log/agentd.log
error_log=/var/log/agentd.log
export USE_BUILTIN_RIPGREP=0

depend() {
	need localmount
	after networking
}
SERVICE
chmod 0755 /etc/init.d/agentd
rc-update add agentd default
