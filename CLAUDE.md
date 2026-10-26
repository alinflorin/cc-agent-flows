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
- `can_self_update()`: on Linux only an AppImage can self-update.
- Tauri permissions live in `src-tauri/capabilities/default.json`. New plugin APIs used from the frontend need entries there.

**QEMU bundling pipeline** (spans several files)
- `scripts/qemu/version.json` pins the QEMU version and source sha256.
- `build.sh` compiles QEMU from source on each OS (MSYS2 on Windows). `.github/workflows/build-qemu.yml` runs it and publishes the result as a prerelease tagged `qemu-<version>`, with assets named `qemu-<target-triple>.tar.gz`. It triggers on changes to the version file, the script or the workflow.
- `scripts/qemu/fetch.mjs` downloads that asset for the target triple (`$TAURI_ENV_TARGET_TRIPLE`, else the host) and stamps `.version`. `tauri.conf.json` bundles `resources/qemu/` as `qemu/`. Everything in that directory except its README is git-ignored.
- Upgrading QEMU means bumping `version.json` and pushing to main, then waiting for the prerelease before building a release.

**Agent VM base image** (`scripts/images/`)
- `build.sh <x86_64|aarch64> <out>` builds an Alpine qcow2 with Claude Code, using the vendored `vendor/alpine-make-vm-image` (v0.13.4, with a marked LOCAL PATCH that kills processes still holding the image before unmounting). Alpine is pinned in `version.json`. Claude Code comes from the official installer at build time, unpinned. It needs root and Linux, and must run natively per arch. `provision.sh` runs in the chroot.
- `build-images.yml` builds both arches on native runners and publishes `agent-base-<arch>.qcow2` to a single rolling prerelease tagged `image-base`, overwriting the previous assets. Only the latest image of each type is kept, so a future type gets its own `image-<type>` release.
- The image has no SSH server, and `provision.sh` fails the build if one is present. It currently has no host-to-guest control channel: that still has to be added (for example a virtio-serial port). Secrets must not go in command lines, fw_cfg strings or the disk.
- Boot agents from a throwaway `qemu-img create -b` overlay, never from the base image itself.

**Release**
- `release.yml` triggers on `v*` tags or manual dispatch, and builds macOS (aarch64 and x86_64), Linux and Windows.
- The version must be kept in sync in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
- `release-cache.yml` warms Rust caches on main because tag builds can only restore default-branch caches.
