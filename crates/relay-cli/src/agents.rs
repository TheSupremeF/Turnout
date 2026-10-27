//! `turnout agents`: worker agents a harness's main model hands work to.
//!
//! Claude Code reads subagent definitions from `.claude/agents/<name>.md` in a
//! project, or `~/.claude/agents/<name>.md` for every project. Each file names
//! the model its subagent runs on; the main session, running the route's
//! default model, is the manager. So "Opus manages three DeepSeek workers" is
//! a route whose default is Opus plus one worker file on DeepSeek.
//!
//! Turnout only writes workers on models the harness can actually reach: a
//! worker on a model the gateway does not serve would fail on its first
//! request, in the middle of someone's task. Changes go through the same
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

const DEFAULT_DESCRIPTION: &str = "Implements one well-scoped coding task inside its own git worktree and runs the checks it is given. Use for delegated implementation work.";
const DEFAULT_INSTRUCTIONS: &str =
    "You are a worker. Implement only the task you are given, inside the current
working directory (your own git worktree). Do not touch files outside it.

Run the checks the task names. In your final report list, separately:
1. Files you changed.
2. Every command you ran for verification, with its exit code.
3. Anything you could not finish or verify.

Never claim a check passed unless you ran it and saw exit code 0.
Never run git push, merge, tag, release or publish anything.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Scope {
    /// `<project>/.claude/agents`: only this project's sessions see it
    Project,
    /// `~/.claude/agents`: every project's sessions see it
    User,
}

/// What a worker file says.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentSpec {
    pub name: String,
    /// Model id as the harness sends it (the route's public id, or a Claude id).
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
    pub fn new(name: &str, model: &str) -> Self {
        Self {
            name: name.to_string(),
            model: model.to_string(),
            description: DEFAULT_DESCRIPTION.to_string(),
            max_turns: Some(40),
            worktree: true,
            tools: Vec::new(),
            parallel: None,
            instructions: DEFAULT_INSTRUCTIONS.to_string(),
        }
    }

    /// The `.md` file Claude Code reads. Strings are written JSON-quoted,
    /// which is valid YAML and survives colons, quotes and `#`.
    pub fn render(&self) -> String {
        let quote = |value: &str| serde_json::to_string(value).expect("strings serialize");
        let mut description = self.description.clone();
        if let Some(parallel) = self.parallel {
            description.push_str(&format!(" Up to {parallel} of these may run at once."));
        }
        let mut text = format!(
            "---\nname: {}\ndescription: {}\nmodel: {}\n",
            self.name,
            quote(&description),
            quote(&self.model)
        );
        if !self.tools.is_empty() {
            text.push_str(&format!("tools: {}\n", self.tools.join(", ")));
        }
        if self.worktree {
            text.push_str("isolation: worktree\n");
        }
        if let Some(turns) = self.max_turns {
            text.push_str(&format!("maxTurns: {turns}\n"));
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

fn parse(text: &str) -> Option<(String, Option<String>)> {
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
    Some((name?, model))
}

fn claude_dir() -> Result<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .ok_or_else(|| anyhow!("cannot locate the home directory"))
}

pub fn agents_dir(scope: Scope, project: Option<&Path>) -> Result<PathBuf> {
    Ok(match scope {
        Scope::User => claude_dir()?.join("agents"),
        Scope::Project => {
            let root = match project {
                Some(path) => path.to_path_buf(),
                None => std::env::current_dir().context("cannot read the current directory")?,
            };
            root.join(".claude").join("agents")
        }
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

/// Claude's own model ids and aliases, which Claude Code resolves itself.
fn is_claude_model(model: &str) -> bool {
    matches!(model, "opus" | "sonnet" | "haiku" | "inherit")
        || (model.starts_with("claude-") && !model.contains('/'))
}

/// Turn `provider::model` or a bare id into the id the harness sends, and
/// refuse models the harness cannot reach with its current route.
pub fn resolve_model(route: Option<&SavedRoute>, value: &str) -> Result<String> {
    let value = value.trim();
    if let Some((provider, model)) = value.split_once("::") {
        if provider == "anthropic" && is_claude_model(model) {
            return resolve_model(route, model);
        }
        let public = route
            .map(SavedRoute::enabled_targets)
            .unwrap_or_default()
            .into_iter()
            .find(|target| target.provider == provider && target.model == model)
            .map(|target| target.public_model);
        return public.ok_or_else(|| not_routable(value));
    }
    if is_claude_model(value) {
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
    let known = route
        .map(SavedRoute::enabled_targets)
        .unwrap_or_default()
        .into_iter()
        .any(|target| target.public_model == value);
    if known {
        Ok(value.to_string())
    } else {
        Err(not_routable(value))
    }
}

fn not_routable(value: &str) -> anyhow::Error {
    anyhow!(
        "`{value}` is not served by the claude-code route; add it first, e.g. \
         `turnout plan-targets claude-code --hybrid --target {value} ...`, then `turnout apply`"
    )
}

fn routable(route: Option<&SavedRoute>, model: &str) -> bool {
    resolve_model(route, model).is_ok()
}

pub fn list(home: &Home, project: Option<&Path>) -> Result<Vec<AgentFile>> {
    let route = home.state()?.routes.get(HarnessId::CLAUDE_CODE).cloned();
    let mut found = Vec::new();
    for (scope, label) in [(Scope::Project, "project"), (Scope::User, "user")] {
        let dir = agents_dir(scope, project)?;
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
            let routable = match &model {
                Some(model) => routable(route.as_ref(), model),
                // No model: the subagent inherits the manager's.
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

fn plan_file(home: &Home, harness: &str) -> PathBuf {
    home.root()
        .join("plans")
        .join(format!("agents-{harness}.json"))
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
pub fn plan(home: &Home, path: PathBuf, spec: Option<&AgentSpec>) -> Result<AgentPlan> {
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
    let after = spec.map(AgentSpec::render);
    if before.is_none() && after.is_none() {
        bail!("{} does not exist", path.display());
    }
    if let Some(text) = &after {
        if relay_config::kit::contains_secret_token(text) {
            bail!("the agent text looks like it contains an API key; keys must stay in the credential store");
        }
    }
    let plan = AgentPlan {
        harness: HarnessId::CLAUDE_CODE.to_string(),
        changes: vec![AgentChange {
            path,
            before,
            after,
        }],
    };
    let file = plan_file(home, &plan.harness);
    relay_config::write_file(&file, &serde_json::to_string_pretty(&plan)?)?;
    Ok(plan)
}

/// Apply the saved plan: refuse a stale one, back up, write, and undo
/// everything if any write fails.
pub fn apply(home: &Home) -> Result<(BackupId, Vec<PathBuf>)> {
    let file = plan_file(home, HarnessId::CLAUDE_CODE);
    let text = std::fs::read_to_string(&file)
        .map_err(|_| anyhow!("no saved agent plan; run `turnout agents plan` first"))?;
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

/// Whether `id` was made by `agents apply`.
pub fn is_agents_backup(home: &Home, id: &BackupId) -> bool {
    home.backups()
        .join(id.to_string())
        .join(BACKUP_MARK)
        .exists()
}

/// The newest backup made by `agents apply`.
pub fn latest_backup(home: &Home) -> Result<Option<BackupId>> {
    Ok(BackupStore::at(home.backups())
        .list()?
        .into_iter()
        .map(|record| record.id)
        .find(|id| is_agents_backup(home, id)))
}

/// Reject names Claude Code would not address or that escape the directory.
pub fn check_name(name: &str) -> Result<()> {
    if valid_name(name) {
        Ok(())
    } else {
        bail!("agent name `{name}` must be lowercase letters, digits and dashes")
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
    fn a_rendered_file_reads_back_and_is_marked_as_turnouts() {
        let mut spec = AgentSpec::new("deepseek-worker", "evren/deepseek-v4.1-flash");
        spec.description = "Fixes: \"quoted\" # things".into();
        spec.parallel = Some(3);
        spec.tools = vec!["Read".into(), "Bash".into()];
        let text = spec.render();
        assert!(text.contains("isolation: worktree\n"), "{text}");
        assert!(text.contains("maxTurns: 40\n"), "{text}");
        assert!(text.contains("Up to 3 of these may run at once."), "{text}");
        assert!(text.contains(MANAGED_MARK));
        let (name, model) = parse(&text).unwrap();
        assert_eq!(name, "deepseek-worker");
        assert_eq!(model.as_deref(), Some("evren/deepseek-v4.1-flash"));
    }

    #[test]
    fn only_models_the_route_serves_are_accepted() {
        let hybrid = route(true);
        assert_eq!(
            resolve_model(Some(&hybrid), "evren::deepseek-v4.1-flash").unwrap(),
            "evren/deepseek-v4.1-flash"
        );
        assert_eq!(
            resolve_model(Some(&hybrid), "evren/deepseek-v4.1-flash").unwrap(),
            "evren/deepseek-v4.1-flash"
        );
        assert_eq!(
            resolve_model(Some(&hybrid), "anthropic::claude-opus-5-5").unwrap(),
            "claude-opus-5-5"
        );
        assert_eq!(resolve_model(Some(&hybrid), "opus").unwrap(), "opus");
        assert!(resolve_model(Some(&hybrid), "openai::gpt-6.1-sol").is_err());
        // A plain gateway route cannot reach Claude's own models.
        assert!(resolve_model(Some(&route(false)), "opus").is_err());
        // No Turnout route: Claude Code talks to Anthropic directly.
        assert!(resolve_model(None, "claude-sonnet-5-5").is_ok());
        assert!(resolve_model(None, "evren::deepseek-v4.1-flash").is_err());
    }

    #[test]
    fn names_are_simple_identifiers() {
        assert!(valid_name("deepseek-worker"));
        assert!(!valid_name("Deepseek"));
        assert!(!valid_name("../x"));
        assert!(!valid_name("-x"));
    }
}
