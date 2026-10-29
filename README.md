# CC Agent Flows

**A team of AI agents that delivers your work end to end, locked inside virtual machines you control.**

CC Agent Flows is a desktop app (macOS, Windows, Linux) that turns [Claude Code](https://claude.com/claude-code) into a coordinated team. You describe what you want in plain language. A group of specialised agents (a project manager, an analyst, an architect, a developer, a reviewer, a tester, or whatever roles you define) plans the work, hands it around, checks each other's results and reports back to you. Every agent works inside its own sealed virtual machine, so you can let it do real work without handing it the keys to your computer or your accounts.

> **Status:** early and under active development. The architecture described below is implemented; parts of it have not yet been exercised on every platform or with real accounts. See the notes under [Security](#security-and-honest-limits).

---

## Why it exists

Coding agents are powerful, and the more freedom you give them the more useful they get. Freedom is also the risk: an agent that can run commands, read files and reach the internet on your own machine can leak a token or break something because of one bad instruction, including one hidden inside a ticket, a web page or a dependency.

Most tools answer with permission prompts. CC Agent Flows answers with architecture: **let the agent do anything, inside a box that has nothing worth stealing and nowhere to go.**

## What you get

### A real team, not a single chat
- **Workflows as an org chart.** Draw who may hand work to whom. Coordinators decide what to delegate, in what order and how often; workers do the job and report back. Loops such as "review, fix, review again" are just agents calling each other.
- **You stay in control, as much or as little as you like.** Agents can ask you questions, or run unattended and write down their assumptions. Approval gates stop a handover until you say yes.
- **Start work from anywhere.** Chat, a schedule (cron), or a webhook can all start a session.
- **Watch it happen.** The live session view shows a graph of the team, who handed what to whom, each agent's activity and the machine it runs on. A stopped or failed session resumes where it left off instead of starting over.
- **Projects.** Each project keeps its own workflows, agents, images, variables, secrets and allowed hosts, on top of settings shared by all.

### Uses your Claude subscription
- Sign in with a Claude Code personal subscription token. **No API keys, no per-token billing surprises.**
- Add several accounts: the app rotates through them and rests an account that hits its usage limit, so long jobs keep going.

### Bring your own tools
- **Images** are the disks agents run on. A ready-made base image is built in; add your own with a short script (git, Python, a browser, a Jira CLI…) and optional docs that are added to the agent's instructions.
- **Integrations** (Jira, GitHub…) are set up once: hosts, variables, secrets and credentials in one place, then picked per agent.
- A **demo project** with a full delivery pipeline (orchestrator, project manager, analyst, architect, developer, reviewer, tester) is one click away.

### Yours to move and keep
- One encrypted file exports everything (projects, agents, workflows, secrets, accounts) and imports it on another machine.
- Analytics show tokens, duration and run counts per session, agent and day.
- Everything runs on your computer. There is no cloud service in the middle.

---

## How it works

1. **You write a request** in a session ("Deliver the tickets marked ready in Jira").
2. **The workflow takes over.** The first agent in the org chart reads the request and decides who should do what.
3. **Each agent run is a fresh virtual machine.** The app boots a small, throwaway Linux VM (QEMU, bundled with the app), starts Claude Code in it, gives it the task and streams back everything it does.
4. **Agents never talk to each other directly.** The app is the hub: it collects each result, decides the next step according to your workflow and starts the next agent.
5. **When an agent finishes, its VM is destroyed.** Nothing carries over except the result.

```
            You
             │  request
             ▼
   ┌───────────────────┐
   │  CC Agent Flows   │  workflow engine · egress proxy · your secrets
   └──┬─────┬─────┬────┘
      │     │     │      one private channel per VM
      ▼     ▼     ▼
    [VM]  [VM]  [VM]     sealed, disposable, nothing inside worth stealing
```

---

## Security, and honest limits

Security is the point of this project, not an add-on. The design is **defence in depth**: several independent layers, so that one failing does not expose the rest.

**What an agent can and cannot do**

| Layer | What it means for you |
|---|---|
| **Full virtual machine** | Each agent is inside its own hardware-virtualised VM, not a container sharing your kernel. It starts from a throwaway copy of the disk and is deleted afterwards. |
| **No shared folders, no code transfer** | The VM has no access to your files. Its only link to the outside is one private channel to the app. |
| **Sealed network** | The VM's network card can reach exactly one thing: a tunnel to the app's own proxy. It cannot see your LAN, your computer or the open internet. |
| **Deny-by-default internet** | The proxy only lets through HTTPS to hosts on your allowlist, per project and per agent. IP addresses, private ranges and everything else are refused. |
| **Your credentials never enter the VM** | The app adds your Claude token, and tokens for services like GitHub or Jira, to matching requests *as they leave your computer*. The agent only sees a placeholder, so it cannot read, print or leak the real thing. |
| **Hardened guest** | No SSH server, no sudo, no setuid programs. The agent runs as an unprivileged user, hidden from the process that supervises it. |
| **Untrusted by design** | Everything an agent sends back is treated as data: size-limited, validated, never executed. |
| **Secrets at rest** | Stored in your operating system's credential store. Exports are encrypted with a key derived from your password (Argon2id, XChaCha20-Poly1305). |

**What we do not claim.** No system is unbreakable, and we would rather tell you where the edges are:

- Secrets you place directly in an agent's environment (instead of using credential injection) do reach that agent. Prefer injection, and use least-privilege tokens.
- An agent can still talk to the hosts you allow. Keep allowlists short: a manipulated agent could send what it knows to a host you permitted.
- Agents can be misled by what they read, such as ticket text or web pages (prompt injection). The sandbox limits the damage; it does not make an agent immune to bad input.
- The isolation rests on the virtual machine monitor (QEMU) and the hardware. A flaw there would affect any VM-based tool.
- The project is young. Several paths (other operating systems, other CPU architectures, long runs with real accounts) are not yet verified end to end.

The result is a setup where giving an agent real autonomy is a reasonable decision rather than a leap of faith.

---

## Under the hood

- **App:** [Tauri 2](https://tauri.app) (Rust backend) with a React 19 + TypeScript frontend.
- **Virtualisation:** QEMU, bundled per platform, hardware-accelerated (KVM, Hypervisor.framework, WHPX) with a software fallback.
- **Guest:** a minimal Alpine Linux image, plus a tiny supervisor script that runs Claude Code and reports back over a virtio-serial channel.
- **Network:** an SSH tunnel into an egress proxy in the app, with per-run TLS inspection used solely to inject credentials.

## Development

Package manager is pnpm; Rust edition 2024.

```bash
pnpm install
pnpm tauri dev      # run the full app
pnpm build          # typecheck and bundle the frontend
cargo test --manifest-path src-tauri/Cargo.toml
```

Linux needs `libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`. See [CLAUDE.md](CLAUDE.md) for the architecture in depth.
