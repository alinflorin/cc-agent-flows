QEMU 11.1.2 builds, one per target triple, committed so builds need no network. `scripts/qemu/fetch.mjs`
unpacks the right one into `src-tauri/resources/qemu`; checksums in `SHA256SUMS`. The build script is in git
history (commit 932a4de, `scripts/qemu/build.sh`). The two Linux builds were redone with
`--enable-seccomp` (libseccomp, so `-sandbox` works; the macOS and Windows builds have no seccomp):
the same script plus that flag and a smoke test of `-sandbox`, in commit `575dfb4`
(`scripts/qemu/build.sh`, `.github/workflows/qemu-linux.yml`, built natively on `ubuntu-22.04` and
`ubuntu-22.04-arm`).
