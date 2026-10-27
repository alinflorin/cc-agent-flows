# Last part of the chroot script (see provision.sh); not run on its own.

# agentd must be installed and its service enabled
[ -x /usr/local/bin/agentd ] || { echo "agentd missing" >&2; exit 1; }
rc-update show default | grep -q agentd || { echo "agentd service not enabled" >&2; exit 1; }

# Deleted files keep their blocks in the image, and the final qemu-img convert only drops zeroed
# ones. Remove leftovers, then overwrite the free space with zeros so they don't ship.
rm -rf /home/agent/.claude/downloads /var/cache/apk/*
dd if=/dev/zero of=/zero bs=1M 2>/dev/null || true # runs until the disk is full
sync
rm -f /zero
sync

touch /mnt/.provisioned # checked by build.sh
