# Turnout

Turnout routes any OpenAI-compatible or Anthropic-compatible provider into Codex,
Claude Code and OpenCode. It is a standalone Rust CLI (with a full-screen
terminal UI) plus a Vite/React prototype of the future desktop app.

The name is the railway switch ("makas"): every harness is pointed at the
provider you choose, with one config plan per change.

## Install / build

```sh
cargo build --release          # target/release/turnout
npm install && npm run dev     # GUI prototype (calls the built CLI)
```

No Node, Python or vendor SDK is needed at runtime. Ports are fixed
(`47821`, plus one per harness) because harness configs embed the gateway URL.

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

## Tests

```sh
cargo test --workspace
npm run build && npm run test:sites
```
