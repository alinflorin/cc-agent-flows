#!/bin/sh
# Runs inside the image's chroot (alpine-make-vm-image --script-chroot).
#
#   provision.sh
set -eu

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
