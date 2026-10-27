// Downloads the base agent image for the target's architecture into src-tauri/resources/images,
// where tauri.conf.json bundles it. Built by .github/workflows/build-images.yml, which replaces
// the same `image-base` release on every build, so the asset's id and update time are the version.
//
//   node scripts/images/fetch.mjs [target-triple] [--required]
//
// The triple defaults to $TAURI_ENV_TARGET_TRIPLE, then the host triple. The guest runs on the
// host's architecture: aarch64 targets get the aarch64 image, everything else x86_64. Without
// --required a failed download only warns.
import { execFileSync } from "node:child_process";
import { createWriteStream, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dest = join(here, "../../src-tauri/resources/images");
const repo = process.env.GITHUB_REPOSITORY ?? "alinflorin/cc-agent-flows";

const args = process.argv.slice(2);
const required = args.includes("--required");
const triple =
  args.find((a) => !a.startsWith("--")) ??
  process.env.TAURI_ENV_TARGET_TRIPLE ??
  /^host: (.+)$/m.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))[1];
const arch = triple.startsWith("aarch64") ? "aarch64" : "x86_64";
const name = `agent-base-${arch}.qcow2`;
const file = join(dest, name);
const marker = join(dest, `${name}.version`);

try {
  const headers = { Accept: "application/vnd.github+json" };
  if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  const res = await fetch(`https://api.github.com/repos/${repo}/releases/tags/image-base`, { headers });
  if (!res.ok) throw new Error(`release image-base: ${res.status} ${res.statusText}`);
  const asset = (await res.json()).assets.find((a) => a.name === name);
  if (!asset) throw new Error(`image-base has no asset ${name}`);

  const stamp = `${asset.id} ${asset.updated_at}`;
  if (existsSync(file) && existsSync(marker) && readFileSync(marker, "utf8") === stamp) {
    console.log(`Agent image ${name} already in place`);
    process.exit(0);
  }

  // Never leave another architecture's image behind.
  rmSync(dest, { recursive: true, force: true });
  mkdirSync(dest, { recursive: true });
  console.log(`Fetching ${asset.browser_download_url}`);
  const download = await fetch(asset.browser_download_url);
  if (!download.ok) throw new Error(`${asset.browser_download_url}: ${download.status} ${download.statusText}`);
  const part = `${file}.part`;
  await pipeline(Readable.fromWeb(download.body), createWriteStream(part));
  renameSync(part, file);
  writeFileSync(marker, stamp);
} catch (e) {
  if (required) throw e;
  console.warn(`Agent image not bundled (${e.message}).`);
}
