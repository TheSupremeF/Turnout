//! Full-screen terminal UI. Launched by running the CLI with no subcommand.

mod app;
mod data;
mod theme;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use crate::app::App;
use crate::data::Backend;

/// Runs the interface until the user quits. `exe` is the CLI binary every
/// action is delegated to (normally the current executable).
pub fn run(exe: PathBuf) -> io::Result<()> {
    let mut terminal = ratatui::init();
    set_window_title(theme::APP_NAME)?;
    let result = event_loop(&mut terminal, App::new(Backend::new(exe)));
    ratatui::restore();
    result
}

/// Terminal window/tab title, so the product name is visible outside the logo.
fn set_window_title(title: &str) -> io::Result<()> {
    use std::io::Write;
    let mut stdout = io::stdout();
    write!(stdout, "\x1b]0;{title}\x07")?;
    stdout.flush()
}

fn event_loop(terminal: &mut DefaultTerminal, mut app: App) -> io::Result<()> {
    app.start();
    while !app.quit {
        terminal.draw(|frame| ui::render(frame, &mut app))?;
        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key);
                }
            }
        }
        if let Some(args) = app.pending_run.take() {
            hand_over(terminal, &mut app, &args)?;
        }
        app.drain();
        app.tick = app.tick.wrapping_add(1);
    }
    Ok(())
}

/// Gives the terminal to a child process (`turnout run ...`) and takes it back.
fn hand_over(terminal: &mut DefaultTerminal, app: &mut App, args: &[String]) -> io::Result<()> {
    ratatui::restore();
    println!("▶ turnout {}\n", args.join(" "));
    let status = std::process::Command::new(app.exe()).args(args).status();
    *terminal = ratatui::init();
    terminal.clear()?;
    match status {
        Ok(status) if status.success() => app.say("Çalıştırma bitti", true),
        Ok(status) => app.say(
            format!("Çıkış kodu: {}", status.code().unwrap_or(-1)),
            false,
        ),
        Err(error) => app.say(format!("Başlatılamadı: {error}"), false),
    }
    app.start();
    Ok(())
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    use crate::app::{
        AgentsLoad, App, Catalog, Cmd, Confirm, ConfirmKind, ConnMode, Menu, MenuItem, MenuKind,
        Modal, Msg, ReviewAction, COMMANDS,
    };
    use crate::data::{Backend, HarnessInfo, OauthInfo, ProviderInfo, Selection, Target, CLAUDE};
    use crate::ui;

    /// An app with fixed data and no background threads.
    fn app() -> App {
        let mut app = App::new(Backend::new("/nonexistent/turnout".into()));
        app.saved.clear();
        app.catalog.clear(); // never inherit the developer machine's cache
        app.drafts = vec![Selection::default(); 3];
        app.harnesses = vec![
            HarnessInfo {
                installed: true,
                detail: "codex-cli 0.162".into(),
            },
            HarnessInfo {
                installed: true,
                detail: "2.1.153 (Claude Code)".into(),
            },
            HarnessInfo {
                installed: false,
                detail: "kurulu değil".into(),
            },
        ];
        app.providers = vec![
            ProviderInfo {
                id: "evren".into(),
                has_key: true,
                max_concurrent: None,
            },
            ProviderInfo {
                id: "openai".into(),
                has_key: true,
                max_concurrent: None,
            },
        ];
        app.oauth = vec![OauthInfo {
            id: "anthropic".into(),
            logged_in: true,
            plan: "pro".into(),
            gateway_capable: false,
        }];
        app.catalog.insert(
            "evren".into(),
            Catalog::Ready(vec![
                "deepseek-v4.1-flash".into(),
                "glm-5.3".into(),
                "qwen3-embedding-8b".into(),
            ]),
        );
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui::render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn home_shows_the_logo_every_harness_and_the_key_hints() {
        let mut app = app();
        let text = screen(&mut app, 110, 32);
        for expected in [
            "█▀▀█",
            "Codex",
            "Claude Code",
            "OpenCode",
            "kurulu değil",
            "uygula",
            "bağlantılar",
        ] {
            assert!(text.contains(expected), "missing {expected:?}\n{text}");
        }
    }

    #[test]
    fn small_terminals_drop_the_logo_but_keep_the_list() {
        let mut app = app();
        let text = screen(&mut app, 80, 18);
        assert!(
            text.contains("Claude Code") && text.contains("OpenCode"),
            "{text}"
        );
    }

    #[test]
    fn picking_models_across_providers_keeps_every_choice() {
        let mut app = app();
        press(&mut app, KeyCode::Down); // Claude Code
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char(' ')); // evren: deepseek
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter); // evren: glm becomes default
        press(&mut app, KeyCode::Tab); // openai (no catalog -> fallback only on failure)
        press(&mut app, KeyCode::Tab); // anthropic
        press(&mut app, KeyCode::Char(' '));

        let draft = &app.drafts[CLAUDE];
        assert_eq!(draft.targets.len(), 3);
        assert_eq!(
            draft.default_target(),
            Some(&Target::new("evren", "glm-5.3"))
        );
        assert!(draft.contains(&Target::new("anthropic", "claude-opus-5-5")));

        let text = screen(&mut app, 110, 32);
        assert!(text.contains("Orijinal Claude listesini koru"), "{text}");
        press(&mut app, KeyCode::Esc);
        let text = screen(&mut app, 110, 32);
        assert!(
            text.contains("glm-5.3") && text.contains("+2 model"),
            "{text}"
        );
        assert!(text.contains("uygulanmamış değişiklik"), "{text}");
    }

    #[test]
    fn the_full_catalog_is_listed_with_non_chat_models_tagged_and_search_filters() {
        let mut app = app();
        press(&mut app, KeyCode::Enter);
        let text = screen(&mut app, 110, 32);
        assert!(
            text.contains("deepseek-v4.1-flash") && text.contains("qwen3-embedding-8b"),
            "{text}"
        );
        assert!(text.contains("sohbet modeli değil"), "{text}");
        type_text(&mut app, "glm");
        let text = screen(&mut app, 110, 32);
        assert!(
            text.contains("glm-5.3") && !text.contains("deepseek-v4.1-flash"),
            "{text}"
        );
    }

    #[test]
    fn original_list_toggle_exists_for_claude_only() {
        let mut app = app();
        press(&mut app, KeyCode::Enter); // Codex
        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(!app.drafts[0].hybrid);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.drafts[CLAUDE].hybrid);
    }

    #[test]
    fn anthropic_subscription_is_labelled_claude_code_only_outside_claude() {
        let mut app = app();
        press(&mut app, KeyCode::Enter); // Codex picker
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab); // anthropic tab
        let text = screen(&mut app, 110, 32);
        assert!(text.contains("yalnızca Claude Code"), "{text}");
    }

    #[test]
    fn planning_without_a_model_explains_instead_of_calling_the_cli() {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        assert!(app.modal.is_none());
        let text = screen(&mut app, 110, 32);
        assert!(text.contains("En az bir model seçin"), "{text}");
    }

    #[test]
    fn plan_review_colours_a_diff_and_any_key_closes_messages() {
        let mut app = app();
        app.modal = Some(Modal::Busy("x".into()));
        app.apply_message(Msg::Plan(
            0,
            Ok("--- a/x\n+++ b/x\n+added\n-removed\nnote: hi\n".into()),
        ));
        let text = screen(&mut app, 110, 32);
        assert!(
            text.contains("+added") && text.contains("Plan — Codex"),
            "{text}"
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());

        app.apply_message(Msg::Applied(Err("boom".into())));
        assert!(screen(&mut app, 110, 32).contains("boom"));
        press(&mut app, KeyCode::Char('x'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn api_keys_are_masked_while_typing() {
        let mut app = app();
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "sk-ant-secret");
        let text = screen(&mut app, 110, 32);
        assert!(
            !text.contains("sk-ant-secret") && text.contains("•••••••••••••"),
            "{text}"
        );
    }

    #[test]
    fn every_command_is_in_the_palette_and_search_finds_it() {
        use std::collections::HashSet;
        let unique = COMMANDS
            .iter()
            .map(|(cmd, _, _)| *cmd)
            .collect::<HashSet<_>>();
        assert_eq!(unique.len(), COMMANDS.len(), "no command is listed twice");
        // The CLI operations the palette must cover.
        for cli in [
            "rollback",
            "gateway status",
            "gateway install",
            "gateway uninstall",
            "auth login openai",
            "auth login anthropic",
            "credentials status",
            "credentials migrate",
            "doctor",
            "doctor --online",
            "run",
            "run --profile",
            "plan-targets --profile",
            "providers",
        ] {
            assert!(
                COMMANDS.iter().any(|(_, _, detail)| *detail == cli),
                "palette misses `{cli}`"
            );
        }
        let mut app = app();
        press(&mut app, KeyCode::Char(':'));
        assert!(screen(&mut app, 110, 34).contains("Yedekten geri al"));
        type_text(&mut app, "agent");
        let text = screen(&mut app, 110, 34);
        assert!(
            text.contains("Turnout Agent durumu") && !text.contains("Yedekten geri al"),
            "{text}"
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
        press(&mut app, KeyCode::Esc);
        assert!(!app.quit, "Esc on the home screen must not quit");
        press(&mut app, KeyCode::Char(':'));
        type_text(&mut app, "no-such-command");
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(app.modal, Some(Modal::Palette(_))),
            "no match keeps the palette open"
        );
    }

    #[test]
    fn run_needs_an_applied_route_and_hands_the_terminal_over() {
        let mut app = app();
        app.run_cmd(Cmd::Run);
        assert!(app.pending_run.is_none());
        assert!(screen(&mut app, 110, 34).contains("önce bir yönlendirmeyi uygulayın"));
        app.saved.insert(
            "codex".into(),
            Selection {
                targets: vec![Target::new("openai", "gpt-6.1-sol")],
                ..Selection::default()
            },
        );
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            app.pending_run,
            Some(vec!["run".to_string(), "codex".into()])
        );
    }

    #[test]
    fn running_a_profile_passes_the_harness_and_profile_name() {
        let mut app = app();
        let mut list = ratatui::widgets::ListState::default();
        list.select(Some(0));
        app.modal = Some(Modal::Menu(Menu {
            kind: MenuKind::RunProfile,
            title: "Profille çalıştır".into(),
            items: vec![MenuItem {
                label: "Claude Code — evren".into(),
                detail: "glm-5.3".into(),
                value: "claude-code\nevren".into(),
            }],
            list,
        }));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.pending_run,
            Some(vec![
                "run".to_string(),
                "claude-code".into(),
                "--profile".into(),
                "evren".into()
            ])
        );
    }

    #[test]
    fn saving_a_profile_validates_the_name_and_never_keeps_the_original_list() {
        let mut app = app();
        app.drafts[CLAUDE].set_default(&Target::new("evren", "glm-5.3"));
        app.drafts[CLAUDE].hybrid = true;
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.modal, Some(Modal::Input(_))));
        type_text(&mut app, "bad name!");
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(app.modal, Some(Modal::Input(_))),
            "invalid name keeps the dialog open"
        );
        for _ in 0..9 {
            press(&mut app, KeyCode::Backspace);
        }
        type_text(&mut app, "evren");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::Busy(_))));
        let args = app.drafts[CLAUDE]
            .profile_args("claude-code", "evren")
            .unwrap();
        assert!(!args.contains(&"--hybrid".to_string()));
        assert!(args.windows(2).any(|pair| pair == ["--profile", "evren"]));
    }

    #[test]
    fn rollback_always_asks_before_restoring() {
        let mut app = app();
        let mut list = ratatui::widgets::ListState::default();
        list.select(Some(0));
        app.modal = Some(Modal::Menu(Menu {
            kind: MenuKind::Rollback,
            title: "Yedekten geri al".into(),
            items: vec![MenuItem {
                label: "En son yedek".into(),
                detail: "".into(),
                value: String::new(),
            }],
            list,
        }));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            app.modal,
            Some(Modal::Confirm(Confirm {
                kind: ConfirmKind::Rollback(_),
                ..
            }))
        ));
        press(&mut app, KeyCode::Char('n'));
        assert!(app.modal.is_none(), "anything but y cancels");
        app.modal = Some(Modal::Confirm(Confirm {
            kind: ConfirmKind::AgentUninstall,
            text: "x".into(),
        }));
        press(&mut app, KeyCode::Char('y'));
        assert!(matches!(app.modal, Some(Modal::Busy(_))));
    }

    #[test]
    fn command_output_is_shown_even_when_the_command_failed() {
        let mut app = app();
        app.modal = Some(Modal::Busy("x".into()));
        app.apply_message(Msg::Cli {
            title: "Sağlık kontrolü".into(),
            ok: false,
            text: "[ ok ] data dir: /x\n[FAIL] opencode: not installed\nerror: 1 check(s) failed"
                .into(),
            refresh: false,
        });
        let text = screen(&mut app, 110, 34);
        assert!(
            text.contains("Sağlık kontrolü") && text.contains("[FAIL] opencode"),
            "{text}"
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
    }

    #[test]
    fn a_custom_provider_form_collects_four_fields_and_masks_the_key() {
        let mut app = app();
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('u'));
        type_text(&mut app, "OpenRouter");
        press(&mut app, KeyCode::Tab);
        type_text(&mut app, "https://openrouter.ai/api/v1");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Tab);
        type_text(&mut app, "sk-or-secret");
        let text = screen(&mut app, 110, 40);
        assert!(
            text.contains("openrouter") && text.contains("openrouter.ai"),
            "{text}"
        );
        assert!(text.contains("openai-responses"), "{text}");
        assert!(
            !text.contains("sk-or-secret") && text.contains("••••••••••••"),
            "{text}"
        );
        match &app.modal {
            Some(Modal::Conn(conn)) => {
                assert!(matches!(conn.mode, ConnMode::Custom { field: 3, .. }))
            }
            _ => panic!("form closed"),
        }
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::Busy(_))));
    }

    #[test]
    fn quitting_works_from_home_and_with_ctrl_c_inside_a_dialog() {
        let mut app = app();
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
        let mut app = self::app();
        press(&mut app, KeyCode::Enter);
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn route_models_stay_deselectable_without_a_catalog() {
        let mut a = app();
        // Codex route made earlier: an OpenAI model whose catalog never loaded
        // here, and one from a provider that has since been removed.
        a.drafts[0].targets = vec![
            Target::new("openai", "gpt-6.1-sol"),
            Target::new("groq", "llama-5-70b"),
        ];
        assert!(a.catalog.get("openai").is_none());
        press(&mut a, KeyCode::Enter); // picker on EVREN
        press(&mut a, KeyCode::Right); // OpenAI
        let view = screen(&mut a, 120, 30);
        assert!(view.contains("gpt-6.1-sol"), "{view}");
        press(&mut a, KeyCode::Char(' '));
        assert_eq!(a.drafts[0].targets, vec![Target::new("groq", "llama-5-70b")]);

        assert!(a.tabs().contains(&"groq".to_string()));
        press(&mut a, KeyCode::Right); // Anthropic
        press(&mut a, KeyCode::Right); // groq, kept for its route model
        assert!(screen(&mut a, 120, 30).contains("llama-5-70b"));
        press(&mut a, KeyCode::Char(' '));
        assert!(a.drafts[0].targets.is_empty());
    }

    #[test]
    fn fallback_is_set_in_the_picker_and_shown_in_the_review() {
        let mut a = app();
        a.cursor = 1; // Claude Code
        press(&mut a, KeyCode::Enter); // picker on EVREN
        press(&mut a, KeyCode::Char(' ')); // pick deepseek
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Char(' ')); // pick glm
        press(&mut a, KeyCode::Up);
        let ctrl = |code| KeyEvent::new(code, KeyModifiers::CONTROL);
        a.handle_key(ctrl(KeyCode::Char('f')));
        assert!(a.drafts[1].fallback);
        assert!(screen(&mut a, 100, 30).contains("(açık)"));

        // Ctrl+L on deepseek: its chain can only name the other picked model.
        a.handle_key(ctrl(KeyCode::Char('l')));
        let chain_screen = screen(&mut a, 100, 30);
        assert!(chain_screen.contains("Fallback — deepseek-v4.1-flash"), "{chain_screen}");
        assert!(chain_screen.contains("glm-5.3"));
        press(&mut a, KeyCode::Char(' '));
        press(&mut a, KeyCode::Enter);
        let deepseek = Target::new("evren", "deepseek-v4.1-flash");
        assert_eq!(a.drafts[1].chains[&deepseek], vec![Target::new("evren", "glm-5.3")]);
        assert!(screen(&mut a, 100, 30).contains("↳ fallback: glm-5.3"));

        // The review lists what the diff cannot show.
        press(&mut a, KeyCode::Esc);
        a.modal = Some(Modal::Busy("plan".into()));
        a.apply_message(Msg::Plan(1, Ok("--- a/settings.json\n+++ b/settings.json".into())));
        let review = screen(&mut a, 100, 30);
        assert!(review.contains("TURNOUT AYARLARI"), "{review}");
        assert!(review.contains("deepseek-v4.1-flash → glm-5.3   yeni"), "{review}");
        assert!(review.contains("fallback (rota geneli)   açık   değişti"), "{review}");
    }

    #[test]
    fn a_provider_quota_is_shown_in_connections() {
        let mut a = app();
        press(&mut a, KeyCode::Char('c'));
        assert!(screen(&mut a, 100, 30).contains("u kota"));
        a.apply_message(Msg::Quota(
            "evren".into(),
            crate::data::parse_quota(
                r#"{"provider":"evren","level":"ok","credits_remaining":1060.0,"credits_held":0.0,
                "daily_limit_tokens":15000000,"daily_remaining_tokens":13407136,
                "daily_reset_at":"2026-10-28T00:00:00Z","window_minutes":5,
                "window_limit_tokens":2500000,"window_used_tokens":118069,
                "window_reset_at":"2026-10-27T09:55:00+00:00",
                "concurrency_limit":32,"concurrent_requests":2}"#,
            ),
        ));
        let view = screen(&mut a, 120, 34);
        assert!(view.contains("Kalan kota — EVREN"), "{view}");
        assert!(view.contains("1.060 CR"), "{view}");
        assert!(view.contains("13.407.136 / 15.000.000 kaldı"), "{view}");
        assert!(view.contains("28.10 00:00 UTC'de sıfırlanır"), "{view}");
        assert!(view.contains("118.069 / 2.500.000 kullanıldı"), "{view}");
        assert!(view.contains("2 / 32 istek sürüyor"), "{view}");
        press(&mut a, KeyCode::Esc);
        assert!(screen(&mut a, 100, 30).contains("u kota"));
    }

    #[test]
    fn a_headers_sourced_quota_is_shown_as_the_last_response() {
        let mut a = app();
        press(&mut a, KeyCode::Char('c'));
        let observed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 180;
        a.apply_message(Msg::Quota(
            "openai".into(),
            crate::data::parse_quota(&format!(
                r#"{{"provider":"openai","observed_at":{observed},"source":"headers",
                "minute_limit_tokens":150000,"minute_remaining_tokens":149780,
                "minute_reset":"12ms","minute_limit_requests":10000,
                "minute_remaining_requests":9998}}"#
            )),
        ));
        let view = screen(&mut a, 120, 34);
        assert!(view.contains("dakikalık token"), "{view}");
        assert!(view.contains("149.780 / 150.000 kaldı"), "{view}");
        assert!(view.contains("dakikalık istek"), "{view}");
        assert!(view.contains("9.998 / 10.000 kaldı"), "{view}");
        assert!(view.contains("son yanıttan · 3 dk önce"), "{view}");
    }

    #[test]
    fn a_quota_failure_returns_to_the_list() {
        let mut a = app();
        press(&mut a, KeyCode::Char('c'));
        a.apply_message(Msg::Quota("openai".into(), Err("no /quota endpoint".into())));
        assert!(matches!(a.modal, Some(Modal::Conn(_))));
        assert!(screen(&mut a, 100, 30).contains("Kota okunamadı"));
    }

    #[test]
    fn subscription_usage_is_shown_in_connections() {
        let mut a = app();
        press(&mut a, KeyCode::Char('c'));
        assert!(screen(&mut a, 120, 30).contains("o oturum kotası"));
        a.apply_message(Msg::Usage(crate::data::parse_usage(
            r#"[{"provider":"openai","plan":"plus","windows":[
                {"label":"5h","window_minutes":300,"used_percent":0.0,
                 "resets_at":"2026-10-28T00:48:58Z"},
                {"label":"weekly","window_minutes":10080,"used_percent":3.0,
                 "resets_at":"2026-11-02T05:29:31Z"}]},
              {"provider":"anthropic","plan":"pro","windows":[
                {"label":"5h","window_minutes":300,"used_percent":23.0,
                 "resets_at":"2026-10-28T00:10:00Z"},
                {"label":"weekly","window_minutes":10080,"used_percent":13.0,
                 "resets_at":"2026-10-30T19:00:00Z"}]}]"#,
        )));
        let view = screen(&mut a, 130, 40);
        assert!(view.contains("Abonelik kullanımı"), "{view}");
        assert!(view.contains("ChatGPT · plus"), "{view}");
        assert!(view.contains("Claude · pro"), "{view}");
        assert!(view.contains("5 saat"), "{view}");
        assert!(view.contains("haftalık"), "{view}");
        assert!(view.contains("%23 kullanıldı"), "{view}");
        assert!(view.contains("28.10 00:10 UTC'de sıfırlanır"), "{view}");
        press(&mut a, KeyCode::Esc);
        assert!(screen(&mut a, 120, 30).contains("o oturum kotası"));
    }

    #[test]
    fn a_subscription_usage_error_is_shown_in_red() {
        let mut a = app();
        press(&mut a, KeyCode::Char('c'));
        a.apply_message(Msg::Usage(crate::data::parse_usage(
            r#"[{"provider":"openai","plan":"plus","windows":[
                {"label":"5h","window_minutes":300,"used_percent":0.0,
                 "resets_at":"2026-10-28T00:48:58Z"}]},
              {"provider":"anthropic",
               "error":"Claude login expired — open Claude Code once to refresh it"}]"#,
        )));
        let view = screen(&mut a, 130, 40);
        assert!(view.contains("ChatGPT · plus"), "{view}");
        assert!(view.contains("Claude"), "{view}");
        assert!(view.contains("Claude login expired"), "{view}");
        assert!(view.contains("o oturum kotası") || view.contains("r yenile"), "{view}");
    }

    #[test]
    fn a_provider_limit_is_shown_and_validated_in_connections() {        let mut a = app();
        a.providers[0].max_concurrent = Some(4);
        press(&mut a, KeyCode::Char('c'));
        let list = screen(&mut a, 100, 30);
        assert!(list.contains("en fazla 4 aynı anda"), "{list}");
        assert!(list.contains("sınırsız"), "{list}");
        assert!(list.contains("l sınır"), "{list}");

        press(&mut a, KeyCode::Char('l'));
        press(&mut a, KeyCode::Backspace);
        type_text(&mut a, "0");
        press(&mut a, KeyCode::Enter);
        let error = screen(&mut a, 100, 30);
        assert!(error.contains("Sınır en az 1 olmalı"), "{error}");
        // Still in the editor: nothing was sent.
        assert!(matches!(a.modal, Some(Modal::Conn(_))));
        press(&mut a, KeyCode::Esc);
        assert!(screen(&mut a, 100, 30).contains("l sınır"));
    }

    fn with_claude_route(a: &mut App) {
        let opus = Target::new("anthropic", "claude-opus-5-5");
        let deepseek = Target::new("evren", "deepseek-v4.1-flash");
        let selection = Selection {
            targets: vec![opus.clone(), deepseek],
            default: Some(opus),
            hybrid: true,
            ..Selection::default()
        };
        a.saved.insert("claude-code".into(), selection.clone());
        a.drafts[1] = selection;
        a.agents.insert(
            1,
            AgentsLoad::Ready(crate::data::parse_agents(
                r#"{"manager":"claude-opus-5-5","agents":[
                  {"name":"deepseek-worker","model":"deepseek-v4.1-flash","scope":"project","managed":true,"routable":true,"path":".claude/agents/deepseek-worker.md"},
                  {"name":"my-helper","model":"sonnet","scope":"project","managed":false,"routable":true,"path":".claude/agents/my-helper.md"},
                  {"name":"gpt-worker","model":"gpt-6.1-sol","scope":"project","managed":true,"routable":false,"path":".claude/agents/gpt-worker.md"}]}"#,
            ).unwrap()),
        );
    }

    #[test]
    fn the_route_card_says_who_manages_how_many_workers_and_the_fallback() {
        let mut a = app();
        with_claude_route(&mut a);
        let home = screen(&mut a, 100, 30);
        assert!(home.contains("yönetici claude-opus-5-5 · 3 işçi · fallback kapalı"), "{home}");
        assert!(home.contains("w ajanlar"), "{home}");
        // Narrow terminals keep the essential keys.
        let narrow = screen(&mut a, 80, 24);
        assert!(narrow.contains("w ajanlar") && narrow.contains("q çık"), "{narrow}");
    }

    #[test]
    fn the_agents_window_lists_workers_flags_unserved_models_and_guards_hand_written_files() {
        let mut a = app();
        with_claude_route(&mut a);
        a.cursor = 1;
        press(&mut a, KeyCode::Char('w'));
        // Opening refreshes in the background; the test keeps its fixed data.
        with_claude_route(&mut a);
        let list = screen(&mut a, 100, 30);
        assert!(list.contains("YÖNETİCİ") && list.contains("claude-opus-5-5"), "{list}");
        assert!(list.contains("elle yazılmış"), "{list}");
        assert!(list.contains("✗ rotada yok"), "{list}");

        // The unserved worker explains how to fix it (WCAG 3.3.3).
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Down);
        let error = screen(&mut a, 100, 30);
        assert!(error.contains("gpt-6.1-sol bu Claude Code rotasında yok"), "{error}");

        // A hand-written file is never changed.
        press(&mut a, KeyCode::Up);
        press(&mut a, KeyCode::Char('d'));
        let guard = screen(&mut a, 100, 30);
        assert!(guard.contains("my-helper elle yazılmış"), "{guard}");
    }

    #[test]
    fn a_new_worker_needs_a_valid_name_and_a_routed_model() {
        let mut a = app();
        with_claude_route(&mut a);
        a.cursor = 1;
        press(&mut a, KeyCode::Char('w'));
        with_claude_route(&mut a);
        press(&mut a, KeyCode::Char('n'));
        type_text(&mut a, "DeepSeek Worker");
        press(&mut a, KeyCode::Enter);
        let error = screen(&mut a, 100, 30);
        assert!(error.contains("Ad yalnızca küçük harf, rakam ve tire"), "{error}");
        for _ in 0.."DeepSeek Worker".len() {
            press(&mut a, KeyCode::Backspace);
        }
        type_text(&mut a, "deepseek-2");
        press(&mut a, KeyCode::Enter);
        let models = screen(&mut a, 100, 30);
        assert!(models.contains("Yeni işçi 2/2"), "{models}");
        assert!(models.contains("deepseek-v4.1-flash") && models.contains("claude-opus-5-5"), "{models}");
        assert!(models.contains("[proje]"), "{models}");
        press(&mut a, KeyCode::Tab);
        assert!(screen(&mut a, 100, 30).contains("[kullanıcı]"));
    }

    #[test]
    fn a_manager_change_is_applied_only_with_y_and_warns_about_shared_settings() {
        let mut a = app();
        with_claude_route(&mut a);
        a.apply_message(Msg::AgentPlan(
            1,
            ReviewAction::Manager,
            Ok("manager: claude-opus-5-5 -> deepseek-v4.1-flash\n-  \"model\": \"claude-opus-5-5\"\n+  \"model\": \"deepseek-v4.1-flash\"\nnot applied: confirm with `turnout apply --harness claude-code`".into()),
        ));
        let review = screen(&mut a, 100, 30);
        assert!(review.contains("Paylaşılan ayar"), "{review}");
        assert!(review.contains("⏎ bu pencerede uygulamaz"), "{review}");
        assert!(!review.contains("not applied"), "{review}");
        press(&mut a, KeyCode::Enter);
        assert!(matches!(a.modal, Some(Modal::Review(_))), "Enter must not apply");
    }

    #[test]
    fn codex_and_routeless_harnesses_explain_instead_of_offering_actions() {
        let mut a = app();
        a.cursor = 0;
        press(&mut a, KeyCode::Char('w'));
        assert!(screen(&mut a, 100, 30).contains("Codex işçi dosyası kullanmaz"));
        let mut a = app();
        a.cursor = 2;
        press(&mut a, KeyCode::Char('w'));
        let empty = screen(&mut a, 100, 30);
        assert!(empty.contains("OpenCode için henüz bir Turnout rotası yok"), "{empty}");
    }
}
