<p align="center">
  <img src="turnout-logo.svg" width="120" alt="Turnout" />
</p>

<h1 align="center">Turnout</h1>

Turnout routes any OpenAI-compatible or Anthropic-compatible provider into Codex,
Claude Code and OpenCode. It is a standalone Rust CLI (with a full-screen
terminal UI) plus a desktop app (Electron) and a Vite/React web build.

The name is the railway switch ("makas"): every harness is pointed at the
provider you choose, with one config plan per change.

## Install

**Windows:** download `turnout-windows-x86_64.exe` from the
[latest release](https://github.com/TheSupremeF/Turnout/releases/latest), then in
PowerShell:

```powershell
.\turnout-windows-x86_64.exe install --agent
```

That copies Turnout to `%LOCALAPPDATA%\Turnout\bin`, adds that folder to your
user `PATH` (no administrator needed), and starts the background Agent from the
installed copy — so deleting the download later cannot break it. Open a new
terminal, then run `turnout`. Without `--agent`, run `turnout gateway install`
whenever you want the Agent.

The first run may be flagged by SmartScreen because releases are unsigned:
More info → Run anyway.

**macOS / Linux:** `turnout install` behaves the same (into `~/.local/bin`) and
prints the `PATH` line to add if the folder is not already on it.

```sh
cargo build --release          # target/release/turnout
npm install && npm run dev     # web GUI in a browser
npm run electron               # desktop app (builds first, then opens)
npm run electron:build         # packaged app (installer + portable, per OS)
```

No Node, Python or vendor SDK is needed at runtime for the CLI. The desktop app
bundles the GUI and drives the CLI; it expects `turnout` beside the executable
or on `PATH`.

Ports are fixed (`47821`, plus one per harness) because harness configs embed
the gateway URL.

## Desktop app (Electron)

`npm run electron` opens the GUI in a native window. The renderer never spawns
processes: `electron/main.cjs` starts a loopback HTTP server on an ephemeral
port that serves the same `/api/turnout/*` bridge the dev server uses, and the
window loads it. Both front ends share `scripts/turnout-bridge.mjs`, so a change
to the API is made once.

The window has no Node integration, no remote module and a strict
Content-Security-Policy; navigation and new windows are denied. The bridge is
bound to `127.0.0.1` and keeps the same `Origin`/`Host` allowlist as the browser
build (see `scripts/bridge-guard.mjs`), because anything that can reach it can
rewrite every harness config on the machine.

`npm run electron:build` produces an installer with electron-builder. Packaging
is unsigned by default; add signing credentials to the environment for
distribution.

## Usage

```sh
turnout                        # full-screen UI on a terminal, help when piped
turnout providers add evren --key-stdin
turnout models list evren
turnout plan-targets claude-code --target evren::deepseek-v4.1-flash \
  --default-target evren::deepseek-v4.1-flash --hybrid
turnout apply
turnout install                # copy into a per-user folder on PATH (--agent too)
turnout gateway install        # background agent (launchd/systemd/Task Scheduler)
turnout run opencode --profile work
turnout rollback <backup-id>
turnout update                 # install the latest release (--check to only look)
```

API keys are encrypted at rest by default: `vault.json` in the data directory is
sealed with XChaCha20-Poly1305 under a random 256-bit key kept in `vault.key`, an
owner-only file beside it. The key is deliberately not in the macOS Keychain:
Keychain access is tied to the binary's code signature, so every rebuild of an
unsigned CLI would prompt again, and the GUI starts the CLI per request. No
passphrase is needed per command. Config files only ever receive
environment-variable *names*.

`TURNOUT_CREDENTIALS` selects another backend: `keyring` (the vault key in the
OS credential service), `vault` (Argon2id passphrase via `TURNOUT_VAULT_PASSWORD`), or `file`
(plain `credentials.json`, `0600` file in a `0700` directory — not encrypted).
Upgrading from a build that used the plain file encrypts the keys on first run
and renames the old file to `credentials.json.imported`.

## Parallel agents

Several coding agents can share one provider through the gateway, for example
Opus in Claude Code handing work to three DeepSeek subagents. Three things keep
that predictable:

- **No silent model switch.** A request for a model is answered by that model
  or fails with the provider's status. Rerouting is opt-in, either for the
  whole route (`--fallback`) or per model, naming exactly where it may go:

  ```sh
  turnout plan-targets claude-code --hybrid \
    --target evren::deepseek-v4.1-flash --target evren::glm-5.3 \
    --default-target evren::deepseek-v4.1-flash \
    --fallback-chain evren::deepseek-v4.1-flash=evren::glm-5.3
  ```

- **Retries before anything else.** A rate limit (429), an overloaded or
  unreachable provider is retried on the same model twice, with exponential
  backoff (1 s, 2 s, capped at 8 s) and jitter so agents that failed together
  do not retry together. Only then is a configured fallback tried. Once a
  response has started streaming it is never retried.
- **A per-provider limit.** `turnout providers limit evren --max-concurrent 4`
  lets four requests run at once; the rest wait in line (up to two minutes)
  instead of tripping the provider's rate limit. The limit covers every
  harness the Agent serves, because provider quotas are per account.

### Managers and workers

The harness's main model manages; worker agents do delegated tasks on their
own model, in their own git worktree. For Claude Code the manager is the
route's default model and each worker is a subagent file Turnout writes:

```sh
# Manager: Opus on Claude's own login. Shows the change and asks first.
turnout agents manager anthropic::claude-opus-5-5

turnout agents plan deepseek-worker --model evren::deepseek-v4.1-flash --parallel 3
turnout agents apply             # writes .claude/agents/deepseek-worker.md
turnout agents list              # manager, workers, and whether each is routable
turnout agents rollback          # undo the last agent change
```

Changing the manager rewrites the Claude Code settings shared by the CLI and
the VS Code extension, so `agents manager` shows the diff and asks before
writing; without a terminal it only saves the plan (confirm with
`turnout apply --harness claude-code`, or pass `--yes`). The worker models must
already be part of the route (`turnout plan-targets`).

Any pairing works the same way: make GPT-6 the manager and the workers
DeepSeek, or Opus the default with one GPT-6 and one DeepSeek worker. Turnout
refuses a worker on a model the route does not serve, never takes over an agent
file it did not write, and `--scope user` writes to `~/.claude/agents` instead
of the project. `--parallel` is advice the manager reads; the hard cap is
`turnout providers limit`. Claude Code loads agents when a session starts, so
open a new session after a change.

OpenCode works the same with `--harness opencode` on every `agents` command:
workers go to `.opencode/agent/<name>.md` (or `agent/` in OpenCode's global
config directory) as `mode: subagent`, with the model spelled the way the
OpenCode route declares it (`turnout/<model>` through the gateway). OpenCode
has no worktree isolation, so parallel workers share the checkout and should
get separate files to change; restart OpenCode to load a change.

Codex needs no worker files: its manager picks a model for each sub-agent it
spawns (`spawn_agent`), from the models the Codex route provides, so the route
alone decides which workers are possible. A GPT manager can hand work to a
DeepSeek worker: Turnout marks routed models as sub-agent capable in Codex's
catalog, passes Codex's namespaced agent tools and inter-agent messages through,
and asks OpenAI for plain-text task messages, since OpenAI-encrypted ones are
readable only by OpenAI models.

Each line in the request log (`requests/<harness>.jsonl` in the data
directory) records the model that was asked for, the one that answered,
`fallback`, `retries` and `queued_ms`.

## Logo

`icon.svg` is the source artwork. It styles itself with CSS classes and a
`prefers-color-scheme` block, so `scripts/build-logo.mjs` resolves that into
plain attributes with colour tokens (`scripts/logo-artwork.mjs`); the class
names would otherwise collide with the app's stylesheet and resvg would ignore
the dark palette. `scripts/logo.mjs` is the only consumer.

After changing the artwork:

```sh
node scripts/build-logo.mjs && node scripts/generate-assets.mjs
```

## Tests

```sh
cargo test --workspace
npm run build && npm run test:sites
```
