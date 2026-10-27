QEMU 11.1.2 builds, one per target triple, committed so builds need no network. `scripts/qemu/fetch.mjs`
unpacks the right one into `src-tauri/resources/qemu`. Built with `scripts/qemu/build.sh` (see
`scripts/qemu/version.json` for the version and source hash); checksums in `SHA256SUMS`.
To upgrade, rebuild per OS, replace the archives and bump `version.json`.
