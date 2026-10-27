//! `turnout agents`: worker agents a harness's main model hands work to.
//!
//! The main session, running the route's default model, is the manager; each
//! worker is a file naming the model it runs on. So "Opus manages three
//! DeepSeek workers" is a route whose default is Opus plus one worker file on
//! DeepSeek. Where the files live and what they say depends on the harness:
//!
//! - Claude Code: `.claude/agents/<name>.md` in a project, or
//!   `~/.claude/agents/<name>.md` for every project. Workers can run in their
//!   own git worktree.
//! - OpenCode: `.opencode/agent/<name>.md`, or `agent/<name>.md` in OpenCode's
//!   global config directory, with `mode: subagent`. Models are referenced as
//!   `<provider>/<model>` the way OpenCode's config declares them. There is no
//!   worktree isolation: workers share the checkout.
//!
//! Codex has no worker files: its manager picks a model for each sub-agent it
//! spawns, from the model catalog the Codex route already provides.
//!
//! Turnout only writes workers on models the harness can actually reach: a
//! worker on a model the route does not serve would fail on its first request,
//! in the middle of someone's task. Changes go through the same
//! review/backup/rollback cycle as routes.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use relay_config::BackupStore;
use relay_core::{BackupId, HarnessId};
use serde::{Deserialize, Serialize};

use crate::home::{Home, SavedRoute};

/// First line of every body Turnout writes, so `list` can tell its files from
/// hand-written ones. A comment, because Claude Code's agent frontmatter
/// rejects unknown keys.
const MANAGED_MARK: &str = "<!-- Managed by Turnout: change with `turnout agents`. -->";
/// Marker file in a backup directory: the backup was made by `agents apply`.
const BACKUP_MARK: &str = "agents.json";

const DEFAULT_DESCRIPTION: &str = "Implements one well-scoped coding task and runs the checks it is given. Use for delegated implementation work.";
const DEFAULT_INSTRUCTIONS: &str = "You are a worker. Implement only the task you are given, inside the current
working directory. Do not touch files outside it.

Run the checks the task names. In your final report list, separately:
1. Files you changed.
2. Every command you ran for verification, with its exit code.
3. Anything you could not finish or verify.

Never claim a check passed unless you ran it and saw exit code 0.
Never run git push, merge, tag, release or publish anything.";

/// Harnesses with file-based workers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Harness {
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    #[value(name = "opencode")]
    OpenCode,
}

impl Harness {
    pub fn id(self) -> &'static str {
        match self {
            Harness::ClaudeCode => HarnessId::CLAUDE_CODE,
            Harness::OpenCode => HarnessId::OPENCODE,
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        [Harness::ClaudeCode, Harness::OpenCode]
            .into_iter()
            .find(|harness| harness.id() == id)
    }

    /// Whether workers can get their own git worktree.
    pub fn has_worktrees(self) -> bool {
        self == Harness::ClaudeCode
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Scope {
    /// The project's agent directory: only sessions in that project see it
    Project,
    /// The harness's global agent directory: every project sees it
    User,
}

/// What a worker file says.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentSpec {
    pub name: String,
    /// Model reference exactly as the harness's config spells it.
    pub model: String,
    pub description: String,
    pub max_turns: Option<u32>,
    pub worktree: bool,
    pub tools: Vec<String>,
    /// How many of these may run at once. Advisory: it is written into the
    /// description the manager reads. The hard cap is the provider's
    /// `turnout providers limit`.
    pub parallel: Option<u32>,
    pub instructions: String,
}

impl AgentSpec {
    pub fn new(harness: Harness, name: &str, model: &str) -> Self {
        Self {
            name: name.to_string(),
            model: model.to_string(),
            description: DEFAULT_DESCRIPTION.to_string(),
            max_turns: Some(40),
            worktree: harness.has_worktrees(),
            tools: Vec::new(),
            parallel: None,
            instructions: DEFAULT_INSTRUCTIONS.to_string(),
        }
    }

    /// The `.md` file the harness reads. Strings are written JSON-quoted,
    /// which is valid YAML and survives colons, quotes and `#`.
    pub fn render(&self, harness: Harness) -> String {
        let quote = |value: &str| serde_json::to_string(value).expect("strings serialize");
        let mut description = self.description.clone();
        if let Some(parallel) = self.parallel {
            description.push_str(&format!(" Up to {parallel} of these may run at once."));
        }
        let mut text = String::from("---\n");
        match harness {
            Harness::ClaudeCode => {
                text.push_str(&format!("name: {}\n", self.name));
                text.push_str(&format!("description: {}\n", quote(&description)));
                text.push_str(&format!("model: {}\n", quote(&self.model)));
                if !self.tools.is_empty() {
                    text.push_str(&format!("tools: {}\n", self.tools.join(", ")));
                }
                if self.worktree {
                    text.push_str("isolation: worktree\n");
                }
                if let Some(turns) = self.max_turns {
                    text.push_str(&format!("maxTurns: {turns}\n"));
                }
            }
            // OpenCode names the agent after the file.
            Harness::OpenCode => {
                text.push_str(&format!("description: {}\n", quote(&description)));
                text.push_str("mode: subagent\n");
                text.push_str(&format!("model: {}\n", quote(&self.model)));
                if let Some(turns) = self.max_turns {
                    text.push_str(&format!("steps: {turns}\n"));
                }
            }
        }
        text.push_str("---\n");
        text.push_str(MANAGED_MARK);
        text.push('\n');
        text.push_str(self.instructions.trim_end());
        text.push('\n');
        text
    }
}

/// What `list` reads back from a file (Turnout's or hand-written).
#[derive(Debug, Serialize)]
pub struct AgentFile {
    pub name: String,
    pub model: Option<String>,
    pub scope: &'static str,
    pub path: PathBuf,
    pub managed: bool,
    /// Whether the harness can reach this model through the current route.
    pub routable: bool,
}

/// `name` and `model` from the frontmatter.
fn parse(text: &str) -> Option<(Option<String>, Option<String>)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---")?;
    let mut name = None;
    let mut model = None;
    for line in rest[..end].lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let value = if value.starts_with('"') {
            serde_json::from_str::<String>(value)
                .unwrap_or_else(|_| value.trim_matches('"').to_string())
        } else {
            value.trim_matches('\'').to_string()
        };
        match key.trim() {
            "name" => name = Some(value),
            "model" => model = Some(value),
            _ => {}
        }
    }
    Some((name, model))
}

fn claude_dir() -> Result<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .ok_or_else(|| anyhow!("cannot locate the home directory"))
}

pub fn agents_dir(harness: Harness, scope: Scope, project: Option<&Path>) -> Result<PathBuf> {
    let project_root = || -> Result<PathBuf> {
        match project {
            Some(path) => Ok(path.to_path_buf()),
            None => std::env::current_dir().context("cannot read the current directory"),
        }
    };
    Ok(match (harness, scope) {
        (Harness::ClaudeCode, Scope::User) => claude_dir()?.join("agents"),
        (Harness::ClaudeCode, Scope::Project) => project_root()?.join(".claude").join("agents"),
        (Harness::OpenCode, Scope::User) => harness_opencode::global_config_dir().join("agent"),
        (Harness::OpenCode, Scope::Project) => project_root()?.join(".opencode").join("agent"),
    })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
}

/// Reject names a harness would not address or that escape the directory.
pub fn check_name(name: &str) -> Result<()> {
    if valid_name(name) {
        Ok(())
    } else {
        bail!("agent name `{name}` must be lowercase letters, digits and dashes")
    }
}

/// Claude's own model ids and aliases, which Claude Code resolves itself.
fn is_claude_model(model: &str) -> bool {
    matches!(model, "opus" | "sonnet" | "haiku" | "inherit")
        || (model.starts_with("claude-") && !model.contains('/'))
}

/// How the harness's config spells a route target.
fn reference(harness: Harness, route: &SavedRoute, provider: &str, model: &str, public: &str) -> String {
    match harness {
        Harness::ClaudeCode => public.to_string(),
        // The gateway is one OpenCode provider serving every public id; a
        // direct route declares the provider under its own id.
        Harness::OpenCode if route.via_gateway => {
            format!("{}/{public}", relay_core::brand::GATEWAY_PROVIDER_ID)
        }
        Harness::OpenCode => format!("{provider}/{model}"),
    }
}

/// Every model reference the route makes reachable for the harness.
fn references(harness: Harness, route: Option<&SavedRoute>) -> Vec<String> {
    let Some(route) = route else {
        return Vec::new();
    };
    route
        .enabled_targets()
        .into_iter()
        .map(|t| reference(harness, route, &t.provider, &t.model, &t.public_model))
        .collect()
}

/// Turn `provider::model` or a reference into what the harness's config
/// expects, and refuse models the harness cannot reach with its route.
pub fn resolve_model(harness: Harness, route: Option<&SavedRoute>, value: &str) -> Result<String> {
    let value = value.trim();
    if let Some((provider, model)) = value.split_once("::") {
        if harness == Harness::ClaudeCode && provider == "anthropic" && is_claude_model(model) {
            return resolve_model(harness, route, model);
        }
        let found = route.and_then(|route| {
            route
                .enabled_targets()
                .into_iter()
                .find(|target| target.provider == provider && target.model == model)
                .map(|t| reference(harness, route, &t.provider, &t.model, &t.public_model))
        });
        return found.ok_or_else(|| not_routable(harness, value));
    }
    if harness == Harness::ClaudeCode && is_claude_model(value) {
        // Without a gateway route (or with a hybrid one) Claude Code reaches
        // Anthropic itself; a plain gateway route serves only its own models.
        return match route {
            Some(route) if route.via_gateway && !route.hybrid => Err(anyhow!(
                "`{value}` is a Claude model, but the claude-code route sends everything to the Turnout gateway; \
                 re-plan it with --hybrid to keep Claude's own models"
            )),
            _ => Ok(value.to_string()),
        };
    }
    if references(harness, route).iter().any(|known| known == value) {
        Ok(value.to_string())
    } else {
        Err(not_routable(harness, value))
    }
}

/// The provider behind a resolved model reference, for limit hints.
pub fn provider_for(harness: Harness, route: Option<&SavedRoute>, model: &str) -> Option<String> {
    let route = route?;
    route
        .enabled_targets()
        .into_iter()
        .find(|t| reference(harness, route, &t.provider, &t.model, &t.public_model) == model)
        .map(|t| t.provider)
}

fn not_routable(harness: Harness, value: &str) -> anyhow::Error {
    anyhow!(
        "`{value}` is not served by the {} route; add it first with \
         `turnout plan-targets {} --target <provider::model> ...`, then `turnout apply`",
        harness.id(),
        harness.id()
    )
}

pub fn list(home: &Home, harness: Harness, project: Option<&Path>) -> Result<Vec<AgentFile>> {
    let route = home.state()?.routes.get(harness.id()).cloned();
    let mut found = Vec::new();
    for (scope, label) in [(Scope::Project, "project"), (Scope::User, "user")] {
        let dir = agents_dir(harness, scope, project)?;
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some((name, model)) = parse(&text) else {
                continue;
            };
            // OpenCode names the agent after its file.
            let name = name.unwrap_or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
            let routable = match &model {
                Some(model) => resolve_model(harness, route.as_ref(), model).is_ok(),
                // No model: the worker inherits the manager's.
                None => true,
            };
            found.push(AgentFile {
                name,
                model,
                scope: label,
                managed: text.contains(MANAGED_MARK),
                path,
                routable,
            });
        }
    }
    Ok(found)
}

/// One file to create, replace or delete. `before` guards against applying a
/// plan to a file that changed after it was reviewed.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentChange {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentPlan {
    pub harness: String,
    pub changes: Vec<AgentChange>,
}

impl AgentPlan {
    pub fn diff(&self) -> String {
        self.changes
            .iter()
            .map(|change| {
                relay_config::unified_diff(
                    change.before.as_deref().unwrap_or(""),
                    change.after.as_deref().unwrap_or(""),
                    &change.path,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn plan_file(home: &Home, harness: Harness) -> PathBuf {
    home.root()
        .join("plans")
        .join(format!("agents-{}.json", harness.id()))
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    if path.exists() {
        Ok(Some(std::fs::read_to_string(path).with_context(|| {
            format!("cannot read {}", path.display())
        })?))
    } else {
        Ok(None)
    }
}

/// Plan writing (or, with `spec = None`, removing) one worker file.
pub fn plan(
    home: &Home,
    harness: Harness,
    path: PathBuf,
    spec: Option<&AgentSpec>,
) -> Result<AgentPlan> {
    let before = read_optional(&path)?;
    if let Some(text) = &before {
        // Never take over a file someone wrote by hand.
        if !text.contains(MANAGED_MARK) {
            bail!(
                "{} was not written by Turnout; rename or remove it first",
                path.display()
            );
        }
    }
    let after = spec.map(|spec| spec.render(harness));
    if before.is_none() && after.is_none() {
        bail!("{} does not exist", path.display());
    }
    if let Some(text) = &after {
        if relay_config::kit::contains_secret_token(text) {
            bail!("the agent text looks like it contains an API key; keys must stay in the credential store");
        }
    }
    let plan = AgentPlan {
        harness: harness.id().to_string(),
        changes: vec![AgentChange {
            path,
            before,
            after,
        }],
    };
    relay_config::write_file(
        &plan_file(home, harness),
        &serde_json::to_string_pretty(&plan)?,
    )?;
    Ok(plan)
}

/// Apply the saved plan: refuse a stale one, back up, write, and undo
/// everything if any write fails.
pub fn apply(home: &Home, harness: Harness) -> Result<(BackupId, Vec<PathBuf>)> {
    let file = plan_file(home, harness);
    let text = std::fs::read_to_string(&file).map_err(|_| {
        anyhow!(
            "no saved {} agent plan; run `turnout agents plan` first",
            harness.id()
        )
    })?;
    let plan: AgentPlan = serde_json::from_str(&text)?;
    for change in &plan.changes {
        if read_optional(&change.path)? != change.before {
            bail!(
                "{} changed since the plan was made; run `turnout agents plan` again",
                change.path.display()
            );
        }
    }
    let paths = plan
        .changes
        .iter()
        .map(|c| c.path.clone())
        .collect::<Vec<_>>();
    let store = BackupStore::at(home.backups());
    let record = store.create(&format!("agents {}", plan.harness), &paths)?;
    let result = (|| -> Result<()> {
        for change in &plan.changes {
            match &change.after {
                Some(text) => relay_config::write_file(&change.path, text)?,
                None => std::fs::remove_file(&change.path)
                    .with_context(|| format!("cannot remove {}", change.path.display()))?,
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = store.restore(&record.id);
        return Err(error);
    }
    let _ = relay_config::write_file(
        &home.backups().join(record.id.to_string()).join(BACKUP_MARK),
        &serde_json::to_string(&plan.harness)?,
    );
    let _ = std::fs::remove_file(&file);
    Ok((record.id, paths))
}

/// The harness an `agents apply` backup belongs to; `None` for other backups.
pub fn backup_harness(home: &Home, id: &BackupId) -> Option<Harness> {
    let text =
        std::fs::read_to_string(home.backups().join(id.to_string()).join(BACKUP_MARK)).ok()?;
    let id: String = serde_json::from_str(&text).ok()?;
    Harness::from_id(&id)
}

/// The newest backup made by `agents apply`.
pub fn latest_backup(home: &Home) -> Result<Option<BackupId>> {
    Ok(BackupStore::at(home.backups())
        .list()?
        .into_iter()
        .map(|record| record.id)
        .find(|id| backup_harness(home, id).is_some()))
}

/// What a harness user has to do for a change to take effect.
pub fn reload_hint(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "Claude Code loads agents when a session starts: open a new session to use the change.",
        Harness::OpenCode => "OpenCode loads agents at startup: restart OpenCode (and its background service) to use the change.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::SavedTarget;

    fn route(hybrid: bool) -> SavedRoute {
        SavedRoute {
            provider: "turnout".into(),
            model: "deepseek-v4.1-flash".into(),
            models: Vec::new(),
            targets: vec![SavedTarget {
                provider: "evren".into(),
                model: "deepseek-v4.1-flash".into(),
                public_model: "evren/deepseek-v4.1-flash".into(),
                fallbacks: Vec::new(),
            }],
            via_gateway: true,
            hybrid,
            fallback: false,
        }
    }

    #[test]
    fn a_rendered_claude_file_reads_back_and_is_marked_as_turnouts() {
        let mut spec = AgentSpec::new(
            Harness::ClaudeCode,
            "deepseek-worker",
            "evren/deepseek-v4.1-flash",
        );
        spec.description = "Fixes: \"quoted\" # things".into();
        spec.parallel = Some(3);
        spec.tools = vec!["Read".into(), "Bash".into()];
        let text = spec.render(Harness::ClaudeCode);
        assert!(text.contains("isolation: worktree\n"), "{text}");
        assert!(text.contains("maxTurns: 40\n"), "{text}");
        assert!(text.contains("Up to 3 of these may run at once."), "{text}");
        assert!(text.contains(MANAGED_MARK));
        let (name, model) = parse(&text).unwrap();
        assert_eq!(name.as_deref(), Some("deepseek-worker"));
        assert_eq!(model.as_deref(), Some("evren/deepseek-v4.1-flash"));
    }

    #[test]
    fn an_opencode_worker_is_a_subagent_on_the_gateway_provider() {
        let route = route(false);
        let model =
            resolve_model(Harness::OpenCode, Some(&route), "evren::deepseek-v4.1-flash").unwrap();
        assert_eq!(model, "turnout/evren/deepseek-v4.1-flash");
        let spec = AgentSpec::new(Harness::OpenCode, "deepseek-worker", &model);
        let text = spec.render(Harness::OpenCode);
        assert!(text.contains("mode: subagent\n"), "{text}");
        assert!(text.contains("steps: 40\n"), "{text}");
        assert!(!text.contains("isolation"), "OpenCode has no worktrees: {text}");
        assert!(!text.contains("name:"), "OpenCode names agents after the file: {text}");
        // The reference itself is accepted back, e.g. when listing.
        assert!(resolve_model(Harness::OpenCode, Some(&route), &model).is_ok());
        // Claude aliases mean nothing to OpenCode.
        assert!(resolve_model(Harness::OpenCode, Some(&route), "opus").is_err());
        // A direct (non-gateway) route is spelled with the provider's own id.
        let mut direct = route.clone();
        direct.via_gateway = false;
        assert_eq!(
            resolve_model(Harness::OpenCode, Some(&direct), "evren::deepseek-v4.1-flash").unwrap(),
            "evren/deepseek-v4.1-flash"
        );
    }

    #[test]
    fn only_models_the_route_serves_are_accepted() {
        let hybrid = route(true);
        let claude = Harness::ClaudeCode;
        assert_eq!(
            resolve_model(claude, Some(&hybrid), "evren::deepseek-v4.1-flash").unwrap(),
            "evren/deepseek-v4.1-flash"
        );
        assert_eq!(
            resolve_model(claude, Some(&hybrid), "evren/deepseek-v4.1-flash").unwrap(),
            "evren/deepseek-v4.1-flash"
        );
        assert_eq!(
            resolve_model(claude, Some(&hybrid), "anthropic::claude-opus-5-5").unwrap(),
            "claude-opus-5-5"
        );
        assert_eq!(resolve_model(claude, Some(&hybrid), "opus").unwrap(), "opus");
        assert!(resolve_model(claude, Some(&hybrid), "openai::gpt-6.1-sol").is_err());
        // A plain gateway route cannot reach Claude's own models.
        assert!(resolve_model(claude, Some(&route(false)), "opus").is_err());
        // No Turnout route: Claude Code talks to Anthropic directly.
        assert!(resolve_model(claude, None, "claude-sonnet-5-5").is_ok());
        assert!(resolve_model(claude, None, "evren::deepseek-v4.1-flash").is_err());
    }

    #[test]
    fn names_are_simple_identifiers() {
        assert!(valid_name("deepseek-worker"));
        assert!(!valid_name("Deepseek"));
        assert!(!valid_name("../x"));
        assert!(!valid_name("-x"));
    }
}
