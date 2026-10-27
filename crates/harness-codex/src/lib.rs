//! Codex adapter. Codex is configured through `~/.codex/config.toml`
//! (`$CODEX_HOME/config.toml` when set); the CLI and the VS Code extension
//! read the same file.

use std::path::PathBuf;

use relay_config::kit::{self, Edit};
use relay_config::{BackupStore, ConfigPath};
use relay_core::format::ConfigFormat;
use relay_core::{
    BackupId, ChangePlan, ConfigSnapshot, HarnessAdapter, HarnessId, Installation, ProtocolDialect,
    RelayError, Result, Route, RouteContext, Surface, Validation, Version,
};
use serde_json::json;

/// Provider ids Codex defines itself; a custom table with these names is not
/// written.
const BUILTIN_PROVIDERS: [&str; 3] = ["openai", "ollama", "lmstudio"];

/// File name of the catalog Turnout generates for Codex's model picker.
pub const MODEL_CATALOG_FILE: &str = "codex-model-catalog.json";

/// Route profile marking a hybrid route: Codex's built-in models stay in the
/// picker and the custom models are added after them.
pub const HYBRID_PROFILE: &str = "hybrid";

/// Codex never lists a custom provider's `/v1/models` in its picker: the
/// picker is the catalog compiled into the binary (it does not read the
/// provider's `model_catalog_url` either). `model_catalog_json` replaces that
/// catalog wholesale, so Turnout writes the routed models there and Codex
/// shows exactly them.
///
/// Every field below is mandatory in Codex 0.16x; the rest of the entry takes
/// Codex's own defaults, so values are deliberately minimal.
fn model_catalog(
    models: &[String],
    default_model: &str,
    levels: &serde_json::Value,
    default_level: &str,
) -> String {
    let entries = custom_entries(models, default_model, 0, levels, default_level);
    serde_json::to_string_pretty(&json!({"models": entries}))
        .expect("model catalog is plain JSON")
}

/// Reasoning levels every Codex build understood before `xhigh`/`max` were
/// added. Used when the installed Codex's own catalog cannot be read.
fn fallback_levels() -> serde_json::Value {
    json!([
        {"effort": "low", "description": "Fast responses with lighter reasoning"},
        {"effort": "medium", "description": "Balances speed and reasoning depth"},
        {"effort": "high", "description": "Greater reasoning depth"},
    ])
}

/// The fullest reasoning list the installed Codex itself advertises.
///
/// Codex validates `supported_reasoning_levels` against its compiled-in
/// `ReasoningEffort` enum, so a level it does not know is rejected; the safe
/// set is therefore the one its own catalog uses. Entries differ (`xhigh` and
/// `max` only appear on some models), so the longest list wins.
pub fn builtin_levels(catalog: &serde_json::Value) -> Option<serde_json::Value> {
    catalog["models"]
        .as_array()?
        .iter()
        .filter_map(|model| model["supported_reasoning_levels"].as_array())
        .max_by_key(|levels| levels.len())
        .filter(|levels| !levels.is_empty())
        .map(|levels| serde_json::Value::Array(levels.clone()))
}

/// Hybrid catalog: Codex's built-in entries unchanged (they carry the real
/// instructions and reasoning levels), then the custom models after them.
fn hybrid_catalog(builtin: &serde_json::Value, models: &[String], default_model: &str) -> String {
    let official = builtin["models"].as_array().cloned().unwrap_or_default();
    let slugs = builtin_slugs(builtin);
    let after = official
        .iter()
        .filter_map(|m| m["priority"].as_i64())
        .max()
        .map_or(0, |p| p + 1);
    let custom = models
        .iter()
        .filter(|model| !slugs.contains(model))
        .cloned()
        .collect::<Vec<_>>();
    let mut entries = official;
    let levels = builtin_levels(builtin).unwrap_or_else(fallback_levels);
    let default_level = builtin["models"]
        .as_array()
        .and_then(|models| {
            models
                .iter()
                .find_map(|m| m["default_reasoning_level"].as_str())
        })
        .unwrap_or("medium");
    entries.extend(custom_entries(
        &custom,
        default_model,
        after,
        &levels,
        default_level,
    ));
    serde_json::to_string_pretty(&json!({"models": entries}))
        .expect("model catalog is plain JSON")
}

/// Slugs of a catalog's entries, hidden ones included.
pub fn builtin_slugs(catalog: &serde_json::Value) -> Vec<String> {
    catalog["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m["slug"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn custom_entries(
    models: &[String],
    default_model: &str,
    first_priority: i64,
    levels: &serde_json::Value,
    default_level: &str,
) -> Vec<serde_json::Value> {
    models
        .iter()
        .enumerate()
        .map(|(index, model)| {
            json!({
                "slug": model,
                "display_name": model,
                "description": if model == default_model {
                    "Turnout route (default)"
                } else {
                    "Turnout route"
                },
                "supported_reasoning_levels": levels,
                "default_reasoning_level": default_level,
                "shell_type": "unified_exec",
                "visibility": "list",
                "supported_in_api": true,
                "priority": first_priority + index as i64,
                "support_verbosity": false,
                "truncation_policy": {"mode": "tokens", "limit": 10000},
                "experimental_supported_tools": [],
                "base_instructions": "You are Codex, an agent working with the user in their workspace until the task is complete. Use the shell and apply_patch tools as needed and be concise.",
            })
        })
        .collect()
}

pub struct CodexAdapter {
    id: HarnessId,
    config: PathBuf,
    backups: BackupStore,
    /// Built-in catalog to use instead of asking the Codex binary (tests).
    builtin: Option<serde_json::Value>,
}

impl CodexAdapter {
    pub fn new(backups: BackupStore) -> Self {
        let dir = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
            .unwrap_or_else(|| PathBuf::from(".codex"));
        Self::with_config(dir.join("config.toml"), backups)
    }

    /// The generated model catalog that `model_catalog_json` points at: it
    /// lives next to the config so it moves with `$CODEX_HOME`.
    pub fn model_catalog_path(&self) -> PathBuf {
        self.config
            .parent()
            .map(|dir| dir.join(MODEL_CATALOG_FILE))
            .unwrap_or_else(|| PathBuf::from(MODEL_CATALOG_FILE))
    }

    /// Use an explicit config file (manual location chosen by the user).
    pub fn with_config(config: PathBuf, backups: BackupStore) -> Self {
        CodexAdapter {
            id: HarnessId::new(HarnessId::CODEX),
            config,
            backups,
            builtin: None,
        }
    }

    /// Use a fixed built-in catalog instead of the installed Codex's.
    pub fn with_builtin_catalog(mut self, catalog: serde_json::Value) -> Self {
        self.builtin = Some(catalog);
        self
    }

    /// The catalog compiled into the installed Codex. `codex debug models`
    /// prints the active catalog, which `model_catalog_json` may already
    /// override, so it runs against an empty `CODEX_HOME`: no config, no
    /// login, nothing on disk changes.
    pub fn builtin_catalog(&self) -> Result<serde_json::Value> {
        if let Some(catalog) = &self.builtin {
            return Ok(catalog.clone());
        }
        let binary = self
            .detect_installations()
            .into_iter()
            .find_map(|i| i.binary)
            .ok_or_else(|| {
                RelayError::Validation(
                    "keeping Codex's original model list needs the Codex binary (CLI or VS Code extension)".into(),
                )
            })?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let home = std::env::temp_dir().join(format!(
            "turnout-codex-catalog-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&home).map_err(|e| RelayError::io(&home, e))?;
        let output = kit::process(&binary)
            .args(["debug", "models"])
            .env("CODEX_HOME", &home)
            .stdin(std::process::Stdio::null())
            .output();
        let _ = std::fs::remove_dir_all(&home);
        let output = output.map_err(|e| RelayError::io(&binary, e))?;
        if !output.status.success() {
            return Err(RelayError::Validation(format!(
                "`codex debug models` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let catalog: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| RelayError::Validation(format!("unreadable Codex model catalog: {e}")))?;
        if builtin_slugs(&catalog).is_empty() {
            return Err(RelayError::Validation(
                "the installed Codex reported an empty model catalog".into(),
            ));
        }
        Ok(catalog)
    }
}

impl HarnessAdapter for CodexAdapter {
    fn id(&self) -> &HarnessId {
        &self.id
    }

    fn config_paths(&self, _surface: Surface) -> Vec<PathBuf> {
        vec![self.config.clone()]
    }

    fn detect_installations(&self) -> Vec<Installation> {
        kit::installations(
            &self.id,
            &["codex"],
            &kit::extension_binaries("openai.chatgpt-", "codex"),
            &self.config,
        )
    }

    fn detect_version(&self) -> Option<Version> {
        self.detect_installations()
            .into_iter()
            .find_map(|i| i.version)
    }

    fn read_config(&self) -> Result<ConfigSnapshot> {
        Ok(ConfigSnapshot {
            files: vec![kit::snapshot_file(&self.config, ConfigFormat::Toml)?],
        })
    }

    fn plan_changes(&self, route: &Route, ctx: &RouteContext) -> Result<ChangePlan> {
        let provider = &ctx.provider;
        // Codex 0.16x rejects `wire_api = "chat"`: it only speaks the Responses
        // API. Providers that offer anything else must go through the gateway,
        // which serves /v1/responses on top of any upstream.
        if provider.dialect != ProtocolDialect::OpenAiResponses && !ctx.via_gateway {
            return Err(RelayError::Validation(format!(
                "Codex only speaks the OpenAI Responses API, but `{}` offers {}; route it through the Turnout gateway (--gateway) or mark the provider `openai-responses`",
                provider.id,
                provider.dialect.as_str()
            )));
        }
        let wire_api = "responses";
        let (base_url, env_key) = if ctx.via_gateway {
            let url = ctx.gateway_url.as_deref().ok_or_else(|| {
                RelayError::Validation(
                    "gateway route requested but the gateway is not running".into(),
                )
            })?;
            (format!("{}/v1", url.trim_end_matches('/')), None)
        } else {
            (
                provider.base_url.trim_end_matches('/').to_string(),
                Some(provider.api_key_env.clone()),
            )
        };

        let id = provider.id.as_str();
        let mut edits = vec![
            Edit::set("model", json!(ctx.model)),
            Edit::set("model_provider", json!(id)),
        ];
        // Drop the gateway table written by the pre-rename release. The old
        // `model_provider = "relay"` is replaced above; leaving the table
        // behind would keep a dead provider in the Codex picker.
        let legacy = relay_core::brand::LEGACY_GATEWAY_PROVIDER_ID;
        let legacy_path = ConfigPath::new(["model_providers", legacy]);
        let mut removed_legacy = false;
        if id != legacy {
            let doc = kit::read_document(&self.config, ConfigFormat::Toml)?;
            if doc.get(&legacy_path)?.is_some() {
                edits.push(Edit::remove(legacy_path));
                removed_legacy = true;
            }
        }
        let mut notes = vec!["Codex CLI and the VS Code extension share this file.".to_string()];
        if ctx.models.len() > 1 {
            notes.push(format!(
                "The provider catalog exposes {} enabled model(s); `{}` is the default.",
                ctx.models.len(),
                ctx.model
            ));
        }
        if let Some(env_key) = &env_key {
            notes.push(format!(
                "The API key is read from ${env_key} at launch and is never written here."
            ));
        }
        if BUILTIN_PROVIDERS.contains(&id) && !ctx.via_gateway {
            notes.push(format!(
                "`{id}` is a built-in Codex provider; no custom table was written."
            ));
        } else {
            let table = |key: &str| ConfigPath::new(["model_providers", id, key]);
            let auth = |key: &str| ConfigPath::new(["model_providers", id, "auth", key]);
            edits.push(Edit::Set(table("name"), json!(provider.label)));
            edits.push(Edit::Set(table("base_url"), json!(base_url)));
            edits.push(Edit::Set(table("wire_api"), json!(wire_api)));
            if ctx.via_gateway {
                let helper = ctx.gateway_token_command.as_deref().ok_or_else(|| {
                    RelayError::Validation(
                        "VS Code gateway route requested but no Turnout token helper was provided"
                            .into(),
                    )
                })?;
                edits.push(Edit::remove(table("env_key")));
                edits.push(Edit::Set(auth("command"), json!(helper.to_string_lossy())));
                edits.push(Edit::Set(auth("args"), json!(["gateway", "token"])));
                edits.push(Edit::Set(auth("refresh_interval_ms"), json!(0)));
                edits.push(Edit::Set(auth("timeout_ms"), json!(5000)));
                notes.push(
                    "Codex obtains the loopback gateway token with its command-backed auth provider; no token is stored in config."
                        .to_string(),
                );
            } else if let Some(env_key) = env_key {
                edits.push(Edit::Set(table("env_key"), json!(env_key)));
                for key in ["command", "args", "refresh_interval_ms", "timeout_ms"] {
                    edits.push(Edit::remove(auth(key)));
                }
            }
        }

        if removed_legacy {
            notes.push(format!(
                "Removed the old `[model_providers.{legacy}]` table; the gateway route is `{id}`."
            ));
        }

        // Codex's picker ignores the provider's `/v1/models`; point it at the
        // catalog Turnout generates so it lists exactly the routed models.
        let catalog_path = self.model_catalog_path();
        let mut catalog_change = None;
        if ctx.via_gateway {
            let hybrid = route.profile.as_deref() == Some(HYBRID_PROFILE);
            // Levels come from the installed Codex when it can be asked, so the
            // picker offers exactly what this build accepts (`xhigh`, `max`, …).
            // Reading it is a subprocess, and a missing binary is not an error
            // for a route that does not need it.
            let builtin = self.builtin_catalog().ok();
            let catalog = if hybrid {
                hybrid_catalog(
                    &builtin.ok_or_else(|| {
                        RelayError::Validation(
                            "keeping Codex's original model list needs the Codex binary (CLI or VS Code extension)".into(),
                        )
                    })?,
                    &ctx.models,
                    &ctx.model,
                )
            } else {
                let levels = builtin
                    .as_ref()
                    .and_then(builtin_levels)
                    .unwrap_or_else(fallback_levels);
                model_catalog(&ctx.models, &ctx.model, &levels, "medium")
            };
            if hybrid {
                notes.push(
                    "Hybrid: Codex's own model list is kept and the custom models are added after it."
                        .to_string(),
                );
            }
            edits.push(Edit::set(
                "model_catalog_json",
                json!(catalog_path.to_string_lossy()),
            ));
            let before = catalog_path
                .exists()
                .then(|| std::fs::read_to_string(&catalog_path).ok())
                .flatten();
            catalog_change = Some(relay_core::PlannedFileChange {
                path: catalog_path.clone(),
                format: ConfigFormat::Json,
                before,
                after: catalog,
                notes: vec![
                    "The Codex model picker reads this catalog instead of the gateway's /v1/models."
                        .to_string(),
                ],
            });
            notes.push(format!(
                "Codex's picker shows the {} routed model(s) from {}.",
                ctx.models.len(),
                catalog_path.display()
            ));
        } else {
            edits.push(Edit::remove(ConfigPath::new(["model_catalog_json"])));
        }
        let change = kit::plan_file(&self.config, ConfigFormat::Toml, &edits, notes)?;
        let mut changes = vec![change];
        changes.extend(catalog_change);
        Ok(ChangePlan::new(self.id.clone(), route.clone(), changes))
    }

    fn validate(&self, plan: &ChangePlan) -> Result<Validation> {
        Ok(kit::validate_plan(plan))
    }

    fn apply(&self, plan: &ChangePlan) -> Result<BackupId> {
        kit::apply_plan(plan, &self.backups)
    }

    fn rollback(&self, backup: &BackupId) -> Result<()> {
        self.backups.restore(backup).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_config::Document;
    use relay_core::ProviderConfig;

    const EXISTING: &str = "# my codex\nmodel = \"gpt-5\"\napproval_policy = \"on-request\"\n\n[projects.\"/work/app\"]\ntrust_level = \"trusted\"\n";

    fn setup() -> (tempfile::TempDir, CodexAdapter, Route) {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, EXISTING).unwrap();
        let adapter = CodexAdapter::with_config(config, BackupStore::at(dir.path().join("bk")));
        let route = Route {
            harness: adapter.id.clone(),
            surfaces: vec![Surface::Cli, Surface::VsCode],
            provider: "opencode-go".into(),
            model: "glm-5.3".into(),
            models: vec!["glm-5.3".into()],
            profile: None,
        };
        (dir, adapter, route)
    }

    fn ctx(dialect_anthropic: bool, via_gateway: bool) -> RouteContext {
        let provider = if dialect_anthropic {
            ProviderConfig::anthropic("claudeish", "https://a.test/v1")
        } else {
            let mut p = ProviderConfig::openai_compatible("opencode-go", "https://x.test/v1/");
            p.dialect = ProtocolDialect::OpenAiResponses;
            p
        };
        RouteContext {
            provider,
            model: "glm-5.3".into(),
            models: vec!["glm-5.3".into()],
            surfaces: vec![Surface::Cli],
            via_gateway,
            gateway_url: via_gateway.then(|| "http://127.0.0.1:4545".to_string()),
            gateway_token_command: via_gateway.then(|| PathBuf::from("/turnout")),
            gateway_token_file: None,
        }
    }

    #[test]
    fn plans_applies_and_rolls_back_without_losing_user_content() {
        let (_dir, adapter, route) = setup();
        let plan = adapter.plan_changes(&route, &ctx(false, false)).unwrap();
        assert!(adapter.validate(&plan).unwrap().ok);
        let backup = adapter.apply(&plan).unwrap();

        let text = std::fs::read_to_string(&adapter.config).unwrap();
        assert!(text.contains("# my codex") && text.contains("approval_policy"));
        let v = Document::parse(ConfigFormat::Toml, text)
            .unwrap()
            .to_value()
            .unwrap();
        assert_eq!(v["model"], "glm-5.3");
        assert_eq!(v["model_provider"], "opencode-go");
        assert_eq!(
            v["model_providers"]["opencode-go"]["base_url"],
            "https://x.test/v1"
        );
        assert_eq!(
            v["model_providers"]["opencode-go"]["env_key"],
            "OPENCODE_GO_API_KEY"
        );
        assert_eq!(v["model_providers"]["opencode-go"]["wire_api"], "responses");
        assert_eq!(v["projects"]["/work/app"]["trust_level"], "trusted");
        assert!(v["projects"]["/work/app"].get("model_provider").is_none());

        adapter.rollback(&backup).unwrap();
        assert_eq!(std::fs::read_to_string(&adapter.config).unwrap(), EXISTING);
    }

    #[test]
    fn gateway_routes_write_the_picker_catalog() {
        let (_dir, adapter, route) = setup();
        let mut context = ctx(false, true);
        context.models = vec!["openai/gpt-6.1-sol".into(), "evren/glm-5.3".into()];
        context.model = "openai/gpt-6.1-sol".into();
        let route = Route {
            model: context.model.clone(),
            models: context.models.clone(),
            ..route
        };

        let plan = adapter.plan_changes(&route, &context).unwrap();
        assert!(adapter.validate(&plan).unwrap().ok);
        // config.toml plus the generated catalog.
        assert_eq!(plan.changes.len(), 2);
        let catalog_change = plan
            .changes
            .iter()
            .find(|c| c.format == ConfigFormat::Json)
            .expect("catalog change");
        assert_eq!(catalog_change.path, adapter.model_catalog_path());
        let catalog: serde_json::Value = serde_json::from_str(&catalog_change.after).unwrap();
        let slugs: Vec<&str> = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["slug"].as_str().unwrap())
            .collect();
        assert_eq!(slugs, ["openai/gpt-6.1-sol", "evren/glm-5.3"]);
        for model in catalog["models"].as_array().unwrap() {
            for field in [
                "display_name",
                "supported_reasoning_levels",
                "shell_type",
                "visibility",
                "supported_in_api",
                "priority",
                "support_verbosity",
                "truncation_policy",
                "base_instructions",
            ] {
                assert!(model.get(field).is_some(), "missing {field}");
            }
        }

        let _backup = adapter.apply(&plan).unwrap();
        let text = std::fs::read_to_string(&adapter.config).unwrap();
        let v = Document::parse(ConfigFormat::Toml, text)
            .unwrap()
            .to_value()
            .unwrap();
        assert_eq!(
            v["model_catalog_json"],
            json!(adapter.model_catalog_path().to_string_lossy())
        );
        assert!(adapter.model_catalog_path().is_file());
    }

    #[test]
    fn hybrid_keeps_the_built_in_list_and_adds_custom_models_after_it() {
        let (_dir, adapter, route) = setup();
        let builtin = json!({"models": [
            {"slug": "gpt-6.1-sol", "display_name": "GPT-6.1-Sol", "priority": 1, "base_instructions": "real"},
            {"slug": "gpt-6-astra", "display_name": "GPT-6-Astra", "priority": 2, "base_instructions": "real"},
            {"slug": "codex-auto-review", "visibility": "hide", "priority": 43, "base_instructions": "real"},
        ]});
        let adapter = adapter.with_builtin_catalog(builtin);
        let mut context = ctx(false, true);
        context.models = vec![
            "gpt-6.1-sol".into(),
            "gpt-6-astra".into(),
            "codex-auto-review".into(),
            "evren/glm-5.3".into(),
        ];
        context.model = "gpt-6.1-sol".into();
        let route = Route {
            model: context.model.clone(),
            models: context.models.clone(),
            profile: Some(HYBRID_PROFILE.into()),
            ..route
        };

        let plan = adapter.plan_changes(&route, &context).unwrap();
        let catalog_change = plan
            .changes
            .iter()
            .find(|c| c.format == ConfigFormat::Json)
            .unwrap();
        let catalog: serde_json::Value = serde_json::from_str(&catalog_change.after).unwrap();
        let models = catalog["models"].as_array().unwrap();
        let slugs: Vec<&str> = models.iter().map(|m| m["slug"].as_str().unwrap()).collect();
        assert_eq!(
            slugs,
            ["gpt-6.1-sol", "gpt-6-astra", "codex-auto-review", "evren/glm-5.3"]
        );
        // Built-in entries are copied untouched, hidden ones stay hidden.
        assert_eq!(models[0]["display_name"], "GPT-6.1-Sol");
        assert_eq!(models[0]["base_instructions"], "real");
        assert_eq!(models[2]["visibility"], "hide");
        // The custom model sorts after every built-in one.
        assert_eq!(models[3]["priority"], 44);
        assert_eq!(models[3]["visibility"], "list");
    }

    #[test]
    fn the_picker_offers_the_levels_the_installed_codex_knows() {
        let (_dir, adapter, route) = setup();
        let levels = json!([
            {"effort": "low", "description": "low"},
            {"effort": "medium", "description": "medium"},
            {"effort": "high", "description": "high"},
            {"effort": "xhigh", "description": "extra high"},
            {"effort": "max", "description": "max"},
        ]);
        let builtin = json!({"models": [
            {"slug": "gpt-6.1-sol", "priority": 1, "base_instructions": "real",
             "supported_reasoning_levels": levels},
            {"slug": "gpt-6-mini", "priority": 2, "base_instructions": "real",
             "supported_reasoning_levels": [{"effort": "low", "description": "low"},
                                             {"effort": "medium", "description": "medium"}]},
        ]});
        let adapter = adapter.with_builtin_catalog(builtin);
        let mut context = ctx(false, true);
        context.models = vec!["evren/glm-5.3".into()];
        context.model = "evren/glm-5.3".into();
        let route = Route {
            model: context.model.clone(),
            models: context.models.clone(),
            ..route
        };

        let plan = adapter.plan_changes(&route, &context).unwrap();
        let catalog_change = plan
            .changes
            .iter()
            .find(|c| c.format == ConfigFormat::Json)
            .unwrap();
        let catalog: serde_json::Value = serde_json::from_str(&catalog_change.after).unwrap();
        // The longest list any built-in model advertises: `xhigh` and `max`
        // are only offered where Codex itself offers them.
        let efforts: Vec<&str> = catalog["models"][0]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["effort"].as_str().unwrap())
            .collect();
        assert_eq!(efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(catalog["models"][0]["default_reasoning_level"], "medium");
    }

    #[test]
    fn the_builtin_levels_prefer_the_longest_list_and_ignore_missing_ones() {
        let catalog = json!({"models": [
            {"slug": "a"},
            {"slug": "b", "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}]},
            {"slug": "c", "supported_reasoning_levels": [
                {"effort": "low"}, {"effort": "medium"}, {"effort": "high"}]},
        ]});
        let levels = builtin_levels(&catalog).unwrap();
        assert_eq!(levels.as_array().unwrap().len(), 3);
        assert_eq!(levels[2]["effort"], "high");
        // No model advertises any: the caller must fall back, not write `[]`.
        assert!(builtin_levels(&json!({"models": [{"slug": "a"}]})).is_none());
        assert!(builtin_levels(&json!({"models": []})).is_none());
        assert_eq!(fallback_levels().as_array().unwrap().len(), 3);
    }

    #[test]
    fn direct_routes_clear_the_picker_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            "model = \"glm-5.3\"\nmodel_catalog_json = \"/tmp/old.json\"\n",
        )
        .unwrap();
        let adapter =
            CodexAdapter::with_config(config.clone(), BackupStore::at(dir.path().join("bk")));
        let route = Route {
            harness: adapter.id.clone(),
            surfaces: vec![Surface::Cli],
            provider: "opencode-go".into(),
            model: "glm-5.3".into(),
            models: vec!["glm-5.3".into()],
            profile: None,
        };

        let plan = adapter.plan_changes(&route, &ctx(false, false)).unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert!(!plan.changes[0].after.contains("model_catalog_json"));
    }

    #[test]
    fn chat_only_providers_are_never_written_as_wire_api_chat() {
        let (_dir, adapter, route) = setup();
        let chat_only = RouteContext {
            provider: ProviderConfig::openai_compatible("opencode-go", "https://x.test/v1"),
            model: "glm-5.3".into(),
            models: vec!["glm-5.3".into()],
            surfaces: vec![Surface::Cli],
            via_gateway: false,
            gateway_url: None,
            gateway_token_command: None,
            gateway_token_file: None,
        };
        let err = adapter
            .plan_changes(&route, &chat_only)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Responses") && err.contains("gateway"),
            "{err}"
        );

        let via_gateway = RouteContext {
            via_gateway: true,
            gateway_url: Some("http://127.0.0.1:47821".into()),
            gateway_token_command: Some(PathBuf::from("/turnout")),
            ..chat_only
        };
        let plan = adapter.plan_changes(&route, &via_gateway).unwrap();
        let after = &plan.changes[0].after;
        assert!(
            after.contains("wire_api = \"responses\"") && !after.contains("\"chat\""),
            "{after}"
        );
        assert!(after.contains("http://127.0.0.1:47821/v1"));
    }

    #[test]
    fn replaces_the_pre_rename_gateway_table_instead_of_stacking_one() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            "# keep me\nmodel = \"glm-5.3\"\nmodel_provider = \"relay\"\n\n[model_providers.relay]\nname = \"Relay\"\nbase_url = \"http://127.0.0.1:47822/v1\"\nwire_api = \"responses\"\n",
        )
        .unwrap();
        let adapter =
            CodexAdapter::with_config(config.clone(), BackupStore::at(dir.path().join("bk")));
        let mut context = ctx(false, true);
        let mut provider = ProviderConfig::openai_compatible("turnout", "http://127.0.0.1:4545/v1");
        provider.dialect = ProtocolDialect::OpenAiResponses;
        context.provider = provider;
        let route = Route {
            harness: HarnessId::new("codex"),
            surfaces: vec![Surface::Cli],
            provider: "turnout".into(),
            model: "glm-5.3".into(),
            models: vec!["glm-5.3".into()],
            profile: None,
        };
        adapter
            .apply(&adapter.plan_changes(&route, &context).unwrap())
            .unwrap();

        let text = std::fs::read_to_string(&config).unwrap();
        let value = Document::parse(ConfigFormat::Toml, text.clone())
            .unwrap()
            .to_value()
            .unwrap();
        assert!(
            value["model_providers"].get("relay").is_none(),
            "old table must be removed\n{text}"
        );
        assert_eq!(value["model_provider"], "turnout");
        assert_eq!(value["model_providers"]["turnout"]["name"], "turnout");
        assert!(text.contains("# keep me"), "{text}");
    }

    #[test]
    fn anthropic_providers_need_the_gateway() {
        let (_dir, adapter, route) = setup();
        assert!(matches!(
            adapter.plan_changes(&route, &ctx(true, false)),
            Err(RelayError::Validation(_))
        ));
        let plan = adapter.plan_changes(&route, &ctx(true, true)).unwrap();
        let after = &plan.changes[0].after;
        assert!(after.contains("http://127.0.0.1:4545/v1"));
        assert!(after.contains("command = \"/turnout\""));
        assert!(after.contains("args = [\"gateway\", \"token\"]"));
        assert!(!after.contains(relay_core::GATEWAY_TOKEN_ENV));
    }
}
