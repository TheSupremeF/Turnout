//! Claude Code adapter. Settings live in `~/.claude/settings.json`
//! (`$CLAUDE_CONFIG_DIR/settings.json` when set); the CLI and the VS Code
//! extension share them. Variable names were checked against Claude Code 2.1.x.

use std::path::PathBuf;

use relay_config::kit::{self, Edit};
use relay_config::{BackupStore, Document};
use relay_core::format::ConfigFormat;
use relay_core::{
    BackupId, ChangePlan, ConfigSnapshot, HarnessAdapter, HarnessId, Installation, ProtocolDialect,
    RelayError, Result, Route, RouteContext, Surface, Validation, Version,
};
use serde_json::json;

const OFFICIAL_HOST: &str = "api.anthropic.com";
/// `Route::profile` value for "official Claude models plus custom ones": the
/// built-in picker is kept and the gateway forwards Claude's own credential to
/// Anthropic for the official models.
pub const HYBRID_PROFILE: &str = "hybrid";
const MODEL_ENVS: [&str; 4] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
];

pub struct ClaudeCodeAdapter {
    id: HarnessId,
    config: PathBuf,
    backups: BackupStore,
}

impl ClaudeCodeAdapter {
    pub fn new(backups: BackupStore) -> Self {
        let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
            .unwrap_or_else(|| PathBuf::from(".claude"));
        Self::with_config(dir.join("settings.json"), backups)
    }

    pub fn with_config(config: PathBuf, backups: BackupStore) -> Self {
        ClaudeCodeAdapter {
            id: HarnessId::new(HarnessId::CLAUDE_CODE),
            config,
            backups,
        }
    }
}

/// Claude Code appends `/v1/messages` itself, so the base URL must not end in `/v1`.
fn claude_base(url: &str) -> String {
    let url = url.trim_end_matches('/');
    url.strip_suffix("/v1").unwrap_or(url).to_string()
}

fn is_official(url: &str) -> bool {
    url.contains(OFFICIAL_HOST)
}

fn token_helper(path: &std::path::Path) -> String {
    format!(
        "\"{}\" gateway token",
        path.display().to_string().replace('"', "\\\"")
    )
}

fn with_loopback_no_proxy(existing: Option<&str>) -> String {
    let mut entries = existing
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    for required in ["127.0.0.1", "localhost"] {
        if !entries.iter().any(|entry| entry == required) {
            entries.push(required.to_string());
        }
    }
    entries.join(",")
}

fn current_env_value(path: &std::path::Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value = Document::parse(ConfigFormat::Json, text)
        .ok()?
        .to_value()
        .ok()?;
    value["env"][name].as_str().map(str::to_string)
}

impl ClaudeCodeAdapter {
    /// Official Claude models (OAuth stays in charge) plus custom gateway models.
    ///
    /// Claude Code sends its own OAuth credential to whatever `ANTHROPIC_BASE_URL`
    /// says, so the gateway can forward official models to Anthropic untouched.
    /// Nothing here sets a token, a helper or a model slot: any of those would
    /// replace the OAuth credential or pin the official models to a custom one.
    fn plan_hybrid(&self, route: &Route, ctx: &RouteContext) -> Result<ChangePlan> {
        let gateway = ctx.gateway_url.as_ref().ok_or_else(|| {
            RelayError::Validation("hybrid route requested but the gateway URL is missing".into())
        })?;
        if ctx.models.is_empty() {
            return Err(RelayError::Validation(
                "a hybrid route needs at least one custom model".into(),
            ));
        }
        let env = |name: &str| format!("env.{name}");
        let mut edits = vec![
            Edit::set("model", json!(ctx.model)),
            Edit::set(
                env("ANTHROPIC_BASE_URL").as_str(),
                json!(claude_base(gateway)),
            ),
            Edit::remove("apiKeyHelper"),
            Edit::remove(env("ANTHROPIC_AUTH_TOKEN").as_str()),
            Edit::remove(env("ANTHROPIC_API_KEY").as_str()),
        ];
        for name in MODEL_ENVS {
            edits.push(Edit::remove(env(name).as_str()));
        }
        for name in ["NO_PROXY", "no_proxy"] {
            let value = with_loopback_no_proxy(current_env_value(&self.config, name).as_deref());
            edits.push(Edit::set(env(name).as_str(), json!(value)));
        }
        let options = ctx
            .models
            .iter()
            .map(|model| {
                json!({
                    "model": model,
                    "label": model,
                    "description": format!("{} üzerinden", ctx.provider.label),
                    "behavesAs": "claude-sonnet-5-5",
                })
            })
            .collect::<Vec<_>>();
        edits.push(Edit::set("modelPicker.options", json!(options)));
        // Added after the built-in lineup instead of replacing it.
        edits.push(Edit::set("modelPicker.replaceBuiltInOptions", json!(false)));
        let notes = vec![
            "Claude Code CLI and the VS Code extension share this file.".to_string(),
            format!(
                "Hybrid: Claude's built-in models keep working through the Turnout gateway ({gateway}); {} custom model(s) are added after them.",
                ctx.models.len()
            ),
            "Claude's own login is forwarded to Anthropic untouched; no token or key is written."
                .to_string(),
            "Claude Code needs the Turnout Agent running (`turnout gateway install`). If it is stopped, run `turnout rollback` to go back to direct Anthropic."
                .to_string(),
        ];
        let change = kit::plan_file(&self.config, ConfigFormat::Json, &edits, notes)?;
        Ok(ChangePlan::new(
            self.id.clone(),
            route.clone(),
            vec![change],
        ))
    }
}

impl HarnessAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &HarnessId {
        &self.id
    }

    fn config_paths(&self, _surface: Surface) -> Vec<PathBuf> {
        vec![self.config.clone()]
    }

    fn detect_installations(&self) -> Vec<Installation> {
        kit::installations(&self.id, &["claude"], &[], &self.config)
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
        if provider.dialect != ProtocolDialect::AnthropicMessages && !ctx.via_gateway {
            return Err(RelayError::Validation(
                "Claude Code speaks the Anthropic Messages protocol; route OpenAI-style providers through the Turnout gateway"
                    .to_string(),
            ));
        }
        if ctx.via_gateway && !ctx.surfaces.contains(&Surface::VsCode) {
            ctx.gateway_url.as_ref().ok_or_else(|| {
                RelayError::Validation(
                    "gateway route requested but the gateway URL is missing".into(),
                )
            })?;

            let mut plan = ChangePlan::new(self.id.clone(), route.clone(), Vec::new());
            plan.notes.push(
                "Launch-only route: Turnout will inject the gateway URL, local token and model into the Claude process; ~/.claude/settings.json is not changed."
                    .to_string(),
            );
            plan.notes.push(
                "The Claude Code VS Code extension remains on its existing provider for this CLI-only route."
                    .to_string(),
            );
            return Ok(plan);
        }
        if ctx.via_gateway && route.profile.as_deref() == Some(HYBRID_PROFILE) {
            return self.plan_hybrid(route, ctx);
        }
        let target = if ctx.via_gateway {
            ctx.gateway_url.clone().ok_or_else(|| {
                RelayError::Validation(
                    "gateway route requested but the gateway is not running".into(),
                )
            })?
        } else {
            provider.base_url.clone()
        };

        let env = |name: &str| format!("env.{name}");
        let mut edits = vec![Edit::set("model", json!(ctx.model))];
        let mut notes = vec![
            "Claude Code CLI and the VS Code extension share this file.".to_string(),
            "The token is never written; use `turnout run claude` or export it before launching."
                .to_string(),
        ];
        if ctx.via_gateway || !is_official(&target) {
            edits.push(Edit::set(
                env("ANTHROPIC_BASE_URL").as_str(),
                json!(claude_base(&target)),
            ));
            for name in MODEL_ENVS {
                edits.push(Edit::set(env(name).as_str(), json!(ctx.model)));
            }
            if ctx.via_gateway {
                let helper = ctx.gateway_token_command.as_deref().ok_or_else(|| {
                    RelayError::Validation(
                        "VS Code gateway route requested but no Turnout token helper was provided"
                            .into(),
                    )
                })?;
                edits.push(Edit::set("apiKeyHelper", json!(token_helper(helper))));
                for name in ["NO_PROXY", "no_proxy"] {
                    let value =
                        with_loopback_no_proxy(current_env_value(&self.config, name).as_deref());
                    edits.push(Edit::set(env(name).as_str(), json!(value)));
                }
                edits.push(Edit::remove(env("ANTHROPIC_AUTH_TOKEN").as_str()));
                edits.push(Edit::remove(env("ANTHROPIC_API_KEY").as_str()));
                notes.push(
                    "Claude Code obtains the loopback gateway token from Turnout's apiKeyHelper; the provider key stays in the credential store."
                        .to_string(),
                );
            }
            notes.push(
                "Background calls (haiku/sonnet/opus slots) use the default model; picker selections keep their own model id."
                    .to_string(),
            );
        } else {
            edits.push(Edit::remove(env("ANTHROPIC_BASE_URL").as_str()));
            edits.push(Edit::remove("apiKeyHelper"));
            for name in MODEL_ENVS {
                edits.push(Edit::remove(env(name).as_str()));
            }
            notes.push("Direct Anthropic: any previous Turnout overrides are removed.".to_string());
        }

        // Official Claude OAuth owns the built-in catalog. Replacing it with a Turnout
        // snapshot hides models and is what previously broke the user's picker.
        let curate_picker = ctx.via_gateway || !is_official(&target);
        if curate_picker {
            let options = ctx
                .models
                .iter()
                .map(|model| {
                    let mut option = json!({
                        "model": model,
                        "label": model,
                        "description": format!("{} üzerinden", provider.label),
                    });
                    if ctx.via_gateway || !is_official(&target) {
                        option["behavesAs"] = json!("claude-sonnet-5-5");
                    }
                    option
                })
                .collect::<Vec<_>>();
            edits.push(Edit::set("modelPicker.options", json!(options)));
            edits.push(Edit::set("modelPicker.replaceBuiltInOptions", json!(true)));
            notes.push(format!(
                "Claude /model picker exposes {} configured model(s); `{}` is the default.",
                ctx.models.len(),
                ctx.model
            ));
        } else {
            // Removing only the two keys leaves `"modelPicker": {}`, which Claude
            // Code rejects ("must be an object with an options array").
            edits.push(Edit::remove("modelPicker"));
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

    const EXISTING: &str = "{\n  // my permissions\n  \"permissions\": { \"allow\": [\"Bash(ls:*)\"] },\n  \"env\": { \"FOO\": \"1\" },\n}\n";

    fn setup(content: Option<&str>) -> (tempfile::TempDir, ClaudeCodeAdapter, Route) {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("settings.json");
        if let Some(content) = content {
            std::fs::write(&config, content).unwrap();
        }
        let adapter =
            ClaudeCodeAdapter::with_config(config, BackupStore::at(dir.path().join("bk")));
        let route = Route {
            harness: adapter.id.clone(),
            surfaces: vec![Surface::Cli],
            provider: "p".into(),
            model: "m".into(),
            models: vec!["m".into()],
            profile: None,
        };
        (dir, adapter, route)
    }

    fn ctx(provider: ProviderConfig, via_gateway: bool, model: &str) -> RouteContext {
        RouteContext {
            provider,
            model: model.into(),
            models: vec![model.into()],
            surfaces: vec![Surface::Cli],
            via_gateway,
            gateway_url: via_gateway.then(|| "http://127.0.0.1:4545".to_string()),
            gateway_token_command: None,
            gateway_token_file: None,
        }
    }

    fn values(adapter: &ClaudeCodeAdapter) -> serde_json::Value {
        let text = std::fs::read_to_string(&adapter.config).unwrap();
        Document::parse(ConfigFormat::Json, text)
            .unwrap()
            .to_value()
            .unwrap()
    }

    #[test]
    fn third_party_anthropic_compatible_provider_keeps_user_settings() {
        let (_d, adapter, route) = setup(Some(EXISTING));
        let provider = ProviderConfig::anthropic("proxy", "https://proxy.test/api/v1");
        let plan = adapter
            .plan_changes(&route, &ctx(provider, false, "glm-5.3"))
            .unwrap();
        let backup = adapter.apply(&plan).unwrap();

        let text = std::fs::read_to_string(&adapter.config).unwrap();
        assert!(text.contains("// my permissions"));
        let v = values(&adapter);
        assert_eq!(
            v["env"]["ANTHROPIC_BASE_URL"], "https://proxy.test/api",
            "must drop /v1"
        );
        assert_eq!(v["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "glm-5.3");
        assert_eq!(v["env"]["FOO"], "1");
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        assert!(!text.contains("AUTH_TOKEN"), "token must never be written");

        adapter.rollback(&backup).unwrap();
        assert_eq!(std::fs::read_to_string(&adapter.config).unwrap(), EXISTING);
    }

    #[test]
    fn openai_style_provider_goes_through_the_gateway() {
        let (_d, adapter, route) = setup(None);
        let provider = ProviderConfig::openai_compatible("evren", "https://e.test/v1");
        assert!(adapter
            .plan_changes(&route, &ctx(provider.clone(), false, "m"))
            .is_err());
        let plan = adapter
            .plan_changes(&route, &ctx(provider, true, "m"))
            .unwrap();
        assert!(plan.changes.is_empty());
        assert!(plan.notes.iter().any(|note| note.contains("Launch-only")));
        adapter.apply(&plan).unwrap();
        assert!(
            !adapter.config.exists(),
            "a transient gateway route must not alter global Claude settings"
        );
    }

    #[test]
    fn editor_gateway_uses_api_key_helper_without_storing_a_token() {
        let (_d, adapter, route) = setup(None);
        let provider = ProviderConfig::openai_compatible("evren", "https://e.test/v1");
        let mut context = ctx(provider, true, "deepseek-v4.1-flash");
        context.surfaces = vec![Surface::Cli, Surface::VsCode];
        context.gateway_token_command = Some(PathBuf::from("/opt/turnout"));
        let plan = adapter.plan_changes(&route, &context).unwrap();
        assert_eq!(plan.changes.len(), 1);
        adapter.apply(&plan).unwrap();
        let value = values(&adapter);
        assert_eq!(value["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:4545");
        assert_eq!(value["model"], "deepseek-v4.1-flash");
        assert_eq!(value["apiKeyHelper"], "\"/opt/turnout\" gateway token");
        assert_eq!(value["env"]["NO_PROXY"], "127.0.0.1,localhost");
        assert_eq!(value["env"]["no_proxy"], "127.0.0.1,localhost");
        let text = std::fs::read_to_string(&adapter.config).unwrap();
        assert!(!text.contains("ANTHROPIC_AUTH_TOKEN"));
    }

    #[test]
    fn hybrid_keeps_the_official_list_and_oauth_while_adding_custom_models() {
        // A previous gateway-only setup must be cleaned out: its helper would
        // replace the OAuth login and its slots would pin official models.
        let previous = r#"{
  "apiKeyHelper": "/old/turnout gateway token",
  "env": {"FOO": "1", "ANTHROPIC_AUTH_TOKEN": "t", "ANTHROPIC_MODEL": "x", "ANTHROPIC_DEFAULT_SONNET_MODEL": "x"},
  "modelPicker": {"options": [{"model": "old"}], "replaceBuiltInOptions": true},
  "permissions": {"allow": ["Bash(ls:*)"]}
}"#;
        let (_d, adapter, mut route) = setup(Some(previous));
        route.profile = Some(HYBRID_PROFILE.into());
        let provider = ProviderConfig::openai_compatible("turnout", "http://127.0.0.1:4545/v1");
        let mut context = ctx(provider, true, "claude-sonnet-5-5");
        context.models = vec!["deepseek-v4.1-flash".into(), "glm-5.3".into()];
        context.surfaces = vec![Surface::Cli, Surface::VsCode];
        let plan = adapter.plan_changes(&route, &context).unwrap();
        adapter.apply(&plan).unwrap();
        let v = values(&adapter);

        assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:4545");
        assert_eq!(v["model"], "claude-sonnet-5-5");
        // OAuth stays in charge: no helper, no token, no pinned slots.
        assert!(v.get("apiKeyHelper").is_none());
        for name in [
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
        ] {
            assert!(v["env"].get(name).is_none(), "{name} must be removed");
        }
        // Built-in picker is kept; custom rows are appended after it.
        assert_eq!(v["modelPicker"]["replaceBuiltInOptions"], false);
        let options = v["modelPicker"]["options"].as_array().unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["model"], "deepseek-v4.1-flash");
        assert_eq!(options[1]["behavesAs"], "claude-sonnet-5-5");
        assert_eq!(v["env"]["NO_PROXY"], "127.0.0.1,localhost");
        // The user's own settings survive.
        assert_eq!(v["env"]["FOO"], "1");
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
    }

    #[test]
    fn hybrid_without_custom_models_is_rejected() {
        let (_d, adapter, mut route) = setup(None);
        route.profile = Some(HYBRID_PROFILE.into());
        let provider = ProviderConfig::openai_compatible("turnout", "http://127.0.0.1:4545/v1");
        let mut context = ctx(provider, true, "claude-sonnet-5-5");
        context.models = Vec::new();
        context.surfaces = vec![Surface::Cli, Surface::VsCode];
        assert!(adapter.plan_changes(&route, &context).is_err());
    }

    #[test]
    fn editor_gateway_writes_all_models_to_claude_picker() {
        let (_d, adapter, route) = setup(None);
        let provider = ProviderConfig::openai_compatible("evren", "https://e.test/v1");
        let mut context = ctx(provider, true, "deepseek-v4.1-flash");
        context.models = vec![
            "deepseek-v4.1-flash".into(),
            "mimo-v2.6-pro".into(),
            "glm-5.3".into(),
        ];
        context.surfaces = vec![Surface::Cli, Surface::VsCode];
        context.gateway_token_command = Some(PathBuf::from("/opt/turnout"));
        let plan = adapter.plan_changes(&route, &context).unwrap();
        adapter.apply(&plan).unwrap();
        let value = values(&adapter);
        let options = value["modelPicker"]["options"].as_array().unwrap();
        assert_eq!(options.len(), 3);
        assert_eq!(options[1]["model"], "mimo-v2.6-pro");
        assert_eq!(options[2]["behavesAs"], "claude-sonnet-5-5");
        assert_eq!(value["modelPicker"]["replaceBuiltInOptions"], true);
        assert_eq!(value["model"], "deepseek-v4.1-flash");
    }

    #[test]
    fn editor_gateway_preserves_existing_no_proxy_entries() {
        let existing = r#"{"env":{"NO_PROXY":"internal.example,localhost"}}"#;
        let (_d, adapter, route) = setup(Some(existing));
        let provider = ProviderConfig::openai_compatible("openai", "https://api.openai.com/v1");
        let mut context = ctx(provider, true, "gpt-test");
        context.surfaces = vec![Surface::Cli, Surface::VsCode];
        context.gateway_token_command = Some(PathBuf::from("/opt/turnout"));
        adapter
            .apply(&adapter.plan_changes(&route, &context).unwrap())
            .unwrap();
        let value = values(&adapter);
        assert_eq!(
            value["env"]["NO_PROXY"],
            "internal.example,localhost,127.0.0.1"
        );
    }

    #[test]
    fn switching_back_to_official_anthropic_removes_overrides() {
        let (_d, adapter, route) = setup(Some(EXISTING));
        let third = ProviderConfig::anthropic("proxy", "https://proxy.test/v1");
        adapter
            .apply(
                &adapter
                    .plan_changes(&route, &ctx(third, false, "glm"))
                    .unwrap(),
            )
            .unwrap();
        let official = ProviderConfig::anthropic("anthropic", "https://api.anthropic.com/v1");
        adapter
            .apply(
                &adapter
                    .plan_changes(&route, &ctx(official, false, "claude-sonnet-5-5"))
                    .unwrap(),
            )
            .unwrap();
        let v = values(&adapter);
        assert!(v["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert!(v["env"].get("ANTHROPIC_MODEL").is_none());
        // An emptied picker must disappear entirely, not linger as `{}`.
        assert!(v.get("modelPicker").is_none());
        assert_eq!(v["model"], "claude-sonnet-5-5");
        assert_eq!(v["env"]["FOO"], "1");
    }

    #[test]
    fn official_oauth_keeps_claudes_builtin_picker_even_with_multiple_models() {
        let existing = r#"{
          "modelPicker": {
            "replaceBuiltInOptions": true,
            "options": [{"model": "broken-proxy-model"}]
          }
        }"#;
        let (_d, adapter, route) = setup(Some(existing));
        let official = ProviderConfig::anthropic("anthropic", "https://api.anthropic.com/v1");
        let mut context = ctx(official, false, "claude-sonnet-4-6");
        context.models = vec!["claude-sonnet-4-6".into(), "claude-opus-4-6".into()];
        adapter
            .apply(&adapter.plan_changes(&route, &context).unwrap())
            .unwrap();
        let value = values(&adapter);
        assert_eq!(value["model"], "claude-sonnet-4-6");
        assert!(value["modelPicker"].get("options").is_none());
        assert!(value["modelPicker"].get("replaceBuiltInOptions").is_none());
    }
}
