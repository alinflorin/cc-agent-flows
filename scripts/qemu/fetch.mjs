// Unpacks the QEMU build for a Rust target triple from vendor/qemu (committed to the repo) into
// src-tauri/resources/qemu, where tauri.conf.json bundles it. No network needed.
//
//   node scripts/qemu/fetch.mjs [target-triple] [--required]
//
// The triple defaults to $TAURI_ENV_TARGET_TRIPLE (set by `tauri dev/build` for its before-commands,
// including cross-compiles), then to the host triple. Without --required a triple with no build only
// warns, since the app falls back to a QEMU on PATH.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dest = join(here, "../../src-tauri/resources/qemu");

const args = process.argv.slice(2);
const required = args.includes("--required");
const triple =
  args.find((a) => !a.startsWith("--")) ??
  process.env.TAURI_ENV_TARGET_TRIPLE ??
  /^host: (.+)$/m.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))[1];

// Firmware for guests the app never runs (only x86_64 and aarch64 are used). Some of it is ELF for
// other CPUs, which makes linuxdeploy crash when it scans the AppDir, and all of it is dead weight.
const foreign = /^(openbios-|palcode-|hppa-firmware|s390-|u-boot|slof|skiboot|pnv-|opensbi-|edk2-(riscv|loongarch64)-|qemu-nsis|trace-events)/;

const archive = join(here, "../../vendor/qemu", `qemu-${triple}.tar.gz`);
const marker = join(dest, ".version");
const stamp = `${triple} ${existsSync(archive) ? statSync(archive).size : 0}`;
if (existsSync(marker) && readFileSync(marker, "utf8") === stamp) {
  console.log(`QEMU ${stamp} already in place`);
  process.exit(0);
}

// Never leave another target's binaries behind.
for (const sub of ["bin", "lib", "share"]) rmSync(join(dest, sub), { recursive: true, force: true });
rmSync(marker, { force: true });
try {
  if (!existsSync(archive)) throw new Error(`no QEMU build for ${triple} in vendor/qemu`);
  console.log(`Unpacking ${archive}`);
  mkdirSync(dest, { recursive: true });
  // Relative path from the destination: GNU tar (Git Bash on Windows) reads "C:\..." as host:path.
  execFileSync("tar", ["-xzf", relative(dest, archive).replaceAll("\\", "/")], { cwd: dest, stdio: "inherit" });
  const share = join(dest, "share/qemu");
  if (existsSync(share)) for (const f of readdirSync(share)) if (foreign.test(f)) rmSync(join(share, f), { force: true });
  writeFileSync(marker, stamp);
} catch (e) {
  if (required) throw e;
  console.warn(`QEMU not bundled (${e.message}); the app will use a QEMU from PATH.`);
}
