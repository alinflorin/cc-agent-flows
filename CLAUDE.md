# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

CC Agent Flows: a Tauri 2 desktop app (React 19 + TypeScript frontend, Rust backend) for running multi-agent, dynamic, end-to-end delivery pipelines built on Claude Code. Design goals:

- Agents run fully sandboxed inside QEMU VMs.
- Authentication uses a Claude Code personal subscription (setup tokens), not API keys.

Current state: projects, the shell (layout, settings, i18n, theming, auto-update, bundled QEMU), agent profiles, images, the workflow engine with sessions and triggers, the egress proxy with credential injection, analytics and import/export are built. The landing page `/` is the Projects page (project cards, a readiness warning, the active sessions). Build new features on the existing seams.

## Where the details are

The notes are long, so they are split by area in `docs/`. Read the one that matches what you are touching before changing it.

- `docs/projects-and-config.md`: projects and scopes (what a run sees), global system prompt, agent profiles and `extends`, integrations, VM settings, Claude accounts, variables, secrets, the MCP server (`ccaf`).
- `docs/workflow-and-sessions.md`: the org-chart workflow, trigger nodes (chat, cron, webhook), follow-up messages, the engine, analytics.
- `docs/network.md`: the sealed network (yamux over virtio-serial), egress proxy, allowlists, plain HTTP per host, credential injection, TLS inspection, limits, leak detection.
- `docs/vm-and-images.md`: QEMU bundling, process confinement, the plain Alpine image and custom kernel, base and custom images, builder VM, starting agents, fast boot.
- `docs/agent-communication.md`: the app-to-VM channel and protocol, `agentd`, hardening, the superseded orchestrator notes.

Keep these notes current when you change behavior, and keep a statement in the area it belongs to rather than repeating it.

## Commands

Package manager is pnpm; Rust is edition 2024.

- `pnpm tauri dev`: run the full app. Its before-command runs `pnpm qemu:fetch && pnpm dev`.
- `pnpm dev`: frontend only in a browser (Vite on port 1420). Tauri APIs are absent, so prefs fall back to localStorage.
- `pnpm build`: `tsc` typecheck, then Vite bundle. This is the only frontend check; there is no linter or JS test runner.
- `pnpm tauri build`: production bundle. Its before-command runs `qemu:fetch --required` and `images:fetch --required` (fails if either is missing).
- `pnpm images:fetch [triple] [--required]`: copy the committed plain Alpine image for the target arch from `vendor/images` into `src-tauri/resources/images` (see Agent VM base image).
- `pnpm qemu:fetch [triple] [--required]`: unpack the committed QEMU build for the target from `vendor/qemu` into `src-tauri/resources/qemu` (no network).
- Backend: `cargo build|clippy|test --manifest-path src-tauri/Cargo.toml`. CI runs clippy with `-D warnings`. Single test: `cargo test --manifest-path src-tauri/Cargo.toml <name>`.
- `pnpm driver:install` / `pnpm driver`: install and run `tauri-driver` (into `.tools/`, git-ignored) on port 4444. **Preferred way to test the UI visually** (screenshots, clicking through pages, reading the DOM): use `tauri-driver` (WebDriver) on the real app, not the browser-based `pnpm dev`, since Tauri APIs, QEMU and the prefs store only exist in the real app. Linux needs `webkit2gtk-driver` (`/usr/bin/WebKitWebDriver`, `sudo apt install webkit2gtk-driver`). The debug binary (`cargo build`, `src-tauri/target/debug/cc-agent-flows`) loads the frontend from Vite, so start `pnpm dev` (port 1420) first, then `pnpm driver`, then `POST /session` with `{"capabilities":{"alwaysMatch":{"tauri:options":{"application":"<path to the binary>"}}}}`; the app uses the real app data dir (images, profiles, sessions). Routes are hash routes (`location.hash = '#/settings'`).

CI (`ci.yml`) runs `pnpm build`, `cargo build`, clippy and test on Linux (x86_64 and arm64), macOS and Windows. Linux needs `libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`.

## Architecture in short

- **Projects** own workflows, sessions, agent profiles, images, variables, secrets and injections; global settings hold the same kinds and a run sees the merge of both (`docs/projects-and-config.md`).
- **One disposable QEMU VM per agent task.** The app is the hub: each VM has a virtio-serial channel to the app and no network card, only a tunnel to the app's egress proxy (`docs/agent-communication.md`, `docs/network.md`).
- **The workflow is an org chart**; the engine runs it over scripted or real hosts, journaling each run so sessions can resume (`docs/workflow-and-sessions.md`).

**Frontend (`src/`)**
- `App.tsx` nests FluentProvider (Fluent UI v9), notifications, updates, then a `createHashRouter` (hash routing because the packaged app serves static files). Pages are in `src/pages/`, and `Layout`/`Sidebar` form the shell: no top bar, the sidebar has the app name; outside a project it lists Projects, Global settings and About; Global settings (`/settings/:tab`) replaces the navigation like a project does (back link, General, LLM, Images, Network, Variables, Secrets, Injections), inside one (`/p/:pid/...`, `ProjectRoute` in `src/project.tsx`) a back link, the project's name and its pages (General = name, description, system prompt and delete, then Workflow, Sessions, Agent profiles, Runs, Analytics, Images, Variables, Secrets, Injections, Network = the project's allowed hosts) (on mobile a menu button opens it). `/` is `Projects.tsx`: a warning bar when `system_status` (the About readiness checks) has an error or warning, the sessions that run or wait for the user across projects (live, from `sessions-changed` and `session-log` status entries), the projects as cards, and a new-project dialog. Settings > General has theme, language, auto-update, the notifications switch and "start the app when I log in" (off by default; `tauri-plugin-autostart`, driven from Rust by `system::autostart_get|set` so it needs no capability; the state is the OS's, not a pref; macOS LaunchAgent; the app opens its window at login, there is no tray or minimized start) (`AppearanceFields`, also step 2 of the welcome dialog; step 1 is the intro with a link to the help).
- **Remote control** (`src-tauri/src/remote.rs`; asked for by the owner): `--serve` (window stays) or `--headless` (window created hidden, never shown; implies `--serve`) starts an HTTP server in the same process, default `127.0.0.1:7880`. Without a flag, **Settings > Remote control** (`settings/RemoteSettings.tsx`, commands `remote_get|set`, `remote.json` in the app data dir, mode 0600, off by default, not in the export) starts it when the app opens; saving restarts the listener ~400 ms later (so the answer reaches a browser that comes through it) and ending the listener ends its connections. Address, port and login come from that file; the flags `--bind <ip>`, `--port <n>`, `--auth user:pass` (or env `CCAF_AUTH`) override it. The password is stored in the file, never sent back to the UI (an empty one on save keeps it). `POST /api/<cmd>` (JSON object of args, `Content-Type: application/json`) runs the Tauri command through the main window's own IPC handler (`WebviewWindow::on_message` with `invoke_key`), so no command, capability or state changed; 200 = result, 400 = `{"error": ...}`. `GET /events` is SSE for `session-log`, `sessions-changed`, `config-changed` (Rust `listen_any`; a new emitted event name must be added to `EVENTS`). Anything else is the frontend: bundled assets (`asset_resolver`), or in dev a proxy to `devUrl` (Vite's hot-reload socket is not forwarded, reload by hand); `index.html` gets `window.__CCAF_REMOTE__`. Optional HTTP basic auth, off by default; without it anyone who can reach the port runs everything (secrets, tokens), so a non-loopback `--bind` logs a warning. Always on, auth or not: `Host` must be loopback when bound to loopback (DNS rebinding), `Origin` must equal `Host`, `/api` bodies must be JSON (a page in the user's browser cannot drive it). The main window is `visible: false` in `tauri.conf.json` and shown in `setup` unless `--headless`. Frontend: `prefs.ts` has `native()` (in the app's window), `remote()` (served by this server) and `inTauri()` = either, "a backend is reachable" (gates the pages); `tauri.ts` `call()` and `listen()` pick invoke/Tauri events or fetch/EventSource. Plugin-only features stay `native()`: updater and relaunch, notifications (the hidden window still sends the desktop ones), welcome dialog, app version, Import/Export (it uses native file dialogs on the host). Prefs in a remote page are the browser's localStorage. Port 7879 is the MCP server's, 7878 the webhook's, 7881 a node's (`nodes/`). Tested by hand with curl and a browser against the dev build (page, `/api`, auth, origin/host refusals, SSE keep-alive, the settings round trip, start with the setting on and off, restart on save); the settings page itself was not looked at in a browser; not tested: the packaged build's asset path, SSE with a real event, Vite HMR.
- **Nodes** (`src-tauri/src/nodes/`, Settings > Nodes; asked for by the owner): VMs of a session can run on other computers running this same app; the hub keeps the engine, proxy and secrets, only the VM and its three serial ports cross the network (`docs/vm-and-images.md`, "Nodes"). `--node` makes a computer accept VMs whatever its setting says.
- `tauri.ts` has the one `call()` wrapper around `invoke` that pages use. Pages: `pages/session/` (SessionView and its graph/call views), `pages/workflow/` (editor, panels, node), `pages/settings/` (the Settings tabs).
- `prefs.ts` holds app preferences. It uses `tauri-plugin-store` (`settings.json`) so Rust can read them too, with a localStorage fallback in a plain browser. `loadPrefs()` must complete before any `getPref` call, and reads are then synchronous from an in-memory cache. New preference keys must be added to the `KEYS` list or they won't be loaded.
- i18n uses i18next with the HTTP backend, so translations are runtime files in `public/locales/{en,ro}/translation.json`. Add every string to both languages.
- `updates.tsx` wraps tauri-plugin-updater. The release endpoint is GitHub `latest.json`.

**Backend (`src-tauri/src/`)**
- Layout: `config/` (profiles, accounts, secrets, variables, injections, network allowlist), `vm/` (QEMU, `machine.rs` = the QEMU command line and `qemu-img`, images and the builder VM, agent channel, protocol, one agent run), `net/` (egress proxy, TLS inspection, the yamux tunnel to the guest; the guest end is the `relay/` crate), `workflow/` (`model.rs` types and validation, `store.rs` files, `commands.rs`, `engine/` = `state`, `prompts`, `decisions`, `run` (the logic and its scripted-host tests), `host` (real host and session commands); plus triggers, cron, webhook, analytics). Top level: `system.rs`, `util.rs` (time, JSON files, `data_dir`, keyring service name), `mcp/` (the MCP server for the user's own agent, `docs/projects-and-config.md`), `lib.rs`. Commands in `generate_handler!` are addressed by their module path, e.g. `workflow::commands::sessions_list`.
- `lib.rs` registers plugins (opener, store, updater, process) and commands in `generate_handler!`. New commands must be added there. On Unix, `adopt_shell_path()` runs first in `run()` to import the login shell's PATH, because GUI launches get a minimal PATH. This is how tools like `claude`, nvm and cargo binaries get found. It must stay before any thread spawns because it mutates the environment.

- `log_entries` (in `workflow/analytics.rs`) reads a session log filtered by kind before parsing (agent events are most of the file). There is no audit export (removed on purpose; do not re-add unasked). **Import/export** (`transfer.rs`, Settings tab "Import / Export" `settings/Transfer.tsx`, commands `transfer_export|import`; asked for by the owner): one encrypted YAML file with everything (global settings, every project's files, image *definitions*, secret values from the keyring, Claude tokens; not session logs, built disks, run logs, app prefs). Argon2id (64 MiB, 3 passes) from the password + XChaCha20-Poly1305 over the whole bundle, YAML only holds the header and a base64 `ciphertext`. The backend picks/writes the file through `tauri-plugin-dialog` (Rust side only) and the plain bundle never reaches the frontend. Import only accepts known relative paths (`classify`), merges by item (variables, secrets index, injections, integrations, allowed hosts, accounts) and never deletes; `overwrite` replaces items that exist, otherwise they stay. Tested: round trip, wrong password, path whitelist, merge; not run end to end in the app.

- Desktop notifications (`Notifier.tsx`, `tauri-plugin-notification`, pref `notifications`, Settings > General): while the window is not focused, the app-wide `session-log` listener notifies on a `wait` (approval or questions) and on a session ending `done|failed`.

- **App log** (`tauri-plugin-log`, `app.log` in the OS log dir, 2 MB, one rotated copy; About > "View app log" runs `system::app_log_open`, which opens it with the default program): use `log::info!|warn!|error!`. Today it gets the image build lines and failures and failed agent runs.
- `can_self_update()`: on Linux only an AppImage can self-update.
- Tauri permissions live in `src-tauri/capabilities/default.json`. New plugin APIs used from the frontend need entries there.

**Release**
- `release.yml` triggers on `v*` tags or manual dispatch, and builds macOS (aarch64 and x86_64), Linux (x86_64 and aarch64, on `ubuntu-22.04` and `ubuntu-22.04-arm`) and Windows.
- The version must be kept in sync in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
- `release-cache.yml` warms Rust caches on main because tag builds can only restore default-branch caches.
