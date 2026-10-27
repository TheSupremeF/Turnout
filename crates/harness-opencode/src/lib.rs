//! OpenCode adapter. Config is `opencode.json` / `opencode.jsonc` under the XDG
//! config dir (`$OPENCODE_CONFIG` overrides the file). Providers are declared
//! under `provider.<id>` and the active model is `"<provider>/<model>"`.
//!
//! Not yet verified against an installed OpenCode on this machine — the schema
//! below follows its documented format.

use std::path::PathBuf;

use relay_config::kit::{self, Edit};
use relay_config::{BackupStore, ConfigPath};
use relay_core::format::ConfigFormat;
use relay_core::{
    BackupId, ChangePlan, ConfigSnapshot, HarnessAdapter, HarnessId, Installation, ProtocolDialect,
    RelayError, Result, Route, RouteContext, Surface, Validation, Version,
};
use serde_json::json;

/// Directories OpenCode may keep its config in, most specific first.
///
/// OpenCode follows the XDG convention, so Windows uses `%APPDATA%\opencode`
/// rather than a `~/.config` directory that only Unix shells populate. The
/// legacy location stays in the list so an existing install is still found.
fn config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME") {
        dirs.push(PathBuf::from(base).join("opencode"));
    }
    if cfg!(windows) {
        if let Some(base) = dirs::config_dir() {
            dirs.push(base.join("opencode"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".config").join("opencode"));
        dirs.push(home.join(".opencode"));
    }
    dirs
}

/// The directory holding OpenCode's global config (where `agent/` lives):
/// the one with an existing config file, else the first candidate.
pub fn global_config_dir() -> PathBuf {
    if let Some(file) = std::env::var_os("OPENCODE_CONFIG") {
        if let Some(parent) = PathBuf::from(file).parent() {
            return parent.to_path_buf();
        }
    }
    let candidates = config_dirs();
    candidates
        .iter()
        .find(|dir| {
            ["opencode.json", "opencode.jsonc", "config.json"]
                .iter()
                .any(|name| dir.join(name).exists())
        })
        .or_else(|| candidates.first())
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."))
}

pub struct OpenCodeAdapter {
    id: HarnessId,
    config: PathBuf,
    backups: BackupStore,
}

impl OpenCodeAdapter {
    pub fn new(backups: BackupStore) -> Self {
        if let Some(file) = std::env::var_os("OPENCODE_CONFIG") {
            return Self::with_config(PathBuf::from(file), backups);
        }
        let candidates = config_dirs();
        let config = candidates
            .iter()
            .flat_map(|dir| {
                ["opencode.json", "opencode.jsonc", "config.json"]
                    .iter()
                    .map(move |name| dir.join(name))
            })
            .find(|path| path.exists())
            .unwrap_or_else(|| {
                candidates
                    .first()
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("opencode.json")
            });
        Self::with_config(config, backups)
    }

    pub fn with_config(config: PathBuf, backups: BackupStore) -> Self {
        OpenCodeAdapter {
            id: HarnessId::new(HarnessId::OPENCODE),
            config,
            backups,
        }
    }
}

impl HarnessAdapter for OpenCodeAdapter {
    fn id(&self) -> &HarnessId {
        &self.id
    }

    fn config_paths(&self, _surface: Surface) -> Vec<PathBuf> {
        vec![self.config.clone()]
    }

    fn detect_installations(&self) -> Vec<Installation> {
        kit::installations(&self.id, &["opencode"], &[], &self.config)
    }

    fn detect_version(&self) -> Option<Version> {
        self.detect_installations()
            .into_iter()
            .find_map(|i| i.version)
    }

    fn read_config(&self) -> Result<ConfigSnapshot> {
        Ok(ConfigSnapshot {
            files: vec![kit::snapshot_file(&self.config, ConfigFormat::Json)?],
        })
    }

    fn plan_changes(&self, route: &Route, ctx: &RouteContext) -> Result<ChangePlan> {
        let provider = &ctx.provider;
        let id = provider.id.as_str();
        let (npm, base_url, key_reference, key_note) = if ctx.via_gateway {
            let url = ctx.gateway_url.as_deref().ok_or_else(|| {
                RelayError::Validation(
                    "gateway route requested but the gateway is not running".into(),
                )
            })?;
            let token_file = ctx.gateway_token_file.as_deref().ok_or_else(|| {
                RelayError::Validation(
                    "editor gateway route requested but no Turnout token file was provided".into(),
                )
            })?;
            (
                "@ai-sdk/openai-compatible",
                format!("{}/v1", url.trim_end_matches('/')),
                format!("{{file:{}}}", token_file.display()),
                format!(
                    "The loopback gateway token is read from the user-only file {}.",
                    token_file.display()
                ),
            )
        } else {
            let npm = match provider.dialect {
                ProtocolDialect::OpenAiChat => "@ai-sdk/openai-compatible",
                ProtocolDialect::OpenAiResponses => "@ai-sdk/openai",
                ProtocolDialect::AnthropicMessages => "@ai-sdk/anthropic",
            };
            (
                npm,
                provider.base_url.trim_end_matches('/').to_string(),
                format!("{{env:{}}}", provider.api_key_env),
                format!(
                    "The API key is read from ${} at launch and is never written here.",
                    provider.api_key_env
                ),
            )
        };

        let at = |rest: &[&str]| {
            let mut path = vec!["provider".to_string(), id.to_string()];
            path.extend(rest.iter().map(|s| s.to_string()));
            ConfigPath::new(path)
        };
        let doc = kit::read_document(&self.config, ConfigFormat::Json)?;
        let mut edits = vec![
            Edit::set("model", json!(format!("{id}/{}", ctx.model))),
            Edit::Set(at(&["npm"]), json!(npm)),
            Edit::Set(at(&["name"]), json!(provider.label)),
            Edit::Set(at(&["options", "baseURL"]), json!(base_url)),
            Edit::Set(at(&["options", "apiKey"]), json!(key_reference)),
        ];
        // The pre-rename release wrote `provider.relay` (with `relay/…` model
        // ids) for the gateway route. Remove that block so a rename never
        // leaves two providers behind; only the gateway block is ours.
        let legacy = relay_core::brand::LEGACY_GATEWAY_PROVIDER_ID;
        let legacy_path = ConfigPath::new(vec!["provider".to_string(), legacy.to_string()]);
        let removed_legacy = id != legacy && doc.get(&legacy_path)?.is_some();
        if removed_legacy {
            edits.push(Edit::remove(legacy_path));
        }
        // Never overwrite model entries the user already customised.
        for model in &ctx.models {
            let model_path = at(&["models", model]);
            if doc.get(&model_path)?.is_none() {
                edits.push(Edit::Set(model_path, json!({ "name": model })));
            }
        }

        let mut notes = vec![
            "OpenCode CLI and the editor integrations share this file.".to_string(),
            key_note,
            format!(
                "OpenCode's model picker exposes {} configured model(s); `{}` is the default.",
                ctx.models.len(),
                ctx.model
            ),
        ];
        if removed_legacy {
            notes.push(format!(
                "Removed the old `{legacy}` provider block; the gateway route is now `{id}`."
            ));
        }
        let change = kit::plan_file(&self.config, ConfigFormat::Json, &edits, notes)?;
        Ok(ChangePlan::new(
            self.id.clone(),
            route.clone(),
            vec![change],
        ))
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

    const EXISTING: &str = "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  // custom limits\n  \"provider\": {\n    \"opencode-go\": {\n      \"models\": { \"glm-5.3\": { \"limit\": { \"context\": 1000 } } }\n    }\n  }\n}\n";

    fn route() -> Route {
        Route {
            harness: HarnessId::new("opencode"),
            surfaces: vec![Surface::Cli],
            provider: "opencode-go".into(),
            model: "glm-5.3".into(),
            models: vec!["glm-5.3".into()],
            profile: None,
        }
    }

    fn ctx() -> RouteContext {
        RouteContext {
            provider: ProviderConfig::openai_compatible("opencode-go", "https://x.test/v1"),
            model: "glm-5.3".into(),
            models: vec!["glm-5.3".into()],
            surfaces: vec![Surface::Cli],
            via_gateway: false,
            gateway_url: None,
            gateway_token_command: None,
            gateway_token_file: None,
        }
    }

    #[test]
    fn creates_a_config_from_scratch_and_rolls_back_by_deleting_it() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = OpenCodeAdapter::with_config(
            dir.path().join("oc/opencode.json"),
            BackupStore::at(dir.path().join("bk")),
        );
        let plan = adapter.plan_changes(&route(), &ctx()).unwrap();
        let backup = adapter.apply(&plan).unwrap();
        let v = adapter.read_config().unwrap().files[0].values.clone();
        assert_eq!(v["model"], "opencode-go/glm-5.3");
        assert_eq!(
            v["provider"]["opencode-go"]["options"]["apiKey"],
            "{env:OPENCODE_GO_API_KEY}"
        );
        assert_eq!(
            v["provider"]["opencode-go"]["npm"],
            "@ai-sdk/openai-compatible"
        );
        adapter.rollback(&backup).unwrap();
        assert!(!adapter.config.exists());
    }

    #[test]
    fn keeps_comments_schema_and_customised_model_entries() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("opencode.json");
        std::fs::write(&config, EXISTING).unwrap();
        let adapter =
            OpenCodeAdapter::with_config(config.clone(), BackupStore::at(dir.path().join("bk")));
        let plan = adapter.plan_changes(&route(), &ctx()).unwrap();
        let backup = adapter.apply(&plan).unwrap();

        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("// custom limits") && text.contains("$schema"));
        let v = Document::parse(ConfigFormat::Json, text)
            .unwrap()
            .to_value()
            .unwrap();
        assert_eq!(
            v["provider"]["opencode-go"]["models"]["glm-5.3"]["limit"]["context"],
            1000
        );
        assert_eq!(
            v["provider"]["opencode-go"]["options"]["baseURL"],
            "https://x.test/v1"
        );

        adapter.rollback(&backup).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), EXISTING);
    }

    #[test]
    fn writes_every_enabled_model_and_keeps_one_default() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = OpenCodeAdapter::with_config(
            dir.path().join("opencode.json"),
            BackupStore::at(dir.path().join("bk")),
        );
        let mut context = ctx();
        context.models = vec!["glm-5.3".into(), "mimo-v2.6-pro".into()];
        let plan = adapter.plan_changes(&route(), &context).unwrap();
        adapter.apply(&plan).unwrap();
        let value = adapter.read_config().unwrap().files[0].values.clone();
        assert_eq!(value["model"], "opencode-go/glm-5.3");
        assert_eq!(
            value["provider"]["opencode-go"]["models"]["mimo-v2.6-pro"]["name"],
            "mimo-v2.6-pro"
        );
    }

    #[test]
    fn replaces_the_pre_rename_gateway_provider_instead_of_stacking_one() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("opencode.json");
        std::fs::write(
            &config,
            r#"{
  "model": "relay/evren/glm-5.3",
  "provider": {
    "relay": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Relay",
      "options": {
        "baseURL": "http://127.0.0.1:47823/v1",
        "apiKey": "{file:/old/Application Support/Relay/gateway.token}"
      },
      "models": { "evren/glm-5.3": { "name": "evren/glm-5.3" } }
    },
    "opencode-go": { "models": { "glm-5.3": {} } }
  }
}"#,
        )
        .unwrap();
        let adapter =
            OpenCodeAdapter::with_config(config.clone(), BackupStore::at(dir.path().join("bk")));
        let mut context = ctx();
        context.via_gateway = true;
        context.gateway_url = Some("http://127.0.0.1:47823".into());
        context.gateway_token_file = Some(dir.path().join("gateway.token"));
        let mut gateway_provider =
            ProviderConfig::openai_compatible("turnout", "http://127.0.0.1:47823/v1");
        gateway_provider.label = "Turnout".into();
        context.provider = gateway_provider;
        context.model = "evren/glm-5.3".into();
        context.models = vec!["evren/glm-5.3".into()];
        let route = Route {
            harness: HarnessId::new("opencode"),
            surfaces: vec![Surface::Cli],
            provider: "turnout".into(),
            model: "evren/glm-5.3".into(),
            models: vec!["evren/glm-5.3".into()],
            profile: None,
        };
        adapter
            .apply(&adapter.plan_changes(&route, &context).unwrap())
            .unwrap();

        let value = adapter.read_config().unwrap().files[0].values.clone();
        assert!(
            value["provider"].get("relay").is_none(),
            "old gateway provider must be removed"
        );
        assert_eq!(value["provider"]["turnout"]["name"], "Turnout");
        assert_eq!(value["model"], "turnout/evren/glm-5.3");
        assert_eq!(
            value["provider"]["turnout"]["options"]["apiKey"],
            format!("{{file:{}}}", dir.path().join("gateway.token").display())
        );
        // Unrelated user providers stay.
        assert_eq!(
            value["provider"]["opencode-go"]["models"]["glm-5.3"],
            json!({})
        );
    }

    #[test]
    fn editor_gateway_reads_only_the_local_token_file() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = OpenCodeAdapter::with_config(
            dir.path().join("opencode.json"),
            BackupStore::at(dir.path().join("bk")),
        );
        let mut context = ctx();
        context.via_gateway = true;
        context.gateway_url = Some("http://127.0.0.1:47823".into());
        context.gateway_token_file = Some(dir.path().join("gateway.token"));
        let plan = adapter.plan_changes(&route(), &context).unwrap();
        adapter.apply(&plan).unwrap();
        let value = adapter.read_config().unwrap().files[0].values.clone();
        assert_eq!(
            value["provider"]["opencode-go"]["options"]["baseURL"],
            "http://127.0.0.1:47823/v1"
        );
        assert!(value["provider"]["opencode-go"]["options"]["apiKey"]
            .as_str()
            .unwrap()
            .starts_with("{file:"));
    }
}
