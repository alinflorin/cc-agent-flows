// Downloads the official nats-server release for a Rust target triple into src-tauri/resources/nats,
// where tauri.conf.json bundles it. The download is checked against the release's SHA256SUMS.
//
//   node scripts/nats/fetch.mjs [target-triple] [--required]
//
// The triple defaults to $TAURI_ENV_TARGET_TRIPLE (set by `tauri dev/build` for its before-commands,
// including cross-compiles), then to the host triple. Without --required a failed download only warns.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dest = join(here, "../../src-tauri/resources/nats");
const { version } = JSON.parse(readFileSync(join(here, "version.json"), "utf8"));

const args = process.argv.slice(2);
const required = args.includes("--required");
const triple =
  args.find((a) => !a.startsWith("--")) ??
  process.env.TAURI_ENV_TARGET_TRIPLE ??
  /^host: (.+)$/m.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))[1];

const ARCH = { x86_64: "amd64", aarch64: "arm64" };
const arch = triple.split("-")[0];
const os = ["linux", "darwin", "windows"].find((k) => triple.includes(`-${k}`));

const marker = join(dest, ".version");
const stamp = `${version} ${triple}`;
if (existsSync(marker) && readFileSync(marker, "utf8") === stamp) {
  console.log(`nats-server ${stamp} already in place`);
  process.exit(0);
}

const exe = os === "windows" ? "nats-server.exe" : "nats-server";
rmSync(join(dest, exe), { force: true });
rmSync(marker, { force: true });
try {
  if (!os || !ARCH[arch]) throw new Error(`unsupported target ${triple}`);
  const name = `nats-server-v${version}-${os}-${ARCH[arch]}`;
  const ext = os === "windows" ? "zip" : "tar.gz";
  const base = `https://github.com/nats-io/nats-server/releases/download/v${version}`;

  const get = async (url) => {
    console.log(`Fetching ${url}`);
    const res = await fetch(url);
    if (!res.ok) throw new Error(`${url}: ${res.status} ${res.statusText}`);
    return Buffer.from(await res.arrayBuffer());
  };
  const archive = await get(`${base}/${name}.${ext}`);
  const sums = (await get(`${base}/SHA256SUMS`)).toString("utf8");
  const expected = sums.split("\n").map((l) => l.trim().split(/\s+/)).find(([, f]) => f?.replace(/^\*/, "") === `${name}.${ext}`)?.[0];
  const actual = createHash("sha256").update(archive).digest("hex");
  if (!expected || expected !== actual) throw new Error(`checksum mismatch for ${name}.${ext}`);

  const tmp = mkdtempSync(join(tmpdir(), "nats-"));
  try {
    const file = join(tmp, `${name}.${ext}`);
    writeFileSync(file, archive);
    // bsdtar (macOS, Windows) reads zip as well as tar.gz.
    execFileSync("tar", ["-xf", file, "-C", tmp], { stdio: "inherit" });
    mkdirSync(dest, { recursive: true });
    copyFileSync(join(tmp, name, exe), join(dest, exe));
    if (readdirSync(join(tmp, name)).includes("LICENSE")) copyFileSync(join(tmp, name, "LICENSE"), join(dest, "LICENSE"));
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
  writeFileSync(marker, stamp);
} catch (e) {
  if (required) throw e;
  console.warn(`nats-server not bundled (${e.message}).`);
}
