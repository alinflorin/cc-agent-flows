QEMU 11.1.2 builds, one per target triple, committed so builds need no network. `scripts/qemu/fetch.mjs`
unpacks the right one into `src-tauri/resources/qemu`; checksums in `SHA256SUMS`. The build script is in git
history (commit 932a4de, `scripts/qemu/build.sh`).
