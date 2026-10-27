//! Drives the real `turnout` binary. Uses a throwaway data dir and the encrypted
//! vault so the developer's own Keychain is never touched.

use std::io::Write;
use std::process::{Command, Stdio};

const KEY: &str = "sk-integration-test-key-0123456789abcdef";

struct Env {
    home: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Env {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> (bool, String) {
        self.run_with(&[], args, stdin)
    }

    fn run_with(
        &self,
        extra_env: &[(&str, &str)],
        args: &[&str],
        stdin: Option<&str>,
    ) -> (bool, String) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_turnout"));
        command
            .args(args)
            .env("TURNOUT_HOME", self.home.path().join("turnout"))
            .env("TURNOUT_FORCE_VAULT", "1")
            .env("TURNOUT_VAULT_PASSWORD", "test-master-password");
        self.exec(command, extra_env, stdin)
    }

    /// Only the pre-rename variable names, to prove they are still honoured.
    fn run_legacy(
        &self,
        extra_env: &[(&str, &str)],
        args: &[&str],
        stdin: Option<&str>,
    ) -> (bool, String) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_turnout"));
        command
            .args(args)
            .env_remove("TURNOUT_HOME")
            .env_remove("TURNOUT_FORCE_VAULT")
            .env_remove("TURNOUT_VAULT_PASSWORD")
            .env_remove("RELAY_HOME")
            .env_remove("RELAY_FORCE_VAULT")
            .env_remove("RELAY_VAULT_PASSWORD");
        self.exec(command, extra_env, stdin)
    }

    fn exec(
        &self,
        mut command: Command,
        extra_env: &[(&str, &str)],
        stdin: Option<&str>,
    ) -> (bool, String) {
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !text.contains(KEY),
            "the API key leaked into CLI output:\n{text}"
        );
        (out.status.success(), text)
    }
}

#[test]
fn provider_plan_apply_rollback_flow_never_leaks_the_key() {
    let env = Env::new();
    let codex = env.home.path().join("codex/config.toml");
    std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
    let original = "# mine\nmodel = \"gpt-5\"\n\n[projects.\"/w\"]\ntrust_level = \"trusted\"\n";
    std::fs::write(&codex, original).unwrap();
    let codex_arg = codex.to_str().unwrap();

    let (ok, out) = env.run(
        &[
            "providers",
            "add",
            "opencode-go",
            "--base-url",
            "https://api.example.test/v1",
            "--key-stdin",
        ],
        Some(&format!("{KEY}\n")),
    );
    assert!(ok, "{out}");

    let (ok, out) = env.run(&["providers", "list"], None);
    assert!(
        ok && out.contains("opencode-go") && out.contains("key stored"),
        "{out}"
    );

    // Plan writes nothing.
    let (ok, out) = env.run(
        &[
            "plan",
            "codex",
            "--provider",
            "opencode-go",
            "--model",
            "glm-5.3",
            "--model",
            "mimo-v2.6-pro",
            "--default-model",
            "glm-5.3",
            "--config",
            codex_arg,
        ],
        None,
    );
    assert!(ok, "{out}");
    assert!(out.contains("+model_provider = \"opencode-go\""), "{out}");
    // Codex only speaks the Responses API, so a chat-only provider goes via the gateway.
    assert!(out.contains("wire_api = \"responses\""), "{out}");
    assert!(out.contains("[model_providers.opencode-go.auth]"), "{out}");
    assert!(out.contains("args = [\"gateway\", \"token\"]"), "{out}");
    assert!(out.contains("http://127.0.0.1:47822/v1"), "{out}");
    assert_eq!(std::fs::read_to_string(&codex).unwrap(), original);

    // Apply writes, preserving the user's content, with no key in the file.
    let (ok, out) = env.run(&["apply"], None);
    assert!(ok, "{out}");
    let applied = std::fs::read_to_string(&codex).unwrap();
    assert!(applied.contains("# mine") && applied.contains("trust_level"));
    assert!(applied.contains("model = \"glm-5.3\"") && !applied.contains(KEY));
    let state = std::fs::read_to_string(env.home.path().join("turnout/state.json")).unwrap();
    assert!(state.contains("mimo-v2.6-pro"), "{state}");

    // Rollback restores the exact original bytes; the generated picker catalog
    // did not exist before the apply, so it is removed again.
    let catalog = codex.parent().unwrap().join("codex-model-catalog.json");
    assert!(catalog.is_file(), "picker catalog was not written");
    let (ok, out) = env.run(&["rollback"], None);
    assert!(ok && out.contains("restored 2 file"), "{out}");
    assert_eq!(std::fs::read_to_string(&codex).unwrap(), original);
    assert!(!catalog.exists(), "rollback left the picker catalog behind");

    // The vault on disk must not contain the key in clear text either.
    let vault = std::fs::read_to_string(env.home.path().join("turnout/vault.json")).unwrap();
    assert!(!vault.contains(KEY));
}

#[test]
fn stale_plan_is_refused_instead_of_clobbering_user_edits() {
    let env = Env::new();
    let cfg = env.home.path().join("settings.json");
    std::fs::write(&cfg, "{}\n").unwrap();
    let cfg_arg = cfg.to_str().unwrap();
    assert!(
        env.run(
            &[
                "providers",
                "add",
                "proxy",
                "--base-url",
                "https://p.test/v1",
                "--dialect",
                "anthropic-messages"
            ],
            None
        )
        .0
    );
    assert!(
        env.run(
            &[
                "plan",
                "claude",
                "--provider",
                "proxy",
                "--model",
                "m",
                "--config",
                cfg_arg
            ],
            None
        )
        .0
    );
    std::fs::write(&cfg, "{\"edited\": true}\n").unwrap(); // user edits after planning
    let (ok, out) = env.run(&["apply"], None);
    assert!(!ok && out.contains("changed since the plan"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&cfg).unwrap(),
        "{\"edited\": true}\n"
    );
}

#[test]
fn native_oauth_cli_profile_does_not_create_a_shared_config_plan() {
    let env = Env::new();
    let (ok, out) = env.run(
        &[
            "plan-targets",
            "codex",
            "--target",
            "openai::gpt-6.1-sol",
            "--default-target",
            "openai::gpt-6.1-sol",
            "--profile",
            "gui-codex",
        ],
        None,
    );
    assert!(ok, "{out}");
    assert!(
        out.contains("official OAuth stays inside the harness"),
        "{out}"
    );

    let root = env.home.path().join("turnout");
    assert!(!root.join("plans/codex.json").exists());
    let state = std::fs::read_to_string(root.join("state.json")).unwrap();
    assert!(state.contains("gui-codex"), "{state}");
    assert!(state.contains("gpt-6.1-sol"), "{state}");
    assert!(state.contains("\"via_gateway\": false"), "{state}");
}

#[test]
fn claude_code_with_openai_style_provider_is_routed_through_the_gateway() {
    let env = Env::new();
    let cfg = env.home.path().join("settings.json");
    assert!(
        env.run(
            &[
                "providers",
                "add",
                "evren",
                "--base-url",
                "https://e.test/v1"
            ],
            None
        )
        .0
    );
    let (ok, out) = env.run(
        &[
            "plan",
            "claude",
            "--provider",
            "evren",
            "--model",
            "glm-5.3",
            "--model",
            "mimo-v2.6-pro",
            "--config",
            cfg.to_str().unwrap(),
        ],
        None,
    );
    assert!(ok, "{out}");
    assert!(
        out.contains("background Turnout Agent") && out.contains("port 47821"),
        "{out}"
    );
    assert!(out.contains("ANTHROPIC_BASE_URL"), "{out}");
    assert!(out.contains("apiKeyHelper"), "{out}");
    assert!(
        !cfg.exists(),
        "planning an editor gateway route must not write config"
    );

    let (ok, out) = env.run(&["apply", "--harness", "claude"], None);
    assert!(ok, "{out}");
    let applied = std::fs::read_to_string(&cfg).unwrap();
    assert!(applied.contains("127.0.0.1:47821"));
    assert!(applied.contains("apiKeyHelper"));
    assert!(applied.contains("modelPicker"));
    assert!(applied.contains("mimo-v2.6-pro"));
    assert!(!applied.contains(KEY));
}

#[test]
fn evren_preset_needs_only_a_key() {
    let env = Env::new();
    let (ok, out) = env.run(
        &["providers", "add", "evren", "--key-stdin"],
        Some(&format!("{KEY}\n")),
    );
    assert!(ok, "{out}");
    let (ok, out) = env.run(&["providers", "list"], None);
    assert!(ok, "{out}");
    assert!(out.contains("https://evren-llmapi.ssyz.org.tr/v1"), "{out}");
    assert!(out.contains("openai-chat"), "{out}");
}

#[test]
fn provider_list_json_is_machine_readable_and_never_contains_secrets() {
    let env = Env::new();
    assert!(
        env.run(
            &["providers", "add", "evren", "--key-stdin"],
            Some(&format!("{KEY}\n")),
        )
        .0
    );

    let (ok, out) = env.run(&["providers", "list", "--json"], None);
    assert!(ok, "{out}");
    let providers: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(providers[0]["id"], "evren");
    assert_eq!(providers[0]["dialect"], "openai-chat");
    assert_eq!(providers[0]["has_key"], true);
    assert!(providers[0].get("credential").is_none());
    assert!(providers[0].get("api_key").is_none());
}

#[test]
fn bad_input_gives_clear_errors() {
    let env = Env::new();
    let (ok, out) = env.run(&["providers", "add", "weird"], None);
    assert!(!ok && out.contains("--base-url"), "{out}");
    let (ok, out) = env.run(&["plan", "nope", "--provider", "x", "--model", "m"], None);
    assert!(!ok && out.contains("unknown harness"), "{out}");
    let (ok, out) = env.run(&["models", "list", "ghost"], None);
    assert!(!ok && out.contains("unknown provider"), "{out}");
}

#[test]
fn legacy_relay_env_names_still_work_and_the_new_name_wins() {
    let env = Env::new();
    let legacy_home = env.home.path().join("legacy-home");
    let new_home = env.home.path().join("new-home");

    // Pre-rename variables only: the tool must still find its state directory.
    let (ok, out) = env.run_legacy(
        &[
            ("RELAY_HOME", legacy_home.to_str().unwrap()),
            ("RELAY_FORCE_VAULT", "1"),
            ("RELAY_VAULT_PASSWORD", "test-master-password"),
        ],
        &["providers", "add", "evren", "--key-stdin"],
        Some(&format!("{KEY}\n")),
    );
    assert!(ok, "{out}");
    assert!(legacy_home.join("providers.json").exists(), "{out}");
    assert!(!env
        .home
        .path()
        .join("turnout")
        .join("providers.json")
        .exists());

    // The new name takes precedence when both are set.
    let (ok, out) = env.run_with(
        &[
            ("RELAY_HOME", legacy_home.to_str().unwrap()),
            ("TURNOUT_HOME", new_home.to_str().unwrap()),
        ],
        &["providers", "add", "evren", "--key-stdin"],
        Some(&format!("{KEY}\n")),
    );
    assert!(ok, "{out}");
    assert!(new_home.join("providers.json").exists(), "{out}");
}

#[test]
fn version_and_help_use_the_new_binary_name() {
    let env = Env::new();
    let (ok, out) = env.run(&["--version"], None);
    assert!(ok && out.contains("turnout"), "{out}");
    let (ok, out) = env.run(&["--help"], None);
    assert!(ok && out.contains("Usage: turnout"), "{out}");
    assert!(!out.contains("Usage: relay"), "{out}");
}

#[test]
fn a_provider_url_must_be_http_and_cannot_be_a_local_file() {
    // `file:///etc/passwd` and `data:` parse as URLs, so a bare `Url::parse`
    // accepted them and stored a provider that could never serve a request.
    let env = Env::new();
    for bad in ["file:///etc/passwd", "data:text/plain,x", "ftp://example.test/v1"] {
        let (ok, out) = env.run(
            &[
                "providers", "add", "badurl", "--base-url", bad, "--key-stdin",
            ],
            Some(&format!("{KEY}\n")),
        );
        assert!(!ok, "{bad} was accepted: {out}");
        assert!(out.contains("http://"), "{out}");
    }
    let (ok, out) = env.run(
        &["providers", "add", "hostless", "--base-url", "http://", "--key-stdin"],
        Some(&format!("{KEY}\n")),
    );
    assert!(!ok, "a hostless URL was accepted: {out}");

    let (ok, out) = env.run(
        &[
            "providers",
            "add",
            "goodurl",
            "--base-url",
            "https://api.example.test/v1",
            "--key-stdin",
        ],
        Some(&format!("{KEY}\n")),
    );
    assert!(ok, "{out}");
}

#[test]
fn rollback_rejects_a_backup_id_that_leaves_the_backups_directory() {
    // `turnout rollback ..` joined straight onto the data dir before this was
    // checked. The bridge passes the same value through `turnout rollback`.
    let env = Env::new();
    for bad in ["..", "../..", "/etc", "../../Library"] {
        let (ok, out) = env.run(&["rollback", bad], None);
        assert!(!ok, "{bad} was accepted: {out}");
        assert!(out.contains("backup not found"), "{out}");
    }
}

#[test]
fn parallel_agent_settings_limits_and_explicit_fallback_chains() {
    let env = Env::new();
    let (ok, out) = env.run(
        &["providers", "add", "evren", "--base-url", "https://api.example.test/v1"],
        None,
    );
    assert!(ok, "{out}");

    // A concurrency limit is stored, shown, kept across a re-add and cleared.
    let (ok, out) = env.run(&["providers", "limit", "evren", "--max-concurrent", "3"], None);
    assert!(ok && out.contains("at most 3"), "{out}");
    let (ok, out) = env.run(
        &["providers", "add", "evren", "--base-url", "https://api.example.test/v1"],
        None,
    );
    assert!(ok, "{out}");
    let (ok, out) = env.run(&["providers", "list", "--json"], None);
    assert!(ok && out.contains("\"max_concurrent\":3"), "{out}");
    let (ok, out) = env.run(&["providers", "limit", "evren", "--max-concurrent", "0"], None);
    assert!(!ok && out.contains("at least 1"), "{out}");
    let (ok, out) = env.run(&["providers", "limit", "evren", "--clear"], None);
    assert!(ok && out.contains("no concurrency limit"), "{out}");

    // Fallback is off unless asked for; a chain names exactly where one model
    // may go. Saved as a launch-only profile so no harness config is touched.
    let plan = |extra: &[&str]| {
        let mut args = vec![
            "plan-targets",
            "claude-code",
            "--target",
            "evren::deepseek-v4.1-flash",
            "--target",
            "evren::glm-5.3",
            "--target",
            "evren::mimo-v2.6-pro",
            "--default-target",
            "evren::deepseek-v4.1-flash",
            "--profile",
            "workers",
        ];
        args.extend_from_slice(extra);
        env.run(&args, None)
    };
    let saved_route = || {
        let (_, out) = env.run(&["profiles", "--json"], None);
        let profiles: serde_json::Value = serde_json::from_str(&out).unwrap();
        profiles["harnesses"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["harness"] == "claude-code")
            .and_then(|h| h["profiles"].as_array())
            .and_then(|p| p.iter().find(|p| p["name"] == "workers"))
            .map(|p| p["route"].clone())
            .expect("profile saved")
    };
    let (ok, out) = plan(&[]);
    assert!(ok, "{out}");
    let route = saved_route();
    assert_ne!(route["fallback"], serde_json::json!(true), "{route}");
    assert!(route["targets"][0].get("fallbacks").is_none(), "{route}");

    let (ok, out) = plan(&[
        "--fallback-chain",
        "evren::deepseek-v4.1-flash=evren::glm-5.3",
    ]);
    assert!(ok, "{out}");
    let route = saved_route();
    assert_eq!(route["targets"][0]["model"], "deepseek-v4.1-flash", "{route}");
    assert_eq!(route["targets"][0]["fallbacks"], serde_json::json!(["glm-5.3"]), "{route}");
    assert!(route["targets"][1].get("fallbacks").is_none(), "{route}");

    // A chain may only point at models of the same route, and not at itself.
    let (ok, out) = plan(&["--fallback-chain", "evren::deepseek-v4.1-flash=openai::gpt-6.1-sol"]);
    assert!(!ok && out.contains("not a --target of this route"), "{out}");
    let (ok, out) = plan(&[
        "--fallback-chain",
        "evren::deepseek-v4.1-flash=evren::deepseek-v4.1-flash",
    ]);
    assert!(!ok && out.contains("cannot fall back to itself"), "{out}");
}

#[test]
fn agents_put_workers_on_routed_models_and_roll_back() {
    let env = Env::new();
    let claude_dir = env.home.path().join("claude");
    let project = env.home.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let settings = claude_dir.join("settings.json");
    let run = |args: &[&str]| {
        env.run_with(
            &[("CLAUDE_CONFIG_DIR", claude_dir.to_str().unwrap())],
            args,
            None,
        )
    };
    let project_arg = project.to_str().unwrap();

    let (ok, out) = run(&["providers", "add", "evren", "--base-url", "https://api.example.test/v1"]);
    assert!(ok, "{out}");
    // Manager: Opus on Claude's own login. Worker model: DeepSeek via the gateway.
    let (ok, out) = run(&[
        "plan-targets",
        "claude-code",
        "--hybrid",
        "--target",
        "anthropic::claude-opus-5-5",
        "--target",
        "evren::deepseek-v4.1-flash",
        "--default-target",
        "anthropic::claude-opus-5-5",
        "--config",
        settings.to_str().unwrap(),
    ]);
    assert!(ok, "{out}");
    let (ok, out) = run(&["apply", "--harness", "claude-code"]);
    assert!(ok, "{out}");

    let (ok, out) = run(&[
        "agents", "plan", "deepseek-worker", "--model", "evren::deepseek-v4.1-flash",
        "--project", project_arg, "--parallel", "3",
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("+model: \"deepseek-v4.1-flash\""), "{out}");
    assert!(out.contains("+isolation: worktree"), "{out}");
    assert!(out.contains("providers limit evren"), "warns that 3 is not enforced: {out}");
    let worker = project.join(".claude/agents/deepseek-worker.md");
    assert!(!worker.exists(), "plan writes nothing");

    let (ok, out) = run(&["agents", "apply"]);
    assert!(ok, "{out}");
    assert!(worker.exists());

    let (ok, out) = run(&["agents", "list", "--project", project_arg, "--json"]);
    assert!(ok, "{out}");
    let listed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(listed["manager"], "claude-opus-5-5", "{listed}");
    assert_eq!(listed["agents"][0]["name"], "deepseek-worker", "{listed}");
    assert_eq!(listed["agents"][0]["routable"], true, "{listed}");
    assert_eq!(listed["agents"][0]["managed"], true, "{listed}");

    // A model the route does not serve would fail mid-task; refuse it now.
    let (ok, out) = run(&[
        "agents", "plan", "gpt-worker", "--model", "openai::gpt-6.1-sol", "--project", project_arg,
    ]);
    assert!(!ok && out.contains("not served by the claude-code route"), "{out}");

    // A hand-written agent is never taken over.
    std::fs::write(project.join(".claude/agents/mine.md"), "---\nname: mine\ndescription: x\n---\nhi\n").unwrap();
    let (ok, out) = run(&[
        "agents", "plan", "mine", "--model", "evren::deepseek-v4.1-flash", "--project", project_arg,
    ]);
    assert!(!ok && out.contains("not written by Turnout"), "{out}");

    // Changing the manager rewrites shared Claude Code settings, so it is
    // never applied without a yes: no terminal and no --yes saves a plan only.
    let before = std::fs::read_to_string(&settings).unwrap();
    let (ok, out) = run(&["agents", "manager", "evren::deepseek-v4.1-flash"]);
    assert!(ok && out.contains("not applied"), "{out}");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), before);
    let (_, out) = run(&["agents", "list", "--project", project_arg, "--json"]);
    assert!(out.contains("\"manager\":\"claude-opus-5-5\""), "{out}");
    let (ok, out) = run(&["agents", "manager", "evren::deepseek-v4.1-flash", "--yes"]);
    assert!(ok, "{out}");
    let (_, out) = run(&["agents", "list", "--project", project_arg, "--json"]);
    assert!(out.contains("\"manager\":\"deepseek-v4.1-flash\""), "{out}");
    let (ok, out) = run(&["agents", "manager", "anthropic::claude-opus-5-5", "--yes"]);
    assert!(ok, "{out}");
    let (_, out) = run(&["agents", "list", "--project", project_arg, "--json"]);
    assert!(out.contains("\"manager\":\"claude-opus-5-5\""), "{out}");
    let (ok, out) = run(&["agents", "manager", "openai::gpt-6.1-sol", "--yes"]);
    assert!(!ok && out.contains("not part of the claude-code route"), "{out}");

    let (ok, out) = run(&["agents", "rollback"]);
    assert!(ok, "{out}");
    assert!(!worker.exists(), "rolling back a new worker removes it");
    assert!(project.join(".claude/agents/mine.md").exists());
}

#[test]
fn opencode_workers_are_subagents_on_the_gateway_provider() {
    let env = Env::new();
    let config = env.home.path().join("opencode").join("opencode.json");
    let project = env.home.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_arg = project.to_str().unwrap();
    let run = |args: &[&str]| {
        env.run_with(&[("OPENCODE_CONFIG", config.to_str().unwrap())], args, None)
    };

    let (ok, out) = run(&["providers", "add", "evren", "--base-url", "https://api.example.test/v1"]);
    assert!(ok, "{out}");
    let (ok, out) = run(&[
        "plan-targets", "opencode",
        "--target", "evren::glm-5.3",
        "--target", "evren::deepseek-v4.1-flash",
        "--default-target", "evren::glm-5.3",
        "--config", config.to_str().unwrap(),
    ]);
    assert!(ok, "{out}");
    let (ok, out) = run(&["apply", "--harness", "opencode"]);
    assert!(ok, "{out}");

    let (ok, out) = run(&[
        "agents", "plan", "deepseek-worker", "--harness", "opencode",
        "--model", "evren::deepseek-v4.1-flash", "--project", project_arg,
    ]);
    assert!(ok, "{out}");
    // OpenCode reaches every gateway model through the `turnout` provider.
    assert!(out.contains("+model: \"turnout/deepseek-v4.1-flash\""), "{out}");
    assert!(out.contains("+mode: subagent"), "{out}");
    assert!(out.contains("no worktree isolation"), "{out}");
    let (ok, out) = run(&["agents", "apply", "--harness", "opencode"]);
    assert!(ok, "{out}");
    let worker = project.join(".opencode/agent/deepseek-worker.md");
    assert!(worker.exists());

    let (ok, out) = run(&["agents", "list", "--harness", "opencode", "--project", project_arg, "--json"]);
    assert!(ok, "{out}");
    let listed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(listed["manager"], "glm-5.3", "{listed}");
    assert_eq!(listed["agents"][0]["name"], "deepseek-worker", "{listed}");
    assert_eq!(listed["agents"][0]["routable"], true, "{listed}");

    // Claude-only options are refused rather than silently dropped.
    let (ok, out) = run(&[
        "agents", "plan", "x", "--harness", "opencode", "--model", "evren::glm-5.3",
        "--tools", "Read", "--project", project_arg,
    ]);
    assert!(!ok && out.contains("only supported for claude-code"), "{out}");

    let (ok, out) = run(&["agents", "manager", "--harness", "opencode", "evren::deepseek-v4.1-flash", "--yes"]);
    assert!(ok, "{out}");
    let (_, out) = run(&["agents", "list", "--harness", "opencode", "--project", project_arg, "--json"]);
    assert!(out.contains("\"manager\":\"deepseek-v4.1-flash\""), "{out}");

    let (ok, out) = run(&["agents", "rollback"]);
    assert!(ok && out.contains("restart OpenCode"), "{out}");
    assert!(!worker.exists());
}
