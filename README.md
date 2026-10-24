<p align="center">
  <img src="turnout-logo.svg" width="120" alt="Turnout" />
</p>

<h1 align="center">Turnout</h1>

Turnout routes any OpenAI-compatible or Anthropic-compatible provider into Codex,
Claude Code and OpenCode. It is a standalone Rust CLI (with a full-screen
terminal UI) plus a desktop app (Electron) and a Vite/React web build.

The name is the railway switch ("makas"): every harness is pointed at the
provider you choose, with one config plan per change.

## Install / build

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
processes: `electron/main.mjs` starts a loopback HTTP server on an ephemeral
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
turnout gateway install        # background agent (launchd/systemd/Task Scheduler)
turnout run opencode --profile work
turnout rollback <backup-id>
```

API keys are encrypted at rest by default: `vault.json` in the data directory is
sealed with XChaCha20-Poly1305 under a random 256-bit data key that lives in the
OS credential service (macOS Keychain, Windows Credential Manager, Linux Secret
Service). A copy of the data directory alone reveals no key, and no passphrase
is needed per command. Config files only ever receive environment-variable
*names*.

`TURNOUT_CREDENTIALS` selects another backend: `keyring` (a separate OS item per
secret), `vault` (Argon2id passphrase via `TURNOUT_VAULT_PASSWORD`), or `file`
(plain `credentials.json`, `0600` file in a `0700` directory — not encrypted).
Upgrading from a build that used the plain file encrypts the keys on first run
and renames the old file to `credentials.json.imported`.

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
