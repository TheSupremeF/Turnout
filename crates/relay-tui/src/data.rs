//! Everything the terminal UI knows about the outside world.
//!
//! The UI never edits config files itself. Every action is delegated to the
//! standalone CLI (`plan-targets`, `apply`, `providers`, ...), exactly like the
//! desktop GUI bridge does, so both front ends share one core.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

/// (CLI id, display name, tagline)
pub const HARNESSES: [(&str, &str, &str); 3] = [
    ("codex", "Codex", "OpenAI kodlama aracı"),
    ("claude-code", "Claude Code", "Anthropic kodlama aracı"),
    ("opencode", "OpenCode", "Açık kaynak kodlama aracı"),
];

pub const CLAUDE: usize = 1;

/// Claude 5 family, as API ids (display names are never sent upstream).
pub const ANTHROPIC_MODELS: [&str; 4] = [
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-fable-5-1",
];

/// Codex picker order; used only when live discovery is unavailable.
pub const OPENAI_FALLBACK: [&str; 7] = [
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
];

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Target {
    pub provider: String,
    pub model: String,
}

impl Target {
    pub fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
        }
    }

    fn arg(&self) -> String {
        format!("{}::{}", self.provider, self.model)
    }
}

/// What a harness should expose: several provider/model pairs and one default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub targets: Vec<Target>,
    pub default: Option<Target>,
    /// Claude Code only: keep the built-in model list and append the rest.
    pub hybrid: bool,
    /// Let every model fall back to the route's other models (off by default).
    pub fallback: bool,
    /// Per-model fallback chains, in order. Kept so that re-applying a route
    /// from the TUI never drops chains set elsewhere.
    pub chains: BTreeMap<Target, Vec<Target>>,
}

impl Selection {
    pub fn contains(&self, target: &Target) -> bool {
        self.targets.contains(target)
    }

    pub fn default_target(&self) -> Option<&Target> {
        self.default
            .as_ref()
            .filter(|default| self.targets.contains(default))
            .or_else(|| self.targets.first())
    }

    pub fn toggle(&mut self, target: &Target) {
        if let Some(index) = self.targets.iter().position(|item| item == target) {
            self.targets.remove(index);
            if self.default.as_ref() == Some(target) {
                self.default = None;
            }
            // A chain can only name models of the route.
            self.chains.remove(target);
            for chain in self.chains.values_mut() {
                chain.retain(|item| item != target);
            }
            self.chains.retain(|_, chain| !chain.is_empty());
        } else {
            self.targets.push(target.clone());
        }
    }

    pub fn set_default(&mut self, target: &Target) {
        if !self.targets.contains(target) {
            self.targets.push(target.clone());
        }
        self.default = Some(target.clone());
    }

    /// CLI arguments for `plan-targets`.
    pub fn plan_args(&self, harness: &str) -> Result<Vec<String>, String> {
        let default = self
            .default_target()
            .ok_or_else(|| "En az bir model seçin".to_string())?;
        let mut args = vec!["plan-targets".to_string(), harness.to_string()];
        for target in &self.targets {
            args.push("--target".into());
            args.push(target.arg());
        }
        args.push("--default-target".into());
        args.push(default.arg());
        if self.hybrid {
            args.push("--hybrid".into());
        }
        if self.fallback {
            args.push("--fallback".into());
        }
        for (from, to) in &self.chains {
            let to: Vec<String> = to
                .iter()
                .filter(|item| self.targets.contains(item))
                .map(Target::arg)
                .collect();
            if self.targets.contains(from) && !to.is_empty() {
                args.push("--fallback-chain".into());
                args.push(format!("{}={}", from.arg(), to.join(",")));
            }
        }
        Ok(args)
    }

    /// `plan-targets` arguments that save a launch-only profile. A profile never
    /// keeps the original Claude list (the CLI rejects the combination).
    pub fn profile_args(&self, harness: &str, name: &str) -> Result<Vec<String>, String> {
        let mut args = Self {
            hybrid: false,
            ..self.clone()
        }
        .plan_args(harness)?;
        args.push("--profile".into());
        args.push(name.to_string());
        Ok(args)
    }

    /// Short human summary: "EVREN 3 · OpenAI 1".
    pub fn providers_summary(&self) -> String {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for target in &self.targets {
            *counts.entry(target.provider.as_str()).or_default() += 1;
        }
        counts
            .into_iter()
            .map(|(provider, count)| format!("{} {count}", provider_name(provider)))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

pub fn provider_name(id: &str) -> String {
    match id {
        "evren" => "EVREN".into(),
        "openai" => "OpenAI".into(),
        "anthropic" => "Anthropic".into(),
        other => other.to_string(),
    }
}

#[derive(Clone, Debug)]
pub struct HarnessInfo {
    pub installed: bool,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct ProviderInfo {
    pub id: String,
    pub has_key: bool,
}

#[derive(Clone, Debug)]
pub struct OauthInfo {
    pub id: String,
    pub logged_in: bool,
    pub plan: String,
    pub gateway_capable: bool,
}

#[derive(Clone)]
pub struct Backend {
    exe: PathBuf,
}

impl Backend {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe }
    }

    /// Runs the CLI. Returns stdout on success, the tail of stderr on failure.
    pub fn run(&self, args: &[String], stdin: Option<&str>) -> Result<String, String> {
        let mut child = Command::new(&self.exe)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("CLI başlatılamadı: {error}"))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            let _ = pipe.write_all(text.as_bytes());
            let _ = pipe.write_all(b"\n");
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("CLI beklenirken hata: {error}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if output.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = if stderr.trim().is_empty() {
                stdout.as_str()
            } else {
                stderr.as_ref()
            };
            let lines = detail
                .trim()
                .lines()
                .rev()
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" ");
            Err(lines.trim_start_matches("error: ").to_string())
        }
    }

    pub fn exe(&self) -> &std::path::Path {
        &self.exe
    }

    /// Runs the CLI and returns `(succeeded, stdout + stderr)`. Unlike `run`
    /// this keeps the output of failing commands (`doctor` exits non-zero but
    /// its report is the point).
    pub fn run_both(&self, args: &[String]) -> (bool, String) {
        match Command::new(&self.exe)
            .args(args)
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) => {
                let mut text = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.trim().is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(stderr.trim_end());
                }
                (output.status.success(), text)
            }
            Err(error) => (false, format!("CLI başlatılamadı: {error}")),
        }
    }

    pub fn args(&self, args: &[&str]) -> Result<String, String> {
        let owned = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        self.run(&owned, None)
    }

    pub fn harnesses(&self) -> Vec<HarnessInfo> {
        parse_harnesses(&self.args(&["harness", "detect"]).unwrap_or_default())
    }

    pub fn providers(&self) -> Result<Vec<ProviderInfo>, String> {
        let text = self.args(&["providers", "list", "--json"])?;
        let value: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        Ok(value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some(ProviderInfo {
                            id: item["id"].as_str()?.to_string(),
                            has_key: item["has_key"].as_bool().unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn oauth(&self) -> Result<Vec<OauthInfo>, String> {
        let text = self.args(&["auth", "status", "--json"])?;
        let value: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        Ok(value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some(OauthInfo {
                            id: item["id"].as_str()?.to_string(),
                            logged_in: item["logged_in"].as_bool().unwrap_or(false),
                            plan: item["plan"].as_str().unwrap_or("").to_string(),
                            gateway_capable: item["gateway_capable"].as_bool().unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Live model discovery; one model id per line.
    pub fn models(&self, provider: &str) -> Result<Vec<String>, String> {
        let text = self.args(&["models", "list", provider])?;
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    pub fn add_custom_provider(
        &self,
        id: &str,
        base_url: &str,
        dialect: &str,
        key: &str,
    ) -> Result<String, String> {
        let args = vec![
            "providers".to_string(),
            "add".into(),
            id.into(),
            "--base-url".into(),
            base_url.into(),
            "--dialect".into(),
            dialect.into(),
            "--key-stdin".into(),
        ];
        self.run(&args, Some(key))
    }

    pub fn add_provider(&self, preset: &str, key: &str) -> Result<String, String> {
        let mut args = vec!["providers".to_string(), "add".into(), preset.into()];
        if preset == "anthropic" {
            args.extend([
                "--base-url".into(),
                "https://api.anthropic.com/v1".into(),
                "--dialect".into(),
                "anthropic-messages".into(),
            ]);
        }
        args.push("--key-stdin".into());
        self.run(&args, Some(key))
    }
}

pub fn home_dir() -> Option<PathBuf> {
    // The CLI performs the one-time legacy-directory move at startup; this is
    // a harmless no-op by the time the UI asks for the same path.
    relay_core::brand::resolve_data_dir(|_| {})
}

/// Routes the CLI has applied so far, read from its state file.
pub fn saved_routes() -> BTreeMap<String, Selection> {
    let mut routes = BTreeMap::new();
    let Some(text) =
        home_dir().and_then(|dir| std::fs::read_to_string(dir.join("state.json")).ok())
    else {
        return routes;
    };
    let Ok(state) = serde_json::from_str::<Value>(&text) else {
        return routes;
    };
    let Some(saved) = state["routes"].as_object() else {
        return routes;
    };
    for (harness, route) in saved {
        let mut selection = Selection {
            hybrid: route["hybrid"].as_bool().unwrap_or(false),
            fallback: route["fallback"].as_bool().unwrap_or(false),
            ..Selection::default()
        };
        let default_public = route["model"].as_str().unwrap_or("");
        let targets = route["targets"].as_array().cloned().unwrap_or_default();
        // Chains name public model ids; map them back once every target is known.
        let by_public: BTreeMap<&str, Target> = targets
            .iter()
            .filter_map(|t| {
                Some((
                    t["public_model"].as_str()?,
                    Target::new(t["provider"].as_str()?, t["model"].as_str()?),
                ))
            })
            .collect();
        for target in &targets {
            let to: Vec<Target> = target["fallbacks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|public| by_public.get(public.as_str()?).cloned())
                .collect();
            if let (Some(provider), Some(model), false) =
                (target["provider"].as_str(), target["model"].as_str(), to.is_empty())
            {
                selection.chains.insert(Target::new(provider, model), to);
            }
        }
        for target in &targets {
            let (Some(provider), Some(model)) =
                (target["provider"].as_str(), target["model"].as_str())
            else {
                continue;
            };
            let item = Target::new(provider, model);
            if target["public_model"].as_str() == Some(default_public) {
                selection.default = Some(item.clone());
            }
            selection.targets.push(item);
        }
        // A hybrid route's default is usually one of Claude's own models, which
        // is not an upstream and so is not among `targets`.
        if selection.hybrid && selection.default.is_none() && !default_public.is_empty() {
            let official = Target::new("anthropic", default_public);
            selection.targets.push(official.clone());
            selection.default = Some(official);
        }
        if targets.is_empty() {
            // Legacy single-provider routes only stored model ids.
            let provider = route["provider"].as_str().unwrap_or("");
            for model in route["models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                let item = Target::new(provider, model);
                if model == default_public {
                    selection.default = Some(item.clone());
                }
                selection.targets.push(item);
            }
        }
        routes.insert(harness.clone(), selection);
    }
    routes
}

fn cache_path() -> Option<PathBuf> {
    home_dir().map(|dir| dir.join("tui-catalog.json"))
}

/// Model lists fetched earlier. Reading these never touches the OS credential
/// store, so opening the UI does not trigger key-access prompts.
pub fn load_catalog_cache() -> BTreeMap<String, Vec<String>> {
    let Some(text) = cache_path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<BTreeMap<String, Vec<String>>>(&text).unwrap_or_default()
}

pub fn save_catalog_cache(provider: &str, models: &[String]) {
    let Some(path) = cache_path() else { return };
    let mut all = load_catalog_cache();
    all.insert(provider.to_string(), models.to_vec());
    if let Ok(text) = serde_json::to_string_pretty(&all) {
        let _ = std::fs::write(path, text);
    }
}

/// Launch-only profiles: (harness id, name, what they expose).
pub fn saved_profiles() -> Vec<(String, String, Selection)> {
    let Some(text) =
        home_dir().and_then(|dir| std::fs::read_to_string(dir.join("state.json")).ok())
    else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut profiles = Vec::new();
    for (harness, named) in state["profiles"].as_object().into_iter().flatten() {
        for (name, route) in named.as_object().into_iter().flatten() {
            let mut selection = Selection::default();
            let default_public = route["model"].as_str().unwrap_or("");
            for target in route["targets"].as_array().into_iter().flatten() {
                if let (Some(provider), Some(model)) =
                    (target["provider"].as_str(), target["model"].as_str())
                {
                    let item = Target::new(provider, model);
                    if target["public_model"].as_str() == Some(default_public) {
                        selection.default = Some(item.clone());
                    }
                    selection.targets.push(item);
                }
            }
            profiles.push((harness.clone(), name.clone(), selection));
        }
    }
    profiles
}

/// Config backups, newest first: (id, unix seconds). Ids look like `b<unix>-<hash>`.
pub fn list_backups() -> Vec<(String, u64)> {
    let Some(dir) = home_dir().map(|dir| dir.join("backups")) else {
        return Vec::new();
    };
    let mut backups = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().to_string();
            let seconds = id
                .strip_prefix('b')?
                .split('-')
                .next()?
                .parse::<u64>()
                .ok()?;
            Some((id, seconds))
        })
        .collect::<Vec<_>>();
    backups.sort_by(|a, b| b.1.cmp(&a.1));
    backups
}

/// "3 dk önce" style age for a unix timestamp.
pub fn age(seconds: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let delta = now.saturating_sub(seconds);
    match delta {
        0..=59 => "az önce".into(),
        60..=3599 => format!("{} dk önce", delta / 60),
        3600..=86399 => format!("{} sa önce", delta / 3600),
        _ => format!("{} gün önce", delta / 86400),
    }
}

/// Models that cannot drive a coding agent (OCR, embeddings, speech...).
pub fn is_chat_model(model: &str) -> bool {
    let id = model.to_lowercase();
    ![
        "ocr",
        "embedding",
        "reranker",
        "rerank",
        "asr",
        "tts",
        "whisper",
    ]
    .iter()
    .any(|marker| id.contains(marker))
}

/// Read `turnout harness detect` output: a harness counts as installed when
/// its line carries a binary path, `/`-separated or Windows `\\`-separated.
fn parse_harnesses(text: &str) -> Vec<HarnessInfo> {
    let is_path = |part: &str| part.contains(['/', '\\']);
    HARNESSES
        .iter()
        .map(|(id, _, _)| {
            let line = text
                .lines()
                .find(|line| line.split_whitespace().next() == Some(id));
            match line {
                Some(line) if is_path(line) => HarnessInfo {
                    installed: true,
                    detail: line
                        .split_whitespace()
                        .skip(1)
                        .take_while(|part| !is_path(part))
                        .collect::<Vec<_>>()
                        .join(" "),
                },
                _ => HarnessInfo {
                    installed: false,
                    detail: "kurulu değil".into(),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_the_default_clears_it_and_adding_a_default_selects_it() {
        let a = Target::new("evren", "glm-5.3");
        let b = Target::new("openai", "gpt-6.1-sol");
        let mut selection = Selection::default();
        selection.toggle(&a);
        selection.set_default(&b);
        assert_eq!(selection.targets.len(), 2);
        assert_eq!(selection.default_target(), Some(&b));
        selection.toggle(&b);
        assert_eq!(
            selection.default_target(),
            Some(&a),
            "falls back to the first target"
        );
    }

    #[test]
    fn plan_args_carry_every_target_the_default_and_hybrid() {
        let mut selection = Selection::default();
        selection.set_default(&Target::new("anthropic", "claude-sonnet-5-5"));
        selection.toggle(&Target::new("evren", "glm-5.3"));
        selection.hybrid = true;
        let args = selection.plan_args("claude-code").unwrap();
        assert_eq!(args[0], "plan-targets");
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--target", "evren::glm-5.3"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--default-target", "anthropic::claude-sonnet-5-5"]));
        assert_eq!(args.last().map(String::as_str), Some("--hybrid"));
        assert!(Selection::default().plan_args("codex").is_err());
    }

    #[test]
    fn non_chat_models_are_recognised() {
        assert!(is_chat_model("deepseek-v4.1-flash"));
        for model in [
            "qwen3-embedding-8b",
            "dots-ocr",
            "qwen3-reranker-8b",
            "whisper-large",
        ] {
            assert!(!is_chat_model(model), "{model}");
        }
    }

    #[test]
    fn windows_paths_count_as_installed() {
        let text = "codex        codex-cli 0.164.0-alpha.11.1  C:\\Users\\x\\.vscode\\extensions\\openai.chatgpt-26.1021.22027-win32-x64\\bin\\windows-x86_64\\codex.exe\n             config: C:\\Users\\x\\.codex\\config.toml\nclaude-code  2.1.153 (Claude Code)  /usr/local/bin/claude\nopencode     not found\n";
        let found = parse_harnesses(text);
        assert!(found[0].installed);
        assert_eq!(found[0].detail, "codex-cli 0.164.0-alpha.11.1");
        assert!(found[1].installed);
        assert_eq!(found[1].detail, "2.1.153 (Claude Code)");
        assert!(!found[2].installed);
    }

    #[test]
    fn re_applying_keeps_fallback_and_chains() {
        let deepseek = Target::new("evren", "deepseek-v4.1-flash");
        let glm = Target::new("evren", "glm-5.3");
        let mut selection = Selection {
            targets: vec![deepseek.clone(), glm.clone()],
            default: Some(deepseek.clone()),
            fallback: true,
            ..Selection::default()
        };
        selection.chains.insert(deepseek.clone(), vec![glm.clone()]);
        let args = selection.plan_args("claude-code").unwrap();
        assert!(args.iter().any(|a| a == "--fallback"), "{args:?}");
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--fallback-chain", "evren::deepseek-v4.1-flash=evren::glm-5.3"]));

        // Deselecting a model takes it out of every chain; an emptied chain goes.
        selection.toggle(&glm);
        assert!(selection.chains.is_empty());
        let args = selection.plan_args("claude-code").unwrap();
        assert!(!args.iter().any(|a| a == "--fallback-chain"), "{args:?}");
    }
}
