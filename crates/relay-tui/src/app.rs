//! Application state and input handling. Rendering lives in `ui.rs`.

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::data::{
    age, list_backups, load_catalog_cache, save_catalog_cache, saved_profiles, saved_routes,
    AgentsInfo, Backend, HarnessInfo, OauthInfo, ProviderInfo, Selection, Target,
    ANTHROPIC_MODELS, CLAUDE, HARNESSES, OPENAI_FALLBACK,
};

/// Harnesses with file-based workers; Codex picks sub-agent models itself.
pub const CODEX: usize = 0;

pub enum Catalog {
    Loading,
    Ready(Vec<String>),
    Failed(String),
}

pub struct Picker {
    pub harness: usize,
    pub tab: usize,
    pub query: String,
    pub list: ListState,
}

/// Editing one model's fallback chain, opened from the picker (Ctrl+L).
pub struct ChainEdit {
    /// The picker to return to.
    pub picker: Picker,
    pub from: Target,
    /// The chain being edited, in order.
    pub chain: Vec<Target>,
    pub cursor: usize,
}

/// What confirming a plan review does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReviewAction {
    /// `turnout apply` for the harness route.
    #[default]
    Route,
    /// `turnout agents apply` for a worker file.
    Agents,
    /// The manager: a shared-settings route change; only `y` applies it.
    Manager,
}

pub struct Review {
    pub harness: usize,
    pub text: String,
    pub scroll: u16,
    pub action: ReviewAction,
}

pub enum AgentsLoad {
    Loading,
    Ready(AgentsInfo),
    Failed(String),
}

pub enum AgentsMode {
    List,
    /// New worker, step 1.
    Name {
        input: String,
        error: Option<String>,
    },
    /// New worker step 2, or changing a worker's model (`editing`).
    Model {
        name: String,
        user_scope: bool,
        cursor: usize,
        editing: bool,
    },
    Manager {
        cursor: usize,
    },
}

pub struct AgentsView {
    pub harness: usize,
    pub cursor: usize,
    pub mode: AgentsMode,
}

pub enum ConnMode {
    List,
    Preset,
    Key {
        preset: String,
        input: String,
    },
    /// A provider with its own endpoint: id, base URL, protocol, key.
    Custom {
        field: usize,
        id: String,
        url: String,
        dialect: usize,
        secret: String,
    },
    ConfirmDelete(String),
    /// What the provider says is left of its credits and token budget.
    Quota {
        provider: String,
        quota: crate::data::QuotaInfo,
    },
    /// What the official logins report about their subscription limits.
    Usage(Vec<crate::data::UsageInfo>),
    /// Editing a provider's concurrency limit; empty input means unlimited.
    Limit {
        provider: String,
        input: String,
        error: Option<String>,
    },
}

pub const DIALECTS: [&str; 3] = ["openai-chat", "openai-responses", "anthropic-messages"];

/// Every operation the CLI offers, reachable from the palette.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cmd {
    Models,
    Apply,
    Test,
    Run,
    SaveProfile,
    RunProfile,
    Rollback,
    AgentStatus,
    AgentInstall,
    AgentUninstall,
    Providers,
    LoginOpenAi,
    LoginAnthropic,
    KeyStatus,
    KeyMigrate,
    KeyMigrateDelete,
    Doctor,
    DoctorOnline,
    Refresh,
    Help,
    Quit,
}

/// (command, label, the CLI command it stands for)
pub const COMMANDS: [(Cmd, &str, &str); 21] = [
    (Cmd::Models, "Model seç", "sağlayıcılar arası seçici"),
    (
        Cmd::Apply,
        "Değişiklikleri önizle ve uygula",
        "plan-targets + apply",
    ),
    (Cmd::Test, "Bağlantıyı test et", "test"),
    (Cmd::Run, "Seçili aracı çalıştır", "run"),
    (
        Cmd::SaveProfile,
        "Seçimi profil olarak kaydet",
        "plan-targets --profile",
    ),
    (Cmd::RunProfile, "Profille çalıştır", "run --profile"),
    (Cmd::Rollback, "Yedekten geri al", "rollback"),
    (Cmd::AgentStatus, "Turnout Agent durumu", "gateway status"),
    (
        Cmd::AgentInstall,
        "Turnout Agent'ı kur / yeniden başlat",
        "gateway install",
    ),
    (
        Cmd::AgentUninstall,
        "Turnout Agent'ı kaldır",
        "gateway uninstall",
    ),
    (
        Cmd::Providers,
        "Bağlantılar: sağlayıcılar ve API anahtarları",
        "providers",
    ),
    (
        Cmd::LoginOpenAi,
        "OpenAI / ChatGPT ile giriş",
        "auth login openai",
    ),
    (
        Cmd::LoginAnthropic,
        "Claude hesabıyla giriş",
        "auth login anthropic",
    ),
    (
        Cmd::KeyStatus,
        "Anahtar deposu durumu",
        "credentials status",
    ),
    (
        Cmd::KeyMigrate,
        "Keychain'deki anahtarları dosyaya kopyala",
        "credentials migrate",
    ),
    (
        Cmd::KeyMigrateDelete,
        "Keychain'den kopyala ve eskileri sil",
        "credentials migrate --delete-old",
    ),
    (Cmd::Doctor, "Sağlık kontrolü (doktor)", "doctor"),
    (
        Cmd::DoctorOnline,
        "Sağlık kontrolü (sağlayıcılara bağlanır)",
        "doctor --online",
    ),
    (Cmd::Refresh, "Her şeyi yeniden oku", ""),
    (Cmd::Help, "Kısayollar", ""),
    (Cmd::Quit, "Çık", ""),
];

pub struct Palette {
    pub query: String,
    pub list: ListState,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKind {
    RunProfile,
    Rollback,
}

pub struct MenuItem {
    pub label: String,
    pub detail: String,
    pub value: String,
}

pub struct Menu {
    pub kind: MenuKind,
    pub title: String,
    pub items: Vec<MenuItem>,
    pub list: ListState,
}

pub struct Input {
    pub title: String,
    pub prompt: String,
    pub value: String,
}

pub struct Output {
    pub title: String,
    pub text: String,
    pub ok: bool,
    pub scroll: u16,
}

pub enum ConfirmKind {
    Rollback(String),
    /// Undo the last `agents apply` for this harness.
    AgentRollback(usize),
    AgentUninstall,
    KeyMigrateDelete,
}

pub struct Confirm {
    pub kind: ConfirmKind,
    pub text: String,
}

pub struct Conn {
    pub cursor: usize,
    pub mode: ConnMode,
}

pub struct Message {
    pub title: String,
    pub text: String,
    pub ok: bool,
}

pub enum Modal {
    Palette(Palette),
    Menu(Menu),
    Input(Input),
    Output(Output),
    Confirm(Confirm),
    Picker(Picker),
    Chain(ChainEdit),
    Agents(AgentsView),
    Review(Review),
    Conn(Conn),
    Message(Message),
    Help,
    Busy(String),
}

pub struct Toast {
    pub text: String,
    pub ok: bool,
    pub at: Instant,
}

pub enum Msg {
    Harnesses(Vec<HarnessInfo>),
    Providers(Result<Vec<ProviderInfo>, String>),
    Oauth(Result<Vec<OauthInfo>, String>),
    Catalog(String, Result<Vec<String>, String>),
    Plan(usize, Result<String, String>),
    Applied(Result<String, String>),
    Tested(Result<String, String>),
    Provider(Result<String, String>),
    /// `providers limit` finished: (provider, new limit, CLI result).
    Limit(String, Option<u32>, Result<String, String>),
    /// `providers quota` finished.
    Quota(String, Result<crate::data::QuotaInfo, String>),
    /// `auth usage` finished.
    Usage(Result<Vec<crate::data::UsageInfo>, String>),
    Agents(usize, Result<AgentsInfo, String>),
    /// An agent or manager plan is ready to review.
    AgentPlan(usize, ReviewAction, Result<String, String>),
    /// An agent change finished: (harness, toast on success, CLI result).
    AgentDone(usize, String, Result<String, String>),
    Cli {
        title: String,
        ok: bool,
        text: String,
        refresh: bool,
    },
}

/// Whether a provider can serve a harness right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Readiness {
    Key,
    Oauth,
    /// Claude subscription: only valid inside Claude Code.
    SubscriptionOnly,
    Missing,
}

pub struct App {
    backend: Backend,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub harnesses: Vec<HarnessInfo>,
    pub saved: BTreeMap<String, Selection>,
    pub drafts: Vec<Selection>,
    pub providers: Vec<ProviderInfo>,
    pub oauth: Vec<OauthInfo>,
    pub catalog: BTreeMap<String, Catalog>,
    /// Worker agents per harness index, loaded in the background.
    pub agents: BTreeMap<usize, AgentsLoad>,
    pub cursor: usize,
    pub modal: Option<Modal>,
    pub toast: Option<Toast>,
    pub tick: u64,
    pub quit: bool,
    /// CLI arguments to run with the terminal handed over (`run`); the event
    /// loop suspends the UI, runs them and comes back.
    pub pending_run: Option<Vec<String>>,
}

impl App {
    pub fn new(backend: Backend) -> Self {
        let (tx, rx) = channel();
        let saved = saved_routes();
        let drafts = HARNESSES
            .iter()
            .map(|(id, _, _)| saved.get(*id).cloned().unwrap_or_default())
            .collect();
        Self {
            backend,
            tx,
            rx,
            harnesses: HARNESSES
                .iter()
                .map(|_| HarnessInfo {
                    installed: true,
                    detail: "…".into(),
                })
                .collect(),
            saved,
            drafts,
            providers: Vec::new(),
            oauth: Vec::new(),
            catalog: load_catalog_cache()
                .into_iter()
                .map(|(provider, models)| (provider, Catalog::Ready(models)))
                .collect(),
            agents: BTreeMap::new(),
            cursor: 0,
            modal: None,
            toast: None,
            tick: 0,
            quit: false,
            pending_run: None,
        }
    }

    /// The CLI binary every action is delegated to.
    pub fn exe(&self) -> std::path::PathBuf {
        self.backend.exe().to_path_buf()
    }

    /// Kicks off the background loads. Not called by tests.
    pub fn start(&mut self) {
        self.spawn(|backend| Msg::Harnesses(backend.harnesses()));
        self.reload_connections();
        for harness in 0..HARNESSES.len() {
            if harness != CODEX {
                self.load_agents(harness);
            }
        }
    }

    fn load_agents(&mut self, harness: usize) {
        if !matches!(self.agents.get(&harness), Some(AgentsLoad::Ready(_))) {
            self.agents.insert(harness, AgentsLoad::Loading);
        }
        let id = HARNESSES[harness].0;
        self.spawn(move |backend| Msg::Agents(harness, backend.agents(id)));
    }

    fn spawn(&self, work: impl FnOnce(Backend) -> Msg + Send + 'static) {
        let backend = self.backend.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(work(backend));
        });
    }

    fn reload_connections(&mut self) {
        self.spawn(|backend| Msg::Providers(backend.providers()));
        self.spawn(|backend| Msg::Oauth(backend.oauth()));
    }

    /// Live discovery reads the provider key from the OS credential store, which
    /// can show a permission prompt. It therefore only runs when the user looks
    /// at that provider (or asks for a refresh), never at startup, and the result
    /// is cached on disk.
    fn ensure_catalog(&mut self, provider: &str, force: bool) {
        if provider == "anthropic" || !self.providers.iter().any(|item| item.id == provider) {
            return;
        }
        let have = matches!(
            self.catalog.get(provider),
            Some(Catalog::Ready(_) | Catalog::Loading)
        );
        if have && !force {
            return;
        }
        // Keep showing the cached list while a forced refresh runs.
        if !matches!(self.catalog.get(provider), Some(Catalog::Ready(_))) {
            self.catalog.insert(provider.to_string(), Catalog::Loading);
        }
        let provider = provider.to_string();
        self.spawn(move |backend| {
            let result = backend.models(&provider);
            if let Ok(models) = &result {
                save_catalog_cache(&provider, models);
            }
            Msg::Catalog(provider, result)
        });
    }

    pub fn say(&mut self, text: impl Into<String>, ok: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            ok,
            at: Instant::now(),
        });
    }

    fn message(&mut self, title: &str, text: String, ok: bool) {
        self.modal = Some(Modal::Message(Message {
            title: title.to_string(),
            text,
            ok,
        }));
    }

    /// Applies results that finished in the background.
    pub fn drain(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            self.apply_message(message);
        }
    }

    pub fn apply_message(&mut self, message: Msg) {
        match message {
            Msg::Harnesses(list) => self.harnesses = list,
            Msg::Providers(Ok(list)) => self.providers = list,
            Msg::Providers(Err(error)) => {
                self.say(format!("Sağlayıcılar okunamadı: {error}"), false)
            }
            Msg::Oauth(Ok(list)) => self.oauth = list,
            Msg::Oauth(Err(_)) => {}
            Msg::Catalog(provider, Ok(models)) => {
                self.catalog.insert(provider, Catalog::Ready(models));
            }
            Msg::Catalog(provider, Err(error)) => {
                self.catalog.insert(provider, Catalog::Failed(error));
            }
            Msg::Plan(harness, Ok(text)) => {
                if let Some(Modal::Busy(_)) = self.modal {
                    let text = match self.turnout_section(harness) {
                        Some(section) => format!("{text}\n\n{section}"),
                        None => text,
                    };
                    self.modal = Some(Modal::Review(Review {
                        harness,
                        text,
                        scroll: 0,
                        action: ReviewAction::Route,
                    }));
                }
            }
            Msg::Plan(_, Err(error)) => self.message("Plan hazırlanamadı", error, false),
            Msg::Applied(Ok(text)) => {
                self.saved = saved_routes();
                self.modal = None;
                let first = text.lines().next().unwrap_or("Uygulandı").to_string();
                self.say(first, true);
                self.reload_connections();
            }
            Msg::Applied(Err(error)) => self.message("Uygulanamadı", error, false),
            Msg::Tested(Ok(text)) => self.message("Bağlantı testi", text.trim().to_string(), true),
            Msg::Tested(Err(error)) => self.message("Bağlantı testi başarısız", error, false),
            Msg::Provider(Ok(_)) => {
                self.modal = Some(Modal::Conn(Conn {
                    cursor: 0,
                    mode: ConnMode::List,
                }));
                self.say("Sağlayıcı güncellendi", true);
                self.reload_connections();
            }
            Msg::Provider(Err(error)) => self.message("Sağlayıcı işlemi başarısız", error, false),
            Msg::Agents(harness, Ok(info)) => {
                self.agents.insert(harness, AgentsLoad::Ready(info));
            }
            Msg::Agents(harness, Err(error)) => {
                self.agents.insert(harness, AgentsLoad::Failed(error));
            }
            Msg::AgentPlan(harness, action, Ok(text)) => {
                let text = if action == ReviewAction::Manager {
                    let file = if harness == CLAUDE {
                        "~/.claude/settings.json"
                    } else {
                        "OpenCode ayar dosyası"
                    };
                    let body: Vec<&str> = text
                        .lines()
                        .filter(|line| !line.starts_with("not applied"))
                        .collect();
                    format!(
                        "⚠ Paylaşılan ayar: {file} dosyasını CLI ve VS Code eklentisi birlikte kullanır.\n⚠ Açık oturumlar yeniden başlatılınca değişir.\n\n{}",
                        body.join("\n")
                    )
                } else {
                    text
                };
                self.modal = Some(Modal::Review(Review {
                    harness,
                    text,
                    scroll: 0,
                    action,
                }));
            }
            Msg::AgentPlan(_, _, Err(error)) => {
                self.message("Önizleme hazırlanamadı", error, false)
            }
            Msg::AgentDone(harness, toast, Ok(_)) => {
                self.modal = None;
                self.say(toast, true);
                self.load_agents(harness);
            }
            Msg::AgentDone(_, _, Err(error)) => self.message("Ajan değişikliği yapılamadı", error, false),
            Msg::Limit(provider, limit, Ok(text)) => {
                self.reload_connections();
                self.modal = Some(Modal::Conn(Conn {
                    cursor: self.providers.iter().position(|p| p.id == provider).unwrap_or(0),
                    mode: ConnMode::List,
                }));
                let name = crate::data::provider_name(&provider);
                let value = match limit {
                    Some(n) => format!("en fazla {n} aynı anda"),
                    None => "sınırsız".to_string(),
                };
                let agent = if text.contains("restarted") {
                    " · Turnout Agent yeniden başlatıldı"
                } else {
                    ""
                };
                self.say(format!("✓ {name}: {value}{agent}"), true);
            }
            Msg::Limit(provider, _, Err(error)) => {
                self.modal = Some(Modal::Conn(Conn {
                    cursor: self.providers.iter().position(|p| p.id == provider).unwrap_or(0),
                    mode: ConnMode::List,
                }));
                self.say(format!("Sınır kaydedilemedi: {}", error.lines().last().unwrap_or("")), false);
            }
            Msg::Quota(provider, result) => {
                let cursor = self.providers.iter().position(|p| p.id == provider).unwrap_or(0);
                let mode = match result {
                    Ok(quota) => ConnMode::Quota { provider, quota },
                    Err(error) => {
                        self.say(format!("Kota okunamadı: {error}"), false);
                        ConnMode::List
                    }
                };
                self.modal = Some(Modal::Conn(Conn { cursor, mode }));
            }
            Msg::Usage(result) => {
                let mode = match result {
                    Ok(usage) => ConnMode::Usage(usage),
                    Err(error) => {
                        self.say(format!("Kullanım okunamadı: {error}"), false);
                        ConnMode::List
                    }
                };
                self.modal = Some(Modal::Conn(Conn { cursor: 0, mode }));
            }
            Msg::Cli {
                title,
                ok,
                text,
                refresh,
            } => {
                if refresh {
                    self.saved = saved_routes();
                    self.reload_connections();
                }
                self.modal = Some(Modal::Output(Output {
                    title,
                    text: if text.trim().is_empty() {
                        "(çıktı yok)".into()
                    } else {
                        text
                    },
                    ok,
                    scroll: 0,
                }));
            }
        }
    }

    // ---- derived data -------------------------------------------------

    /// Provider tabs in the model picker: configured providers plus Anthropic.
    pub fn tabs(&self) -> Vec<String> {
        let mut tabs = vec!["evren".to_string(), "openai".into(), "anthropic".into()];
        for provider in &self.providers {
            if !tabs.contains(&provider.id) {
                tabs.push(provider.id.clone());
            }
        }
        // A route can still name a provider that has since been removed; its
        // tab is the only place those models can be deselected.
        for target in self.drafts.iter().flat_map(|draft| &draft.targets) {
            if !tabs.contains(&target.provider) {
                tabs.push(target.provider.clone());
            }
        }
        tabs
    }

    pub fn readiness(&self, provider: &str, harness: usize) -> Readiness {
        let configured = self.providers.iter().find(|item| item.id == provider);
        if configured.is_some_and(|item| item.has_key) {
            return Readiness::Key;
        }
        let oauth = self.oauth.iter().find(|item| item.id == provider);
        match (provider, oauth) {
            ("anthropic", Some(item)) if item.logged_in && harness == CLAUDE => Readiness::Oauth,
            ("anthropic", Some(item)) if item.logged_in => Readiness::SubscriptionOnly,
            ("openai", Some(item)) if item.logged_in && (harness == 0 || item.gateway_capable) => {
                Readiness::Oauth
            }
            _ => Readiness::Missing,
        }
    }

    /// Model ids shown for one provider, honoring the search box. Models the
    /// route uses that the catalog does not list (it failed, is not loaded yet
    /// or dropped them) come first, so they can always be deselected.
    pub fn entries(&self, provider: &str, harness: usize, query: &str) -> Vec<String> {
        let listed: Vec<String> = match provider {
            "anthropic" => ANTHROPIC_MODELS.iter().map(|id| id.to_string()).collect(),
            _ => match self.catalog.get(provider) {
                // The complete live catalog; non-chat models are tagged in the UI.
                Some(Catalog::Ready(models)) => models.clone(),
                Some(Catalog::Failed(_)) if provider == "openai" => {
                    OPENAI_FALLBACK.iter().map(|id| id.to_string()).collect()
                }
                _ => Vec::new(),
            },
        };
        let mut all: Vec<String> = self.drafts[harness]
            .targets
            .iter()
            .filter(|target| target.provider == provider && !listed.contains(&target.model))
            .map(|target| target.model.clone())
            .collect();
        all.extend(listed);
        let query = query.to_lowercase();
        all.into_iter()
            .filter(|id| query.is_empty() || id.to_lowercase().contains(&query))
            .collect()
    }

    pub fn dirty(&self, harness: usize) -> bool {
        let saved = self
            .saved
            .get(HARNESSES[harness].0)
            .cloned()
            .unwrap_or_default();
        saved != self.drafts[harness]
    }

    // ---- input --------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.modal.take() {
            None => self.home_key(key),
            Some(Modal::Palette(palette)) => self.palette_key(palette, key),
            Some(Modal::Menu(menu)) => self.menu_key(menu, key),
            Some(Modal::Input(input)) => self.input_key(input, key),
            Some(Modal::Output(output)) => self.output_key(output, key),
            Some(Modal::Confirm(confirm)) => self.confirm_key(confirm, key),
            Some(Modal::Picker(picker)) => self.picker_key(picker, key),
            Some(Modal::Chain(edit)) => self.chain_key(edit, key),
            Some(Modal::Agents(view)) => self.agents_key(view, key),
            Some(Modal::Review(review)) => self.review_key(review, key),
            Some(Modal::Conn(conn)) => self.conn_key(conn, key),
            Some(Modal::Message(_)) | Some(Modal::Help) => {}
            Some(Modal::Busy(text)) => {
                // Busy cannot be dismissed; the background job closes it.
                self.modal = Some(Modal::Busy(text));
            }
        }
    }

    fn home_key(&mut self, key: KeyEvent) {
        let last = HARNESSES.len() - 1;
        match key.code {
            // Esc deliberately does nothing here: a stray Esc must not close the app.
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Enter | KeyCode::Char('m') => self.open_picker(),
            KeyCode::Char('a') => self.start_plan(),
            KeyCode::Char('t') => self.start_test(),
            KeyCode::Char('c') => {
                self.modal = Some(Modal::Conn(Conn {
                    cursor: 0,
                    mode: ConnMode::List,
                }))
            }
            KeyCode::Char('r') => {
                self.saved = saved_routes();
                self.start();
                self.say("Yenileniyor…", true);
            }
            KeyCode::Char('w') => self.open_agents(),
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char(':') => self.open_palette(),
            KeyCode::Char('x') => self.run_cmd(Cmd::Run),
            KeyCode::Char('b') => self.run_cmd(Cmd::Rollback),
            KeyCode::Char('d') => self.run_cmd(Cmd::Doctor),
            KeyCode::Char('g') => self.run_cmd(Cmd::AgentStatus),
            KeyCode::Char('p') => self.run_cmd(Cmd::SaveProfile),
            _ => {}
        }
    }

    // ---- command palette and the dialogs it opens --------------------------

    pub fn palette_entries(query: &str) -> Vec<(Cmd, &'static str, &'static str)> {
        let query = query.to_lowercase();
        COMMANDS
            .iter()
            .filter(|(_, label, detail)| {
                query.is_empty()
                    || label.to_lowercase().contains(&query)
                    || detail.to_lowercase().contains(&query)
            })
            .copied()
            .collect()
    }

    fn open_palette(&mut self) {
        let mut list = ListState::default();
        list.select(Some(0));
        self.modal = Some(Modal::Palette(Palette {
            query: String::new(),
            list,
        }));
    }

    fn palette_key(&mut self, mut palette: Palette, key: KeyEvent) {
        let entries = Self::palette_entries(&palette.query);
        let selected = palette.list.selected().unwrap_or(0);
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                // No match: stay open so the search can be corrected.
                if let Some((cmd, _, _)) = entries.get(selected) {
                    self.run_cmd(*cmd);
                    return;
                }
            }
            KeyCode::Up => palette.list.select(Some(selected.saturating_sub(1))),
            KeyCode::Down => palette
                .list
                .select(Some((selected + 1).min(entries.len().saturating_sub(1)))),
            KeyCode::Backspace => {
                palette.query.pop();
                palette.list.select(Some(0));
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                palette.query.push(c);
                palette.list.select(Some(0));
            }
            _ => {}
        }
        self.modal = Some(Modal::Palette(palette));
    }

    fn run_cli(&mut self, title: &str, args: Vec<String>, refresh: bool) {
        self.modal = Some(Modal::Busy(format!("{title}…")));
        let title = title.to_string();
        self.spawn(move |backend| {
            let (ok, text) = backend.run_both(&args);
            Msg::Cli {
                title,
                ok,
                text,
                refresh,
            }
        });
    }

    fn cli(&mut self, title: &str, args: &[&str], refresh: bool) {
        self.run_cli(
            title,
            args.iter().map(|arg| arg.to_string()).collect(),
            refresh,
        );
    }

    /// Runs one palette command. Opens a dialog, starts a background job, or
    /// hands the terminal to a child process.
    pub fn run_cmd(&mut self, cmd: Cmd) {
        let (id, name, _) = HARNESSES[self.cursor];
        match cmd {
            Cmd::Models => self.open_picker(),
            Cmd::Apply => self.start_plan(),
            Cmd::Test => self.start_test(),
            Cmd::Refresh => {
                self.saved = saved_routes();
                self.start();
                self.say("Yenileniyor…", true);
            }
            Cmd::Help => self.modal = Some(Modal::Help),
            Cmd::Quit => self.quit = true,
            Cmd::Providers => {
                self.modal = Some(Modal::Conn(Conn {
                    cursor: 0,
                    mode: ConnMode::List,
                }))
            }
            Cmd::Run => {
                if self.saved.contains_key(id) {
                    self.pending_run = Some(vec!["run".into(), id.into()]);
                } else {
                    self.say(format!("{name}: önce bir yönlendirmeyi uygulayın (a)"), false);
                }
            }
            Cmd::SaveProfile => {
                if self.drafts[self.cursor].default_target().is_none() {
                    self.say(format!("{name}: önce model seçin"), false);
                } else {
                    self.modal = Some(Modal::Input(Input {
                        title: format!("Profil — {name}"),
                        prompt: "Profil adı (harf, rakam, - _)".into(),
                        value: String::new(),
                    }));
                }
            }
            Cmd::RunProfile => {
                let items = saved_profiles()
                    .into_iter()
                    .filter_map(|(harness, profile, selection)| {
                        let display = HARNESSES.iter().find(|(hid, _, _)| *hid == harness)?.1;
                        let default = selection
                            .default_target()
                            .map(|target| target.model.clone())
                            .unwrap_or_default();
                        Some(MenuItem {
                            label: format!("{display} — {profile}"),
                            detail: format!("{default}  +{} model", selection.targets.len().saturating_sub(1)),
                            value: format!("{harness}\n{profile}"),
                        })
                    })
                    .collect::<Vec<_>>();
                if items.is_empty() {
                    self.say("Kayıtlı profil yok — p ile oluşturun", false);
                } else {
                    self.open_menu(MenuKind::RunProfile, "Profille çalıştır", items);
                }
            }
            Cmd::Rollback => {
                let mut items = vec![MenuItem {
                    label: "En son yedek".into(),
                    detail: "rollback (varsayılan)".into(),
                    value: String::new(),
                }];
                items.extend(list_backups().into_iter().take(15).map(|(backup, seconds)| MenuItem {
                    label: age(seconds),
                    detail: backup.clone(),
                    value: backup,
                }));
                if items.len() == 1 {
                    self.say("Henüz yedek yok", false);
                } else {
                    self.open_menu(MenuKind::Rollback, "Yedekten geri al", items);
                }
            }
            Cmd::AgentStatus => self.cli("Turnout Agent", &["gateway", "status"], false),
            Cmd::AgentInstall => self.cli("Turnout Agent kuruluyor", &["gateway", "install"], false),
            Cmd::AgentUninstall => {
                self.modal = Some(Modal::Confirm(Confirm {
                    kind: ConfirmKind::AgentUninstall,
                    text: "Turnout Agent kaldırılsın mı? Gateway'e bağlı editör rotaları (örn. Claude Code'un hibrit listesi) çalışmaz.".into(),
                }))
            }
            Cmd::LoginOpenAi => {
                self.cli("OpenAI girişi", &["auth", "login", "openai"], false);
            }
            Cmd::LoginAnthropic => {
                self.cli("Claude girişi", &["auth", "login", "anthropic"], false);
            }
            Cmd::KeyStatus => self.cli("Anahtar deposu", &["credentials", "status"], false),
            Cmd::KeyMigrate => self.cli("Anahtarlar kopyalanıyor", &["credentials", "migrate"], true),
            Cmd::KeyMigrateDelete => {
                self.modal = Some(Modal::Confirm(Confirm {
                    kind: ConfirmKind::KeyMigrateDelete,
                    text: "Anahtarlar dosyaya kopyalanacak ve Keychain'deki eski kayıtlar silinecek. Devam edilsin mi?".into(),
                }))
            }
            Cmd::Doctor => self.cli("Sağlık kontrolü", &["doctor"], false),
            Cmd::DoctorOnline => self.cli("Sağlık kontrolü (çevrimiçi)", &["doctor", "--online"], false),
        }
    }

    fn open_menu(&mut self, kind: MenuKind, title: &str, items: Vec<MenuItem>) {
        let mut list = ListState::default();
        list.select(Some(0));
        self.modal = Some(Modal::Menu(Menu {
            kind,
            title: title.to_string(),
            items,
            list,
        }));
    }

    fn menu_key(&mut self, mut menu: Menu, key: KeyEvent) {
        let selected = menu.list.selected().unwrap_or(0);
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Up | KeyCode::Char('k') => menu.list.select(Some(selected.saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') => menu
                .list
                .select(Some((selected + 1).min(menu.items.len().saturating_sub(1)))),
            KeyCode::Enter => {
                let value = menu.items[selected].value.clone();
                match menu.kind {
                    MenuKind::RunProfile => {
                        if let Some((harness, profile)) = value.split_once('\n') {
                            self.pending_run = Some(vec![
                                "run".into(),
                                harness.into(),
                                "--profile".into(),
                                profile.into(),
                            ]);
                        }
                    }
                    MenuKind::Rollback => {
                        let which = if value.is_empty() {
                            "En son yedek".to_string()
                        } else {
                            format!("{value} yedeği")
                        };
                        self.modal = Some(Modal::Confirm(Confirm {
                            kind: ConfirmKind::Rollback(value),
                            text: format!("{which} geri yüklensin mi? Yedekteki dosyaların şu anki hâli ezilir."),
                        }));
                    }
                }
                return;
            }
            _ => {}
        }
        self.modal = Some(Modal::Menu(menu));
    }

    fn input_key(&mut self, mut input: Input, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Backspace => {
                input.value.pop();
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && input.value.len() < 32 =>
            {
                input.value.push(c);
            }
            KeyCode::Enter => {
                let name = input.value.trim().to_lowercase();
                let valid = !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if !valid {
                    self.say("Profil adı yalnızca harf, rakam, - ve _ içerebilir", false);
                } else {
                    let id = HARNESSES[self.cursor].0;
                    match self.drafts[self.cursor].profile_args(id, &name) {
                        Ok(args) => {
                            self.run_cli("Profil kaydediliyor", args, true);
                            return;
                        }
                        Err(error) => self.say(error, false),
                    }
                }
            }
            _ => {}
        }
        self.modal = Some(Modal::Input(input));
    }

    fn output_key(&mut self, mut output: Output, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return,
            KeyCode::Up | KeyCode::Char('k') => output.scroll = output.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => output.scroll = output.scroll.saturating_add(1),
            KeyCode::PageUp => output.scroll = output.scroll.saturating_sub(10),
            KeyCode::PageDown => output.scroll = output.scroll.saturating_add(10),
            _ => {}
        }
        self.modal = Some(Modal::Output(output));
    }

    fn confirm_key(&mut self, confirm: Confirm, key: KeyEvent) {
        if key.code != KeyCode::Char('y') {
            return;
        }
        match confirm.kind {
            ConfirmKind::Rollback(id) => {
                let mut args = vec!["rollback".to_string()];
                if !id.is_empty() {
                    args.push(id);
                }
                self.run_cli("Geri alınıyor", args, true);
            }
            ConfirmKind::AgentRollback(harness) => {
                let args = vec!["agents".to_string(), "rollback".into()];
                self.modal = Some(Modal::Busy("Geri alınıyor…".into()));
                self.spawn(move |backend| {
                    let (ok, text) = backend.run_both(&args);
                    Msg::AgentDone(
                        harness,
                        "✓ Son ajan değişikliği geri alındı · yeni oturumda yüklenir".into(),
                        if ok { Ok(text) } else { Err(text) },
                    )
                });
            }
            ConfirmKind::AgentUninstall => self.cli(
                "Turnout Agent kaldırılıyor",
                &["gateway", "uninstall"],
                false,
            ),
            ConfirmKind::KeyMigrateDelete => self.cli(
                "Anahtarlar taşınıyor",
                &["credentials", "migrate", "--delete-old"],
                true,
            ),
        }
    }

    fn open_picker(&mut self) {
        let first = self.tabs()[0].clone();
        self.ensure_catalog(&first, false);
        let mut list = ListState::default();
        list.select(Some(0));
        self.modal = Some(Modal::Picker(Picker {
            harness: self.cursor,
            tab: 0,
            query: String::new(),
            list,
        }));
    }

    fn picker_key(&mut self, mut picker: Picker, key: KeyEvent) {
        let tabs = self.tabs();
        let provider = tabs[picker.tab].clone();
        let entries = self.entries(&provider, picker.harness, &picker.query);
        let selected = picker.list.selected().unwrap_or(0);
        let target = entries
            .get(selected)
            .map(|model| Target::new(&provider, model));
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                if let Some(target) = target {
                    self.drafts[picker.harness].set_default(&target);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(target) = target {
                    self.drafts[picker.harness].toggle(&target);
                }
            }
            KeyCode::Char('o') if ctrl && picker.harness == CLAUDE => {
                let selection = &mut self.drafts[CLAUDE];
                selection.hybrid = !selection.hybrid;
            }
            KeyCode::Char('r') if ctrl => self.ensure_catalog(&provider, true),
            KeyCode::Char('f') if ctrl => {
                let selection = &mut self.drafts[picker.harness];
                selection.fallback = !selection.fallback;
            }
            KeyCode::Char('l') if ctrl => match target {
                Some(target) if self.drafts[picker.harness].contains(&target) => {
                    let chain = self.drafts[picker.harness]
                        .chains
                        .get(&target)
                        .cloned()
                        .unwrap_or_default();
                    self.modal = Some(Modal::Chain(ChainEdit {
                        picker,
                        from: target,
                        chain,
                        cursor: 0,
                    }));
                    return;
                }
                Some(_) => self.say("Önce modeli seçin (␣); fallback yalnızca seçili modeller arasında olur", false),
                None => {}
            },
            KeyCode::Right | KeyCode::Tab => {
                picker.tab = (picker.tab + 1) % tabs.len();
                picker.list.select(Some(0));
                self.ensure_catalog(&tabs[picker.tab], false);
            }
            KeyCode::Left | KeyCode::BackTab => {
                picker.tab = (picker.tab + tabs.len() - 1) % tabs.len();
                picker.list.select(Some(0));
                self.ensure_catalog(&tabs[picker.tab], false);
            }
            KeyCode::Up => picker.list.select(Some(selected.saturating_sub(1))),
            KeyCode::Down => {
                picker
                    .list
                    .select(Some((selected + 1).min(entries.len().saturating_sub(1))));
            }
            KeyCode::Backspace => {
                picker.query.pop();
                picker.list.select(Some(0));
            }
            KeyCode::Char(c) if !ctrl => {
                picker.query.push(c);
                picker.list.select(Some(0));
            }
            _ => {}
        }
        self.modal = Some(Modal::Picker(picker));
    }

    /// Models the harness's applied route serves: the only ones a worker or
    /// the manager can use.
    pub fn route_models(&self, harness: usize) -> Vec<Target> {
        self.saved
            .get(HARNESSES[harness].0)
            .map(|selection| selection.targets.clone())
            .unwrap_or_default()
    }

    pub fn agents_info(&self, harness: usize) -> Option<&AgentsInfo> {
        match self.agents.get(&harness) {
            Some(AgentsLoad::Ready(info)) => Some(info),
            _ => None,
        }
    }

    fn open_agents(&mut self) {
        if self.cursor == CODEX {
            let models: Vec<String> = self
                .route_models(CODEX)
                .iter()
                .map(|target| target.model.clone())
                .collect();
            let list = if models.is_empty() {
                "Codex için henüz bir Turnout rotası yok.".to_string()
            } else {
                format!("Alt ajan olabilecek modeller (Codex rotası):\n  {}", models.join(" · "))
            };
            self.message(
                "Ajanlar — Codex",
                format!("Codex işçi dosyası kullanmaz: yönetici her alt ajanın modelini kendisi seçer (spawn_agent).\n\n{list}\n\nListeyi değiştirmek için modeller (⏎)."),
                true,
            );
            return;
        }
        let harness = self.cursor;
        self.load_agents(harness);
        self.modal = Some(Modal::Agents(AgentsView {
            harness,
            cursor: 0,
            mode: AgentsMode::List,
        }));
    }

    fn agent_plan(&mut self, harness: usize, args: Vec<String>, action: ReviewAction) {
        self.modal = Some(Modal::Busy("Önizleme hazırlanıyor…".into()));
        self.spawn(move |backend| {
            let (ok, text) = backend.run_both(&args);
            Msg::AgentPlan(harness, action, if ok { Ok(text) } else { Err(text) })
        });
    }

    fn agents_key(&mut self, mut view: AgentsView, key: KeyEvent) {
        let harness = view.harness;
        let id = HARNESSES[harness].0;
        let models = self.route_models(harness);
        let workers = self
            .agents_info(harness)
            .map(|info| info.agents.clone())
            .unwrap_or_default();
        let selected = workers.get(view.cursor).cloned();
        let not_turnouts = |worker: &crate::data::AgentInfo| {
            format!(
                "⚠ {} elle yazılmış: {}\n\nTurnout kendi yazmadığı bir dosyayı değiştirmez. Düzenlemek için dosyayı elle açın; Turnout'un yönetmesini istiyorsanız başka bir adla n ile yeni işçi oluşturun.",
                worker.name, worker.path
            )
        };
        match &mut view.mode {
            AgentsMode::List => match key.code {
                KeyCode::Esc => return,
                KeyCode::Up | KeyCode::Char('k') => view.cursor = view.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    view.cursor = (view.cursor + 1).min(workers.len().saturating_sub(1))
                }
                KeyCode::Char('r') => self.load_agents(harness),
                KeyCode::Char('n') if !models.is_empty() => {
                    view.mode = AgentsMode::Name {
                        input: String::new(),
                        error: None,
                    }
                }
                KeyCode::Char('m') if !models.is_empty() => {
                    view.mode = AgentsMode::Manager { cursor: 0 }
                }
                KeyCode::Char('e') => match selected {
                    Some(worker) if worker.managed => {
                        view.mode = AgentsMode::Model {
                            name: worker.name.clone(),
                            user_scope: worker.scope == "user",
                            cursor: 0,
                            editing: true,
                        }
                    }
                    Some(worker) => {
                        self.message("Değiştirilemez", not_turnouts(&worker), false);
                        return;
                    }
                    None => {}
                },
                KeyCode::Char('d') => match selected {
                    Some(worker) if worker.managed => {
                        let args = vec![
                            "agents".into(), "plan".into(), worker.name.clone(),
                            "--harness".into(), id.into(), "--remove".into(),
                            "--scope".into(), worker.scope.clone(),
                        ];
                        self.agent_plan(harness, args, ReviewAction::Agents);
                        return;
                    }
                    Some(worker) => {
                        self.message("Değiştirilemez", not_turnouts(&worker), false);
                        return;
                    }
                    None => {}
                },
                KeyCode::Char('u') => {
                    self.modal = Some(Modal::Confirm(Confirm {
                        kind: ConfirmKind::AgentRollback(harness),
                        text: "Son ajan değişikliği geri alınsın mı?".into(),
                    }));
                    return;
                }
                _ => {}
            },
            AgentsMode::Name { input, error } => match key.code {
                KeyCode::Esc => view.mode = AgentsMode::List,
                KeyCode::Backspace => {
                    input.pop();
                    *error = None;
                }
                KeyCode::Char(c) if input.chars().count() < 64 && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.push(c);
                    *error = None;
                }
                KeyCode::Enter => {
                    let name = input.trim().to_string();
                    let valid = !name.is_empty()
                        && !name.starts_with('-')
                        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                    if valid {
                        view.mode = AgentsMode::Model {
                            name,
                            user_scope: false,
                            cursor: 0,
                            editing: false,
                        };
                    } else {
                        *error = Some("Ad yalnızca küçük harf, rakam ve tire içerebilir. Örnek: deepseek-worker".into());
                    }
                }
                _ => {}
            },
            AgentsMode::Model { name, user_scope, cursor, editing } => match key.code {
                KeyCode::Esc => {
                    view.mode = if *editing {
                        AgentsMode::List
                    } else {
                        AgentsMode::Name { input: name.clone(), error: None }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = (*cursor + 1).min(models.len().saturating_sub(1))
                }
                KeyCode::Tab if !*editing => *user_scope = !*user_scope,
                KeyCode::Enter => {
                    if let Some(model) = models.get(*cursor) {
                        let args = vec![
                            "agents".into(), "plan".into(), name.clone(),
                            "--harness".into(), id.into(),
                            "--model".into(), format!("{}::{}", model.provider, model.model),
                            "--scope".into(), if *user_scope { "user".into() } else { "project".into() },
                        ];
                        self.agent_plan(harness, args, ReviewAction::Agents);
                        return;
                    }
                }
                _ => {}
            },
            AgentsMode::Manager { cursor } => match key.code {
                KeyCode::Esc => view.mode = AgentsMode::List,
                KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = (*cursor + 1).min(models.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(model) = models.get(*cursor) {
                        let args = vec![
                            "agents".into(), "manager".into(), "--harness".into(), id.into(),
                            format!("{}::{}", model.provider, model.model),
                        ];
                        self.agent_plan(harness, args, ReviewAction::Manager);
                        return;
                    }
                }
                _ => {}
            },
        }
        self.modal = Some(Modal::Agents(view));
    }

    /// The models `edit.from` may fall back to: the other selected models.
    pub fn chain_candidates(&self, edit: &ChainEdit) -> Vec<Target> {
        self.drafts[edit.picker.harness]
            .targets
            .iter()
            .filter(|target| **target != edit.from)
            .cloned()
            .collect()
    }

    fn chain_key(&mut self, mut edit: ChainEdit, key: KeyEvent) {
        let candidates = self.chain_candidates(&edit);
        let current = candidates.get(edit.cursor).cloned();
        match key.code {
            // Esc leaves the chain as it was.
            KeyCode::Esc => {
                self.modal = Some(Modal::Picker(edit.picker));
                return;
            }
            KeyCode::Enter => {
                let selection = &mut self.drafts[edit.picker.harness];
                if edit.chain.is_empty() {
                    selection.chains.remove(&edit.from);
                } else {
                    selection.chains.insert(edit.from.clone(), edit.chain);
                }
                self.modal = Some(Modal::Picker(edit.picker));
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => edit.cursor = edit.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                edit.cursor = (edit.cursor + 1).min(candidates.len().saturating_sub(1))
            }
            KeyCode::Char(' ') => {
                if let Some(target) = current {
                    match edit.chain.iter().position(|item| *item == target) {
                        Some(index) => {
                            edit.chain.remove(index);
                        }
                        None => edit.chain.push(target),
                    }
                }
            }
            // Order within the chain: + earlier, - later.
            KeyCode::Char('+') | KeyCode::Char('-') => {
                if let Some(index) = current.and_then(|t| edit.chain.iter().position(|i| *i == t)) {
                    let to = if key.code == KeyCode::Char('+') {
                        index.saturating_sub(1)
                    } else {
                        (index + 1).min(edit.chain.len() - 1)
                    };
                    edit.chain.swap(index, to);
                }
            }
            _ => {}
        }
        self.modal = Some(Modal::Chain(edit));
    }

    /// Fallback lives in Turnout's own state, not in the harness file, so the
    /// diff alone would hide it: list what the plan sets, next to what is saved.
    pub fn turnout_section(&self, harness: usize) -> Option<String> {
        let id = HARNESSES[harness].0;
        let draft = &self.drafts[harness];
        let saved = self.saved.get(id).cloned().unwrap_or_default();
        if !draft.fallback && !saved.fallback && draft.chains.is_empty() && saved.chains.is_empty() {
            return None;
        }
        let onoff = |on: bool| if on { "açık" } else { "kapalı" };
        let mut lines = vec!["TURNOUT AYARLARI  (harness dosyasında görünmez)".to_string()];
        let changed = if draft.fallback != saved.fallback { "   değişti" } else { "" };
        lines.push(format!("  fallback (rota geneli)   {}{changed}", onoff(draft.fallback)));
        let names = |chain: &[Target]| chain.iter().map(|t| t.model.clone()).collect::<Vec<_>>().join(", ");
        for (from, chain) in &draft.chains {
            let state = match saved.chains.get(from) {
                None => "yeni",
                Some(old) if old == chain => "aynı",
                Some(_) => "değişti",
            };
            lines.push(format!("  {} → {}   {state}", from.model, names(chain)));
        }
        for (from, chain) in &saved.chains {
            if !draft.chains.contains_key(from) {
                lines.push(format!("  {} → {}   kaldırılacak", from.model, names(chain)));
            }
        }
        Some(lines.join("\n"))
    }

    fn start_plan(&mut self) {
        let (id, name, _) = HARNESSES[self.cursor];
        match self.drafts[self.cursor].plan_args(id) {
            Err(error) => self.say(format!("{name}: {error}"), false),
            Ok(args) => {
                let harness = self.cursor;
                self.modal = Some(Modal::Busy(format!("{name} için plan hazırlanıyor…")));
                self.spawn(move |backend| Msg::Plan(harness, backend.run(&args, None)));
            }
        }
    }

    fn review_key(&mut self, mut review: Review, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => return,
            // A shared-settings change (the manager) is never applied by Enter.
            KeyCode::Enter if review.action == ReviewAction::Manager => {}
            KeyCode::Char('y') | KeyCode::Enter => {
                let harness = review.harness;
                let id = HARNESSES[harness].0.to_string();
                self.modal = Some(Modal::Busy("Uygulanıyor…".into()));
                if review.action == ReviewAction::Agents {
                    self.spawn(move |backend| {
                        let (ok, text) =
                            backend.run_both(&["agents".into(), "apply".into(), "--harness".into(), id]);
                        Msg::AgentDone(
                            harness,
                            "✓ İşçi değişikliği uygulandı · geri al: w → u · yeni oturumda yüklenir".into(),
                            if ok { Ok(text) } else { Err(text) },
                        )
                    });
                } else {
                    self.spawn(move |backend| {
                        Msg::Applied(backend.run(&["apply".into(), "--harness".into(), id], None))
                    });
                }
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => review.scroll = review.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => review.scroll = review.scroll.saturating_add(1),
            KeyCode::PageUp => review.scroll = review.scroll.saturating_sub(10),
            KeyCode::PageDown => review.scroll = review.scroll.saturating_add(10),
            _ => {}
        }
        self.modal = Some(Modal::Review(review));
    }

    fn start_test(&mut self) {
        let (_, name, _) = HARNESSES[self.cursor];
        let Some(target) = self.drafts[self.cursor].default_target().cloned() else {
            self.say(format!("{name}: önce bir model seçin"), false);
            return;
        };
        if target.provider == "anthropic"
            && !self.providers.iter().any(|item| item.id == "anthropic")
        {
            self.message(
                "Bağlantı testi",
                "Claude modelleri kendi girişinizle çalışır; Turnout üzerinden test edilecek bir Anthropic API anahtarı yok.".into(),
                true,
            );
            return;
        }
        self.modal = Some(Modal::Busy(format!("{} test ediliyor…", target.model)));
        self.spawn(move |backend| {
            Msg::Tested(backend.run(
                &[
                    "test".into(),
                    target.provider,
                    "--model".into(),
                    target.model,
                ],
                None,
            ))
        });
    }

    fn ask_quota(&mut self, provider: String) {
        self.modal = Some(Modal::Busy("Kota soruluyor…".into()));
        self.spawn(move |backend| {
            let result = backend.quota(&provider);
            Msg::Quota(provider, result)
        });
    }

    fn ask_usage(&mut self) {
        self.modal = Some(Modal::Busy("Oturum kotası soruluyor…".into()));
        self.spawn(|backend| Msg::Usage(backend.usage()));
    }

    fn conn_key(&mut self, mut conn: Conn, key: KeyEvent) {
        let count = self.providers.len();
        match &mut conn.mode {
            ConnMode::List => match key.code {
                KeyCode::Esc => return,
                KeyCode::Up | KeyCode::Char('k') => conn.cursor = conn.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    conn.cursor = (conn.cursor + 1).min(count.saturating_sub(1))
                }
                KeyCode::Char('n') => conn.mode = ConnMode::Preset,
                KeyCode::Char('d') => {
                    if let Some(provider) = self.providers.get(conn.cursor) {
                        conn.mode = ConnMode::ConfirmDelete(provider.id.clone());
                    }
                }
                KeyCode::Char('r') => self.reload_connections(),
                KeyCode::Char('u') => {
                    if let Some(provider) = self.providers.get(conn.cursor) {
                        self.ask_quota(provider.id.clone());
                        return;
                    }
                }
                KeyCode::Char('o') => {
                    self.ask_usage();
                    return;
                }
                KeyCode::Char('l') => {
                    if let Some(provider) = self.providers.get(conn.cursor) {
                        conn.mode = ConnMode::Limit {
                            provider: provider.id.clone(),
                            input: provider.max_concurrent.map(|n| n.to_string()).unwrap_or_default(),
                            error: None,
                        };
                    }
                }
                _ => {}
            },
            ConnMode::Quota { ref provider, .. } => match key.code {
                KeyCode::Esc | KeyCode::Enter => conn.mode = ConnMode::List,
                KeyCode::Char('r') => {
                    self.ask_quota(provider.clone());
                    return;
                }
                _ => {}
            },
            ConnMode::Usage(_) => match key.code {
                KeyCode::Esc | KeyCode::Enter => conn.mode = ConnMode::List,
                KeyCode::Char('r') => {
                    self.ask_usage();
                    return;
                }
                _ => {}
            },
            ConnMode::Limit {
                ref provider,
                ref mut input,
                ref mut error,
            } => match key.code {
                KeyCode::Esc => conn.mode = ConnMode::List,
                KeyCode::Backspace => {
                    input.pop();
                    *error = None;
                }
                KeyCode::Char(c) if input.len() < 6 && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.push(c);
                    *error = None;
                }
                KeyCode::Enter => {
                    let value = input.trim();
                    let limit = if value.is_empty() {
                        Ok(None)
                    } else {
                        match value.parse::<u32>() {
                            Ok(0) => Err("Sınır en az 1 olmalı. Sınırı kaldırmak için kutuyu boş bırakın."),
                            Ok(n) => Ok(Some(n)),
                            Err(_) => Err("Yalnızca bir sayı girin (örnek: 4). Sınırsız için boş bırakın."),
                        }
                    };
                    match limit {
                        Err(message) => *error = Some(message.to_string()),
                        Ok(limit) => {
                            let provider = provider.clone();
                            let mut args = vec!["providers".to_string(), "limit".into(), provider.clone()];
                            match limit {
                                Some(n) => {
                                    args.push("--max-concurrent".into());
                                    args.push(n.to_string());
                                }
                                None => args.push("--clear".into()),
                            }
                            self.modal = Some(Modal::Busy("Sınır kaydediliyor…".into()));
                            self.spawn(move |backend| {
                                let (ok, text) = backend.run_both(&args);
                                Msg::Limit(provider, limit, if ok { Ok(text) } else { Err(text) })
                            });
                            return;
                        }
                    }
                }
                _ => {}
            },
            ConnMode::Preset => match key.code {
                KeyCode::Char('u') => {
                    conn.mode = ConnMode::Custom {
                        field: 0,
                        id: String::new(),
                        url: String::new(),
                        dialect: 0,
                        secret: String::new(),
                    }
                }
                KeyCode::Char('e') => {
                    conn.mode = ConnMode::Key {
                        preset: "evren".into(),
                        input: String::new(),
                    }
                }
                KeyCode::Char('a') => {
                    conn.mode = ConnMode::Key {
                        preset: "anthropic".into(),
                        input: String::new(),
                    }
                }
                _ => conn.mode = ConnMode::List,
            },
            ConnMode::Key { preset, input } => match key.code {
                KeyCode::Esc => conn.mode = ConnMode::List,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => input.push(c),
                KeyCode::Enter if !input.trim().is_empty() => {
                    let (preset, key_text) = (preset.clone(), input.trim().to_string());
                    self.modal = Some(Modal::Busy("Anahtar kasaya yazılıyor…".into()));
                    self.spawn(move |backend| {
                        Msg::Provider(backend.add_provider(&preset, &key_text))
                    });
                    return;
                }
                _ => {}
            },
            ConnMode::Custom {
                field,
                id,
                url,
                dialect,
                secret,
            } => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => conn.mode = ConnMode::List,
                    KeyCode::Tab | KeyCode::Down => *field = (*field + 1) % 4,
                    KeyCode::BackTab | KeyCode::Up => *field = (*field + 3) % 4,
                    KeyCode::Left if *field == 2 => {
                        *dialect = (*dialect + DIALECTS.len() - 1) % DIALECTS.len()
                    }
                    KeyCode::Right if *field == 2 => *dialect = (*dialect + 1) % DIALECTS.len(),
                    KeyCode::Backspace => {
                        match *field {
                            0 => id.pop(),
                            1 => url.pop(),
                            3 => secret.pop(),
                            _ => None,
                        };
                    }
                    KeyCode::Char(c) if !ctrl => match *field {
                        0 => id.push(c.to_ascii_lowercase()),
                        1 => url.push(c),
                        3 => secret.push(c),
                        _ => {}
                    },
                    KeyCode::Enter if *field < 3 => *field += 1,
                    KeyCode::Enter => {
                        if id.trim().is_empty() || url.trim().is_empty() || secret.trim().is_empty()
                        {
                            self.say("Ad, adres ve anahtar boş olamaz", false);
                        } else {
                            let (id, url, secret) = (
                                id.trim().to_string(),
                                url.trim().to_string(),
                                secret.trim().to_string(),
                            );
                            let dialect = DIALECTS[*dialect].to_string();
                            self.modal = Some(Modal::Busy("Sağlayıcı ekleniyor…".into()));
                            self.spawn(move |backend| {
                                Msg::Provider(
                                    backend.add_custom_provider(&id, &url, &dialect, &secret),
                                )
                            });
                            return;
                        }
                    }
                    _ => {}
                }
            }
            ConnMode::ConfirmDelete(id) => {
                if key.code == KeyCode::Char('y') {
                    let id = id.clone();
                    self.modal = Some(Modal::Busy("Kaldırılıyor…".into()));
                    self.spawn(move |backend| {
                        Msg::Provider(backend.run(&["providers".into(), "remove".into(), id], None))
                    });
                    return;
                }
                conn.mode = ConnMode::List;
            }
        }
        self.modal = Some(Modal::Conn(conn));
    }
}
