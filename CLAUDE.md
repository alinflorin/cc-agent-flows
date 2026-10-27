# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

CC Agent Flows: a Tauri 2 desktop app (React 19 + TypeScript frontend, Rust backend) for running multi-agent, dynamic, end-to-end delivery pipelines built on Claude Code. Design goals:

- Agents run fully sandboxed inside QEMU VMs.
- Authentication uses a Claude Code personal subscription (setup tokens), not API keys.

Current state: the shell is in place (layout, settings, i18n, theming, auto-update, bundled QEMU plumbing). The pipeline/agent orchestration and Claude auth are not implemented yet; `Home` is still a placeholder and the only QEMU command is `qemu_version`. Build new features on the existing seams below.

## Commands

Package manager is pnpm; Rust is edition 2024.

- `pnpm tauri dev`: run the full app. Its before-command runs `pnpm qemu:fetch && pnpm dev`.
- `pnpm dev`: frontend only in a browser (Vite on port 1420). Tauri APIs are absent, so prefs fall back to localStorage.
- `pnpm build`: `tsc` typecheck, then Vite bundle. This is the only frontend check; there is no linter or JS test runner.
- `pnpm tauri build`: production bundle. Its before-command runs `qemu:fetch --required` (fails if QEMU can't be downloaded).
- `pnpm qemu:fetch [triple] [--required]`: download prebuilt QEMU into `src-tauri/resources/qemu`.
- `pnpm nats:fetch [triple] [--required]`: download the official `nats-server` into `src-tauri/resources/nats`, verified against the release's `SHA256SUMS`. The version is pinned in `scripts/nats/version.json`. Both dev and build before-commands run it.
- Backend: `cargo build|clippy|test --manifest-path src-tauri/Cargo.toml`. CI runs clippy with `-D warnings`. Single test: `cargo test --manifest-path src-tauri/Cargo.toml <name>`.
- `pnpm driver:install` / `pnpm driver`: install and run `tauri-driver` (into `.tools/`, git-ignored) on port 4444. Use `tauri-driver` (WebDriver) for interactive testing of the running app, not only the browser-based `pnpm dev`, since Tauri APIs, QEMU and the prefs store only exist in the real app.

CI (`ci.yml`) runs `pnpm build`, `cargo build`, clippy and test on Linux, macOS and Windows. Linux needs `libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`.

## Architecture

**Frontend (`src/`)**
- `App.tsx` nests FluentProvider (Fluent UI v9), notifications, updates, then a `createHashRouter` (hash routing because the packaged app serves static files). Pages are in `src/pages/`, and `Layout`/`Sidebar`/`TopBar` form the shell.
- `prefs.ts` holds app preferences. It uses `tauri-plugin-store` (`settings.json`) so Rust can read them too, with a localStorage fallback in a plain browser. `loadPrefs()` must complete before any `getPref` call, and reads are then synchronous from an in-memory cache. New preference keys must be added to the `KEYS` list or they won't be loaded.
- i18n uses i18next with the HTTP backend, so translations are runtime files in `public/locales/{en,ro}/translation.json`. Add every string to both languages.
- `updates.tsx` wraps tauri-plugin-updater. The release endpoint is GitHub `latest.json`.

**Backend (`src-tauri/src/`)**
- `lib.rs` registers plugins (opener, store, updater, process) and commands in `generate_handler!`. New commands must be added there. On Unix, `adopt_shell_path()` runs first in `run()` to import the login shell's PATH, because GUI launches get a minimal PATH. This is how tools like `claude`, nvm and cargo binaries get found. It must stay before any thread spawns because it mutates the environment.
- `qemu.rs`: `command(app, program)` builds a `Command` for any QEMU binary. It uses the bundled tree at `<resource_dir>/qemu/{bin,lib,share/qemu}` (adding `-L share/qemu` for `qemu-system-*`) and falls back to PATH if the build shipped no QEMU. On Windows it sets `CREATE_NO_WINDOW`. Spawn all QEMU processes through it.
- `nats.rs`: `command(app)` builds a `Command` for the bundled `nats-server` (`<resource_dir>/nats/`), falling back to PATH. Spawn the broker through it. `nats_version` is the only command so far.
- `broker.rs`: runs the bundled `nats-server` on `127.0.0.1` (random port, started in `setup`, killed on `RunEvent::Exit`, status via `broker_status`). Auth is NATS operator mode with JWTs minted in memory (operator, system and `agents` account keys are regenerated each start and the JetStream store under the app local data dir is wiped with them), so per-agent credentials need no config reload. `Broker::prepare_agent(id)` creates the pull consumer `in-<id>` on stream `AGENTS` and returns creds allowed only that consumer, `agent.<id>.in` (via it) and publishing `agent.<id>.out`; `publish_task` queues a task. An agent cannot look up the stream, so `agentd` must use `get_consumer_from_stream`. The test `agent_gets_its_task_and_nothing_else` runs against the real server (skipped if `pnpm nats:fetch` was not run, and on Windows). Nothing calls `prepare_agent` yet.
- `secrets.rs`: the Claude setup token lives in the OS credential store (`keyring`), never in `settings.json`. It is write-only from the frontend (`claude_token_set|status|clear`); backend code reads it with `secrets::claude_token()`. Linux needs a running Secret Service.
- `can_self_update()`: on Linux only an AppImage can self-update.
- Tauri permissions live in `src-tauri/capabilities/default.json`. New plugin APIs used from the frontend need entries there.

**QEMU bundling pipeline** (spans several files)
- `scripts/qemu/version.json` pins the QEMU version and source sha256.
- `build.sh` compiles QEMU from source on each OS (MSYS2 on Windows). `.github/workflows/build-qemu.yml` runs it and publishes the result as a prerelease tagged `qemu-<version>`, with assets named `qemu-<target-triple>.tar.gz`. It triggers on changes to the version file, the script or the workflow.
- `scripts/qemu/fetch.mjs` downloads that asset for the target triple (`$TAURI_ENV_TARGET_TRIPLE`, else the host) and stamps `.version`. `tauri.conf.json` bundles `resources/qemu/` as `qemu/`. Everything in that directory except its README is git-ignored.
- Upgrading QEMU means bumping `version.json` and pushing to main, then waiting for the prerelease before building a release.

**Agent VM base image** (`scripts/images/`)
- `build.sh <x86_64|aarch64> <out> [profile]` builds an Alpine qcow2 with Claude Code, using the vendored `vendor/alpine-make-vm-image` (v0.13.4, with a marked LOCAL PATCH that kills processes still holding the image before unmounting). Alpine is pinned in `version.json`. Claude Code comes from the official installer at build time, unpinned. It needs root and Linux, and must run natively per arch. `provision.sh` runs in the chroot.
- Profiles: `scripts/images/profiles/<profile>.json` lists the extra apk `packages` and the `image_size` of an image on top of the common provisioning (`base` adds nothing, `developer` adds a dev toolchain). A new profile needs its JSON file plus an entry in the `build-images.yml` matrix.
- `build-images.yml` builds every profile for both arches on native runners and publishes `agent-<profile>-<arch>.qcow2` to a rolling prerelease tagged `image-<profile>`, overwriting the previous assets. Only the latest image of each profile is kept.
- The image has no SSH server, and `provision.sh` fails the build if one is present. It currently has no host-to-guest control channel: that still has to be added (for example a virtio-serial port). Secrets must not go in command lines, fw_cfg strings or the disk.
- Boot agents from a throwaway `qemu-img create -b` overlay, never from the base image itself.

**Agent communication (planned, not built yet)**

The app is the hub. Agents never talk to each other directly; everything goes through a NATS broker the app runs (bundled `nats-server`, bound to `127.0.0.1`, started by the app). The model is one task per VM:

1. The app creates per-run NATS credentials for the agent, allowed only `agent.<id>.in` (subscribe) and `agent.<id>.out` (publish), and publishes the task on `agent.<id>.in` (JetStream, so it waits if the VM is still booting).
2. The app boots a QEMU VM from a throwaway overlay. `agentd` (see below) connects at boot and consumes its queue at once.
3. `agentd` runs `claude` headless (`--input-format stream-json --output-format stream-json`) and publishes the events (text, tool calls, cost, state) and finally the result on `agent.<id>.out`. The app renders the stream live.
4. Questions for the user (`ask_user`), hand-offs and agent-to-agent messages are more messages on the same subjects, routed by the app. The VM just waits. The app enforces who may message whom, and message size and budget limits.
5. After the result (or a timeout/heartbeat loss) the app kills QEMU and deletes the overlay. Unacknowledged messages stay in JetStream so a run can be retried.

Pieces to build, in this order:
1. (Done, see `broker.rs`.) Broker lifecycle in Rust: start `nats-server` with a generated config (JetStream on, a store dir under the app data dir, no anonymous access), create and revoke per-agent users, `async-nats` client in the app.
2. Networking: boot with `-netdev user,restrict=on` plus `guestfwd` to the broker port only. Claude Code needs `api.anthropic.com`, so egress goes through an HTTP proxy in the app, also exposed by `guestfwd`, with a domain allowlist. Never leave plain user-mode networking on: the guest could reach every loopback service on the host through `10.0.2.2`.
3. `agentd`: one static Rust (musl) binary in the image, an OpenRC service. Built once per arch in `build-images.yml` and copied into the image. It is the NATS consumer and runs `claude`. `agentd mcp` is the same binary acting as an MCP server over stdio, configured into Claude Code, with the tools `ask_user`, `send_message`, `handoff`, `report_result` and `list_agents`. The token and the NATS credentials are passed after boot (never on the QEMU command line, fw_cfg or disk). The token reaches only the `claude` process environment.
4. Protocol: JSON messages with a version field, defined in a shared Rust crate; generate the TypeScript types from it.
5. UI: a run page with the live event stream, backed by the JetStream log (also gives replay).
6. Pipeline graph (stages, dependencies, conditions) on top of the messages. Start with the flow declared in the pipeline plus `ask_user` and `handoff`; free-form agent-to-agent chat only when a concrete case needs it.

Code goes in and out as messages (a `git bundle` or patch), each agent on its own branch, and the app owns the central repository and the merges. No folders are shared with the host. Treat everything an agent sends as untrusted: validate the schema and the size.

**Release**
- `release.yml` triggers on `v*` tags or manual dispatch, and builds macOS (aarch64 and x86_64), Linux and Windows.
- The version must be kept in sync in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
- `release-cache.yml` warms Rust caches on main because tag builds can only restore default-branch caches.
