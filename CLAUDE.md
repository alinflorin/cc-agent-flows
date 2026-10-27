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
- `broker.rs`, `callout.rs`, `idp.rs`: the agent message bus and identity. `broker.rs` runs the bundled `nats-server` on `127.0.0.1` (random port, started in `setup`, killed on `RunEvent::Exit`, a stale one from a killed run is stopped via `nats.pid`, status via `broker_status`, restart via `broker_restart`). The server runs in config mode with one account (`APP`, JetStream on) and an **auth callout**: the app itself connects with an nkey (public key only in the config file), and every other connection is handed to `callout::serve`, a service in the app that checks the connection's auth token. Keys are regenerated on each start and the JetStream store under the app local data dir is wiped with them.

  Rule: agents never talk to each other, only to the app, and the app decides the flow. `Broker::allocate(project_id, session_id, profile, ttl)` creates an in-memory `Identity` (id = a random guid, name `spiffe://ccagentflows.local/<projectId>/<sessionId>/<profile>/agent-<guid>` where profile is the image profile such as `developer`, expiry; the three segments are checked to be `[a-z0-9-]` so they cannot inject path parts), the pull consumer `in-<id>` on stream `AGENTS`, and a key pair, and registers the identity with the provider. It returns the id, the consumer name and the private key (seed): **that is all an agent needs**, there are no NATS credentials.

  `idp.rs` is an OIDC provider for the agents, on `http://127.0.0.1:<random port>` (`/.well-known/openid-configuration`, `/jwks.json`, `POST /token`). An agent trades a single-use proof signed with its key (OAuth2 `client_credentials` with `private_key_jwt`: `iss`=`sub`=its SPIFFE name, `aud`=the token endpoint, `exp` within 5 minutes) for a standard EdDSA JWT for the audience it asks for (`sub` = SPIFFE name, 5 minute `exp` capped at the identity's expiry, plus `agent`, `project`, `session`, `profile`). Any service can verify these from the JWKS; services inside the app call `Idp::verify`. `Idp::mint(id, audience)` makes a token host-side. The signing key and issuer port change on every start.

  NATS accepts those tokens through the callout: an agent connects with the token for audience `nats` as its auth token (async-nats `with_token`; `agentd` should use `with_auth_callback` so each reconnect fetches a fresh one). `callout::answer` verifies it with `Idp::verify` and answers with a NATS user allowed only: publish `agent.<id>.out`, read its own consumer (`$JS.API.CONSUMER.INFO|MSG.NEXT`, `$JS.ACK`), subscribe `_INBOX.>`. A session lasts at most `SESSION_TTL` (15 minutes) or until the identity expires, then the agent reconnects with a new token. `release(id)` removes the identity from the provider, deletes its consumer and makes the callout refuse its reconnects, but a connection already open lasts until its session ends. The agent cannot look up the stream, so `agentd` must use `get_consumer_from_stream`. The NATS callout requests are not signature-checked (see `callout.rs`).

  The router collects `agent.*.out`, stamps each `{kind, body}` with the sender taken from the subject (unforgeable), and puts it on `Broker::inbox` (broadcast) for the orchestrator; malformed messages get an `error` from `app`. The orchestrator replies or forwards with `Broker::send(id, msg)`. Since an agent can publish nowhere else, it cannot reach another agent's queue or speak as one. Tests run against the real server (skipped if `pnpm nats:fetch` was not run, and on Windows). Nothing calls `allocate` yet.
- `secrets.rs`: the Claude setup token lives in the OS credential store (`keyring`), never in `settings.json`. It is write-only from the frontend (`claude_token_set|status|clear`); backend code reads it with `secrets::claude_token()`. Linux needs a running Secret Service.
- `can_self_update()`: on Linux only an AppImage can self-update.
- Tauri permissions live in `src-tauri/capabilities/default.json`. New plugin APIs used from the frontend need entries there.

**QEMU bundling pipeline** (spans several files)
- `scripts/qemu/version.json` pins the QEMU version and source sha256.
- `build.sh` compiles QEMU from source on each OS (MSYS2 on Windows). `.github/workflows/build-qemu.yml` runs it and publishes the result as a prerelease tagged `qemu-<version>`, with assets named `qemu-<target-triple>.tar.gz`. It triggers on changes to the version file, the script or the workflow.
- `scripts/qemu/fetch.mjs` downloads that asset for the target triple (`$TAURI_ENV_TARGET_TRIPLE`, else the host) and stamps `.version`. `tauri.conf.json` bundles `resources/qemu/` as `qemu/`. Everything in that directory except its README is git-ignored.
- Upgrading QEMU means bumping `version.json` and pushing to main, then waiting for the prerelease before building a release.

**Agent VM base image** (`scripts/images/`)
- `build.sh <x86_64|aarch64> <out> <agentd-binary>` builds an Alpine qcow2 with Claude Code, using the vendored `vendor/alpine-make-vm-image` (v0.13.4, with a marked LOCAL PATCH that kills processes still holding the image before unmounting). Alpine is pinned in `version.json`. Claude Code comes from the official installer at build time, unpinned. It needs root and Linux, and must run natively per arch. `provision.sh` runs in the chroot.
- Profiles are presets and user-made specs applied on top of the base image by the app, in a builder VM (not built in CI, not built yet). A profile is `<name>.json` (`description`, `packages`, `disk_size`) plus an optional `<name>.sh` hook run as root in the guest after the packages. Presets live in `src-tauri/resources/profiles/` (`base`, `developer`), ship with the app and cannot be edited or deleted in the UI, only built; user profiles live in the app data dir. Built images are cached by hash of the spec and the base image. After a build the app checks `agentd` and its service are still intact. A profile that needs a bigger disk than the base (4G) must grow it (`qemu-img resize` plus `resize2fs`).
- `build-images.yml` builds `agentd` (musl, per arch) and the **base** image for both arches on native runners and publishes `agent-base-<arch>.qcow2` to a rolling prerelease tagged `image-base`, overwriting the previous assets. `build.sh <arch> <out> <agentd-binary>` takes no profile.
- Image/app version drift: `agentd` is built into the image (same CI run, same commit), but the app is released separately on tags, and there is no protocol version number, so an app can meet an image whose `agentd` speaks different messages. Accepted for now; keep message changes backward compatible, or release app and image together.
- The image has no SSH server, and `provision.sh` fails the build if one is present. It currently has no host-to-guest control channel: that still has to be added (for example a virtio-serial port). Secrets must not go in command lines, fw_cfg strings or the disk.
- Boot agents from a throwaway `qemu-img create -b` overlay, never from the base image itself.

**Agent communication (planned, not built yet)**

The app is the hub. Agents never talk to each other directly; everything goes through a NATS broker the app runs (bundled `nats-server`, bound to `127.0.0.1`, started by the app). The model is one task per VM:

1. The app allocates an identity for the agent (`Broker::allocate`): a key pair, and via the callout a NATS session allowed only its own queue `agent.<id>.in` (subscribe) and `agent.<id>.out` (publish). It publishes the task on `agent.<id>.in` (JetStream, so it waits if the VM is still booting). The task message carries everything else: prompt, model, thinking level, the Claude setup token, the code.
2. The app boots a QEMU VM from a throwaway overlay and hands it the agent's key, the identity provider's address and the NATS address over a one-shot virtio-serial bootstrap channel (the key is the only secret that travels outside NATS). `agentd` starts at boot, connects and consumes its queue at once.
3. `agentd` runs `claude` headless (`--output-format stream-json`, partial messages on) and publishes every event (thinking, text, tool calls and results, cost) as `kind: "event"` with a sequence number on `agent.<id>.out`, plus a periodic `heartbeat`. The app renders the stream live.
4. When `claude` stops, `agentd` publishes one `kind: "final"` (status success/failed, result, cost, exit code), waits until it is flushed, and powers the VM off. There is no mid-run interaction and no MCP server: an agent that needs input ends with a question in its result, and the app decides what to do next (a new run). Agents never message each other: everything goes to the app, which decides the flow.
5. The app knows a run is over when `final` arrives, or when QEMU exits without one (crash: failed) or the heartbeat stops (hung: it kills QEMU). Either way it kills QEMU if still alive and deletes the overlay. Unacknowledged messages stay in JetStream so a run can be retried.

Pieces to build, in this order:
1. (Done, see `broker.rs`.) Broker lifecycle in Rust: start `nats-server` with a generated config (JetStream on, a store dir under the app data dir, no anonymous access), create and revoke per-agent users, `async-nats` client in the app.
2. Networking. **For now the guest has full network access**: plain QEMU user-mode networking, `-nic user`, no egress control, no proxy, no guest firewall. The guest reaches the host's loopback services at `10.0.2.2:<port>`, so the app's NATS and identity provider stay bound to `127.0.0.1` and are reachable from the VM that way. Because the guest can reach every loopback port of the host, every service the app exposes must authenticate. NATS (auth callout) and the identity provider (signed proofs) already do. Consequences of this stage: the VM can reach any internet host and the host's LAN, and the Claude token and any tokens placed in the VM can be sent anywhere by a prompt-injected agent. Do not put more valuable credentials in the VM until egress control exists. Not verified yet: that the bundled QEMU maps `10.0.2.2` to the host's loopback as expected.

   Egress control (deferred, design decided):
   - Seal the NIC in QEMU instead of in the guest: `-nic user,restrict=on,guestfwd=tcp:10.0.2.100:<port>-tcp:127.0.0.1:<host port>` per allowed service (proxy, NATS, identity provider). The guest keeps a NIC and DHCP, but QEMU on the host lets only those forwards through, so root in the guest cannot bypass it and no nftables or tunnel is needed. A forward target must already be listening when QEMU starts (QEMU connects at startup; the syntax is accepted by the bundled QEMU 11.1.2). Each VM gets its own forward targets, so the proxy's listener port identifies the agent. Not verified: that a real guest connects through `guestfwd` under `restrict=on`, how parallel connections behave, whether guest DNS (10.0.2.3) still answers, and that Alpine's DHCP works.
   - Egress proxy in the app (HTTP CONNECT, on `hyper`; `hudsucker` + `rcgen` for inspect mode), deny by default. Rules per profile: `{host regex, ports, mode: tunnel | inspect, inject}`. Host regexes are anchored, lowercase, trailing dot stripped, IDNs as punycode, compiled with the `regex` crate (linear time) and checked in a `RegexSet`; validated when saved. Injection rules name exact hosts, never wildcards, so a token cannot reach another tenant of the same domain. A guard refuses private, loopback and link-local addresses after the name matches (the proxy resolves the name itself and connects to that address). In inspect mode the CONNECT host, the TLS SNI and the `Host` header must agree. One log line per connection with the agent id, and a clear 403 with the reason on denial (it can become an event for the orchestrator, which may ask the user and add a session-scoped rule). `agentd` sets `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY`; with the sealed NIC, tools that ignore them fail instead of leaking.
   - Credential injection (GitHub, Jira, Confluence tokens from the keyring, never in the VM): inspect mode with a per-start CA the VM trusts (passed at bootstrap, its key never leaves the app; `agentd` sets `NODE_EXTRA_CA_CERTS`, `GIT_SSL_CAINFO`, `SSL_CERT_FILE`), setting the `Authorization` header per host rule (Basic for git over HTTPS, Bearer for APIs), plus placeholder tokens in the VM that the proxy swaps for tools that refuse to run without one. Git push and pull requests need no credential in the VM: the app owns the repository and does them itself. Keep `api.anthropic.com` in tunnel mode. Untested: Claude Code, git and `gh` through an intercepting proxy. HTTP(S) only; git uses HTTPS, not SSH.
   - Rules and credentials are configured in a settings tab, with tokens in the keyring like the Claude one.
3. `agentd`: one static Rust (musl) binary in the image, an OpenRC service. Built once per arch in `build-images.yml` and copied into the image. It reads the bootstrap channel, is the NATS consumer, runs `claude` as an unprivileged user (the VM is the sandbox, so permission prompts are skipped), streams its output and ends the VM as described above. Large events are chunked or truncated to stay under the message limit. The Claude token reaches only the `claude` process environment.
4. Protocol: JSON messages, defined in a shared Rust crate; generate the TypeScript types from it.
5. UI: a run page with the live event stream, backed by the JetStream log (also gives replay).
6. Orchestrator: the run store and engine first (run one agent end to end, handle `needs_input` by asking the user in the UI), then the planner, then the pipeline graph (stages, dependencies, conditions) on top of the messages, run by the engine. There is no direct agent-to-agent chat.

**agentd (built; in the image via CI, untested in a VM)**

Code: `crates/protocol` (messages, `Bootstrap`, `RESULT_SCHEMA`) and `crates/agentd` (`main.rs`, `identity.rs` token exchange, `link.rs` NATS + heartbeat + task queue, `claude.rs` the run), a separate cargo workspace in `crates/` (the app depends on `protocol` by path). Flags: `--bootstrap <path>` (default `/dev/virtio-ports/bootstrap`), `--workspace`, `--claude`, `--run-as <user>` (drops to that user with no_new_privs), `--poweroff`. `src-tauri/src/agentd_e2e.rs` runs the real binary against the real broker with a fake `claude`. The image build (`build-images.yml`) compiles `agentd` per arch for musl in an `agentd` job, `build.sh` takes the binary, `provision.sh` installs it to `/usr/local/bin` with an OpenRC service (`--run-as agent --poweroff`, runlevel default) and `finalize.sh` fails the build if it is missing. Not verified yet: that CI build, that `/dev/virtio-ports/bootstrap` exists on Alpine (needs udev rules or mdev config), and the service in a booted VM. Not done yet: the bootstrap channel on the app side (virtio-serial), hidepid/ptrace hardening, real `claude` run. Workspaces over ~700 KB come back without a bundle (`workspace_error`) until large transfers get another path.

Design:

The program that runs inside each agent VM, one task per boot: it executes the task and reports. A static Rust (musl) binary, one per arch, built in `build-images.yml` and installed in the image as an OpenRC service. It shares its message types with the app through a Rust crate. No SSH, no MCP server, no decisions, no talking to other agents, no state kept between runs.

Lifecycle:
1. Read one message from the one-shot virtio-serial bootstrap channel, then close it: the agent's private key, its id and consumer name, and the addresses of the identity provider and of NATS.
2. Trade a single-use proof signed with that key for an access token (audience `nats`) at `POST <issuer>/token`.
3. Connect to NATS with the token as auth token, using `with_auth_callback` so every reconnect (at least every 15 minutes, the session limit) fetches a fresh one.
4. Consume the task from its own queue with `get_consumer_from_stream` (the agent cannot look up the stream) and ack it once work has started. The task carries the prompt, model, thinking level, the Claude setup token, the code (a git bundle) and, on a retry, the session to resume.
5. Unpack the workspace in a folder of an unprivileged user.
6. Run `claude -p <prompt> --output-format stream-json --verbose --include-partial-messages --model <model> --effort <low|medium|high|xhigh|max> --json-schema <result schema> --session-id <uuid> --dangerously-skip-permissions` (`--resume <id>` instead of `--session-id` on a retry). The token goes only in that process's `CLAUDE_CODE_OAUTH_TOKEN`. The VM is the sandbox, hence no permission prompts. These flags exist in Claude Code 2.1.310; `--effort` takes exactly the levels of the thinking setting, and `--json-schema` gives the structured final answer the orchestrator needs (if a task still ends without it, the planner reads the free text).
7. Stream every line `claude` prints (thinking, partial text, tool calls and results, cost) as `kind: "event"` with a sequence number on `agent.<id>.out`, plus a periodic `heartbeat`. Chunk or truncate events that would exceed the message limit.
8. When `claude` stops, publish one `final`: `status` (`done | needs_input | failed`), result, questions, Claude session (id and transcript), workspace (git bundle), cost and exit code. Wait for the flush, then power the VM off.

Keeping `claude` away from `agentd` (planned; none of it checked against the Alpine image yet):
- Separate users: `agentd` runs as root (or a dedicated `agentd` user) from a root-owned binary and service file; `claude` runs as the unprivileged `agent` user in a workspace only that user owns. `agentd` starts it after `setuid`/`setgid`, with `PR_SET_NO_NEW_PRIVS` and capabilities dropped.
- Nothing to steal from disk: the agent key comes over the serial channel and stays in `agentd`'s memory (never a file, command line or `claude`'s environment); so does the NATS token. The Claude setup token is the exception, it has to be in `claude`'s own environment.
- Hide the process: mount `/proc` with `hidepid=2` and set `kernel.yama.ptrace_scope` to 2 or higher, so `claude` cannot read `agentd`'s `/proc/<pid>` files or ptrace it. Needs checking that the Alpine kernel supports both.
- No escalation path: no `sudo`/`doas`, no root password, and `provision.sh` fails the build on any setuid/setgid binary outside an allowlist (busybox ships several applets that need stripping), like the SSH check.
- One-way channel: `agentd` spawns `claude` and reads its stdout/stderr through pipes; there is no socket, file or signal for `claude` to command `agentd`. Everything `claude` prints is untrusted data: parsed, wrapped in events, truncated, never run as instructions.
- If `claude` still gets root in the guest, the damage is bounded by the host: it can only act as its own agent (publish on `agent.<id>.out`, read its own consumer), it can forge its own `final` or heartbeats (the app validates and decides the flow itself), and the identity, the 15 minute NATS session and the run timeout all expire. While the guest has full network access, root could also reach the internet and the host's loopback ports, which is why egress control matters.

Failure handling:
- `claude` fails to start or crashes: publish `final` with `failed` and the reason.
- `agentd` itself dies: it cannot report, so the app sees QEMU exit without a `final`, or the heartbeat stop, and marks the run failed (killing the VM in the second case).
- The identity provider or NATS cannot be reached: retry for a while, then power off; the app sees the missing heartbeat.

What it needs from outside, not built yet:
- VM networking (step 2 of the plan): for now the guest has full network access and reaches the identity provider and NATS at `10.0.2.2:<port>`; the bootstrap message carries those ports. No proxy settings yet; once egress control exists, `agentd` also sets the proxy and CA environment.
- The issuer. The issuer stays `http://127.0.0.1:<random port>` (it changes on every start). That is fine: the VM connects to `10.0.2.2:<port>`, and only puts the issuer string it got at bootstrap into the `aud` of its proof, which the app compares with its own. Services in the app compare `iss` the same way.
- The shared protocol crate with the messages `task`, `event`, `heartbeat`, `final`.

First step when we build it: the protocol crate, then `agentd` run locally (not in a VM) against the app's real NATS and identity provider, then in the image.

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
