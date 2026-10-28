# Turns the plain Alpine image into the base image. Runs as root in the builder VM (busybox ash),
# fed by the app over the serial console with agentd.sh and agentd.initd in /tmp/setup. It must
# succeed or the build fails.
set -eu

# openssh-client carries the agent's traffic to the app's egress proxy (see tunnel.rs)
apk add --no-cache --no-progress jq setpriv ripgrep curl bash libgcc libstdc++ ca-certificates openssh-client

# The user `claude` runs as. No password, no sudo/doas.
adduser -D -s /bin/sh agent
install -d -o agent -g agent /home/agent/work

# Claude Code, the official native installer, as that user. The system ripgrep is used because
# the bundled one does not run on musl.
su agent -c 'export PATH="$HOME/.local/bin:$PATH"; curl -fsSL https://claude.ai/install.sh | bash'
ln -s /home/agent/.local/bin/claude /usr/local/bin/claude
# Claude Code's own bin directory on PATH (for logins; the symlink above covers the service)
cat >/etc/profile.d/claude.sh <<'EOP'
export USE_BUILTIN_RIPGREP=0
export PATH="$HOME/.local/bin:$PATH"
EOP

install -m 755 /tmp/setup/agentd.sh /usr/local/bin/agentd
install -m 755 /tmp/setup/agentd.initd /etc/init.d/agentd
rc-update add agentd default
# the init agent VMs boot with (init=/sbin/ccaf-init): agentd without OpenRC
install -m 755 /tmp/setup/ccaf-init.sh /sbin/ccaf-init

# Checks: everything agentd needs is there, and nothing that would let an agent be reached or
# escalate.
claude --version
for c in agentd jq setpriv setsid rg ssh timeout; do command -v "$c" >/dev/null || { echo "missing $c"; exit 1; }; done
# busybox also has a setpriv, but without --reuid: it must be the util-linux one
setpriv --reuid agent --regid agent --init-groups --no-new-privs true
# the ssh *client* is wanted; a server, dropbear and privilege tools are not
if apk info -e openssh openssh-server dropbear sudo doas >/dev/null || [ -e /usr/sbin/sshd ]; then
	echo "unexpected ssh server or sudo package"
	exit 1
fi

# no way for the agent to become root: no setuid or setgid file at all (busybox's `su` would accept
# the plain image's empty root password). Logins on the builder's console run as root already.
find / -xdev -type f \( -perm -4000 -o -perm -2000 \) -exec chmod ug-s {} +
if [ -n "$(find / -xdev -type f \( -perm -4000 -o -perm -2000 \))" ]; then
	echo "setuid or setgid files are still there"
	exit 1
fi

rm -rf /home/agent/.cache
