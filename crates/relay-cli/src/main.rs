//! `turnout` — the standalone CLI. Same core as the desktop app; usable over SSH/CI.

mod agents;
mod home;
mod install;
mod update;
mod usage;

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use std::sync::mpsc;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use futures::future::BoxFuture;
use rand::RngCore;
use relay_config::BackupStore;
use relay_core::provider::EventStream;
use relay_core::{
    BackupId, ChatRequest, ChatResponse, CredentialRef, HarnessAdapter, HarnessId, Model,
    ModelCapabilities, ProtocolDialect, ProviderAdapter, ProviderConfig, ProviderId, Route,
    RouteContext, Surface, TestResult, GATEWAY_TOKEN_ENV,
};
use relay_credentials::CredentialStore;
use relay_gateway::{Gateway, GatewayConfig, GatewayModelRoute, TrafficPolicy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use home::{Home, SavedRoute, SavedTarget};

#[derive(Parser)]
#[command(
    name = relay_core::BIN_NAME,
    version,
    about = "Route any AI provider into Codex, Claude Code and OpenCode"
)]
struct Cli {
    /// Without a subcommand the full-screen interface opens.
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detect or start official harness OAuth sessions
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Manage provider connections
    #[command(subcommand)]
    Providers(ProvidersCmd),
    /// Model discovery
    #[command(subcommand)]
    Models(ModelsCmd),
    /// Test a provider connection with one tiny request
    Test {
        provider: String,
        #[arg(long)]
        model: String,
    },
    /// Detect installed harnesses
    #[command(subcommand)]
    Harness(HarnessCmd),
    /// Preview the config changes for a route (writes nothing)
    Plan {
        harness: String,
        #[arg(long)]
        provider: String,
        /// Model to expose; repeat the flag or pass a comma-separated list
        #[arg(long, required = true, value_delimiter = ',')]
        model: Vec<String>,
        /// Model selected when the harness starts (defaults to the first --model)
        #[arg(long)]
        default_model: Option<String>,
        /// Config file to edit instead of the discovered one
        #[arg(long)]
        config: Option<PathBuf>,
        /// Force the local gateway even if the protocols already match
        #[arg(long)]
        gateway: bool,
    },
    /// Preview a provider-aware route. Target format: provider::model.
    PlanTargets {
        harness: String,
        #[arg(long, required = true)]
        target: Vec<String>,
        #[arg(long)]
        default_target: String,
        #[arg(long)]
        config: Option<PathBuf>,
        /// Save a launch-only profile instead of editing the harness config.
        /// The harness config keeps its official provider; `turnout run --profile`
        /// injects this route into one process.
        #[arg(long)]
        profile: Option<String>,
        /// Claude Code only: keep the built-in Claude models (Claude's own
        /// login, forwarded to Anthropic by the gateway) and add the selected
        /// non-Anthropic models after them.
        #[arg(long, conflicts_with = "profile")]
        hybrid: bool,
        /// Let every model of the route be rerouted to the route's other
        /// models when its provider keeps failing. Off by default.
        #[arg(long)]
        fallback: bool,
        /// Reroute one model only to the listed ones, in order:
        /// `provider::model=provider::model[,provider::model...]`. Repeatable;
        /// takes precedence over --fallback for that model.
        #[arg(long = "fallback-chain", value_name = "FROM=TO[,TO...]")]
        fallback_chain: Vec<String>,
        /// Accepted for older scripts; fallback is already off by default.
        #[arg(long, hide = true, conflicts_with = "fallback")]
        no_fallback: bool,
    },
    /// Apply the most recent plan (with backup)
    Apply {
        #[arg(long)]
        harness: Option<String>,
    },
    /// Restore the files from a backup (latest when no id is given)
    Rollback { id: Option<String> },
    /// Launch a harness with the saved route, injecting keys at launch only
    Run {
        harness: String,
        /// Use a launch-only profile saved with `turnout plan-targets --profile`
        /// instead of the harness's applied config route
        #[arg(long)]
        profile: Option<String>,
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Persistent loopback gateway used by editor extensions
    #[command(subcommand)]
    Gateway(GatewayCmd),
    /// Manage where API keys are stored
    #[command(subcommand)]
    Credentials(CredentialsCmd),
    /// List saved launch profiles and the route applied to each harness
    Profiles {
        /// Machine-readable output for the GUI
        #[arg(long)]
        json: bool,
    },
    /// Inspect backups (applied plans) and the Agent log
    History {
        #[arg(long)]
        json: bool,
        /// How many backup records to include
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Check credentials, harnesses and (with --online) providers
    Doctor {
        #[arg(long)]
        online: bool,
    },
    /// Worker agents the harness's main model hands work to (Claude Code, OpenCode)
    #[command(subcommand)]
    Agents(AgentsCmd),
    /// Copy this executable to a per-user folder on PATH (re-points the Agent)
    Install {
        /// Also install the background Agent from the installed copy
        #[arg(long)]
        agent: bool,
    },
    /// Install the latest release from GitHub (restarts the Agent if installed)
    Update {
        /// Only report whether a newer release exists
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum AuthCmd {
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Show the subscription usage limits of the official logins
    Usage {
        /// `openai`/`chatgpt` or `anthropic`/`claude`; both when omitted
        provider: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Open the official provider sign-in flow in the system browser
    Login { provider: String },
    #[command(hide = true)]
    CompleteOpenAi,
}

#[derive(Subcommand)]
enum CredentialsCmd {
    /// Show which store is in use and which credentials it holds (never the secrets)
    Status,
    /// Copy keys from the OS keyring into the credentials file (asks the OS once per item)
    Migrate {
        /// Remove each keyring item after it was copied successfully
        #[arg(long)]
        delete_old: bool,
    },
}

#[derive(Subcommand)]
enum ProvidersCmd {
    List {
        /// Emit safe machine-readable connection metadata (never API keys)
        #[arg(long)]
        json: bool,
    },
    /// Add an OpenAI-compatible or Anthropic-compatible provider
    Add {
        id: String,
        #[arg(long)]
        base_url: Option<String>,
        /// openai-chat (default), openai-responses or anthropic-messages
        #[arg(long)]
        dialect: Option<String>,
        /// Read the API key from the first line of stdin
        #[arg(long)]
        key_stdin: bool,
    },
    Remove {
        id: String,
    },
    /// Show or set how many requests may run against a provider at once
    /// (parallel agents beyond the limit wait in line instead of hitting 429s)
    Limit {
        id: String,
        /// Most requests in flight at once
        #[arg(long, conflicts_with = "clear")]
        max_concurrent: Option<u32>,
        /// Remove the limit
        #[arg(long)]
        clear: bool,
    },
    /// Ask the provider how many credits and tokens are left (EVREN's `/quota`)
    Quota {
        id: String,
        /// Print the provider's raw answer as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum AgentsCmd {
    /// List worker agents and the model that manages them
    List {
        /// claude-code (default) or opencode
        #[arg(long, value_enum, default_value_t = agents::Harness::ClaudeCode)]
        harness: agents::Harness,
        /// Project whose `.claude/agents` to read (default: current directory)
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Preview adding, changing or removing a worker (writes nothing)
    Plan {
        /// claude-code (default) or opencode
        #[arg(long, value_enum, default_value_t = agents::Harness::ClaudeCode)]
        harness: agents::Harness,
        /// Agent name the manager calls it by (lowercase, digits, dashes)
        name: String,
        /// Worker model: provider::model from the claude-code route
        /// (e.g. evren::deepseek-v4.1-flash) or a Claude model (opus, claude-…)
        #[arg(long, required_unless_present = "remove")]
        model: Option<String>,
        #[arg(long, value_enum, default_value_t = agents::Scope::Project)]
        scope: agents::Scope,
        /// Project for --scope project (default: current directory)
        #[arg(long)]
        project: Option<PathBuf>,
        /// When the manager should use this worker
        #[arg(long)]
        description: Option<String>,
        /// How many may run at once; written for the manager to follow. The
        /// hard cap is `turnout providers limit`.
        #[arg(long)]
        parallel: Option<u32>,
        #[arg(long, default_value_t = 40)]
        max_turns: u32,
        /// Work in the main checkout instead of a fresh git worktree
        #[arg(long)]
        no_worktree: bool,
        /// Comma-separated tool allowlist (default: the harness's full set)
        #[arg(long, value_delimiter = ',')]
        tools: Vec<String>,
        /// The worker's instructions, read from a file
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Plan removing this worker instead
        #[arg(long, conflicts_with = "model")]
        remove: bool,
    },
    /// Change which model manages the workers (the claude-code route's
    /// default). Shows the change and asks before writing anything.
    Manager {
        /// claude-code (default) or opencode
        #[arg(long, value_enum, default_value_t = agents::Harness::ClaudeCode)]
        harness: agents::Harness,
        /// provider::model, e.g. anthropic::claude-opus-5-5 or a target of
        /// the route such as openai::gpt-6.1-sol
        model: String,
        /// Apply without asking (for scripts and the desktop app, which ask
        /// in their own UI)
        #[arg(long)]
        yes: bool,
    },
    /// Apply the saved agent plan (with backup)
    Apply {
        /// claude-code (default) or opencode
        #[arg(long, value_enum, default_value_t = agents::Harness::ClaudeCode)]
        harness: agents::Harness,
    },
    /// Undo an agent change (latest when no id is given)
    Rollback { id: Option<String> },
}

#[derive(Subcommand)]
enum ModelsCmd {
    List { provider: String },
}

#[derive(Subcommand)]
enum HarnessCmd {
    Detect,
}

#[derive(Subcommand)]
enum GatewayCmd {
    /// Run gateways for saved editor routes until stopped
    Serve {
        /// Serve only one harness (codex, claude, opencode)
        #[arg(long)]
        harness: Option<String>,
    },
    /// Print the local gateway token for command-backed extension auth
    Token,
    /// Show configured routes and whether their loopback ports are listening
    Status,
    /// Install and start the per-user background agent
    Install,
    /// Stop and remove the per-user background agent
    Uninstall,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!(
            "error: {}",
            relay_core::redact_secrets(&format!("{error:#}"))
        );
        std::process::exit(1);
    }
}

/// Bare invocation: the full-screen interface on a terminal, plain help when
/// output is piped (scripts must never block on an interactive screen).
fn open_interface() -> Result<()> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        let exe = std::env::current_exe().context("cannot locate the Turnout executable")?;
        return relay_tui::run(exe).context("terminal interface failed");
    }
    use clap::CommandFactory;
    Cli::command().print_help()?;
    println!();
    Ok(())
}

async fn run(cli: Cli) -> Result<()> {
    update::remove_retired_executable();
    let home = Home::locate()?;
    let Some(command) = cli.command else {
        return open_interface();
    };
    match command {
        Cmd::Auth(cmd) => auth(&home, cmd).await,
        Cmd::Providers(ProvidersCmd::Quota { id, json }) => provider_quota(&home, &id, json).await,
        Cmd::Providers(cmd) => providers(&home, cmd),
        Cmd::Models(ModelsCmd::List { provider }) => {
            let adapter = provider_adapter(&home, &provider)?;
            for model in adapter.discover_models().await? {
                println!("{}", model.id);
            }
            Ok(())
        }
        Cmd::Test { provider, model } => {
            let adapter = provider_adapter(&home, &provider)?;
            let result = adapter.test_connection(&model).await?;
            let mark = if result.ok { "ok" } else { "FAILED" };
            println!(
                "{mark}  HTTP {}  {} ms  {}",
                result.status, result.latency_ms, result.detail
            );
            if result.ok {
                Ok(())
            } else {
                bail!("connection test failed")
            }
        }
        Cmd::Harness(HarnessCmd::Detect) => {
            for adapter in all_adapters(&home) {
                let found = adapter.detect_installations();
                if found.is_empty() {
                    println!("{:<12} not found", adapter.id());
                }
                for i in found {
                    let version = i
                        .version
                        .map(|v| v.raw)
                        .unwrap_or_else(|| "unknown version".into());
                    let binary = i
                        .binary
                        .map(|b| b.display().to_string())
                        .unwrap_or_else(|| "(config only)".into());
                    println!(
                        "{:<12} {version}  {binary}\n{:<12} config: {}",
                        adapter.id(),
                        "",
                        i.config_path.display()
                    );
                }
            }
            Ok(())
        }
        Cmd::Plan {
            harness,
            provider,
            model,
            default_model,
            config,
            gateway,
        } => plan(
            &home,
            &harness,
            &provider,
            &model,
            default_model.as_deref(),
            config,
            gateway,
        ),
        Cmd::PlanTargets {
            harness,
            target,
            default_target,
            config,
            profile,
            hybrid,
            fallback,
            fallback_chain,
            no_fallback: _,
        } => plan_targets(
            &home,
            &harness,
            &target,
            &default_target,
            config,
            profile.as_deref(),
            hybrid,
            fallback,
            &fallback_chain,
        ),
        Cmd::Apply { harness } => apply(&home, harness),
        Cmd::Rollback { id } => {
            let store = BackupStore::at(home.backups());
            let id = match id {
                Some(id) => BackupId(id),
                None => store.latest()?.ok_or_else(|| anyhow!("no backups yet"))?.id,
            };
            let restored = store.restore(&id)?;
            println!("restored {} file(s) from {id}", restored.len());
            for path in restored {
                println!("  {}", path.display());
            }
            // Keep the saved route in step with the restored files; otherwise the
            // UI would keep showing a route the config no longer contains.
            match home.route_before(&id.to_string()) {
                Some((harness, Some(route))) => {
                    let gateway = route.via_gateway;
                    home.save_route(&harness, route)?;
                    println!("saved route for {harness} restored too");
                    if gateway {
                        let _ = restart_agent();
                    }
                }
                Some((harness, None)) => {
                    home.remove_route(&harness)?;
                    println!("saved route for {harness} cleared (there was none before)");
                }
                None if agents::backup_harness(&home, &id).is_some() => {
                    let harness = agents::backup_harness(&home, &id).expect("checked above");
                    println!("agent files restored. {}", agents::reload_hint(harness))
                }
                None => println!(
                    "note: this backup predates route tracking; the saved route was left as is"
                ),
            }
            Ok(())
        }
        Cmd::Run {
            harness,
            profile,
            args,
        } => launch(&home, &harness, profile.as_deref(), args).await,
        Cmd::Gateway(cmd) => gateway_command(&home, cmd).await,
        Cmd::Credentials(cmd) => credentials(&home, cmd),
        Cmd::Profiles { json } => profiles(&home, json),
        Cmd::History { json, limit } => history(&home, json, limit),
        Cmd::Doctor { online } => doctor(&home, online).await,
        Cmd::Agents(cmd) => agents_command(&home, cmd),
        Cmd::Install { agent } => {
            let had_agent = agent_installed();
            let outcome = install::run()?;
            if agent || had_agent {
                // Register the Agent from the installed copy, not from wherever
                // this one was started (often Downloads), so deleting the
                // download later cannot stop it.
                let status = std::process::Command::new(&outcome.installed)
                    .args(["gateway", "install"])
                    .status()
                    .context("cannot run the installed Turnout")?;
                if !status.success() {
                    bail!("the Agent could not be installed from {}", outcome.installed.display());
                }
            } else {
                println!("Run `turnout gateway install` to start the background Agent.");
            }
            if outcome.copied {
                println!("You can delete {} now.", std::env::current_exe()?.display());
            }
            Ok(())
        }
        Cmd::Update { check } => {
            if update::run(check).await? {
                // The Agent still runs the old binary until it restarts.
                match restart_agent() {
                    Ok(true) => println!("Turnout Agent restarted on the new version"),
                    Ok(false) => {}
                    Err(error) => eprintln!("warning: could not restart the Turnout Agent: {error}"),
                }
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct OAuthConnection {
    id: &'static str,
    harness: &'static str,
    installed: bool,
    logged_in: bool,
    account: Option<String>,
    plan: Option<String>,
    detail: String,
    native_logged_in: bool,
    gateway_capable: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct OpenAiOAuthMeta {
    host_id: String,
    #[serde(default)]
    client_id: Option<String>,
    /// Product name used when this dynamic OAuth client was registered.
    /// Missing on pre-Turnout registrations, which OpenAI still labels Relay.
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    expires_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OpenAiOAuthTokens {
    client_id: String,
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_at: u64,
    scopes: Vec<String>,
}

// Open-source Sign in with ChatGPT dynamic clients use the accounts endpoint.
// This intentionally differs from Codex's own managed OAuth token endpoint.
const OPENAI_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";

fn openai_oauth_credential() -> CredentialRef {
    CredentialRef {
        service: "relay".to_string(),
        account: "oauth:openai".to_string(),
    }
}

fn openai_oauth_meta_path(home: &Home) -> PathBuf {
    home.root().join("openai-oauth.json")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_openai_oauth_meta(home: &Home) -> OpenAiOAuthMeta {
    std::fs::read_to_string(openai_oauth_meta_path(home))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn oauth_connections(home: &Home) -> Vec<OAuthConnection> {
    let codex = match harness_command("codex")
        .args(["login", "status"])
        .output()
    {
        Ok(output) => {
            let detail = String::from_utf8_lossy(if output.stdout.is_empty() {
                &output.stderr
            } else {
                &output.stdout
            })
            .trim()
            .to_string();
            (
                true,
                output.status.success() && detail.to_lowercase().contains("logged in"),
                detail,
            )
        }
        Err(error) => (false, false, error.to_string()),
    };
    let meta = read_openai_oauth_meta(home);
    let relay_oauth = meta.client_id.is_some()
        && credential_present(home, &openai_oauth_credential()) == Some(true);
    let openai = OAuthConnection {
        id: "openai",
        harness: "codex",
        installed: true,
        logged_in: codex.1 || relay_oauth,
        account: meta.email,
        plan: (codex.1 || relay_oauth).then(|| "ChatGPT".to_string()),
        detail: if relay_oauth {
            if meta.expires_at > unix_now() + 30 {
                "Turnout ChatGPT OAuth · Claude gateway ready".to_string()
            } else {
                "Turnout ChatGPT OAuth · access renews automatically".to_string()
            }
        } else {
            codex.2
        },
        native_logged_in: codex.1,
        gateway_capable: relay_oauth,
    };

    let anthropic = match harness_command("claude")
        .args(["auth", "status", "--json"])
        .output()
    {
        Ok(output) => {
            let parsed = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok();
            OAuthConnection {
                id: "anthropic",
                harness: "claude",
                installed: true,
                logged_in: parsed
                    .as_ref()
                    .and_then(|value| value.get("loggedIn"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                account: parsed
                    .as_ref()
                    .and_then(|value| value.get("email"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                plan: parsed
                    .as_ref()
                    .and_then(|value| value.get("subscriptionType"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                detail: parsed
                    .as_ref()
                    .and_then(|value| value.get("authMethod"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("not logged in")
                    .to_string(),
                native_logged_in: parsed
                    .as_ref()
                    .and_then(|value| value.get("loggedIn"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                gateway_capable: false,
            }
        }
        Err(error) => OAuthConnection {
            id: "anthropic",
            harness: "claude",
            installed: false,
            logged_in: false,
            account: None,
            plan: None,
            detail: error.to_string(),
            native_logged_in: false,
            gateway_capable: false,
        },
    };

    vec![openai, anthropic]
}

async fn auth(home: &Home, cmd: AuthCmd) -> Result<()> {
    match cmd {
        AuthCmd::Status { json } => {
            let connections = oauth_connections(home);
            if json {
                println!("{}", serde_json::to_string(&connections)?);
            } else {
                for connection in connections {
                    let status = if connection.logged_in {
                        "connected"
                    } else if connection.installed {
                        "signed out"
                    } else {
                        "not installed"
                    };
                    println!("{:<12} {:<12} {}", connection.id, status, connection.detail);
                }
            }
            Ok(())
        }
        AuthCmd::Login { provider } => {
            let provider = provider.trim().to_lowercase();
            let mut command = if matches!(provider.as_str(), "openai" | "codex") {
                let mut command = std::process::Command::new(
                    std::env::current_exe().context("cannot locate Turnout executable")?,
                );
                command.args(["auth", "complete-open-ai"]);
                command
            } else if matches!(provider.as_str(), "anthropic" | "claude") {
                let mut command = harness_command("claude");
                command.args(["auth", "login", "--claudeai"]);
                command
            } else {
                bail!("OAuth login is supported for `openai` and `anthropic`");
            };
            command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .with_context(|| format!("cannot start {provider} OAuth login"))?;
            println!("{provider} OAuth login started in the system browser");
            Ok(())
        }
        AuthCmd::Usage { provider, json } => usage::run(home, provider.as_deref(), json).await,
        AuthCmd::CompleteOpenAi => complete_openai_oauth(home).await,
    }
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

fn random_host_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "urn:uuid:{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes(bytes[0..4].try_into().unwrap()),
        u16::from_be_bytes(bytes[4..6].try_into().unwrap()),
        u16::from_be_bytes(bytes[6..8].try_into().unwrap()),
        u16::from_be_bytes(bytes[8..10].try_into().unwrap()),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ])
    )
}

fn open_system_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    };
    // Not `cmd /C start`: cmd splits the command line at `&`, so an OAuth URL
    // reached the browser cut after its first query parameter.
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32.exe");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command.status().context("cannot open the system browser")?;
    Ok(())
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_in: u64,
    scope: String,
}

#[derive(Deserialize)]
struct OAuthRefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
}

struct OpenAiRefreshLock(PathBuf);

impl Drop for OpenAiRefreshLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn openai_refresh_lock(home: &Home) -> Result<OpenAiRefreshLock> {
    std::fs::create_dir_all(home.root())?;
    let path = home.root().join("openai-oauth-refresh.lock");
    for _ in 0..100 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(OpenAiRefreshLock(path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(120));
                if stale {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    bail!("OpenAI OAuth refresh is busy; try again in a moment")
}

fn read_openai_oauth_tokens(home: &Home) -> Result<OpenAiOAuthTokens> {
    let value = credential_store(home)?
        .get(&openai_oauth_credential())?
        .ok_or_else(|| anyhow!("Turnout has no ChatGPT authorization; connect OpenAI first"))?;
    serde_json::from_str(&value).context("stored OpenAI OAuth session is invalid")
}

async fn current_openai_access_token(home: &Home) -> Result<String> {
    let tokens = read_openai_oauth_tokens(home)?;
    if tokens.expires_at > unix_now() + 300 {
        return Ok(tokens.access_token);
    }

    // The refresh token rotates. A small cross-process lock prevents the GUI,
    // background gateway and CLI from refreshing the same session concurrently.
    let _lock = openai_refresh_lock(home).await?;
    let mut tokens = read_openai_oauth_tokens(home)?;
    if tokens.expires_at > unix_now() + 300 {
        return Ok(tokens.access_token);
    }

    let response = reqwest::Client::new()
        .post(OPENAI_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", tokens.client_id.as_str()),
            ("refresh_token", tokens.refresh_token.as_str()),
            ("resource", "https://api.openai.com/v1"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<OAuthRefreshResponse>()
        .await?;

    tokens.access_token = response.access_token;
    if let Some(refresh_token) = response.refresh_token {
        tokens.refresh_token = refresh_token;
    }
    if let Some(id_token) = response.id_token {
        tokens.id_token = id_token;
    }
    if let Some(scope) = response.scope {
        tokens.scopes = scope.split_whitespace().map(str::to_string).collect();
    }
    if !tokens
        .scopes
        .iter()
        .any(|scope| scope == "chatgpt.tokens.use.direct")
    {
        bail!("renewed session does not grant ChatGPT plan usage")
    }
    tokens.expires_at = unix_now() + response.expires_in;
    credential_store(home)?.set(&openai_oauth_credential(), &serde_json::to_string(&tokens)?)?;
    let mut meta = read_openai_oauth_meta(home);
    meta.expires_at = tokens.expires_at;
    relay_config::write_file(
        &openai_oauth_meta_path(home),
        &serde_json::to_string_pretty(&meta)?,
    )?;
    Ok(tokens.access_token)
}

async fn validate_openai_id_token(
    token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<serde_json::Value> {
    let header = jsonwebtoken::decode_header(token).context("invalid OpenAI ID token header")?;
    if header.alg != jsonwebtoken::Algorithm::RS256 {
        bail!("OpenAI ID token uses an unexpected signing algorithm");
    }
    let kid = header
        .kid
        .as_deref()
        .ok_or_else(|| anyhow!("OpenAI ID token has no key id"))?;
    let jwks: jsonwebtoken::jwk::JwkSet =
        reqwest::get("https://auth.openai.com/.well-known/jwks.json")
            .await?
            .error_for_status()?
            .json()
            .await?;
    let jwk = jwks
        .find(kid)
        .ok_or_else(|| anyhow!("OpenAI signing key was not found"))?;
    let key = jsonwebtoken::DecodingKey::from_jwk(jwk)?;
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_audience(&[client_id]);
    validation.set_issuer(&["https://auth.openai.com"]);
    validation.leeway = 5;
    let claims = jsonwebtoken::decode::<serde_json::Value>(token, &key, &validation)?.claims;
    if claims["nonce"].as_str() != Some(nonce) {
        bail!("OpenAI ID token nonce did not match");
    }
    if claims["sub"].as_str().unwrap_or_default().is_empty() {
        bail!("OpenAI ID token has no subject");
    }
    Ok(claims)
}

async fn complete_openai_oauth(home: &Home) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
    let state = random_urlsafe(32);
    let nonce = random_urlsafe(32);
    let verifier = random_urlsafe(64);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut meta = read_openai_oauth_meta(home);
    if meta.host_id.is_empty() {
        meta.host_id = random_host_id();
    }
    // Dynamic OAuth client registrations retain their original display name.
    // Re-register once after the product rename so OpenAI's consent screen says
    // Turnout instead of Relay. Keep the old tokens untouched until the new
    // authorization succeeds, so cancelling this flow remains non-destructive.
    if meta.agent_name.as_deref() != Some(relay_core::APP_NAME) {
        meta.client_id = None;
    }
    let initial_registration = meta.client_id.is_none();
    let client_id = meta.client_id.as_deref().unwrap_or("dynamic_agent_client");
    let mut authorize = url::Url::parse("https://auth.openai.com/api/accounts/authorize")?;
    {
        let mut query = authorize.query_pairs_mut();
        query
            .append_pair("client_id", client_id)
            .append_pair("ext_agent_host_id", &meta.host_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair(
                "scope",
                "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
            )
            .append_pair("resource", "https://api.openai.com/v1")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge);
        if initial_registration {
            query.append_pair("agent_name_hint", relay_core::APP_NAME);
        } else {
            if let Ok(tokens) = read_openai_oauth_tokens(home) {
                query.append_pair("id_token_hint", &tokens.id_token);
            }
            if let Some(email) = &meta.email {
                query.append_pair("login_hint", email);
            }
        }
    }
    eprintln!("Sign in with ChatGPT in your browser. If it did not open, visit:\n  {authorize}");
    if let Err(error) = open_system_browser(authorize.as_str()) {
        eprintln!("{error:#}");
    }

    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(600), listener.accept())
        .await
        .context("OpenAI OAuth timed out")??;
    let mut request = vec![0u8; 16 * 1024];
    let read = socket.read(&mut request).await?;
    let request = String::from_utf8_lossy(&request[..read]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| anyhow!("invalid OAuth callback"))?;
    let callback = url::Url::parse(&format!("http://127.0.0.1{target}"))?;
    let params = callback
        .query_pairs()
        .into_owned()
        .collect::<std::collections::BTreeMap<_, _>>();
    let result: Result<()> = async {
        if params.get("state") != Some(&state) {
            bail!("OpenAI OAuth state did not match");
        }
        if let Some(error) = params.get("error") {
            bail!("OpenAI authorization failed: {error}");
        }
        let code = params
            .get("code")
            .ok_or_else(|| anyhow!("OAuth callback has no code"))?;
        let issued_client_id = params
            .get("client_id")
            .cloned()
            .or(meta.client_id.clone())
            .ok_or_else(|| anyhow!("OpenAI did not issue a client id"))?;
        if let Some(existing) = &meta.client_id {
            if existing != &issued_client_id {
                bail!("OpenAI returned a different client id");
            }
        }
        let response = reqwest::Client::new()
            .post(OPENAI_OAUTH_TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", issued_client_id.as_str()),
                ("code", code.as_str()),
                ("code_verifier", verifier.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("resource", "https://api.openai.com/v1"),
            ])
            .send()
            .await?
            .error_for_status()?;
        let response: OAuthTokenResponse = response.json().await?;
        let scopes = response
            .scope
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !scopes
            .iter()
            .any(|scope| scope == "chatgpt.tokens.use.direct")
        {
            bail!("ChatGPT plan usage permission was not granted");
        }
        let claims =
            validate_openai_id_token(&response.id_token, &issued_client_id, &nonce).await?;
        let subject = claims["sub"]
            .as_str()
            .ok_or_else(|| anyhow!("OpenAI ID token has no subject"))?;
        // OIDC subjects may be pairwise and therefore change when OpenAI issues
        // a new dynamic client ID. Enforce subject continuity only when
        // reauthorizing the same issued client; a fresh registration establishes
        // a new client/account binding after its token is fully validated.
        if !initial_registration {
            if let Some(existing) = &meta.subject {
                if existing != subject {
                    bail!("OpenAI returned a different ChatGPT account for this registration");
                }
            }
        }
        let expires_at = unix_now() + response.expires_in;
        let tokens = OpenAiOAuthTokens {
            client_id: issued_client_id.clone(),
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            id_token: response.id_token,
            expires_at,
            scopes,
        };
        credential_store(home)?
            .set(&openai_oauth_credential(), &serde_json::to_string(&tokens)?)?;
        meta.client_id = Some(issued_client_id);
        meta.agent_name = Some(relay_core::APP_NAME.to_string());
        meta.email = claims["email"].as_str().map(str::to_string);
        meta.subject = Some(subject.to_string());
        meta.expires_at = expires_at;
        relay_config::write_file(
            &openai_oauth_meta_path(home),
            &serde_json::to_string_pretty(&meta)?,
        )?;

        let mut provider = ProviderConfig::openai_compatible("openai", "https://api.openai.com/v1");
        provider.label = "OpenAI (ChatGPT OAuth)".to_string();
        provider.dialect = ProtocolDialect::OpenAiResponses;
        provider.credential = openai_oauth_credential();
        provider.api_key_env = "TURNOUT_OPENAI_OAUTH_TOKEN".to_string();
        provider
            .extra
            .insert("auth_mode".to_string(), "chatgpt-oauth".to_string());
        home.save_provider(provider)?;
        Ok(())
    }
    .await;
    let (status, message) = if result.is_ok() {
        (
            "200 OK",
            "Turnout bağlantısı hazır. Bu pencereyi kapatabilirsiniz.",
        )
    } else {
        (
            "400 Bad Request",
            "Turnout bağlantısı kurulamadı. Uygulamaya dönüp tekrar deneyin.",
        )
    };
    let body = format!("<html><body style=\"font-family:system-ui;padding:48px;background:#101820;color:#eef\"><h2>{message}</h2></body></html>");
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = socket.write_all(response.as_bytes()).await;
    result
}

// ---------------------------------------------------------------------------

fn credentials(home: &Home, cmd: CredentialsCmd) -> Result<()> {
    match cmd {
        CredentialsCmd::Status => {
            let store = credential_store(home)?;
            println!("store: {}", store.name());
            match backend_choice() {
                BackendChoice::File => {
                    println!("file:  {}", home.credentials_file().display())
                }
                BackendChoice::Encrypted | BackendChoice::Vault => {
                    println!("vault: {}", home.vault().display())
                }
                BackendChoice::Keyring => {}
            }
            let mut stored = store.list()?;
            stored.sort_by(|a, b| a.account.cmp(&b.account));
            if stored.is_empty() {
                println!("no credentials stored");
            }
            for credential in stored {
                println!("  {}/{}", credential.service, credential.account);
            }
            Ok(())
        }
        CredentialsCmd::Migrate { delete_old } => {
            // Move secrets into whatever store this build uses by default, so
            // the command keeps working after the default became encrypted.
            let target = credential_store(home)?;
            let keyring = relay_credentials::OsKeyringStore::new();
            let mut refs = home
                .providers()?
                .into_iter()
                .map(|provider| provider.credential)
                .collect::<Vec<_>>();
            refs.push(openai_oauth_credential());
            refs.push(gateway_credential());
            refs.dedup();
            println!(
                "Copying keys from the OS keyring to the {} store\n(the OS may ask for permission once per item; no secret is printed)\n",
                target.name()
            );
            let mut copied = 0;
            for credential in refs {
                let label = format!("{}/{}", credential.service, credential.account);
                if target.has(&credential)? {
                    println!("  = {label}: already in the target store");
                    // The copy exists, so the old item can go without being read
                    // (reading it is what triggers the OS permission prompt).
                    if delete_old {
                        match keyring.delete(&credential) {
                            Ok(true) => println!("      removed from the keyring"),
                            Ok(false) => println!("      (nothing left in the keyring)"),
                            Err(error) => {
                                println!("      could not remove it from the keyring: {error}")
                            }
                        }
                    }
                    continue;
                }
                match keyring.get(&credential) {
                    Ok(Some(secret)) => {
                        target.set(&credential, &secret)?;
                        copied += 1;
                        println!("  + {label}: copied");
                        if delete_old {
                            match keyring.delete(&credential) {
                                Ok(_) => println!("      removed from the keyring"),
                                Err(error) => {
                                    println!("      could not remove it from the keyring: {error}")
                                }
                            }
                        }
                    }
                    Ok(None) => println!("  - {label}: not in the keyring"),
                    Err(error) => println!("  ! {label}: {error}"),
                }
            }
            println!("\n{copied} credential(s) copied.");
            if copied > 0 && !delete_old {
                println!(
                    "The keyring items were kept; run again with --delete-old to remove them."
                );
            }
            Ok(())
        }
    }
}

fn providers(home: &Home, cmd: ProvidersCmd) -> Result<()> {
    match cmd {
        ProvidersCmd::List { json } => {
            let list = home.providers()?;
            if json {
                #[derive(Serialize)]
                struct ProviderSummary<'a> {
                    id: &'a str,
                    label: &'a str,
                    base_url: &'a str,
                    dialect: &'a str,
                    has_key: bool,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    max_concurrent: Option<u32>,
                }

                let summaries = list
                    .iter()
                    .map(|provider| ProviderSummary {
                        id: provider.id.as_str(),
                        label: &provider.label,
                        base_url: &provider.base_url,
                        dialect: provider.dialect.as_str(),
                        has_key: credential_present(home, &provider.credential) == Some(true),
                        max_concurrent: provider.max_concurrent,
                    })
                    .collect::<Vec<_>>();
                println!("{}", serde_json::to_string(&summaries)?);
                return Ok(());
            }
            if list.is_empty() {
                println!("no providers yet — try: turnout providers add openai --key-stdin");
            }
            for p in list {
                let key = match credential_present(home, &p.credential) {
                    Some(true) => "key stored",
                    Some(false) => "no key",
                    None => "key store unavailable",
                };
                let limit = p
                    .max_concurrent
                    .map(|limit| format!("  max {limit} at once"))
                    .unwrap_or_default();
                println!(
                    "{:<16} {:<18} {}  [{key}]{limit}",
                    p.id,
                    p.dialect.as_str(),
                    p.base_url
                );
            }
            Ok(())
        }
        ProvidersCmd::Add {
            id,
            base_url,
            dialect,
            key_stdin,
        } => {
            let id = ProviderId::new(id);
            let dialect = match dialect.as_deref() {
                Some(d) => ProtocolDialect::from_str_value(d).ok_or_else(|| {
                    anyhow!(
                        "unknown dialect `{d}` (openai-chat, openai-responses, anthropic-messages)"
                    )
                })?,
                None if id.as_str() == "anthropic" => ProtocolDialect::AnthropicMessages,
                None => ProtocolDialect::OpenAiChat,
            };
            let base_url = match (base_url, id.as_str()) {
                (Some(url), _) => url,
                (None, "openai") => "https://api.openai.com/v1".to_string(),
                (None, "anthropic") => "https://api.anthropic.com/v1".to_string(),
                (None, "evren") => provider_evren::DEFAULT_BASE_URL.to_string(),
                (None, other) => {
                    bail!("`{other}` has no built-in URL; pass --base-url (include the /v1 prefix)")
                }
            };
            let parsed = url::Url::parse(&base_url)
                .with_context(|| format!("invalid --base-url `{base_url}`"))?;
            // `file:`, `data:` and friends parse fine but the transport only
            // speaks HTTP; accepting them here would store a provider that can
            // never work and hides a typo behind a late runtime error.
            if !matches!(parsed.scheme(), "http" | "https") {
                bail!(
                    "--base-url must start with http:// or https://, got `{}`",
                    parsed.scheme()
                );
            }
            if parsed.host_str().is_none() {
                bail!("--base-url has no host: `{base_url}`");
            }
            let mut config = match dialect {
                ProtocolDialect::AnthropicMessages => {
                    ProviderConfig::anthropic(id.clone(), base_url)
                }
                _ => ProviderConfig::openai_compatible(id.clone(), base_url),
            };
            config.dialect = dialect;
            if id.as_str() == "anthropic" {
                config.api_key_env = "ANTHROPIC_API_KEY".to_string();
            }
            if key_stdin {
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                let key = line.trim();
                if key.is_empty() {
                    bail!("--key-stdin given but stdin was empty");
                }
                credential_store(home)?.set(&config.credential, key)?;
                println!("key stored in {}", credential_store(home)?.name());
            } else {
                println!("no key stored; re-run with --key-stdin (e.g. `pbpaste | turnout providers add {id} --key-stdin`)");
            }
            // Re-adding (e.g. to replace the key) keeps the concurrency limit.
            config.max_concurrent = home
                .providers()?
                .into_iter()
                .find(|existing| existing.id == id)
                .and_then(|existing| existing.max_concurrent);
            home.save_provider(config)?;
            println!("provider `{id}` saved");
            Ok(())
        }
        ProvidersCmd::Remove { id } => {
            let id = ProviderId::new(id);
            let Some(config) = home.providers()?.into_iter().find(|p| p.id == id) else {
                bail!("no provider named `{id}`");
            };
            if let Ok(store) = credential_store(home) {
                let _ = store.delete(&config.credential);
            }
            home.remove_provider(&id)?;
            println!("removed `{id}` and its stored key");
            Ok(())
        }
        ProvidersCmd::Limit {
            id,
            max_concurrent,
            clear,
        } => {
            let id = ProviderId::new(id);
            let Some(mut config) = home.providers()?.into_iter().find(|p| p.id == id) else {
                bail!("no provider named `{id}`");
            };
            if max_concurrent.is_none() && !clear {
                match config.max_concurrent {
                    Some(limit) => println!("`{id}`: at most {limit} request(s) at once"),
                    None => println!("`{id}`: no concurrency limit"),
                }
                return Ok(());
            }
            if max_concurrent == Some(0) {
                bail!("--max-concurrent must be at least 1 (use --clear for no limit)");
            }
            config.max_concurrent = max_concurrent;
            home.save_provider(config)?;
            match max_concurrent {
                Some(limit) => println!("`{id}`: at most {limit} request(s) at once"),
                None => println!("`{id}`: no concurrency limit"),
            }
            // The Agent reads provider settings once at startup.
            match restart_agent() {
                Ok(true) => println!("Turnout Agent restarted with the new limit"),
                Ok(false) => {}
                Err(error) => eprintln!("warning: could not restart the Turnout Agent: {error}"),
            }
            Ok(())
        }
        ProvidersCmd::Quota { .. } => unreachable!("dispatched in run()"),
    }
}

async fn provider_quota(home: &Home, id: &str, json: bool) -> Result<()> {
    let id = ProviderId::new(id);
    let Some(config) = home.providers()?.into_iter().find(|p| p.id == id) else {
        bail!("no provider named `{id}`");
    };
    let key = credential_store(home)
        .ok()
        .and_then(|store| store.get(&config.credential).ok().flatten());
    // Only EVREN serves `/quota` today; any provider at a compatible URL works.
    let live = provider_evren::EvrenProvider::new(config.base_url.clone(), key)?
        .quota_summary()
        .await;
    let quota = match live {
        Ok(quota) => quota,
        // Most providers never answer `/quota`, but they do send their limits
        // as headers on ordinary responses — use the last one we saw.
        Err(error) => match captured_quota(home, id.as_str()) {
            Some(quota) => quota,
            None => match error {
                // ChatGPT sign-in answers without rate-limit headers; its limits
                // are the subscription windows `auth usage` reads.
                relay_core::RelayError::ProviderStatus { status: 404, .. }
                    if config.credential == openai_oauth_credential() =>
                {
                    bail!(
                        "`{id}` uses ChatGPT sign-in, which does not report API rate limits \
                         — see the 5-hour and weekly windows with `turnout auth usage openai`"
                    )
                }
                relay_core::RelayError::ProviderStatus { status: 404, .. } => bail!(
                    "`{id}` does not report its quota (no /quota endpoint at {}) \
                     — send one request through Turnout first so its rate-limit \
                     headers are recorded",
                    config.base_url
                ),
                error => return Err(error.into()),
            },
        },
    };
    if json {
        let mut value = serde_json::to_value(&quota)?;
        value["provider"] = serde_json::Value::String(id.to_string());
        println!("{}", serde_json::to_string(&value)?);
        return Ok(());
    }
    println!("{id}  {}", quota.level.as_deref().unwrap_or("-"));
    if let Some(credits) = quota.credits_remaining {
        let held = quota
            .credits_held
            .filter(|held| *held > 0.0)
            .map(|held| format!("  ({} held)", group_digits(held)))
            .unwrap_or_default();
        println!("  {:<14} {} CR{held}", "credits", group_digits(credits));
    }
    if let Some(left) = quota.daily_remaining_tokens {
        println!(
            "  {:<14} {} / {} left{}",
            "daily tokens",
            group_digits(left as f64),
            quota.daily_limit_tokens.map_or("?".into(), |l| group_digits(l as f64)),
            reset_suffix(quota.daily_reset_at.as_deref()),
        );
    }
    if let Some(used) = quota.window_used_tokens {
        let label = quota
            .window_minutes
            .map_or("window".to_string(), |m| format!("{m}-min window"));
        println!(
            "  {label:<14} {} / {} used{}",
            group_digits(used as f64),
            quota.window_limit_tokens.map_or("?".into(), |l| group_digits(l as f64)),
            reset_suffix(quota.window_reset_at.as_deref()),
        );
    }
    if let Some(limit) = quota.concurrency_limit {
        println!(
            "  {:<14} {} / {limit} in flight",
            "concurrency",
            quota.concurrent_requests.unwrap_or(0)
        );
    }
    if let Some(left) = quota.minute_remaining_tokens {
        println!(
            "  {:<14} {} / {} left{}",
            "tokens/min",
            group_digits(left as f64),
            quota.minute_limit_tokens.map_or("?".into(), |l| group_digits(l as f64)),
            reset_suffix(quota.minute_reset.as_deref()),
        );
    }
    if let Some(left) = quota.minute_remaining_requests {
        println!(
            "  {:<14} {} / {} left",
            "requests/min",
            group_digits(left as f64),
            quota.minute_limit_requests.map_or("?".into(), |l| group_digits(l as f64)),
        );
    }
    if quota.source.as_deref() == Some("headers") {
        let ago = quota
            .observed_at
            .map(|at| {
                let age = relay_core::quota::unix_time().saturating_sub(at) / 60;
                format!("as of {age} min ago (last response)")
            })
            .unwrap_or_else(|| "from the last response".to_string());
        println!("  {ago}");
    }
    Ok(())
}

/// The last rate-limit headers Turnout saw for `id`, if any.
///
/// A capture that cannot be read is treated as absent: the live answer and
/// the error path are both more useful than a parse failure here.
fn captured_quota(home: &Home, id: &str) -> Option<provider_evren::Quota> {
    let path = relay_core::QuotaSink::new(home.quota_dir(), id).path();
    let text = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let at = value["at"].as_u64()?;
    let headers = value["headers"]
        .as_object()?
        .iter()
        .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_string())))
        .collect();
    Some(provider_evren::Quota::from_headers(&headers, at))
}

/// `1234567.5` → `1,234,567.5`; whole numbers lose the decimals.
fn group_digits(value: f64) -> String {
    let whole = value.trunc() as u64;
    let digits = whole.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    let fraction = value - value.trunc();
    if fraction > 0.005 {
        out.push_str(&format!("{fraction:.2}")[1..]);
    }
    out
}

/// `2026-10-28T00:00:00Z` → `  (resets 2026-10-28 00:00 UTC)`.
fn reset_suffix(at: Option<&str>) -> String {
    let Some(at) = at else { return String::new() };
    let utc = at.ends_with('Z') || at.ends_with("+00:00");
    let short: String = at.replacen('T', " ", 1).chars().take(16).collect();
    format!("  (resets {short}{})", if utc { " UTC" } else { "" })
}

/// Credential services may display an OS approval prompt. Listing connections must never
/// leave the CLI (and therefore the desktop UI) waiting forever for that prompt.
fn credential_present(home: &Home, credential: &CredentialRef) -> Option<bool> {
    if backend_choice() == BackendChoice::File {
        return credential_store(home)
            .ok()
            .and_then(|store| store.has(credential).ok());
    }
    if backend_choice() != BackendChoice::Keyring {
        // The encrypted vault needs its data key from the OS credential
        // service, and that read can wait on an access prompt. Bound it like
        // the keyring path below so a listing never hangs the desktop UI.
        let home = home.clone();
        let credential = credential.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let present = credential_store(&home)
                .ok()
                .and_then(|store| store.has(&credential).ok());
            let _ = sender.send(present);
        });
        return receiver
            .recv_timeout(Duration::from_millis(1500))
            .ok()
            .flatten();
    }

    // `security find-generic-password` without `-w` reads metadata only. It does not
    // reveal the secret or trigger the access prompt caused by reading the password.
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                &credential.service,
                "-a",
                &credential.account,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()
            .map(|status| status.success());
    }

    #[cfg(not(target_os = "macos"))]
    {
        let credential = credential.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let store = relay_credentials::OsKeyringStore::new();
            let _ = sender.send(store.has(&credential).ok());
        });
        receiver
            .recv_timeout(Duration::from_millis(1500))
            .ok()
            .flatten()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BackendChoice {
    File,
    Keyring,
    Vault,
    /// Default: `vault.json` encrypted with the OS-held data key.
    Encrypted,
}

/// Where API keys live.
///
/// Default: `vault.json`, encrypted with a random data key held in the OS
/// credential service. The key itself is one item, so the per-item prompt that
/// once made the keyring unusable does not come back, and a copied data
/// directory yields no usable secrets. `file` (plain `credentials.json`),
/// `keyring` (a separate OS item per secret) and `vault` (passphrase) remain
/// explicit opt-outs.
fn backend_choice() -> BackendChoice {
    if relay_core::brand::env_flag("FORCE_VAULT") {
        return BackendChoice::Vault;
    }
    match relay_core::brand::env_var("CREDENTIALS").ok().as_deref() {
        Some("keyring") | Some("keychain") => BackendChoice::Keyring,
        Some("vault") => BackendChoice::Vault,
        Some("file") | Some("plain") => BackendChoice::File,
        _ => BackendChoice::Encrypted,
    }
}

/// One-time import of the pre-encryption `credentials.json`.
///
/// Builds before encryption-by-default stored every key in clear text there.
/// On the first encrypted run the entries are copied into the vault and the
/// old file is renamed rather than deleted, so an upgrade never loses a key
/// and never leaves a plaintext secret behind. Failures are non-fatal: the
/// store is usable either way, and `doctor` reports what is left.
fn import_plaintext_credentials(home: &Home, target: &dyn CredentialStore) {
    let source = home.credentials_file();
    if !source.exists() {
        return;
    }
    let plain = relay_credentials::FileStore::open(&source);
    let Ok(entries) = plain.list() else {
        return;
    };
    let mut imported = 0;
    let mut complete = true;
    for credential in &entries {
        if target.has(credential).unwrap_or(false) {
            continue;
        }
        match plain.get(credential) {
            Ok(Some(secret)) if target.set(credential, &secret).is_ok() => imported += 1,
            // Leave the file in place: a key that could not be copied must not
            // be lost just because the upgrade ran.
            _ => complete = false,
        }
    }
    if imported > 0 {
        eprintln!(
            "turnout: encrypted {imported} existing key(s) into {}",
            home.vault().display()
        );
    }
    if complete {
        // `credentials.json` -> `credentials.json.imported`, keeping the
        // original readable in case a key needs to be recovered by hand.
        let archived = PathBuf::from(format!("{}.imported", source.display()));
        eprintln!(
            "turnout: the plaintext file was renamed to {}; delete it once you have checked the keys",
            archived.display()
        );
        let _ = std::fs::rename(&source, archived);
    } else {
        eprintln!(
            "turnout: some keys could not be encrypted; {} was left untouched",
            source.display()
        );
    }
}

fn credential_store(home: &Home) -> Result<Box<dyn CredentialStore>> {
    match backend_choice() {
        BackendChoice::File => {
            return Ok(Box::new(relay_credentials::FileStore::open(
                home.credentials_file(),
            )))
        }
        // Opt-in: the data key lives in the OS credential service, so at most
        // one prompt per build is possible instead of one per secret.
        BackendChoice::Keyring => {
            let store = relay_credentials::open_encrypted(home.vault(), home.vault_key(), true)
                .map_err(|error| {
                    anyhow!("{error}; the OS credential service could not hold the vault key")
                })?;
            import_plaintext_credentials(home, store.as_ref());
            return Ok(store);
        }
        BackendChoice::Encrypted => {
            let store = relay_credentials::open_encrypted(
                home.vault(),
                home.vault_key(),
                false,
            )
            .map_err(|error| {
                anyhow!(
                    "{error}; set TURNOUT_CREDENTIALS=file to store keys unencrypted, \
                     or TURNOUT_VAULT_PASSWORD to use a passphrase vault"
                )
            })?;
            import_plaintext_credentials(home, store.as_ref());
            return Ok(store);
        }
        BackendChoice::Vault => {}
    }
    let password = relay_core::brand::env_var("VAULT_PASSWORD").map_err(|_| {
        anyhow!("the passphrase vault needs TURNOUT_VAULT_PASSWORD to be set")
    })?;
    let mut vault = relay_credentials::Vault::open(home.vault());
    if relay_credentials::Vault::exists(vault.path()) {
        vault.unlock(&password)?;
    } else {
        vault.create(&password)?;
    }
    Ok(Box::new(vault))
}

struct OpenAiOAuthProvider {
    home: Home,
    config: ProviderConfig,
}

impl OpenAiOAuthProvider {
    async fn adapter(&self) -> Result<provider_openai::OpenAiCompatible> {
        let adapter = provider_openai::OpenAiCompatible::new(
            self.config.clone(),
            Some(current_openai_access_token(&self.home).await?),
        )?;
        Ok(adapter.with_header_sink(quota_sink(&self.home, self.config.id.as_str())))
    }
}

impl ProviderAdapter for OpenAiOAuthProvider {
    fn id(&self) -> &ProviderId {
        &self.config.id
    }

    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn discover_models(&self) -> BoxFuture<'_, relay_core::Result<Vec<Model>>> {
        Box::pin(async move {
            self.adapter()
                .await
                .map_err(|error| relay_core::RelayError::other(error.to_string()))?
                .discover_models()
                .await
        })
    }

    fn test_connection(&self, model: &str) -> BoxFuture<'_, relay_core::Result<TestResult>> {
        let model = model.to_string();
        Box::pin(async move {
            self.adapter()
                .await
                .map_err(|error| relay_core::RelayError::other(error.to_string()))?
                .test_connection(&model)
                .await
        })
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        provider_openai::wire::infer_capabilities(model)
    }

    fn stream(&self, request: ChatRequest) -> BoxFuture<'_, relay_core::Result<EventStream>> {
        Box::pin(async move {
            self.adapter()
                .await
                .map_err(|error| relay_core::RelayError::other(error.to_string()))?
                .stream(request)
                .await
        })
    }

    fn chat(&self, request: ChatRequest) -> BoxFuture<'_, relay_core::Result<ChatResponse>> {
        Box::pin(async move {
            self.adapter()
                .await
                .map_err(|error| relay_core::RelayError::other(error.to_string()))?
                .chat(request)
                .await
        })
    }
}

fn provider_adapter(home: &Home, id: &str) -> Result<Box<dyn ProviderAdapter>> {
    let id = ProviderId::new(id);
    let config = home
        .providers()?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| anyhow!("unknown provider `{id}` — see `turnout providers list`"))?;
    if config.credential == openai_oauth_credential() {
        return Ok(Box::new(OpenAiOAuthProvider {
            home: home.clone(),
            config,
        }));
    }
    let key = credential_store(home)
        .ok()
        .and_then(|store| store.get(&config.credential).ok().flatten());
    let sink = quota_sink(home, config.id.as_str());
    Ok(match config.dialect {
        ProtocolDialect::AnthropicMessages => Box::new(
            provider_anthropic::AnthropicProvider::new(config, key)?.with_header_sink(sink),
        ),
        _ => Box::new(
            provider_openai::OpenAiCompatible::new(config, key)?.with_header_sink(sink),
        ),
    })
}

/// Every real chat request records the provider's rate-limit headers, so
/// `providers quota` has something to show when there is no `/quota` endpoint.
fn quota_sink(home: &Home, provider: &str) -> Option<std::sync::Arc<dyn relay_core::HeaderSink>> {
    Some(std::sync::Arc::new(relay_core::QuotaSink::new(
        home.quota_dir(),
        provider,
    )))
}

// ---------------------------------------------------------------------------

fn canonical_harness(name: &str) -> Result<&'static str> {
    match name.to_lowercase().as_str() {
        "codex" => Ok(HarnessId::CODEX),
        "claude" | "claude-code" | "claudecode" => Ok(HarnessId::CLAUDE_CODE),
        "opencode" => Ok(HarnessId::OPENCODE),
        other => bail!("unknown harness `{other}` (codex, claude, opencode)"),
    }
}

fn adapter_for(
    home: &Home,
    name: &str,
    config: Option<PathBuf>,
) -> Result<Box<dyn HarnessAdapter>> {
    let backups = BackupStore::at(home.backups());
    Ok(match (canonical_harness(name)?, config) {
        (HarnessId::CODEX, None) => Box::new(harness_codex::CodexAdapter::new(backups)),
        (HarnessId::CODEX, Some(c)) => {
            Box::new(harness_codex::CodexAdapter::with_config(c, backups))
        }
        (HarnessId::CLAUDE_CODE, None) => Box::new(harness_claude::ClaudeCodeAdapter::new(backups)),
        (HarnessId::CLAUDE_CODE, Some(c)) => {
            Box::new(harness_claude::ClaudeCodeAdapter::with_config(c, backups))
        }
        (_, None) => Box::new(harness_opencode::OpenCodeAdapter::new(backups)),
        (_, Some(c)) => Box::new(harness_opencode::OpenCodeAdapter::with_config(c, backups)),
    })
}

/// A command for a harness executable found the same way adapters find it.
///
/// `Command::new("claude")` only consults `PATH`. That misses the extra
/// install directories adapter discovery scans, and on Windows it resolves
/// `claude` to nothing at all because the npm shim is `claude.cmd`.
fn harness_command(name: &str) -> std::process::Command {
    match relay_config::kit::find_binary(&[name]) {
        Some(binary) => relay_config::kit::process(&binary),
        None => std::process::Command::new(name),
    }
}

fn all_adapters(home: &Home) -> Vec<Box<dyn HarnessAdapter>> {
    ["codex", "claude", "opencode"]
        .iter()
        .filter_map(|n| adapter_for(home, n, None).ok())
        .collect()
}

/// Does the harness speak the provider's protocol natively?
fn needs_gateway(harness: &str, dialect: ProtocolDialect) -> bool {
    match harness {
        HarnessId::CLAUDE_CODE => dialect != ProtocolDialect::AnthropicMessages,
        // Codex only speaks the Responses API.
        HarnessId::CODEX => dialect != ProtocolDialect::OpenAiResponses,
        _ => false, // OpenCode speaks all three
    }
}

fn gateway_port(base: u16, harness: &str) -> u16 {
    base + match harness {
        HarnessId::CODEX => 1,
        HarnessId::OPENCODE => 2,
        _ => 0,
    }
}

fn plan(
    home: &Home,
    harness: &str,
    provider: &str,
    models: &[String],
    default_model: Option<&str>,
    config: Option<PathBuf>,
    force_gateway: bool,
) -> Result<()> {
    let mut enabled_models = Vec::new();
    for model in models
        .iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
    {
        if !enabled_models.contains(&model) {
            enabled_models.push(model);
        }
    }
    let models = enabled_models;
    let model = default_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| models.first().map(String::as_str).unwrap_or(""));
    if model.is_empty() {
        bail!("at least one --model is required");
    }
    if !models.iter().any(|candidate| candidate == model) {
        bail!("--default-model `{model}` must also be included with --model");
    }
    let harness = canonical_harness(harness)?;
    let provider_id = ProviderId::new(provider);
    let provider = home
        .providers()?
        .into_iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| anyhow!("unknown provider `{provider_id}`"))?;
    let via_gateway = force_gateway || needs_gateway(harness, provider.dialect);
    let state = home.state()?;
    let surfaces = vec![Surface::Cli, Surface::VsCode];
    let port = gateway_port(state.gateway_port, harness);
    let ctx = RouteContext {
        provider: provider.clone(),
        model: model.to_string(),
        models: models.clone(),
        surfaces,
        via_gateway,
        gateway_url: via_gateway.then(|| format!("http://127.0.0.1:{port}")),
        gateway_token_command: via_gateway
            .then(std::env::current_exe)
            .transpose()
            .context("cannot locate the Turnout executable")?,
        gateway_token_file: via_gateway.then(|| home.gateway_token_file()),
    };
    let route = Route {
        harness: HarnessId::new(harness),
        surfaces: ctx.surfaces.clone(),
        provider: provider.id.clone(),
        model: model.to_string(),
        models: models.clone(),
        profile: None,
    };
    let adapter = adapter_for(home, harness, config)?;
    let plan = adapter.plan_changes(&route, &ctx)?;
    let validation = adapter.validate(&plan)?;

    let diff = plan.unified_diff();
    println!(
        "{}",
        if diff.is_empty() {
            "(no changes — already up to date)".to_string()
        } else {
            diff
        }
    );
    for change in &plan.changes {
        for note in &change.notes {
            println!("note: {note}");
        }
    }
    for note in &plan.notes {
        println!("note: {note}");
    }
    if via_gateway {
        println!("note: this route uses the background Turnout Agent on port {port}; install it once with `turnout gateway install`");
    }
    for message in &validation.messages {
        println!("{:?}: {}", message.level, message.text);
    }
    if !validation.ok {
        bail!("plan did not validate; nothing was saved");
    }
    home.save_plan(
        &plan,
        via_gateway,
        models
            .iter()
            .map(|enabled| SavedTarget {
                provider: provider.id.to_string(),
                model: enabled.clone(),
                public_model: enabled.clone(),
                fallbacks: Vec::new(),
            })
            .collect(),
        true,
    )?;
    println!("\nplan {} saved — run `turnout apply` to write it", plan.id);
    Ok(())
}

fn parse_target(value: &str) -> Result<(String, String)> {
    let (provider, model) = value
        .split_once("::")
        .ok_or_else(|| anyhow!("invalid target `{value}`; expected provider::model"))?;
    let provider = ProviderId::new(provider).to_string();
    let model = model.trim().to_string();
    if provider.is_empty() || model.is_empty() {
        bail!("invalid target `{value}`; provider and model are required");
    }
    Ok((provider, model))
}

/// `--fallback-chain FROM=TO[,TO...]`: FROM may only be rerouted to the listed
/// models of the same route, in that order. Each side uses `provider::model`.
fn apply_fallback_chains(
    mut targets: Vec<SavedTarget>,
    chains: &[String],
) -> Result<Vec<SavedTarget>> {
    let public = |value: &str, targets: &[SavedTarget]| -> Result<String> {
        let (provider, model) = parse_target(value)?;
        targets
            .iter()
            .find(|target| target.provider == provider && target.model == model)
            .map(|target| target.public_model.clone())
            .ok_or_else(|| anyhow!("`{value}` in --fallback-chain is not a --target of this route"))
    };
    for chain in chains {
        let (from, to) = chain.split_once('=').ok_or_else(|| {
            anyhow!("invalid --fallback-chain `{chain}`; expected provider::model=provider::model[,...]")
        })?;
        let from = public(from.trim(), &targets)?;
        let mut list = Vec::new();
        for item in to.split(',').map(str::trim).filter(|item| !item.is_empty()) {
            let candidate = public(item, &targets)?;
            if candidate == from {
                bail!("--fallback-chain: `{item}` cannot fall back to itself");
            }
            if !list.contains(&candidate) {
                list.push(candidate);
            }
        }
        if list.is_empty() {
            bail!("--fallback-chain `{chain}` names no model to fall back to");
        }
        if let Some(target) = targets.iter_mut().find(|target| target.public_model == from) {
            target.fallbacks = list;
        }
    }
    Ok(targets)
}

fn plan_targets(
    home: &Home,
    harness: &str,
    values: &[String],
    default_value: &str,
    config: Option<PathBuf>,
    profile: Option<&str>,
    hybrid: bool,
    fallback: bool,
    fallback_chains: &[String],
) -> Result<()> {
    let mut pairs = Vec::new();
    for value in values {
        let pair = parse_target(value)?;
        if !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    if pairs.is_empty() {
        bail!("at least one --target is required");
    }
    let default_pair = parse_target(default_value)?;
    if !pairs.contains(&default_pair) {
        bail!("--default-target must also be included with --target");
    }

    let harness = canonical_harness(harness)?;
    // Hybrid keeps the harness's own model list. Claude: Anthropic models are
    // not routed by Turnout at all (Claude's own login serves them). Codex:
    // its built-in models are routed to Turnout's OpenAI connection under
    // their original ids. Either way only the other providers are added.
    let official_provider = match harness {
        HarnessId::CLAUDE_CODE => "anthropic",
        HarnessId::CODEX => "openai",
        _ => "",
    };
    let hybrid_default = if hybrid {
        if official_provider.is_empty() {
            bail!("--hybrid is only available for claude-code and codex");
        }
        let custom = pairs
            .iter()
            .filter(|(provider, _)| provider != official_provider)
            .cloned()
            .collect::<Vec<_>>();
        if custom.is_empty() {
            bail!(
                "--hybrid needs at least one --target from a provider other than `{official_provider}` to add to the original list"
            );
        }
        let official = (default_pair.0 == official_provider).then(|| default_pair.1.clone());
        pairs = custom;
        Some(official)
    } else {
        None
    };
    let native_oauth_provider = if hybrid {
        None
    } else {
        match harness {
            HarnessId::CODEX if pairs.iter().all(|(provider, _)| provider == "openai") => {
                Some("openai")
            }
            HarnessId::CLAUDE_CODE if pairs.iter().all(|(provider, _)| provider == "anthropic") => {
                Some("anthropic")
            }
            _ => None,
        }
    };
    if let (Some(provider), Some(name)) = (native_oauth_provider, profile) {
        let targets = pairs
            .iter()
            .map(|(provider, model)| SavedTarget {
                provider: provider.clone(),
                model: model.clone(),
                public_model: model.clone(),
                fallbacks: Vec::new(),
            })
            .collect::<Vec<_>>();
        home.save_profile(
            harness,
            name,
            SavedRoute {
                provider: provider.to_string(),
                model: default_pair.1.clone(),
                models: targets
                    .iter()
                    .map(|target| target.public_model.clone())
                    .collect(),
                targets,
                via_gateway: false,
                hybrid: false,
                fallback: false,
            },
        )?;
        println!(
            "profile `{name}` saved — launch it with `turnout run {} --profile {name}`; official OAuth stays inside the harness",
            harness
        );
        return Ok(());
    }
    if native_oauth_provider.is_some() && (fallback || !fallback_chains.is_empty()) {
        eprintln!("note: fallback is ignored; an official sign-in route talks to its provider directly");
    }
    if let Some(provider) = native_oauth_provider {
        return plan_native_oauth(home, harness, provider, &pairs, &default_pair, config);
    }

    let configured = home.providers()?;
    for (provider, _) in &pairs {
        if !configured
            .iter()
            .any(|candidate| candidate.id.as_str() == provider)
        {
            bail!("unknown provider `{provider}` — add its connection before applying this route");
        }
    }

    // Codex hybrid: the built-in models keep their original ids and are served
    // by Turnout's OpenAI connection, so that connection must exist.
    let codex_hybrid = hybrid && harness == HarnessId::CODEX;
    let builtin_slugs = if codex_hybrid {
        if !configured.iter().any(|c| c.id.as_str() == official_provider) {
            bail!(
                "keeping Codex's original list needs the `{official_provider}` connection (sign in with `turnout auth login openai`)"
            );
        }
        let backups = BackupStore::at(home.backups());
        let adapter = match &config {
            Some(path) => harness_codex::CodexAdapter::with_config(path.clone(), backups),
            None => harness_codex::CodexAdapter::new(backups),
        };
        harness_codex::builtin_slugs(&adapter.builtin_catalog()?)
    } else {
        Vec::new()
    };

    let provider_count = pairs
        .iter()
        .map(|(provider, _)| provider)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    // Added Codex models are always namespaced so they never shadow a
    // built-in id.
    let public_model = |provider: &str, model: &str| {
        if provider_count > 1 || codex_hybrid {
            format!("{provider}/{model}")
        } else {
            model.to_string()
        }
    };
    let targets = builtin_slugs
        .iter()
        .map(|slug| SavedTarget {
            provider: official_provider.to_string(),
            model: slug.clone(),
            public_model: slug.clone(),
            fallbacks: Vec::new(),
        })
        .chain(pairs.iter().map(|(provider, model)| SavedTarget {
            provider: provider.clone(),
            model: model.clone(),
            public_model: public_model(provider, model),
            fallbacks: Vec::new(),
        }))
        .collect::<Vec<_>>();
    let targets = apply_fallback_chains(targets, fallback_chains)?;
    let models = targets
        .iter()
        .map(|target| target.public_model.clone())
        .collect::<Vec<_>>();
    // In hybrid mode the default may be an official Claude model that Turnout does
    // not route; it is written to `model` verbatim.
    let model = match &hybrid_default {
        Some(Some(official)) => official.clone(),
        _ => public_model(&default_pair.0, &default_pair.1),
    };
    let state = home.state()?;
    let port = gateway_port(state.gateway_port, harness);
    let gateway_url = format!("http://127.0.0.1:{port}");
    let mut relay_provider = ProviderConfig::openai_compatible(
        relay_core::brand::GATEWAY_PROVIDER_ID,
        format!("{}/v1", gateway_url.trim_end_matches('/')),
    );
    relay_provider.label = "Turnout".to_string();
    // A launch-only profile must never edit the shared harness config: the
    // editor extension keeps the official provider, and only `turnout run
    // --profile` gets this route through the environment.
    let surfaces = if profile.is_some() {
        vec![Surface::Cli]
    } else {
        vec![Surface::Cli, Surface::VsCode]
    };
    let ctx = RouteContext {
        provider: relay_provider.clone(),
        model: model.clone(),
        models: models.clone(),
        surfaces,
        via_gateway: true,
        gateway_url: Some(gateway_url),
        gateway_token_command: Some(
            std::env::current_exe().context("cannot locate the Turnout executable")?,
        ),
        gateway_token_file: Some(home.gateway_token_file()),
    };
    let route = Route {
        harness: HarnessId::new(harness),
        surfaces: ctx.surfaces.clone(),
        provider: relay_provider.id,
        model,
        models,
        profile: if hybrid {
            Some(harness_claude::HYBRID_PROFILE.to_string())
        } else {
            profile.map(str::to_string)
        },
    };
    if let Some(name) = profile {
        home.save_profile(
            harness,
            name,
            SavedRoute {
                provider: route.provider.to_string(),
                model: route.model.clone(),
                models: route.models.clone(),
                targets,
                via_gateway: true,
                hybrid: false,
                fallback,
            },
        )?;
        println!(
            "profile `{name}` saved — launch it with `turnout run {} --profile {name}`; shared harness config was not planned or changed",
            harness
        );
        return Ok(());
    }
    let adapter = adapter_for(home, harness, config)?;
    let plan = adapter.plan_changes(&route, &ctx)?;
    let validation = adapter.validate(&plan)?;
    let diff = plan.unified_diff();
    println!(
        "{}",
        if diff.is_empty() {
            "(no changes — already up to date)".to_string()
        } else {
            diff
        }
    );
    for change in &plan.changes {
        for note in &change.notes {
            println!("note: {note}");
        }
    }
    for message in &validation.messages {
        println!("{:?}: {}", message.level, message.text);
    }
    if !validation.ok {
        bail!("plan did not validate; nothing was saved");
    }
    home.save_plan(&plan, true, targets, fallback)?;
    println!(
        "\nplan {} saved — {} provider-aware model route(s)",
        plan.id,
        pairs.len()
    );
    Ok(())
}

fn plan_native_oauth(
    home: &Home,
    harness: &str,
    provider_id: &str,
    pairs: &[(String, String)],
    default_pair: &(String, String),
    config: Option<PathBuf>,
) -> Result<()> {
    let signed_in = oauth_connections(home)
        .into_iter()
        .find(|connection| connection.id == provider_id)
        .is_some_and(|connection| connection.logged_in);
    if !signed_in {
        bail!("`{provider_id}` OAuth session is not connected; sign in before applying this route");
    }

    let provider = if provider_id == "anthropic" {
        ProviderConfig::anthropic("anthropic", "https://api.anthropic.com/v1")
    } else {
        let mut provider = ProviderConfig::openai_compatible("openai", "https://api.openai.com/v1");
        provider.dialect = ProtocolDialect::OpenAiResponses;
        provider
    };
    let models = pairs
        .iter()
        .map(|(_, model)| model.clone())
        .collect::<Vec<_>>();
    let model = default_pair.1.clone();
    let surfaces = vec![Surface::Cli, Surface::VsCode];
    let context = RouteContext {
        provider: provider.clone(),
        model: model.clone(),
        models: models.clone(),
        surfaces: surfaces.clone(),
        via_gateway: false,
        gateway_url: None,
        gateway_token_command: None,
        gateway_token_file: None,
    };
    let route = Route {
        harness: HarnessId::new(harness),
        surfaces,
        provider: provider.id,
        model,
        models,
        profile: Some("official-oauth".to_string()),
    };
    let adapter = adapter_for(home, harness, config)?;
    let plan = adapter.plan_changes(&route, &context)?;
    let validation = adapter.validate(&plan)?;
    if !validation.ok {
        bail!("native OAuth plan did not validate; nothing was saved");
    }
    let diff = plan.unified_diff();
    println!(
        "{}",
        if diff.is_empty() {
            "(no changes — official OAuth profile is already active)".to_string()
        } else {
            diff
        }
    );
    println!("note: uses the official {provider_id} OAuth session; no API key or Turnout gateway is involved");
    let targets = pairs
        .iter()
        .map(|(provider, model)| SavedTarget {
            provider: provider.clone(),
            model: model.clone(),
            public_model: model.clone(),
            fallbacks: Vec::new(),
        })
        .collect();
    home.save_plan(&plan, false, targets, false)?;
    println!("\nplan {} saved — official OAuth route", plan.id);
    Ok(())
}

fn profiles(home: &Home, json: bool) -> Result<()> {
    let state = home.state()?;
    let route_json = |route: &SavedRoute| {
        serde_json::json!({
            "provider": route.provider,
            "model": route.model,
            "models": route.enabled_models(),
            "targets": route.enabled_targets(),
            "via_gateway": route.via_gateway,
            "hybrid": route.hybrid,
        })
    };
    if json {
        let mut harnesses = Vec::new();
        for harness in [HarnessId::CODEX, HarnessId::CLAUDE_CODE, HarnessId::OPENCODE] {
            let applied = state.routes.get(harness);
            let saved = state.profiles.get(harness);
            harnesses.push(serde_json::json!({
                "harness": harness,
                "applied": applied.map(route_json),
                // The launch command is the only way a profile reaches the harness:
                // `turnout run <harness> --profile <name>`.
                "profiles": saved.map(|profiles| profiles.iter().map(|(name, route)| {
                    serde_json::json!({
                        "name": name,
                        "command": format!("turnout run {harness} --profile {name}"),
                        "route": route_json(route),
                    })
                }).collect::<Vec<_>>()).unwrap_or_default(),
            }));
        }
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "data_dir": home.root(),
            "harnesses": harnesses,
        }))?);
        return Ok(());
    }
    for harness in [HarnessId::CODEX, HarnessId::CLAUDE_CODE, HarnessId::OPENCODE] {
        match state.routes.get(harness) {
            Some(route) => println!(
                "{harness:<12} applied   {} ({} model(s), default {})",
                route.provider,
                route.enabled_models().len(),
                route.model
            ),
            None => println!("{harness:<12} no applied route"),
        }
        for (name, route) in state.profiles.get(harness).into_iter().flatten() {
            println!(
                "{:<12} profile   {name}: {} ({} model(s), default {})",
                "",
                route.provider,
                route.enabled_models().len(),
                route.model
            );
            println!("{:<12}           turnout run {harness} --profile {name}", "");
        }
    }
    Ok(())
}

fn history(home: &Home, json: bool, limit: usize) -> Result<()> {
    let store = BackupStore::at(home.backups());
    let records = store.list()?;
    // The Agent log holds one line per started route; it is the only record of
    // what the background service actually served.
    let log_path = home.gateway_log();
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let entries = log
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .collect::<Vec<_>>();
    // Per-request lines written by the gateway: what was actually served.
    let requests = request_entries(home, limit);
    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "backups": records.iter().take(limit).map(|record| serde_json::json!({
                "id": record.id,
                "created_unix": record.created_unix,
                "label": record.label,
                "files": record.files.iter().map(|file| serde_json::json!({
                    "path": file.path,
                    "existed": file.existed,
                    "len": file.len,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "log": {
                "path": log_path,
                "entries": entries,
            },
            "requests": requests,
        }))?);
        return Ok(());
    }
    println!("{} backup(s):", records.len());
    for record in records.iter().take(limit) {
        println!(
            "  {:<28} {}  {} file(s)",
            record.id.to_string(),
            record.label,
            record.files.len()
        );
    }
    println!("\nAgent log ({} line(s)): {}", entries.len(), log_path.display());
    for line in entries.iter().rev().take(limit).rev() {
        println!("  {line}");
    }
    if !requests.is_empty() {
        println!("\nRequests ({} shown, newest first):", requests.len());
        for entry in &requests {
            let fallback = if entry["fallback"] == serde_json::json!(true) {
                " fallback"
            } else {
                ""
            };
            println!(
                "  {}  {:<28} {}:{}{}  {} ms  {} in / {} out",
                format_unix(entry["ts"].as_u64().unwrap_or(0)),
                entry["model"].as_str().unwrap_or(""),
                entry["provider"].as_str().unwrap_or(""),
                entry["upstream_model"].as_str().unwrap_or(""),
                fallback,
                entry["ms"].as_u64().unwrap_or(0),
                entry["input_tokens"].as_u64().unwrap_or(0),
                entry["output_tokens"].as_u64().unwrap_or(0),
            );
        }
    }
    Ok(())
}

/// Newest-first request lines across every harness, capped at `limit`.
fn request_entries(home: &Home, limit: usize) -> Vec<serde_json::Value> {
    let dir = relay_gateway::requests_dir(home.root());
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut merged = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .flat_map(|entry| {
            let harness = entry
                .path()
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_default();
            std::fs::read_to_string(entry.path())
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .map(|mut value| {
                    if let Some(object) = value.as_object_mut() {
                        object.insert("harness".into(), serde_json::json!(harness));
                    }
                    value
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    // The timestamps are second-granular, so keep file order within a second.
    merged.reverse();
    merged.sort_by_key(|value| std::cmp::Reverse(value["ts"].as_u64().unwrap_or(0)));
    merged.truncate(limit);
    merged
}

fn format_unix(seconds: u64) -> String {
    // Civil-from-days (Hinnant): exact UTC date without pulling in a date crate.
    let days = (seconds / 86_400) as i64;
    let time = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

fn apply(home: &Home, harness: Option<String>) -> Result<()> {
    let harness = harness.map(|h| canonical_harness(&h)).transpose()?;
    let (plan, via_gateway, targets, fallback) = home.load_plan(harness)?;
    let adapter = adapter_for(home, plan.harness.as_str(), None)?;
    // The saved plan carries the exact file paths and contents it was made for;
    // apply refuses it if any of those files changed in the meantime.
    let validation = adapter.validate(&plan)?;
    if !validation.ok {
        bail!(
            "plan no longer validates: {}",
            validation
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    let route_before = home.state()?.routes.get(plan.harness.as_str()).cloned();
    let id = adapter.apply(&plan)?;
    home.save_route_before(
        &id.to_string(),
        plan.harness.as_str(),
        route_before.as_ref(),
    )?;
    home.save_route(
        plan.harness.as_str(),
        SavedRoute {
            provider: plan.route.provider.to_string(),
            model: plan.route.model.clone(),
            models: if plan.route.models.is_empty() {
                vec![plan.route.model.clone()]
            } else {
                plan.route.models.clone()
            },
            targets,
            via_gateway,
            hybrid: plan.route.profile.as_deref() == Some(harness_claude::HYBRID_PROFILE)
                || plan.route.profile.as_deref() == Some(harness_codex::HYBRID_PROFILE),
            fallback,
        },
    )?;
    println!("applied {} file(s); backup {id}", plan.changes.len());
    println!("undo with: turnout rollback {id}");
    // The background agent reads its route once at startup, so a newly applied
    // gateway route is invisible to the editor extensions until the agent is
    // restarted. This is why an applied model could still be rejected as
    // "not enabled for this Turnout route".
    if via_gateway {
        match restart_agent() {
            Ok(restarted) if restarted => println!("Turnout Agent restarted with the new route"),
            Ok(_) => {}
            Err(error) => eprintln!("warning: could not restart the Turnout Agent: {error}"),
        }
    }
    Ok(())
}

/// Restarts the per-user background agent when one is installed. Returns
/// `false` when there is no agent, so callers know the route only becomes
/// active on the next `turnout gateway serve`.
fn restart_agent() -> Result<bool> {
    // The installed Agent serves the default data directory. A command run
    // against another one (`TURNOUT_HOME`, as the tests do) must not restart it.
    if std::env::var_os("TURNOUT_HOME").is_some() || std::env::var_os("RELAY_HOME").is_some() {
        return Ok(false);
    }
    if cfg!(target_os = "macos") {
        let path = agent_file()?;
        if !path.exists() {
            return Ok(false);
        }
        let uid = std::process::Command::new("id").arg("-u").output()?;
        let domain = format!("gui/{}", String::from_utf8_lossy(&uid.stdout).trim());
        let status = std::process::Command::new("launchctl")
            .args([
                "kickstart",
                "-k",
                &format!("{domain}/{}", relay_core::brand::LAUNCHD_LABEL),
            ])
            .status()?;
        if !status.success() {
            bail!("launchctl could not restart the Turnout Agent");
        }
        Ok(true)
    } else if cfg!(target_os = "linux") {
        if !agent_file()?.exists() {
            return Ok(false);
        }
        let status = std::process::Command::new("systemctl")
            .args(["--user", "restart", relay_core::brand::SYSTEMD_UNIT])
            .status()?;
        if !status.success() {
            bail!("systemd could not restart the Turnout Agent");
        }
        Ok(true)
    } else if cfg!(target_os = "windows") {
        let end = std::process::Command::new("schtasks")
            .args(["/End", "/TN", relay_core::brand::WINDOWS_TASK])
            .status();
        if end.map(|s| !s.success()).unwrap_or(true) {
            // No scheduled task (or it was not running); nothing to restart.
            return Ok(false);
        }
        let status = std::process::Command::new("schtasks")
            .args(["/Run", "/TN", relay_core::brand::WINDOWS_TASK])
            .status()?;
        if !status.success() {
            bail!("Task Scheduler could not restart the Turnout Agent");
        }
        Ok(true)
    } else {
        Ok(false)
    }
}

fn gateway_credential() -> CredentialRef {
    CredentialRef {
        service: "relay".to_string(),
        account: "gateway:local".to_string(),
    }
}

fn write_private_token(path: &Path, token: &str) -> Result<()> {
    relay_config::write_file(path, &format!("{token}\n"))?;
    if let Err(error) = relay_config::perms::owner_only(path, false) {
        // Unix mode bits are the only protection there, so failing is right.
        // On Windows the file already sits in the per-user profile; a failed
        // ACL rewrite must not stop the gateway from being installed.
        if cfg!(unix) {
            return Err(error.into());
        }
        eprintln!(
            "warning: could not restrict {} to the current user: {error}",
            path.display()
        );
    }
    Ok(())
}

fn gateway_token(home: &Home) -> Result<String> {
    if let Ok(token) = std::fs::read_to_string(home.gateway_token_file()) {
        let token = token.trim();
        if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(token.to_string());
        }
    }
    let store = credential_store(home)?;
    let credential = gateway_credential();
    let token = match store.get(&credential)? {
        Some(token) => token,
        None => {
            let token = Gateway::generate_token();
            store.set(&credential, &token)?;
            token
        }
    };
    // OpenCode supports file substitution but not command-backed auth. This
    // file contains only the loopback capability token, never a provider key.
    write_private_token(&home.gateway_token_file(), &token)?;
    Ok(token)
}

/// One JSONL file per harness under `<data dir>/requests/`. Bounded by the
/// gateway itself (oldest half dropped past 5000 lines), so no rotation daemon.
fn harness_log(home: &Home, harness: &str) -> Option<std::path::PathBuf> {
    Some(relay_gateway::requests_dir(&home.root()).join(format!("{harness}.jsonl")))
}

fn gateway_running(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok()
}

/// First loopback port above `base` that a profile gateway can bind. The
/// editor agent owns `base` and the per-harness offsets above it.
fn free_gateway_port(base: u16) -> u16 {
    (base + 10..=base + 40)
        .find(|port| !gateway_running(*port))
        .unwrap_or(base + 10)
}

/// One policy for every gateway this process runs, so each provider's
/// `max_concurrent` is a single limit across harnesses.
fn traffic_policy(home: &Home) -> Result<TrafficPolicy> {
    Ok(TrafficPolicy::default().with_limits(home.providers()?.into_iter().filter_map(
        |provider| {
            provider
                .max_concurrent
                .map(|limit| (provider.id.to_string(), limit as usize))
        },
    )))
}

fn gateway_config(
    home: &Home,
    harness: &str,
    route: &SavedRoute,
    token: String,
    port: u16,
    traffic: TrafficPolicy,
) -> Result<GatewayConfig> {
    let targets = route.enabled_targets();
    let default_target = targets
        .iter()
        .find(|target| target.public_model == route.model)
        .or_else(|| targets.first())
        .ok_or_else(|| anyhow!("saved route has no model targets"))?;
    let mut upstreams: std::collections::BTreeMap<String, Arc<dyn ProviderAdapter>> =
        std::collections::BTreeMap::new();
    for target in &targets {
        if !upstreams.contains_key(&target.provider) {
            upstreams.insert(
                target.provider.clone(),
                Arc::from(provider_adapter(home, &target.provider)?),
            );
        }
    }
    let upstream = upstreams
        .get(&default_target.provider)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "default provider `{}` is unavailable",
                default_target.provider
            )
        })?;
    let model_routes = targets
        .iter()
        .map(|target| {
            Ok(GatewayModelRoute {
                public_model: target.public_model.clone(),
                upstream_model: target.model.clone(),
                upstream: upstreams
                    .get(&target.provider)
                    .cloned()
                    .ok_or_else(|| anyhow!("provider `{}` is unavailable", target.provider))?,
                fallbacks: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // Fallbacks: a target's own `--fallback-chain` wins; otherwise the
    // route-wide switch offers every other model of the route, in route order;
    // otherwise none, and a failing model stays failed.
    let strip = |route: &GatewayModelRoute| GatewayModelRoute {
        fallbacks: Vec::new(),
        ..route.clone()
    };
    let model_routes = model_routes
        .iter()
        .zip(&targets)
        .map(|(primary, target)| {
            let fallbacks = if !target.fallbacks.is_empty() {
                target
                    .fallbacks
                    .iter()
                    .filter_map(|public| {
                        model_routes
                            .iter()
                            .find(|candidate| &candidate.public_model == public)
                    })
                    .map(strip)
                    .collect()
            } else if route.fallback {
                model_routes
                    .iter()
                    .filter(|other| other.public_model != primary.public_model)
                    .map(strip)
                    .collect()
            } else {
                Vec::new()
            };
            GatewayModelRoute {
                fallbacks,
                ..primary.clone()
            }
        })
        .collect::<Vec<_>>();
    let allowed_models = targets
        .iter()
        .map(|target| target.public_model.clone())
        .collect::<Vec<_>>();
    Ok(GatewayConfig {
        upstream,
        model_routes,
        token,
        // A hybrid route must not pin every request to one custom model.
        model_override: (targets.len() == 1 && !route.hybrid).then(|| default_target.model.clone()),
        allowed_models,
        port,
        // Only Claude's hybrid forwards its own login; Codex's hybrid routes
        // its built-in models through Turnout's OpenAI connection instead.
        anthropic_passthrough: (route.hybrid && harness == HarnessId::CLAUDE_CODE)
            .then(|| "https://api.anthropic.com".to_string()),
        request_log: harness_log(home, harness),
        traffic,
    })
}

async fn serve_gateways(home: &Home, only: Option<&str>) -> Result<()> {
    let only = only.map(canonical_harness).transpose()?;
    let state = home.state()?;
    let token = gateway_token(home)?;
    let traffic = traffic_policy(home)?;
    let mut handles = Vec::new();

    for (harness, route) in &state.routes {
        if only.is_some_and(|wanted| wanted != harness) || !route.via_gateway {
            continue;
        }
        let models = route.enabled_models();
        let port = gateway_port(state.gateway_port, harness);
        let handle = Gateway::start(gateway_config(
            home,
            harness,
            route,
            token.clone(),
            port,
            traffic.clone(),
        )?)
            .await
            .with_context(|| format!("cannot start {harness} gateway on 127.0.0.1:{port}"))?;
        eprintln!(
            "turnout-agent: {harness} -> {} ({} model(s), default {}) on {}",
            route.provider,
            models.len(),
            route.model,
            handle.url()
        );
        handles.push(handle);
    }

    if handles.is_empty() {
        bail!("no saved gateway routes; apply a provider/model route first");
    }
    wait_for_shutdown()
        .await
        .context("waiting for shutdown signal")?;
    for handle in handles {
        handle.shutdown().await;
    }
    Ok(())
}

/// Blocks until the process is asked to stop.
///
/// Unix services get SIGTERM/SIGINT. A Windows scheduled task has no SIGTERM:
/// `schtasks /End` asks the console control handler, which surfaces as
/// Ctrl-Break, and a sign-off arrives as Ctrl-Shutdown. Listening for those
/// lets the gateways close their listeners and in-flight responses instead of
/// being killed with the process.
async fn wait_for_shutdown() -> Result<()> {
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        let mut ctrl_break = windows::ctrl_break()
            .map_err(|error| anyhow!("cannot listen for Ctrl-Break: {error}"))?;
        let mut ctrl_shutdown = windows::ctrl_shutdown()
            .map_err(|error| anyhow!("cannot listen for Ctrl-Shutdown: {error}"))?;
        let mut ctrl_close = windows::ctrl_close()
            .map_err(|error| anyhow!("cannot listen for Ctrl-Close: {error}"))?;
        tokio::select! {
            _ = ctrl_break.recv() => {}
            _ = ctrl_shutdown.recv() => {}
            _ = ctrl_close.recv() => {}
            result = tokio::signal::ctrl_c() => {
                result?;
            }
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        tokio::signal::ctrl_c().await?;
        Ok(())
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Task Scheduler definition for the background agent.
///
/// Registered at sign-in like the launchd agent, but with the same crash
/// behaviour: `RestartOnFailure` restarts `gateway serve` a minute after it
/// exits, and `ExecutionTimeLimit` is disabled so the gateway is never
/// stopped just for running for more than three days (the Task Scheduler
/// default). `turnout.exe` is a console program, so started directly it opens
/// a terminal window that stops the gateway when closed; `conhost --headless`
/// hosts it without one.
fn task_xml(executable: &Path) -> String {
    let exe = xml_escape(&executable.to_string_lossy());
    let working_dir = executable
        .parent()
        .map(|dir| xml_escape(&dir.to_string_lossy()))
        .unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
  <RegistrationInfo>\n\
    <Description>Turnout gateway for editor extensions</Description>\n\
  </RegistrationInfo>\n\
  <Triggers>\n\
    <LogonTrigger>\n\
      <Enabled>true</Enabled>\n\
    </LogonTrigger>\n\
  </Triggers>\n\
  <Settings>\n\
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
    <AllowHardTerminate>true</AllowHardTerminate>\n\
    <StartWhenAvailable>true</StartWhenAvailable>\n\
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\n\
    <IdleSettings>\n\
      <StopOnIdleEnd>false</StopOnIdleEnd>\n\
      <RestartOnIdle>false</RestartOnIdle>\n\
    </IdleSettings>\n\
    <AllowStartOnDemand>true</AllowStartOnDemand>\n\
    <Enabled>true</Enabled>\n\
    <Hidden>true</Hidden>\n\
    <RunOnlyIfIdle>false</RunOnlyIfIdle>\n\
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n\
    <RestartOnFailure>\n\
      <Interval>PT1M</Interval>\n\
      <Count>999</Count>\n\
    </RestartOnFailure>\n\
  </Settings>\n\
  <Actions Context=\"Author\">\n\
    <Exec>\n\
      <Command>conhost.exe</Command>\n\
      <Arguments>--headless \"{exe}\" gateway serve</Arguments>\n\
      <WorkingDirectory>{working_dir}</WorkingDirectory>\n\
    </Exec>\n\
  </Actions>\n\
</Task>\n"
    )
}

/// Whether a per-user background agent is registered (running or not).
fn agent_installed() -> bool {
    if cfg!(target_os = "windows") {
        std::process::Command::new("schtasks")
            .args(["/Query", "/TN", relay_core::brand::WINDOWS_TASK])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    } else {
        agent_file().map(|path| path.exists()).unwrap_or(false)
    }
}

fn agent_file() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(dirs::home_dir()
            .ok_or_else(|| anyhow!("cannot locate home directory"))?
            .join(format!(
                "Library/LaunchAgents/{}.plist",
                relay_core::brand::LAUNCHD_LABEL
            )))
    } else if cfg!(target_os = "linux") {
        Ok(dirs::config_dir()
            .ok_or_else(|| anyhow!("cannot locate config directory"))?
            .join(format!("systemd/user/{}", relay_core::brand::SYSTEMD_UNIT)))
    } else {
        Ok(PathBuf::from(relay_core::brand::WINDOWS_TASK))
    }
}

/// Removes a pre-rename background service (launchd label, systemd unit or
/// scheduled task) so the old `relay gateway serve` process cannot keep
/// running against the same ports after the upgrade. A missing service is not
/// an error.
fn remove_legacy_agent() -> Result<()> {
    if cfg!(target_os = "macos") {
        let Some(home) = dirs::home_dir() else {
            return Ok(());
        };
        let path = home.join(format!(
            "Library/LaunchAgents/{}.plist",
            relay_core::brand::LEGACY_LAUNCHD_LABEL
        ));
        let uid = std::process::Command::new("id").arg("-u").output()?;
        let domain = format!("gui/{}", String::from_utf8_lossy(&uid.stdout).trim());
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &domain, path.to_string_lossy().as_ref()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::process::Command::new("launchctl")
            .args([
                "bootout",
                &format!("{domain}/{}", relay_core::brand::LEGACY_LAUNCHD_LABEL),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    } else if cfg!(target_os = "linux") {
        let _ = std::process::Command::new("systemctl")
            .args([
                "--user",
                "disable",
                "--now",
                relay_core::brand::LEGACY_SYSTEMD_UNIT,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Some(config) = dirs::config_dir() {
            let path = config.join(format!(
                "systemd/user/{}",
                relay_core::brand::LEGACY_SYSTEMD_UNIT
            ));
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
        }
    } else if cfg!(target_os = "windows") {
        let _ = std::process::Command::new("schtasks")
            .args([
                "/Delete",
                "/F",
                "/TN",
                relay_core::brand::LEGACY_WINDOWS_TASK,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    Ok(())
}

fn install_agent(home: &Home) -> Result<()> {
    let executable = std::env::current_exe().context("cannot locate Turnout executable")?;
    if cfg!(target_os = "macos") {
        let path = agent_file()?;
        let exe = xml_escape(&executable.to_string_lossy());
        let log = xml_escape(&home.gateway_log().to_string_lossy());
        let label = relay_core::brand::LAUNCHD_LABEL;
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{label}</string>\n<key>ProgramArguments</key><array><string>{exe}</string><string>gateway</string><string>serve</string></array>\n<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>\n<key>StandardOutPath</key><string>{log}</string><key>StandardErrorPath</key><string>{log}</string>\n</dict></plist>\n"
        );
        remove_legacy_agent()?;
        relay_config::write_file(&path, &plist)?;
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .context("cannot determine user id")?;
        let domain = format!("gui/{}", String::from_utf8_lossy(&uid.stdout).trim());
        // Bootout fails harmlessly when the agent is not loaded; keep that noise out.
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &domain, path.to_string_lossy().as_ref()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let status = std::process::Command::new("launchctl")
            .args(["bootstrap", &domain, path.to_string_lossy().as_ref()])
            .status()?;
        if !status.success() {
            bail!("launchctl could not install the Turnout Agent");
        }
        println!("Turnout Agent installed and started");
    } else if cfg!(target_os = "linux") {
        let path = agent_file()?;
        remove_legacy_agent()?;
        let unit = format!(
            "[Unit]\nDescription=Turnout editor gateway\n\n[Service]\nExecStart={} gateway serve\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
            executable.display()
        );
        relay_config::write_file(&path, &unit)?;
        // A running unit keeps its old ExecStart until reloaded and restarted,
        // which matters when `turnout install` re-points it.
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "enable", relay_core::brand::SYSTEMD_UNIT])
            .status();
        let status = std::process::Command::new("systemctl")
            .args(["--user", "restart", relay_core::brand::SYSTEMD_UNIT])
            .status()?;
        if !status.success() {
            bail!("systemd could not install the Turnout Agent");
        }
        println!("Turnout Agent installed and started");
    } else if cfg!(target_os = "windows") {
        remove_legacy_agent()?;
        // Stop a running instance first: `/Create /F` replaces the definition
        // but leaves the old process (possibly an older binary) holding the
        // ports, and `/Run` below would then be a no-op.
        let _ = std::process::Command::new("schtasks")
            .args(["/End", "/TN", relay_core::brand::WINDOWS_TASK])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        // `/Create` accepts no restart policy, so a crashed gateway would stay
        // down until the next sign-in (launchd KeepAlive and systemd
        // Restart=on-failure both cover this). Register the task from an XML
        // definition instead: RestartOnFailure plus unlimited execution time.
        let task_file = std::env::temp_dir().join(format!(
            "turnout-agent-{}.xml",
            std::process::id()
        ));
        let xml = task_xml(&executable);
        // `schtasks /XML` rejects a UTF-8 file; the declaration below says
        // UTF-16 and the file must actually be UTF-16LE with a byte-order mark.
        let mut encoded = Vec::with_capacity(xml.len() * 2 + 2);
        encoded.extend_from_slice(&[0xFF, 0xFE]);
        for unit in xml.encode_utf16() {
            encoded.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&task_file, &encoded)?;
        let status = std::process::Command::new("schtasks")
            .args([
                "/Create",
                "/F",
                "/TN",
                relay_core::brand::WINDOWS_TASK,
                "/XML",
            ])
            .arg(&task_file)
            .status();
        let _ = std::fs::remove_file(&task_file);
        if !status.map(|status| status.success()).unwrap_or(false) {
            bail!("Task Scheduler could not install the Turnout Agent");
        }
        // /Create only registers the task; it starts at the next sign-in unless
        // we run it now, so an install behaves like the other platforms.
        let _ = std::process::Command::new("schtasks")
            .args(["/Run", "/TN", relay_core::brand::WINDOWS_TASK])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        println!("Turnout Agent installed and started");
    }
    let _ = gateway_token(home)?;
    Ok(())
}

fn uninstall_agent() -> Result<()> {
    if cfg!(target_os = "macos") {
        let path = agent_file()?;
        let uid = std::process::Command::new("id").arg("-u").output()?;
        let domain = format!("gui/{}", String::from_utf8_lossy(&uid.stdout).trim());
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &domain, path.to_string_lossy().as_ref()])
            .status();
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    } else if cfg!(target_os = "linux") {
        let _ = std::process::Command::new("systemctl")
            .args([
                "--user",
                "disable",
                "--now",
                relay_core::brand::SYSTEMD_UNIT,
            ])
            .status();
        let path = agent_file()?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    } else if cfg!(target_os = "windows") {
        let _ = std::process::Command::new("schtasks")
            .args(["/Delete", "/F", "/TN", relay_core::brand::WINDOWS_TASK])
            .status();
    }
    println!("Turnout Agent removed");
    Ok(())
}

fn agents_command(home: &Home, command: AgentsCmd) -> Result<()> {
    let route = |harness: agents::Harness| -> Result<Option<SavedRoute>> {
        Ok(home.state()?.routes.get(harness.id()).cloned())
    };
    match command {
        AgentsCmd::List {
            harness,
            project,
            json,
        } => {
            let found = agents::list(home, harness, project.as_deref())?;
            let manager = route(harness)?.map(|route| route.model);
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "harness": harness.id(), "manager": manager, "agents": found })
                );
                return Ok(());
            }
            match &manager {
                Some(model) => println!(
                    "manager: {model} ({} route default; the harness's model picker changes it per session)",
                    harness.id()
                ),
                None => println!(
                    "manager: the harness's own default (no Turnout route for {})",
                    harness.id()
                ),
            }
            if found.is_empty() {
                println!(
                    "no worker agents — try: turnout agents plan deepseek-worker --harness {} --model evren::deepseek-v4.1-flash",
                    harness.id()
                );
            }
            for agent in found {
                println!(
                    "{:<20} {:<36} {:<8} {}{}",
                    agent.name,
                    agent.model.as_deref().unwrap_or("(inherits manager)"),
                    agent.scope,
                    if agent.managed { "turnout" } else { "hand-written" },
                    if agent.routable { "" } else { "  NOT SERVED BY THE ROUTE" }
                );
            }
            Ok(())
        }
        AgentsCmd::Plan {
            harness,
            name,
            model,
            scope,
            project,
            description,
            parallel,
            max_turns,
            no_worktree,
            tools,
            instructions_file,
            remove,
        } => {
            agents::check_name(&name)?;
            let path = agents::agents_dir(harness, scope, project.as_deref())?
                .join(format!("{name}.md"));
            let spec = match model {
                Some(model) if !remove => {
                    let route = route(harness)?;
                    let model = agents::resolve_model(harness, route.as_ref(), &model)?;
                    let mut spec = agents::AgentSpec::new(harness, &name, &model);
                    if let Some(description) = description {
                        spec.description = description;
                    }
                    spec.parallel = parallel;
                    spec.max_turns = Some(max_turns);
                    if no_worktree {
                        spec.worktree = false;
                    } else if !harness.has_worktrees() {
                        println!(
                            "note: {} has no worktree isolation; workers share the checkout, so give parallel workers separate files",
                            harness.id()
                        );
                    }
                    if !tools.is_empty() && harness != agents::Harness::ClaudeCode {
                        bail!("--tools is only supported for claude-code workers");
                    }
                    spec.tools = tools;
                    if let Some(file) = instructions_file {
                        spec.instructions = std::fs::read_to_string(&file)
                            .with_context(|| format!("cannot read {}", file.display()))?;
                    }
                    if parallel.is_some() {
                        // The advisory count is only as good as the provider's
                        // real limit; say so when there is none.
                        if let Some(provider) =
                            agents::provider_for(harness, route.as_ref(), &model)
                        {
                            let limited = home
                                .providers()?
                                .iter()
                                .any(|p| p.id.as_str() == provider && p.max_concurrent.is_some());
                            if !limited {
                                println!(
                                    "note: `{provider}` has no concurrency limit; to enforce one: turnout providers limit {provider} --max-concurrent <n>"
                                );
                            }
                        }
                    }
                    Some(spec)
                }
                _ => None,
            };
            let plan = agents::plan(home, harness, path, spec.as_ref())?;
            let diff = plan.diff();
            println!(
                "{}",
                if diff.trim().is_empty() {
                    "(no changes — already up to date)".to_string()
                } else {
                    diff
                }
            );
            println!(
                "\nagent plan saved — run `turnout agents apply --harness {}` to write it",
                harness.id()
            );
            if let Some(route) = route(harness)? {
                println!(
                    "manager: {} (change it with `turnout agents manager --harness {} <provider::model>`)",
                    route.model,
                    harness.id()
                );
            }
            Ok(())
        }
        AgentsCmd::Manager {
            harness,
            model,
            yes,
        } => {
            let id = harness.id();
            let route = route(harness)?.ok_or_else(|| {
                anyhow!("{id} has no Turnout route yet; set one up with `turnout plan-targets {id} ...`")
            })?;
            let (provider, name) = parse_target(&model)?;
            let mut targets = route.enabled_targets();
            let current = route.model.clone();
            // Only Claude Code's hybrid route keeps Claude's own models outside
            // the route's targets; everything else must be a target.
            let official = harness == agents::Harness::ClaudeCode && provider == "anthropic";
            if official && !route.hybrid {
                bail!("this claude-code route sends everything to the gateway; a Claude model can manage only on a --hybrid route");
            }
            if !official && !targets.iter().any(|t| t.provider == provider && t.model == name) {
                bail!("`{model}` is not part of the {id} route; add it with `turnout plan-targets` first");
            }
            let is_current = targets
                .iter()
                .find(|t| t.provider == provider && t.model == name)
                .map(|t| t.public_model == current || t.model == current)
                .unwrap_or(name == current);
            if is_current {
                println!("`{model}` already manages {id}; nothing to change");
                return Ok(());
            }
            // Rebuild the route exactly as it is, with only the default changed.
            if official {
                targets.retain(|t| t.provider != "anthropic");
            }
            let mut values = targets
                .iter()
                .map(|t| format!("{}::{}", t.provider, t.model))
                .collect::<Vec<_>>();
            if official {
                values.insert(0, format!("anthropic::{name}"));
            }
            let chains = targets
                .iter()
                .filter(|t| !t.fallbacks.is_empty())
                .map(|t| {
                    let to = t
                        .fallbacks
                        .iter()
                        .filter_map(|public| targets.iter().find(|o| &o.public_model == public))
                        .map(|o| format!("{}::{}", o.provider, o.model))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("{}::{}={to}", t.provider, t.model)
                })
                .collect::<Vec<_>>();
            println!("manager: {current} -> {name}\n");
            plan_targets(
                home,
                id,
                &values,
                &format!("{provider}::{name}"),
                None,
                None,
                route.hybrid,
                route.fallback,
                &chains,
            )?;
            let confirmed = if yes {
                true
            } else {
                use std::io::{BufRead, IsTerminal, Write};
                if !std::io::stdin().is_terminal() {
                    println!("not applied: confirm with `turnout apply --harness {id}`, or rerun with --yes");
                    return Ok(());
                }
                let label = match harness {
                    agents::Harness::ClaudeCode => "Claude Code (CLI and VS Code)",
                    agents::Harness::OpenCode => "OpenCode",
                };
                print!("\nMake `{name}` the manager for {label}? [y/N] ");
                std::io::stdout().flush()?;
                let mut answer = String::new();
                std::io::stdin().lock().read_line(&mut answer)?;
                matches!(answer.trim().to_lowercase().as_str(), "y" | "yes" | "e" | "evet")
            };
            if !confirmed {
                println!("not applied; the route is unchanged (the plan stays saved for `turnout apply`)");
                return Ok(());
            }
            apply(home, Some(id.to_string()))
        }
        AgentsCmd::Apply { harness } => {
            let (id, paths) = agents::apply(home, harness)?;
            for path in &paths {
                println!("  {}", path.display());
            }
            println!("applied; backup {id}");
            println!("undo with: turnout agents rollback {id}");
            println!("{}", agents::reload_hint(harness));
            Ok(())
        }
        AgentsCmd::Rollback { id } => {
            let id = match id {
                Some(id) => BackupId(id),
                None => agents::latest_backup(home)?
                    .ok_or_else(|| anyhow!("no agent changes to roll back"))?,
            };
            let Some(harness) = agents::backup_harness(home, &id) else {
                bail!("{id} is not an agent change; use `turnout rollback {id}`");
            };
            let restored = BackupStore::at(home.backups()).restore(&id)?;
            println!("restored {} file(s) from {id}", restored.len());
            for path in restored {
                println!("  {}", path.display());
            }
            println!("{}", agents::reload_hint(harness));
            Ok(())
        }
    }
}

async fn gateway_command(home: &Home, command: GatewayCmd) -> Result<()> {
    match command {
        GatewayCmd::Serve { harness } => serve_gateways(home, harness.as_deref()).await,
        GatewayCmd::Token => {
            println!("{}", gateway_token(home)?);
            Ok(())
        }
        GatewayCmd::Status => {
            let state = home.state()?;
            for (harness, route) in state.routes {
                if !route.via_gateway {
                    continue;
                }
                let port = gateway_port(state.gateway_port, &harness);
                println!(
                    "{:<12} {:<7} 127.0.0.1:{port}  {} ({} model(s), default {})",
                    harness,
                    if gateway_running(port) {
                        "ready"
                    } else {
                        "stopped"
                    },
                    route.provider,
                    route.enabled_models().len(),
                    route.model
                );
            }
            Ok(())
        }
        GatewayCmd::Install => install_agent(home),
        GatewayCmd::Uninstall => uninstall_agent(),
    }
}

async fn launch(
    home: &Home,
    harness: &str,
    profile: Option<&str>,
    args: Vec<String>,
) -> Result<()> {
    let harness = canonical_harness(harness)?;
    let route = home.find_route(harness, profile)?;
    let binary = all_adapters(home)
        .into_iter()
        .find(|a| a.id().as_str() == harness)
        .and_then(|a| a.detect_installations().into_iter().find_map(|i| i.binary))
        .ok_or_else(|| anyhow!("{harness} is not installed (not found on PATH)"))?;

    let mut command = tokio::process::Command::from(relay_config::kit::process(&binary));
    let mut prefix_args = Vec::<String>::new();
    let mut opencode_standalone = false;
    if profile.is_some() && !route.via_gateway {
        if harness == HarnessId::CODEX && route.provider == ProviderId::OPENAI {
            prefix_args.extend(["--model".to_string(), route.model.clone()]);
        } else if harness == HarnessId::CLAUDE_CODE && route.provider == ProviderId::ANTHROPIC {
            prefix_args.extend(["--model".to_string(), route.model.clone()]);
        }
    }
    let mut gateway = None;
    if route.via_gateway {
        let base = gateway_port(home.state()?.gateway_port, harness);
        // A profile carries its own model list. Reusing the editor gateway's
        // port would hand the process a route that rejects those models, so a
        // profile always gets a private, throwaway gateway.
        let (_port, running) = if profile.is_some() {
            (free_gateway_port(base), false)
        } else {
            (base, gateway_running(base))
        };
        let port = _port;
        let token = if running {
            gateway_token(home)?
        } else {
            Gateway::generate_token()
        };
        let gateway_url = format!("http://127.0.0.1:{port}");
        if !running {
            let handle = Gateway::start(gateway_config(
                home,
                harness,
                &route,
                token.clone(),
                port,
                traffic_policy(home)?,
            )?)
                .await
                .context("cannot start the gateway")?;
            eprintln!("turnout: temporary gateway on {}", handle.url());
            gateway = Some(handle);
        }
        command.env(GATEWAY_TOKEN_ENV, &token);
        command
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost");
        if harness == HarnessId::CLAUDE_CODE {
            command
                .env("ANTHROPIC_BASE_URL", gateway_url)
                .env("ANTHROPIC_AUTH_TOKEN", &token)
                .env("ANTHROPIC_MODEL", &route.model)
                .env("ANTHROPIC_DEFAULT_HAIKU_MODEL", &route.model)
                .env("ANTHROPIC_DEFAULT_SONNET_MODEL", &route.model)
                .env("ANTHROPIC_DEFAULT_OPUS_MODEL", &route.model);
        } else if profile.is_some() && harness == HarnessId::OPENCODE {
            // OpenCode 2.x normally reuses its background server. That server
            // was started before this process and cannot see per-process
            // config/env, so a launch profile must use a private server.
            // Inline config is a final merge and never touches the global file.
            let models = route
                .enabled_models()
                .into_iter()
                .map(|model| (model.clone(), serde_json::json!({ "name": model })))
                .collect::<serde_json::Map<_, _>>();
            let inline = serde_json::json!({
                "model": format!("turnout-profile/{}", route.model),
                "provider": {
                    "turnout-profile": {
                        "npm": "@ai-sdk/openai-compatible",
                        "name": "Turnout CLI Profile",
                        "options": {
                            "baseURL": format!("{gateway_url}/v1"),
                            "apiKey": token,
                        },
                        "models": models,
                    }
                }
            });
            command.env("OPENCODE_CONFIG_CONTENT", inline.to_string());
            opencode_standalone = true;
        } else if profile.is_some() && harness == HarnessId::CODEX {
            // Codex exposes process-local config overrides. Supplying the
            // provider this way keeps ~/.codex/config.toml (and therefore the
            // VS Code extension) byte-for-byte untouched.
            let q = |value: &str| serde_json::to_string(value).expect("strings serialize");
            for override_value in [
                format!("model={}", q(&route.model)),
                "model_provider=\"turnout-profile\"".to_string(),
                "model_providers.turnout-profile.name=\"Turnout CLI Profile\"".to_string(),
                format!(
                    "model_providers.turnout-profile.base_url={}",
                    q(&format!("{gateway_url}/v1"))
                ),
                "model_providers.turnout-profile.wire_api=\"responses\"".to_string(),
                format!(
                    "model_providers.turnout-profile.env_key={}",
                    q(GATEWAY_TOKEN_ENV)
                ),
            ] {
                prefix_args.push("-c".to_string());
                prefix_args.push(override_value);
            }
        }
    } else if (harness == HarnessId::CODEX && route.provider == ProviderId::OPENAI)
        || (harness == HarnessId::CLAUDE_CODE && route.provider == ProviderId::ANTHROPIC)
    {
        // Official harness OAuth is owned and refreshed by the harness itself.
        // Never inject Turnout credentials into a native OAuth launch.
    } else {
        let upstream = provider_adapter(home, &route.provider)?;
        let config = upstream.config().clone();
        let key = credential_store(home)?.get(&config.credential)?;
        if let Some(key) = key {
            command.env(&config.api_key_env, &key);
            if harness == HarnessId::CLAUDE_CODE && config.base_url.contains("api.anthropic.com") {
                command.env("ANTHROPIC_API_KEY", &key);
            }
        } else {
            eprintln!("turnout: warning: no stored key for `{}`", config.id);
        }
    }
    command.args(prefix_args);
    if opencode_standalone
        && args.first().is_some_and(|arg| {
            matches!(
                arg.as_str(),
                "run"
                    | "mini"
                    | "models"
                    | "debug"
                    | "acp"
                    | "api"
                    | "mcp"
                    | "plugin"
                    | "session"
                    | "serve"
            )
        })
    {
        command.arg(&args[0]).arg("--standalone").args(&args[1..]);
    } else {
        command.args(args);
        if opencode_standalone {
            command.arg("--standalone");
        }
    }
    let status = command
        .status()
        .await
        .context("failed to start the harness")?;
    if let Some(handle) = gateway {
        handle.shutdown().await;
    }
    std::process::exit(status.code().unwrap_or(1));
}

async fn doctor(home: &Home, online: bool) -> Result<()> {
    let mut problems = 0;
    let mut report = |ok: bool, text: String| {
        println!("[{}] {text}", if ok { " ok " } else { "FAIL" });
        if !ok {
            problems += 1;
        }
    };
    report(true, format!("data dir: {}", home.root().display()));
    match credential_store(home) {
        Ok(store) => {
            let location = match backend_choice() {
                BackendChoice::File => format!(" ({})", home.credentials_file().display()),
                BackendChoice::Encrypted | BackendChoice::Vault => {
                    format!(" ({})", home.vault().display())
                }
                BackendChoice::Keyring => String::new(),
            };
            report(true, format!("credential store: {}{location}", store.name()));
            if backend_choice() != BackendChoice::File && home.credentials_file().exists() {
                report(
                    false,
                    format!(
                        "plaintext {} still exists; keys in it are unused — copy them with \
                         TURNOUT_CREDENTIALS=file turnout credentials migrate, then delete it",
                        home.credentials_file().display()
                    ),
                );
            }
        }
        Err(e) => report(false, format!("credential store: {e}")),
    }
    for adapter in all_adapters(home) {
        let found = adapter.detect_installations();
        report(
            !found.is_empty(),
            format!(
                "{}: {}",
                adapter.id(),
                if found.is_empty() {
                    "not installed".to_string()
                } else {
                    found
                        .iter()
                        .map(|i| {
                            i.version
                                .as_ref()
                                .map(|v| v.raw.clone())
                                .unwrap_or_else(|| "installed".into())
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        );
        if let Err(e) = adapter.read_config() {
            report(false, format!("{}: config unreadable: {e}", adapter.id()));
        }
    }
    let providers = home.providers()?;
    report(true, format!("{} provider(s) configured", providers.len()));
    if online {
        for p in providers {
            let adapter = provider_adapter(home, p.id.as_str())?;
            match adapter.discover_models().await {
                Ok(models) => report(
                    true,
                    format!("{}: reachable, {} model(s)", p.id, models.len()),
                ),
                Err(e) => report(false, format!("{}: {e}", p.id)),
            }
        }
    }
    if problems > 0 {
        bail!("{problems} check(s) failed")
    }
    Ok(())
}
