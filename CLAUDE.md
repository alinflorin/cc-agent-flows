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
- `pnpm tauri build`: production bundle. Its before-command runs `qemu:fetch --required` and `images:fetch --required` (fails if either is missing).
- `pnpm images:fetch [triple] [--required]`: copy the committed plain Alpine image for the target arch from `vendor/images` into `src-tauri/resources/images` (see Agent VM base image).
- `pnpm qemu:fetch [triple] [--required]`: unpack the committed QEMU build for the target from `vendor/qemu` into `src-tauri/resources/qemu` (no network).
- Backend: `cargo build|clippy|test --manifest-path src-tauri/Cargo.toml`. CI runs clippy with `-D warnings`. Single test: `cargo test --manifest-path src-tauri/Cargo.toml <name>`.
- `pnpm driver:install` / `pnpm driver`: install and run `tauri-driver` (into `.tools/`, git-ignored) on port 4444. Use `tauri-driver` (WebDriver) for interactive testing of the running app, not only the browser-based `pnpm dev`, since Tauri APIs, QEMU and the prefs store only exist in the real app.

CI (`ci.yml`) runs `pnpm build`, `cargo build`, clippy and test on Linux (x86_64 and arm64), macOS and Windows. Linux needs `libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`.

## Architecture

**Frontend (`src/`)**
- `App.tsx` nests FluentProvider (Fluent UI v9), notifications, updates, then a `createHashRouter` (hash routing because the packaged app serves static files). Pages are in `src/pages/`, and `Layout`/`Sidebar`/`TopBar` form the shell.
- `prefs.ts` holds app preferences. It uses `tauri-plugin-store` (`settings.json`) so Rust can read them too, with a localStorage fallback in a plain browser. `loadPrefs()` must complete before any `getPref` call, and reads are then synchronous from an in-memory cache. New preference keys must be added to the `KEYS` list or they won't be loaded.
- i18n uses i18next with the HTTP backend, so translations are runtime files in `public/locales/{en,ro}/translation.json`. Add every string to both languages.
- `updates.tsx` wraps tauri-plugin-updater. The release endpoint is GitHub `latest.json`.

**Backend (`src-tauri/src/`)**
- `lib.rs` registers plugins (opener, store, updater, process) and commands in `generate_handler!`. New commands must be added there. On Unix, `adopt_shell_path()` runs first in `run()` to import the login shell's PATH, because GUI launches get a minimal PATH. This is how tools like `claude`, nvm and cargo binaries get found. It must stay before any thread spawns because it mutates the environment.
- `qemu.rs`: `command(app, program)` builds a `Command` for any QEMU binary. It uses the bundled tree at `<resource_dir>/qemu/{bin,lib,share/qemu}` (adding `-L share/qemu` for `qemu-system-*`) and falls back to PATH if the build shipped no QEMU. On Windows it sets `CREATE_NO_WINDOW`. Spawn all QEMU processes through it.
- `agent.rs`: the app's end of an agent VM's channel. `drive(stream, bootstrap, task)` takes the VM's virtio-serial socket (any async stream), writes the `Bootstrap`, sends the `Task` when the agent says `ready`, and returns a receiver of everything the agent sends (events, the `final`). Lines are capped at `MAX_MESSAGE`, unparseable lines are skipped, an oversized one ends the run. The stream must stay open until the run is over (see Agent communication). Nothing calls it yet; the run engine will.
- `secrets.rs`: the Claude setup token lives in the OS credential store (`keyring`), never in `settings.json`. It is write-only from the frontend (`claude_token_set|status|clear`); backend code reads it with `secrets::claude_token()`. Linux needs a running Secret Service.
- `can_self_update()`: on Linux only an AppImage can self-update.
- Tauri permissions live in `src-tauri/capabilities/default.json`. New plugin APIs used from the frontend need entries there.

**QEMU bundling pipeline** (spans several files)
- The built QEMU trees are committed as `vendor/qemu/qemu-<target-triple>.tar.gz` (linux x86_64 and aarch64, macOS x86_64 and aarch64, windows x86_64; checksums in `SHA256SUMS`), so no CI or network is involved.
- `scripts/qemu/fetch.mjs` unpacks the archive for the target triple (`$TAURI_ENV_TARGET_TRIPLE`, else the host) and stamps `.version`. `tauri.conf.json` bundles `resources/qemu/` as `qemu/`. Everything in that directory except its README is git-ignored.
- Upgrading QEMU means building it from source on each OS (Linux builds should use an old glibc, e.g. Ubuntu 22.04; macOS needs the HVF entitlement and ad-hoc signing; the old build script is in git history, commit 932a4de), replacing the archives in `vendor/qemu`, and updating `SHA256SUMS`.

**Agent VM base image** (`scripts/images/`)
- The base image is plain Alpine (`alpine-base`, 8G disk, DHCP on eth0, serial console, no SSH, nothing else), committed as `vendor/images/alpine-<x86_64|aarch64>.qcow2` (~60 MB each, qcow2 compressed). No CI builds it. It was made with `alpine-make-vm-image` v0.13.4 (script in git history, commit 932a4de: `scripts/images/build-plain.sh`), natively per arch; x86_64 boots with BIOS, aarch64 with UEFI.
- Claude Code and `agentd` are **not** in the image yet. The app is meant to install them in a builder VM on the user's machine (Claude Code from the official installer, `agentd` is the script `resources/agentd/agentd.sh`, the same for both arches; also `apk add jq util-linux-misc` for `jq`/`setpriv`, no git), cache the result as the real base, and rebuild when the app updates. Not built yet. The old in-image installer (Claude Code, the `agent` user, the `agentd` OpenRC service) is `scripts/images/provision.sh` in commit 932a4de, the model for that step. The builder VM also needs a way to run commands in the plain image (planned: a first-boot hook that mounts a setup disk and runs its script).
- Profiles are presets and user-made specs applied on top of the base image by the app, in a builder VM (not built in CI, not built yet). A profile is `<name>.json` (`description`, `packages`, `disk_size`) plus an optional `<name>.sh` hook run as root in the guest after the packages. Presets live in `src-tauri/resources/profiles/` (`base`, `developer`), ship with the app and cannot be edited or deleted in the UI, only built; user profiles live in the app data dir. Built images are cached by hash of the spec and the base image. After a build the app checks `agentd` and its service are still intact. A profile that needs a bigger disk than the base (4G) must grow it (`qemu-img resize` plus `resize2fs`).
- The plain image ships inside the app: `scripts/images/fetch.mjs` (`pnpm images:fetch [--required]`, run by the dev and build before-commands) copies `alpine-<arch>.qcow2` for the target (aarch64 targets get aarch64, everything else x86_64) from `vendor/images/` into `src-tauri/resources/images/` (git-ignored), and `tauri.conf.json` bundles it as `images/` together with `profiles/`.
- Custom images are valid only for the base they were built from. Their cache key is the hash of the profile spec plus the base image's stamp. After an app update with a new base, the app marks such images stale (shown as "needs rebuild", with a rebuild-all action), keeps the old one usable until its replacement builds, then deletes it. Custom images are built standalone (flattened), not as overlays: the bundled base sits in the install directory, which an update replaces and an AppImage mounts at a different path each run, so a backing file would break.
- There is no image or QEMU build in CI: both are committed (`vendor/qemu`, `vendor/images`).
- Image/app version drift: `agentd.sh` is bundled with the app and installed into the base the app builds, so the app and its `agentd` always match; there is no protocol version number.
- The image has no SSH server, and the setup step must fail if one is present
- Boot agents from a throwaway `qemu-img create -b` overlay, never from the base image itself.

**Agent communication**

The app is the hub. Agents never talk to each other; each VM has exactly one channel, a virtio-serial port to the app. There is no message broker, no identity provider and no tokens: the channel belongs to one VM, so the app knows who is talking by which socket a message arrived on. The model is one disposable VM per task:

1. The app boots a QEMU VM from a throwaway overlay of the base image (or a profile image) with `-device virtio-serial-pci -chardev socket,id=agent,path=<sock>,server=on,wait=off -device virtserialport,chardev=agent,name=agent`. The guest sees `/dev/virtio-ports/agent` (created by mdev).
2. The app connects to the socket and `agent::drive` writes the `Bootstrap` (agent id, Claude setup token). The app keeps this connection open for the whole run: QEMU drops what the guest has not read when the host disconnects, and the VM boots long after the write. Until the app connects, guest reads return end of file, which `agentd` retries.
3. `agentd` starts at boot, reads the bootstrap, answers `ready` (it asks for its task) and the app sends the `Task` (prompt, model, effort, session id, schema, timeout). There are no heartbeats.
4. `agentd` runs `claude` headless (`--output-format stream-json`, partial messages on) and sends every line (thinking, text, tool calls and results, cost) as `kind: "event"` with a sequence number. The app renders the stream live and appends it to the run log.
5. When `claude` stops, `agentd` sends one `final` (status, result, questions, session, cost, exit code), waits until it is written and powers the VM off. There is no mid-run interaction and no MCP server: an agent that needs input ends with questions in its result, and the app decides what to do next (a new run).
6. The app knows a run is over when `final` arrives, or when the stream ends or QEMU exits without one (crash: failed), or its own deadline passes (the task timeout plus a grace period for boot and shutdown: hung, it kills QEMU). QEMU runs with `-no-reboot -action panic=exit-failure` so a guest kernel panic ends QEMU instead of leaving it silent. Either way it kills QEMU if still alive and deletes the overlay.

Messages are one JSON object per line, `{"kind": ..., "body": ...}`, defined in `src-tauri/src/protocol.rs` (the script builds and parses them by hand with `jq`, so keep the two in step; `agentd_e2e.rs` checks they agree). The wire format has no version number: keep changes backward compatible, or release app and image together.

Pieces to build, in this order:
1. (Done.) The channel: `agent.rs` on the app side and `agentd.sh` on the guest side, tested against the real script over two fifos with a fake `claude`.
2. Networking. **For now the guest has full network access**: plain QEMU user-mode networking, `-nic user`, no egress control, no proxy, no guest firewall (`claude` needs the internet to reach its API). Consequences: the VM can reach any internet host, the host's LAN and the host's loopback services at `10.0.2.2:<port>` (the app exposes none today; any it ever exposes must authenticate), and the Claude token in the VM can be sent anywhere by a prompt-injected agent. Do not put more valuable credentials in the VM until egress control exists. The VM test boots with `-nic none`, since the channel needs no network.

   Egress control (deferred, design decided):
   - Seal the NIC in QEMU instead of in the guest: `-nic user,restrict=on,guestfwd=tcp:10.0.2.100:<port>-tcp:127.0.0.1:<host port>` for the proxy only. The guest keeps a NIC and DHCP, but QEMU on the host lets only that forward through, so root in the guest cannot bypass it and no nftables or tunnel is needed. A forward target must already be listening when QEMU starts (the syntax is accepted by the bundled QEMU 11.1.2). Each VM gets its own forward target, so the proxy's listener port identifies the agent. Not verified: that a real guest connects through `guestfwd` under `restrict=on`, how parallel connections behave, whether guest DNS (10.0.2.3) still answers, and that Alpine's DHCP works.
   - Egress proxy in the app (HTTP CONNECT, on `hyper`; `hudsucker` + `rcgen` for inspect mode), deny by default. Rules per profile: `{host regex, ports, mode: tunnel | inspect, inject}`. Host regexes are anchored, lowercase, trailing dot stripped, IDNs as punycode, compiled with the `regex` crate (linear time) and checked in a `RegexSet`; validated when saved. Injection rules name exact hosts, never wildcards, so a token cannot reach another tenant of the same domain. A guard refuses private, loopback and link-local addresses after the name matches (the proxy resolves the name itself and connects to that address). In inspect mode the CONNECT host, the TLS SNI and the `Host` header must agree. One log line per connection with the agent id, and a clear 403 with the reason on denial (it can become an event for the orchestrator, which may ask the user and add a session-scoped rule). `agentd` sets `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY`; with the sealed NIC, tools that ignore them fail instead of leaking.
   - Credential injection (GitHub, Jira, Confluence tokens from the keyring, never in the VM): inspect mode with a per-start CA the VM trusts (passed in the bootstrap, its key never leaves the app; `agentd` sets `NODE_EXTRA_CA_CERTS`, `GIT_SSL_CAINFO`, `SSL_CERT_FILE`), setting the `Authorization` header per host rule (Basic for git over HTTPS, Bearer for APIs), plus placeholder tokens in the VM that the proxy swaps for tools that refuse to run without one. Git push and pull requests need no credential in the VM: the app owns the repository and does them itself. Keep `api.anthropic.com` in tunnel mode. Untested: Claude Code, git and `gh` through an intercepting proxy. HTTP(S) only; git uses HTTPS, not SSH.
   - Rules and credentials are configured in a settings tab, with tokens in the keyring like the Claude one.
3. (Done, see below.) `agentd`.
4. (Done.) Protocol: JSON messages in a shared Rust crate. Still to do: generate the TypeScript types from it.
5. UI: a run page with the live event stream, backed by the run log on disk (also gives replay).
6. Orchestrator: the run store and engine first (run one agent end to end, handle `needs_input` by asking the user in the UI), then the planner, then the pipeline graph (stages, dependencies, conditions) on top of the messages, run by the engine. There is no direct agent-to-agent chat.

**agentd (built; installed into the image by the app, not yet implemented)**

Code: `src-tauri/resources/agentd/agentd.sh`, a POSIX/busybox `sh` script bundled with the app (`tauri.conf.json` maps `resources/agentd/` to `agentd/`); the messages are `src-tauri/src/protocol.rs`. It needs `jq`, `setpriv` and `setsid` in the image, and nothing else (no git, no compiled code, one file for both arches). Flags: `--channel <path>` (default `/dev/virtio-ports/agent`), `--out <path>` (tests only: a second file for output), `--workspace`, `--claude`, `--run-as <user>` (setpriv to that user with no_new_privs), `--poweroff`. The channel is opened once, read-write (a virtio port cannot be opened twice). Events go through one `jq -cR` that wraps each line with its line number as the sequence, turns non-JSON into `{type: raw}` and replaces lines over 400 KB with a stub; the final is built by `jq` from the `result` line. `claude` runs under `setsid` so a timeout kills its whole process group. `src-tauri/src/agentd_e2e.rs` runs the script with a fake `claude` over fifos (Linux, needs `jq`): a full task and a timeout. Not done yet: the OpenRC service that starts it (`--run-as agent --poweroff`, runlevel default) in the image setup, a boot test in the real image, hidepid/ptrace hardening, a real `claude` run with a valid token. The prompt travels as an argument, so it is limited to ~128 KB. Feeding it to `claude` through stdin instead is a possible later fix, only if the limit is ever hit.

Design:

The program that runs inside each agent VM, one task per boot: it executes the task and reports. A shell script installed in the image by the app as an OpenRC service. Its messages are defined in the app (`protocol.rs`). No SSH, no MCP server, no decisions, no talking to other agents, no state kept between runs.

Lifecycle:
1. Open the virtio-serial port and read the `Bootstrap` (waiting up to 5 minutes for the app to connect): the agent id and the Claude setup token.
2. Answer `ready` and read the `Task`: the prompt, model, thinking level, the result schema and, on a retry, the session to resume.
3. Create the workspace, an empty scratch folder owned by the unprivileged user.
4. Run `claude -p <prompt> --output-format stream-json --verbose --include-partial-messages --model <model> --effort <low|medium|high|xhigh|max> --json-schema <result schema> --session-id <uuid> --dangerously-skip-permissions` (`--resume <id>` instead of `--session-id` on a retry). The token goes only in that process's `CLAUDE_CODE_OAUTH_TOKEN`. The VM is the sandbox, hence no permission prompts. These flags exist in Claude Code 2.1.310; `--effort` takes exactly the levels of the thinking setting, and `--json-schema` gives the structured final answer the orchestrator needs (if a task still ends without it, the planner reads the free text).
5. Send every line `claude` prints as `kind: "event"` with a sequence number. A line that would exceed half the message limit is replaced by a stub with its size and first 4 KB.
6. When `claude` stops, send one `final`: `status` (`done | needs_input | failed`), result, questions, Claude session id, cost and exit code. Wait until it is written, then power the VM off.

Keeping `claude` away from `agentd` (planned; none of it checked against the Alpine image yet):
- Separate users: `agentd` runs as root (or a dedicated `agentd` user) from a root-owned binary and service file; `claude` runs as the unprivileged `agent` user in a workspace only that user owns. `agentd` starts it after `setuid`/`setgid`, with `PR_SET_NO_NEW_PRIVS` and capabilities dropped.
- Nothing to steal from disk: the bootstrap comes over the serial channel and stays in `agentd`'s memory (never a file or command line). The Claude setup token has to be in `claude`'s own environment, nothing else is.
- Hide the process: mount `/proc` with `hidepid=2` and set `kernel.yama.ptrace_scope` to 2 or higher, so `claude` cannot read `agentd`'s `/proc/<pid>` files or ptrace it. Needs checking that the Alpine kernel supports both.
- No escalation path: no `sudo`/`doas`, no root password, and the image setup fails on any setuid/setgid binary outside an allowlist (busybox ships several applets that need stripping), like the SSH check.
- One-way channel: `agentd` spawns `claude` and reads its stdout/stderr through pipes; there is no socket, file or signal for `claude` to command `agentd`. Everything `claude` prints is untrusted data: parsed, wrapped in events, truncated, never run as instructions.
- If `claude` still gets root in the guest, the damage is bounded by the host: the guest has only its own channel, so it can forge its own events, `final` (the app validates and decides the flow itself), and the run timeout ends it. While the guest has full network access, root could also reach the internet, the host's LAN and loopback ports, which is why egress control matters.

Failure handling:
- `claude` fails to start or crashes: send `final` with `failed` and the reason.
- `agentd` itself dies, or never gets its bootstrap or task: it cannot report, so the app sees the stream end, QEMU exit without a `final`, or the run's deadline pass, and marks the run failed (killing the VM in the last case).

**Orchestrator (planned)**

The orchestrator is the only party that talks to the user and to agents. Two parts:
- The **engine** (Rust, deterministic, in the app): allocates identities, boots and supervises runs, stores their results, enforces limits (retries, clarification rounds, budget, timeouts) and executes decisions. It is the only thing that can start a run.
- The **planner** (an LLM, itself a Claude Code run in a VM with no tools and no workspace): reads agent results, talks with the user and decides the next step. It answers in a structured decision (`ask_user`, `run_agent`, `done`) that the engine validates before acting. This keeps the engine safe even when the planner is wrong, and needs no API key.

Vocabulary: an **agent** is a logical worker (stable id, role, profile, its task thread); a **run** is one VM execution of an agent with its own short-lived identity. An agent can have several runs.

Clarification loop: `final` carries `status: done | needs_input | failed`, the `questions` when input is needed, the Claude `session` (id and transcript). `agentd` asks Claude to end with a structured answer (`--json-schema`; if it is missing the planner reads the free text). On `needs_input` the engine stores the run, the planner turns the questions into something for the user, the user answers in the UI, and the engine starts a new run for the same agent: the session resumed from the stored transcript (`--resume`), or, if that is not possible, the prompt carries the earlier summary plus the Q&A. Cap the rounds.

The run store (events, finals, session transcripts) lives on disk in the app data dir, so a conversation survives restarts.

**Workspace transfers will never happen:** the protocol carries no code in either direction (no archive in the `Task`, none in the `final`), and the image has no git. The workspace in the VM is a scratch folder; how an agent gets code and hands work back is decided elsewhere (not by this channel). No folders are shared with the host. Treat everything an agent sends as untrusted: validate the schema and the size.

**Release**
- `release.yml` triggers on `v*` tags or manual dispatch, and builds macOS (aarch64 and x86_64), Linux (x86_64 and aarch64, on `ubuntu-22.04` and `ubuntu-22.04-arm`) and Windows.
- The version must be kept in sync in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
- `release-cache.yml` warms Rust caches on main because tag builds can only restore default-branch caches.
