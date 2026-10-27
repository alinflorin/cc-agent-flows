# Hook for the developer profile: runs in the image's chroot as root, after the common provisioning
# and the profile's packages, before cleanup. Spliced into one script by build.sh, so no shebang,
# no `exit`, and the shell options of provision.sh (set -eu) apply.
install -d -o agent -g agent /home/agent/work
