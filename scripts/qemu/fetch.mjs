// Downloads the prebuilt QEMU for a Rust target triple into src-tauri/resources/qemu, where
// tauri.conf.json bundles it. Built by .github/workflows/build-qemu.yml.
//
//   node scripts/qemu/fetch.mjs [target-triple]   (defaults to the host triple)
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dest = join(here, "../../src-tauri/resources/qemu");
const { version } = JSON.parse(readFileSync(join(here, "version.json"), "utf8"));
const repo = process.env.GITHUB_REPOSITORY ?? "alinflorin/cc-agent-flows";

const triple =
  process.argv[2] ??
  /^host: (.+)$/m.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))[1];

const marker = join(dest, ".version");
const stamp = `${version} ${triple}`;
if (existsSync(marker) && readFileSync(marker, "utf8") === stamp) {
  console.log(`QEMU ${stamp} already in place`);
  process.exit(0);
}

const url = `https://github.com/${repo}/releases/download/qemu-${version}/qemu-${triple}.tar.gz`;
console.log(`Fetching ${url}`);
const res = await fetch(url);
if (!res.ok) throw new Error(`${url}: ${res.status} ${res.statusText}`);

const archive = join(tmpdir(), `qemu-${triple}.tar.gz`);
writeFileSync(archive, Buffer.from(await res.arrayBuffer()));

for (const sub of ["bin", "lib", "share"]) rmSync(join(dest, sub), { recursive: true, force: true });
mkdirSync(dest, { recursive: true });
execFileSync("tar", ["-xzf", archive, "-C", dest], { stdio: "inherit" });
rmSync(archive);
writeFileSync(marker, stamp);
