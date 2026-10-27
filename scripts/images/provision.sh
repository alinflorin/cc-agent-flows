#!/bin/sh
# Runs inside the image's chroot (alpine-make-vm-image --script-chroot).
#
#   provision.sh [extra-apk-package...]
#
# The extra packages come from the profile (profiles/<profile>.json). build.sh runs this as one
# script: this file, then the profile's hook (profiles/<profile>.sh, if any), then finalize.sh.
set -eu

# bash/curl/ca-certificates: the installer below; ripgrep: see the note below; libgcc/libstdc++:
# runtime libs of the Claude Code native binary. git is left out: not every agent needs it
apk add --no-cache bash curl ca-certificates ripgrep libgcc libstdc++
[ $# -eq 0 ] || apk add --no-cache "$@"

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
