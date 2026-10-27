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
- `broker.rs`: runs the bundled `nats-server` on `127.0.0.1` (random port, started in `setup`, killed on `RunEvent::Exit`, status via `broker_status`). Auth is NATS operator mode with JWTs minted in memory (operator, system and `agents` account keys are regenerated each start and the JetStream store under the app local data dir is wiped with them), so per-agent credentials need no config reload. Rule: agents never talk to each other, only to the app, and the app decides the flow. `Broker::allocate(project_id, session_id, profile, ttl)` creates an in-memory `Identity` (id = a random guid, name `spiffe://ccagentflows.local/<projectId>/<sessionId>/<profile>/agent-<guid>` where profile is the image profile such as `developer`, expiry; the three segments are checked to be `[a-z0-9-]` so they cannot inject path parts), the pull consumer `in-<id>` on stream `AGENTS`, and NATS creds allowed only that consumer, `agent.<id>.in` (via it) and publishing `agent.<id>.out`. Since an agent can publish nowhere else, it cannot reach another agent's queue or speak as one. The router collects `agent.*.out`, stamps each `{kind, body}` with the sender taken from the subject (unforgeable), and puts it on `Broker::inbox` (broadcast) for the orchestrator; malformed messages get an `error` from `app`. The orchestrator replies or forwards with `Broker::send(id, msg)`. The identity is the general credential; NATS is one place it is used. `idp.rs` makes the app an OIDC provider for the agents: `Broker::allocate` registers the identity (with the agent's public key) in `Broker::idp`, and the agent then gets tokens by itself for any audience from `POST <issuer>/token` (OAuth2 `client_credentials` with `private_key_jwt`: it signs a single-use proof, `iss`=`sub`=its SPIFFE name, `aud`=the token endpoint, `exp` within 5 minutes, with the key from `allocate`). Tokens are standard EdDSA JWTs (`sub` = SPIFFE name, `aud`, 5 minute `exp` capped at the identity's expiry, plus `agent`, `project`, `session`, `profile`) that any service can verify from `/.well-known/openid-configuration` and `/jwks.json`; services inside the app can call `Idp::verify`. The issuer is `http://127.0.0.1:<random port>` and the signing key is regenerated on every start, so tokens do not survive a restart. Host-side: `Idp::mint(id, audience)` makes a token without the exchange. `release(id)` withdraws the identity from the provider and NATS at once; tokens and proofs stop working immediately; NATS creds already handed out cannot be revoked (in-memory resolver) and lapse at their TTL. An agent cannot look up the stream, so `agentd` must use `get_consumer_from_stream`. Tests run against the real server (skipped if `pnpm nats:fetch` was not run, and on Windows). Nothing calls `allocate` yet.
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

1. The app allocates an identity for the agent (`Broker::allocate`): NATS creds allowed only its own queue `agent.<id>.in` (subscribe) and `agent.<id>.out` (publish). It publishes the task on `agent.<id>.in` (JetStream, so it waits if the VM is still booting). The task message carries everything else: prompt, model, thinking level, the Claude setup token, the code.
2. The app boots a QEMU VM from a throwaway overlay and hands it the NATS creds over a one-shot virtio-serial bootstrap channel (the only secret that travels outside NATS). `agentd` starts at boot, connects and consumes its queue at once.
3. `agentd` runs `claude` headless (`--output-format stream-json`, partial messages on) and publishes every event (thinking, text, tool calls and results, cost) as `kind: "event"` with a sequence number on `agent.<id>.out`, plus a periodic `heartbeat`. The app renders the stream live.
4. When `claude` stops, `agentd` publishes one `kind: "final"` (status success/failed, result, cost, exit code), waits until it is flushed, and powers the VM off. There is no mid-run interaction and no MCP server: an agent that needs input ends with a question in its result, and the app decides what to do next (a new run). Agents never message each other: everything goes to the app, which decides the flow.
5. The app knows a run is over when `final` arrives, or when QEMU exits without one (crash: failed) or the heartbeat stops (hung: it kills QEMU). Either way it kills QEMU if still alive and deletes the overlay. Unacknowledged messages stay in JetStream so a run can be retried.

Pieces to build, in this order:
1. (Done, see `broker.rs`.) Broker lifecycle in Rust: start `nats-server` with a generated config (JetStream on, a store dir under the app data dir, no anonymous access), create and revoke per-agent users, `async-nats` client in the app.
2. Networking: boot with `-netdev user,restrict=on` plus `guestfwd` to the broker port only. Claude Code needs `api.anthropic.com`, so egress goes through an HTTP proxy in the app, also exposed by `guestfwd`, with a domain allowlist. Never leave plain user-mode networking on: the guest could reach every loopback service on the host through `10.0.2.2`.
3. `agentd`: one static Rust (musl) binary in the image, an OpenRC service. Built once per arch in `build-images.yml` and copied into the image. It reads the bootstrap channel, is the NATS consumer, runs `claude` as an unprivileged user (the VM is the sandbox, so permission prompts are skipped), streams its output and ends the VM as described above. Large events are chunked or truncated to stay under the message limit. The Claude token reaches only the `claude` process environment.
4. Protocol: JSON messages with a version field, defined in a shared Rust crate; generate the TypeScript types from it.
5. UI: a run page with the live event stream, backed by the JetStream log (also gives replay).
6. Orchestrator: the run store and engine first (run one agent end to end, handle `needs_input` by asking the user in the UI), then the planner, then the pipeline graph (stages, dependencies, conditions) on top of the messages, run by the engine. There is no direct agent-to-agent chat.

**agentd (planned, not built yet)**

The program that runs inside each agent VM, one task per boot. A static Rust (musl) binary, one per arch, built in `build-images.yml` and installed in the image as an OpenRC service. It shares its message types with the app through a Rust crate (TypeScript types generated from it, messages carry a protocol version). No SSH, no MCP server. Lifecycle:
1. Read the NATS creds from the one-shot virtio-serial bootstrap channel, connect, and consume the task from its own queue (`get_consumer_from_stream`, the agent cannot look up the stream).
2. Start `claude` headless as an unprivileged user with the model and thinking level from the task. The VM is the sandbox, so permission prompts are skipped. The Claude token goes only into that process's environment.
3. Stream every event as `kind: "event"` with a sequence number, plus a periodic `heartbeat`. Chunk or truncate events that would exceed the message limit.
4. When `claude` stops, publish one `final` (`done | needs_input | failed`, result, questions, session, workspace, cost, exit code), wait for the flush, and power the VM off. If `claude` or `agentd` itself fails, still try to publish a `failed` final.
The app treats QEMU exiting without a `final` as a failed run and kills a VM whose heartbeat stops.

**Orchestrator (planned)**

The orchestrator is the only party that talks to the user and to agents. Two parts:
- The **engine** (Rust, deterministic, in the app): allocates identities, boots and supervises runs, stores their results, enforces limits (retries, clarification rounds, budget, timeouts) and executes decisions. It is the only thing that can start a run.
- The **planner** (an LLM, itself a Claude Code run in a VM with no tools and no workspace): reads agent results, talks with the user and decides the next step. It answers in a structured decision (`ask_user`, `run_agent`, `done`) that the engine validates before acting. This keeps the engine safe even when the planner is wrong, and needs no API key.

Vocabulary: an **agent** is a logical worker (stable id, role, profile, its task thread); a **run** is one VM execution of an agent with its own short-lived identity. An agent can have several runs.

Clarification loop: `final` carries `status: done | needs_input | failed`, the `questions` when input is needed, the Claude `session` (id and transcript) and the `workspace` (git bundle or patch). `agentd` asks Claude to end with a structured block (the Claude Code JSON-schema output option if available, else a fenced JSON block it parses; if neither parses, the planner reads the free text). On `needs_input` the engine stores the run, the planner turns the questions into something for the user, the user answers in the UI, and the engine starts a new run for the same agent: workspace restored from the bundle and the session resumed from the stored transcript (`--resume`), or, if that is not possible, the prompt carries the earlier summary plus the Q&A. Cap the rounds.

The run store (events, finals, session transcripts, workspaces) lives on disk in the app data dir, separate from NATS (whose store is wiped on start), so a conversation survives restarts.

Code goes in and out as messages (a `git bundle` or patch), each agent on its own branch, and the app owns the central repository and the merges. No folders are shared with the host. Treat everything an agent sends as untrusted: validate the schema and the size.

**Release**
- `release.yml` triggers on `v*` tags or manual dispatch, and builds macOS (aarch64 and x86_64), Linux and Windows.
- The version must be kept in sync in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
- `release-cache.yml` warms Rust caches on main because tag builds can only restore default-branch caches.
