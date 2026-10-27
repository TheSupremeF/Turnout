//! Turnout's own data directory: providers, saved plans/routes, backups, vault.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use relay_core::{ChangePlan, ProviderConfig, ProviderId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTarget {
    pub provider: String,
    /// Model id sent to this provider.
    pub model: String,
    /// Model id exposed to the harness. Namespaced when a route spans providers.
    pub public_model: String,
    /// Public model ids this target may be rerouted to, in order, when its
    /// provider keeps failing. Overrides the route-wide `fallback` switch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedRoute {
    pub provider: String,
    /// Default model.
    pub model: String,
    /// Models exposed by the route. Empty in legacy state files.
    #[serde(default)]
    pub models: Vec<String>,
    /// Provider-aware model routing. Empty in legacy state files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<SavedTarget>,
    pub via_gateway: bool,
    /// Official Claude models stay on Claude's own login and are forwarded to
    /// Anthropic by the gateway; `targets` are only the added custom models.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hybrid: bool,
    /// Let the gateway reroute a target to the route's other models when its
    /// provider keeps failing. Off unless explicitly enabled: a request for one
    /// model must not be answered by another without the user asking for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
}

impl SavedRoute {
    pub fn enabled_models(&self) -> Vec<String> {
        if self.models.is_empty() {
            vec![self.model.clone()]
        } else {
            self.models.clone()
        }
    }

    pub fn enabled_targets(&self) -> Vec<SavedTarget> {
        if self.targets.is_empty() {
            self.enabled_models()
                .into_iter()
                .map(|model| SavedTarget {
                    provider: self.provider.clone(),
                    public_model: model.clone(),
                    model,
                    fallbacks: Vec::new(),
                })
                .collect()
        } else {
            self.targets.clone()
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct State {
    pub gateway_port: u16,
    /// The route applied to the harness's own config file.
    #[serde(default)]
    pub routes: BTreeMap<String, SavedRoute>,
    /// Launch-only routes that never touch the harness config file. Each one is
    /// injected into a single `relay run <harness> --profile <name>` process,
    /// so the config file can stay on the official provider.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, BTreeMap<String, SavedRoute>>,
}

impl Default for State {
    fn default() -> Self {
        State {
            gateway_port: 47821,
            routes: BTreeMap::new(),
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedPlan {
    via_gateway: bool,
    #[serde(default)]
    targets: Vec<SavedTarget>,
    /// Whether the gateway may reroute a model to the route's other models.
    #[serde(default)]
    fallback: bool,
    plan: ChangePlan,
}

#[derive(Clone)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// `$TURNOUT_HOME` (legacy `$RELAY_HOME` still works), else
    /// `<platform data dir>/Turnout`. An old `<data dir>/Relay` directory is
    /// moved once, before anything reads from it.
    pub fn locate() -> Result<Self> {
        let root = relay_core::brand::resolve_data_dir(|message| eprintln!("{message}"))
            .ok_or_else(|| anyhow!("cannot locate a data directory; set TURNOUT_HOME"))?;
        Ok(Home { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }
    pub fn gateway_token_file(&self) -> PathBuf {
        self.root.join("gateway.token")
    }
    pub fn gateway_log(&self) -> PathBuf {
        self.root.join("gateway.log")
    }
    pub fn credentials_file(&self) -> PathBuf {
        self.root.join("credentials.json")
    }
    pub fn vault(&self) -> PathBuf {
        self.root.join("vault.json")
    }
    /// Captured rate-limit headers, one file per provider.
    pub fn quota_dir(&self) -> PathBuf {
        self.root.join("quota")
    }

    /// Owner-only file holding the key that seals [`Home::vault`].
    ///
    /// A file, not an OS credential item: macOS ties a Keychain entry to the
    /// binary's code signature, so every rebuild would be asked for permission
    /// again — and the GUI spawns the CLI per request.
    pub fn vault_key(&self) -> PathBuf {
        self.root.join("vault.key")
    }
    fn providers_file(&self) -> PathBuf {
        self.root.join("providers.json")
    }
    fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }
    fn plan_file(&self, harness: &str) -> PathBuf {
        self.root.join("plans").join(format!("{harness}.json"))
    }

    fn read<T: for<'de> Deserialize<'de> + Default>(&self, path: &Path) -> Result<T> {
        if !path.exists() {
            return Ok(T::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", path.display()))
    }

    fn write<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        let text = serde_json::to_string_pretty(value)?;
        relay_config::write_file(path, &text)?;
        Ok(())
    }

    pub fn providers(&self) -> Result<Vec<ProviderConfig>> {
        self.read(&self.providers_file())
    }

    pub fn save_provider(&self, config: ProviderConfig) -> Result<()> {
        let mut all = self.providers()?;
        all.retain(|p| p.id != config.id);
        all.push(config);
        all.sort_by(|a, b| a.id.cmp(&b.id));
        self.write(&self.providers_file(), &all)
    }

    pub fn remove_provider(&self, id: &ProviderId) -> Result<()> {
        let mut all = self.providers()?;
        all.retain(|p| &p.id != id);
        self.write(&self.providers_file(), &all)
    }

    pub fn state(&self) -> Result<State> {
        self.read(&self.state_file())
    }

    pub fn save_route(&self, harness: &str, route: SavedRoute) -> Result<()> {
        let mut state = self.state()?;
        state.routes.insert(harness.to_string(), route);
        self.write(&self.state_file(), &state)
    }

    pub fn save_profile(&self, harness: &str, name: &str, route: SavedRoute) -> Result<()> {
        let mut state = self.state()?;
        let profiles = state.profiles.entry(harness.to_string()).or_default();
        profiles.insert(name.to_string(), route);
        self.write(&self.state_file(), &state)
    }

    /// A saved launch profile, else a route by that name, else the harness route
    /// when `name` is `None`.
    pub fn find_route(&self, harness: &str, name: Option<&str>) -> Result<SavedRoute> {
        let state = self.state()?;
        if let Some(name) = name {
            return state
                .profiles
                .get(harness)
                .and_then(|profiles| profiles.get(name))
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "no profile `{name}` for {harness}; apply one with `turnout plan-targets`"
                    )
                });
        }
        state.routes.get(harness).cloned().ok_or_else(|| {
            anyhow!("no saved route for {harness}; run `turnout plan-targets` and `turnout apply` first")
        })
    }

    pub fn remove_route(&self, harness: &str) -> Result<()> {
        let mut state = self.state()?;
        state.routes.remove(harness);
        self.write(&self.state_file(), &state)
    }

    /// Remembers the route a backup replaces, so a rollback can put the saved
    /// route back in step with the restored config files.
    pub fn save_route_before(
        &self,
        backup_id: &str,
        harness: &str,
        route: Option<&SavedRoute>,
    ) -> Result<()> {
        let dir = self.backups().join(backup_id);
        if !dir.is_dir() {
            return Ok(());
        }
        self.write(
            &dir.join("route-before.json"),
            &serde_json::json!({ "harness": harness, "route": route }),
        )
    }

    /// The `(harness, previous route)` stored by `save_route_before`, if any.
    /// Backups made before this existed return `None`.
    pub fn route_before(&self, backup_id: &str) -> Option<(String, Option<SavedRoute>)> {
        let text =
            std::fs::read_to_string(self.backups().join(backup_id).join("route-before.json"))
                .ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let harness = value["harness"].as_str()?.to_string();
        let route = serde_json::from_value(value["route"].clone()).ok();
        Some((harness, route))
    }

    pub fn save_plan(
        &self,
        plan: &ChangePlan,
        via_gateway: bool,
        targets: Vec<SavedTarget>,
        fallback: bool,
    ) -> Result<()> {
        self.write(
            &self.plan_file(plan.harness.as_str()),
            &SavedPlan {
                via_gateway,
                targets,
                fallback,
                plan: plan.clone(),
            },
        )
    }

    /// The saved plan for `harness`, or the most recently written one.
    #[allow(clippy::type_complexity)]
    pub fn load_plan(
        &self,
        harness: Option<&str>,
    ) -> Result<(ChangePlan, bool, Vec<SavedTarget>, bool)> {
        let path = match harness {
            Some(h) => self.plan_file(h),
            None => std::fs::read_dir(self.root.join("plans"))
                .map_err(|_| anyhow!("no saved plan; run `turnout plan-targets` first"))?
                .flatten()
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
                .map(|e| e.path())
                .ok_or_else(|| anyhow!("no saved plan; run `turnout plan-targets` first"))?,
        };
        if !path.exists() {
            return Err(anyhow!(
                "no saved plan for that harness; run `turnout plan-targets` first"
            ));
        }
        let saved: SavedPlan = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        Ok((saved.plan, saved.via_gateway, saved.targets, saved.fallback))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(model: &str) -> SavedRoute {
        SavedRoute {
            provider: "openai".into(),
            model: model.into(),
            models: vec![model.into()],
            targets: Vec::new(),
            via_gateway: false,
            hybrid: false,
            fallback: true,
        }
    }

    #[test]
    fn a_backup_remembers_the_route_it_replaced_so_rollback_can_restore_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home {
            root: dir.path().to_path_buf(),
        };
        std::fs::create_dir_all(home.backups().join("b1-aa")).unwrap();

        home.save_route_before("b1-aa", "codex", Some(&route("gpt-6.1-sol")))
            .unwrap();
        let (harness, before) = home.route_before("b1-aa").unwrap();
        assert_eq!(harness, "codex");
        assert_eq!(before.unwrap().model, "gpt-6.1-sol");

        // First ever apply: there was no route to go back to.
        std::fs::create_dir_all(home.backups().join("b2-bb")).unwrap();
        home.save_route_before("b2-bb", "codex", None).unwrap();
        assert!(home.route_before("b2-bb").unwrap().1.is_none());

        // Backups from before this existed are reported as unknown, not guessed.
        std::fs::create_dir_all(home.backups().join("b0-old")).unwrap();
        assert!(home.route_before("b0-old").is_none());

        home.save_route("codex", route("x")).unwrap();
        home.remove_route("codex").unwrap();
        assert!(home.state().unwrap().routes.is_empty());
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    #[test]
    fn a_route_never_falls_back_unless_asked() {
        // A request for one model must not be answered by another silently:
        // routes saved without the field (including older ones) stay strict.
        let route: SavedRoute = serde_json::from_str(
            r#"{"provider":"openai","model":"gpt-6.1-sol","models":[],"via_gateway":true}"#,
        )
        .unwrap();
        assert!(!route.fallback);
        let opted_in: SavedRoute = serde_json::from_str(
            r#"{"provider":"openai","model":"gpt-6.1-sol","models":[],"via_gateway":true,"fallback":true}"#,
        )
        .unwrap();
        assert!(opted_in.fallback);
    }

    #[test]
    fn fallback_can_be_turned_on_and_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home {
            root: dir.path().to_path_buf(),
        };
        let route = SavedRoute {
            provider: "openai".into(),
            model: "gpt-6.1-sol".into(),
            models: Vec::new(),
            targets: Vec::new(),
            via_gateway: true,
            hybrid: false,
            fallback: true,
        };
        home.save_route("codex", route).unwrap();
        let saved = home.state().unwrap().routes.remove("codex").unwrap();
        assert!(saved.fallback);
    }
}
