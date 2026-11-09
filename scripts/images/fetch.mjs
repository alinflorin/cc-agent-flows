// Copies the plain Alpine image (disk and kernel) for the target's architecture from vendor/images (committed to the
// repo) into src-tauri/resources/images, where tauri.conf.json bundles it. No network needed.
//
//   node scripts/images/fetch.mjs [target-triple] [--required]
//
// The triple defaults to $TAURI_ENV_TARGET_TRIPLE, then the host triple. The guest runs on the
// host's architecture: aarch64 targets get the aarch64 image, everything else x86_64. Without
// --required a missing image only warns.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { gzipSync } from "node:zlib";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dest = join(here, "../../src-tauri/resources/images");

const args = process.argv.slice(2);
const required = args.includes("--required");
const triple =
  args.find((a) => !a.startsWith("--")) ??
  process.env.TAURI_ENV_TARGET_TRIPLE ??
  /^host: (.+)$/m.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))[1];
const arch = triple.startsWith("aarch64") ? "aarch64" : "x86_64";
const name = `alpine-${arch}.qcow2`;
// the disk and the kernel the VMs boot directly
const names = [name, `alpine-${arch}.vmlinuz`];
const source = join(here, "../../vendor/images", name);
const file = join(dest, name);
const marker = `${file}.version`;

// The guest's network relay (relay/, built on CI for both arches and committed in vendor/relay): a
// setup file of the base image, so it goes where the others are. It is shipped gzipped: the Linux
// AppImage tooling rewrites every ELF file it finds in the bundle (patchelf), which corrupted this
// static binary (it crashed at start, the VM's tunnel never came up and the run hung); `setup.sh`
// unpacks it in the guest.
const relayName = `ccaf-relay-${arch}`;
const relaySource = join(here, "../../vendor/relay", relayName);
const relayDest = join(here, "../../src-tauri/resources/agentd/ccaf-relay.gz");

try {
  if (!existsSync(relaySource)) throw new Error(`vendor/relay has no ${relayName}`);
  writeFileSync(relayDest, gzipSync(readFileSync(relaySource), { level: 9 }));
  rmSync(join(here, "../../src-tauri/resources/agentd/ccaf-relay"), { force: true });
  for (const n of names) if (!existsSync(join(here, "../../vendor/images", n))) throw new Error(`vendor/images has no ${n}`);
  const stamp = names.map((n) => { const st = statSync(join(here, "../../vendor/images", n)); return `${st.size} ${st.mtimeMs}`; }).join(" ");
  if (existsSync(file) && existsSync(marker) && readFileSync(marker, "utf8") === stamp) {
    console.log(`Image ${name} already in place`);
    process.exit(0);
  }
  // Never leave another architecture's image behind.
  mkdirSync(dest, { recursive: true });
  for (const f of readdirSync(dest)) if (f.startsWith("alpine-")) rmSync(join(dest, f));
  for (const n of names) {
    const from = join(here, "../../vendor/images", n);
    copyFileSync(from, join(dest, n));
  }
  writeFileSync(marker, stamp);
} catch (e) {
  if (required) throw e;
  console.warn(`Image not bundled (${e.message}).`);
}
