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
        App, Catalog, Cmd, Confirm, ConfirmKind, ConnMode, Menu, MenuItem, MenuKind, Modal, Msg,
        COMMANDS,
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
            },
            ProviderInfo {
                id: "openai".into(),
                has_key: true,
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
}
