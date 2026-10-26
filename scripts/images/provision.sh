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

# no SSH server of any kind
! apk info -e openssh openssh-server dropbear | grep -q . || { echo "an SSH server is installed" >&2; exit 1; }
! command -v sshd dropbear >/dev/null || { echo "an SSH server is installed" >&2; exit 1; }

# The installer can leave a process running from the agent's home (the binary is versioned, so
# its name isn't `claude`), which makes the final umount fail with "target is busy". Match by
# where the process runs, not by name: the chroot sees host processes too.
for _ in 1 2 3 4 5; do
  found=
  for p in /proc/[0-9]*; do
    pid=${p#/proc/}
    [ "$pid" = "$$" ] && continue
    case "$(readlink "$p/exe" 2>/dev/null):$(readlink "$p/cwd" 2>/dev/null)" in
      /home/agent/*|*:/home/agent|*:/home/agent/*)
        echo "stopping leftover process $pid: $(readlink "$p/exe" 2>/dev/null)"
        kill -9 "$pid" 2>/dev/null || true
        found=1
        ;;
    esac
  done
  [ -n "$found" ] || break
  sleep 1
done
