//! Drawing. Pure functions of `App`; nothing here changes application state
//! except list scroll positions that belong to the open dialog.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap,
};
use ratatui::Frame;

use crate::app::{
    App, Catalog, Confirm, Conn, ConnMode, Input, Menu, Message, Modal, Output, Palette, Picker,
    Readiness, Review, DIALECTS,
};
use crate::data::{is_chat_model, provider_name, Target, CLAUDE, HARNESSES};
use crate::theme::{self, bold, dim, text, ACCENT, AMBER, DIM, FAINT, MINT, PANEL, RED, WORDMARK};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    home(frame, app, area);
    if app.modal.is_some() {
        // Dim everything behind a dialog so it reads as the focus.
        frame.buffer_mut().set_style(
            area,
            Style::default().fg(FAINT).remove_modifier(Modifier::BOLD),
        );
    }
    // The dialog owns scroll state, so it is taken out while `app` is borrowed.
    if let Some(mut modal) = app.modal.take() {
        match &mut modal {
            Modal::Palette(palette) => palette_dialog(frame, palette, area),
            Modal::Menu(menu) => menu_dialog(frame, menu, area),
            Modal::Input(input) => input_dialog(frame, input, area),
            Modal::Output(output) => output_dialog(frame, output, area),
            Modal::Confirm(confirm) => confirm_dialog(frame, confirm, area),
            Modal::Picker(picker) => picker_dialog(frame, app, picker, area),
            Modal::Chain(edit) => chain_dialog(frame, app, edit, area),
            Modal::Review(review) => review_dialog(frame, review, area),
            Modal::Conn(conn) => conn_dialog(frame, app, conn, area),
            Modal::Message(message) => message_dialog(frame, message, area),
            Modal::Help => help_dialog(frame, area),
            Modal::Busy(label) => busy_dialog(frame, app.tick, label, area),
        }
        app.modal = Some(modal);
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn keys(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (key, label)) in pairs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(key.to_string(), bold(ACCENT)));
        spans.push(Span::styled(format!(" {label}"), dim()));
    }
    Line::from(spans)
}

fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(Span::width).sum();
    let gap = (width as usize).saturating_sub(used).max(1);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

fn dialog(title: &str, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(format!(" {title} "), bold(color)))
}

// ---- home ---------------------------------------------------------------

fn home(frame: &mut Frame, app: &App, area: Rect) {
    let compact = area.height < 24;
    let logo_height = if compact { 0 } else { 5 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(logo_height),
            Constraint::Length(1 + 4 * HARNESSES.len() as u16),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    if !compact {
        let mut lines = theme::logo(WORDMARK);
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Her sağlayıcıyı Codex, Claude Code ve OpenCode'a yönlendir",
            dim(),
        )));
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), rows[0]);
    }

    let width = rows[1].width.min(100);
    let column = Rect {
        x: rows[1].x + (rows[1].width - width) / 2,
        width,
        ..rows[1]
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("YÖNLENDİRMELER", bold(DIM)))),
        Rect {
            height: 1,
            ..column
        },
    );
    for (index, (id, name, _)) in HARNESSES.iter().enumerate() {
        let card = Rect {
            y: column.y + 1 + 4 * index as u16,
            height: 4,
            ..column
        };
        harness_card(frame, app, index, id, name, card);
    }

    toast_line(frame, app, rows[3]);
    frame.render_widget(
        Paragraph::new(keys(&[
            ("↑↓", "seç"),
            ("⏎", "modeller"),
            ("a", "uygula"),
            ("x", "çalıştır"),
            ("c", "bağlantılar"),
            (":", "tüm komutlar"),
            ("?", "yardım"),
            ("q", "çık"),
        ]))
        .alignment(Alignment::Center),
        rows[4],
    );
}

fn harness_card(frame: &mut Frame, app: &App, index: usize, id: &str, name: &str, area: Rect) {
    let selected = app.cursor == index && app.modal.is_none();
    let info = &app.harnesses[index];
    let draft = &app.drafts[index];
    let inner_width = area.width.saturating_sub(4);

    let dot = if info.installed {
        Span::styled("●", Style::default().fg(MINT))
    } else {
        Span::styled("●", Style::default().fg(RED))
    };
    let status = if app.dirty(index) {
        Span::styled("● uygulanmamış değişiklik", Style::default().fg(AMBER))
    } else if app.saved.contains_key(id) {
        Span::styled("✓ uygulandı", Style::default().fg(MINT))
    } else {
        Span::styled("henüz ayarlanmadı", dim())
    };
    let first = spread(
        vec![
            dot,
            Span::raw(" "),
            Span::styled(name.to_string(), bold(theme::TEXT)),
            Span::styled(format!("  {}", info.detail), dim()),
        ],
        vec![status],
        inner_width,
    );

    let second = match draft.default_target() {
        None => Line::from(Span::styled("  model seçilmedi — ⏎ ile seçin", dim())),
        Some(target) => {
            let mut spans = vec![
                Span::styled("  ", dim()),
                Span::styled(target.model.clone(), bold(ACCENT)),
                Span::styled(format!("  {}", provider_name(&target.provider)), dim()),
            ];
            if draft.targets.len() > 1 {
                spans.push(Span::styled(
                    format!(
                        "  +{} model · {}",
                        draft.targets.len() - 1,
                        draft.providers_summary()
                    ),
                    dim(),
                ));
            }
            if index == CLAUDE && draft.hybrid {
                spans.push(Span::styled(
                    "  [orijinal liste korunuyor]",
                    Style::default().fg(MINT),
                ));
            }
            Line::from(spans)
        }
    };

    let border = if selected { ACCENT } else { FAINT };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if selected {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(Style::default().fg(border))
        .padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(vec![first, second]).block(block), area);
}

fn toast_line(frame: &mut Frame, app: &App, area: Rect) {
    let Some(toast) = &app.toast else { return };
    if toast.at.elapsed().as_secs() >= 6 {
        return;
    }
    let color = if toast.ok { MINT } else { RED };
    let mark = if toast.ok { "✓" } else { "✕" };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{mark} "), bold(color)),
            Span::styled(toast.text.clone(), Style::default().fg(color)),
        ]))
        .alignment(Alignment::Center),
        area,
    );
}

// ---- model picker ---------------------------------------------------------

fn readiness_line(app: &App, provider: &str, harness: usize) -> Line<'static> {
    let (mark, label, color) = match app.readiness(provider, harness) {
        Readiness::Key => ("●", "anahtar hazır".to_string(), MINT),
        Readiness::Oauth => ("●", "resmî oturum bağlı".to_string(), MINT),
        Readiness::SubscriptionOnly => (
            "●",
            "yalnızca Claude Code — burada API anahtarı gerekir (c)".to_string(),
            AMBER,
        ),
        Readiness::Missing => ("○", "bağlı değil — c ile bağlantı ekleyin".to_string(), RED),
    };
    Line::from(vec![
        Span::styled(format!("{mark} "), Style::default().fg(color)),
        Span::styled(label, Style::default().fg(color)),
    ])
}

fn picker_dialog(frame: &mut Frame, app: &App, picker: &mut Picker, area: Rect) {
    let harness = picker.harness;
    let name = HARNESSES[harness].1;
    let tabs = app.tabs();
    let provider = tabs[picker.tab].clone();
    let shown = app.entries(&provider, &picker.query).len().max(4) as u16;
    let footer = if harness == CLAUDE { 3 } else { 2 };
    let height = (shown + 4 + footer + 2).clamp(12, area.height.saturating_sub(2).max(12));
    let rect = centered(area, 84, height);
    frame.render_widget(Clear, rect);
    let block = dialog(&format!("Modeller — {name}"), ACCENT);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let draft = &app.drafts[harness];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(footer),
        ])
        .split(inner);

    // Provider tabs with the number of models picked from each.
    let mut tab_spans = Vec::new();
    for (index, id) in tabs.iter().enumerate() {
        let count = draft
            .targets
            .iter()
            .filter(|target| &target.provider == id)
            .count();
        let label = if count > 0 {
            format!(" {} {count} ", provider_name(id))
        } else {
            format!(" {} ", provider_name(id))
        };
        let style = if index == picker.tab {
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            dim()
        };
        tab_spans.push(Span::styled(label, style));
        tab_spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(tab_spans)), rows[0]);
    frame.render_widget(
        Paragraph::new(readiness_line(app, &provider, harness)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("⌕ ", bold(ACCENT)),
            Span::styled(picker.query.clone(), text()),
            Span::styled("▏", Style::default().fg(ACCENT)),
        ])),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(PANEL),
        ))),
        rows[3],
    );

    let entries = app.entries(&provider, &picker.query);
    let placeholder = match app.catalog.get(provider.as_str()) {
        Some(Catalog::Loading) => Some(format!(
            "{} model listesi yükleniyor…",
            SPINNER[(app.tick as usize / 2) % SPINNER.len()]
        )),
        Some(Catalog::Failed(error)) if entries.is_empty() => {
            Some(format!("Liste alınamadı: {error}"))
        }
        _ if entries.is_empty()
            && provider != "anthropic"
            && app.catalog.get(provider.as_str()).is_none() =>
        {
            Some("Bu sağlayıcı bağlı değil — c ile ekleyin".to_string())
        }
        _ if entries.is_empty() => Some("Eşleşen model yok".to_string()),
        _ => None,
    };
    if let Some(note) = placeholder {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(note, dim())))
                .wrap(Wrap { trim: true }),
            rows[4],
        );
    } else {
        let items = entries
            .iter()
            .map(|model| {
                let target = Target::new(&provider, model);
                let picked = draft.contains(&target);
                let is_default = draft.default_target() == Some(&target);
                let mut spans = vec![
                    Span::styled(
                        if picked { " ◉ " } else { " ○ " },
                        Style::default().fg(if picked { ACCENT } else { FAINT }),
                    ),
                    Span::styled(
                        model.clone(),
                        if picked { bold(theme::TEXT) } else { dim() },
                    ),
                ];
                if is_default {
                    spans.push(Span::styled("  ★ varsayılan", Style::default().fg(AMBER)));
                }
                if let Some(chain) = draft.chains.get(&target) {
                    let names = chain.iter().map(|t| t.model.as_str()).collect::<Vec<_>>().join(", ");
                    spans.push(Span::styled(format!("  ↳ fallback: {names}"), dim()));
                }
                if !is_chat_model(model) {
                    spans.push(Span::styled(
                        "  sohbet modeli değil",
                        Style::default().fg(FAINT),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .highlight_style(Style::default().bg(PANEL))
            .highlight_symbol("▌");
        frame.render_stateful_widget(list, rows[4], &mut picker.list);
    }

    let fallback = draft.fallback;
    let mut footer = vec![
        Line::from(vec![
            Span::styled("Ctrl+F ", bold(ACCENT)),
            Span::styled(
                if fallback { "[x] " } else { "[ ] " },
                Style::default().fg(if fallback { MINT } else { DIM }),
            ),
            Span::styled("Hata verirse rotadaki diğer modellere geç ", text()),
            Span::styled(if fallback { "(açık)" } else { "(kapalı)" }, dim()),
        ]),
        keys(&[
            ("␣", "seç"),
            ("⏎", "varsayılan"),
            ("Ctrl+L", "bu modelin fallback'i"),
            ("←→", "sağlayıcı"),
            ("Esc", "bitti"),
        ]),
    ];
    if harness == CLAUDE {
        let on = draft.hybrid;
        footer.insert(
            0,
            Line::from(vec![
                Span::styled("Ctrl+O ", bold(ACCENT)),
                Span::styled(
                    if on { "[x] " } else { "[ ] " },
                    Style::default().fg(if on { MINT } else { DIM }),
                ),
                Span::styled("Orijinal Claude listesini koru", text()),
            ]),
        );
    }
    frame.render_widget(Paragraph::new(footer), rows[5]);
}

// ---- fallback chain -------------------------------------------------------------

fn chain_dialog(frame: &mut Frame, app: &App, edit: &crate::app::ChainEdit, area: Rect) {
    let candidates = app.chain_candidates(edit);
    let height = (candidates.len().max(1) as u16 + 9).min(area.height.saturating_sub(2).max(9));
    let rect = centered(area, 76, height);
    frame.render_widget(Clear, rect);
    let block = dialog(&format!("Fallback — {}", edit.from.model), ACCENT);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{} hata verirse (429, aşırı yük, ulaşılamıyor) sırayla:", edit.from.model),
            text(),
        )),
        Line::from(""),
    ];
    if candidates.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Seçili başka model yok — önce modellere ekleyin.",
            dim(),
        )));
    }
    for (index, target) in candidates.iter().enumerate() {
        let order = edit.chain.iter().position(|item| item == target);
        let marker = if index == edit.cursor { "▌ " } else { "  " };
        let mut spans = vec![Span::styled(marker, Style::default().fg(ACCENT))];
        spans.push(match order {
            Some(n) => Span::styled(format!("{} ◉ ", n + 1), bold(ACCENT)),
            None => Span::styled("  ○ ", Style::default().fg(FAINT)),
        });
        spans.push(Span::styled(
            format!("{}  {}", target.model, provider_name(&target.provider)),
            if order.is_some() { bold(theme::TEXT) } else { text() },
        ));
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Önce aynı model 2 kez yeniden denenir. Yanıt akmaya başladıysa başka modele geçilmez.",
        dim(),
    )));
    lines.push(Line::from(""));
    lines.push(keys(&[("␣", "ekle/çıkar"), ("+ -", "sıra"), ("⏎", "tamam"), ("Esc", "vazgeç")]));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// ---- plan review ------------------------------------------------------------

fn review_dialog(frame: &mut Frame, review: &mut Review, area: Rect) {
    let width = area.width.saturating_sub(6).min(120);
    let wrap_width = width.saturating_sub(2).max(1) as usize;
    let wrapped: usize = review
        .text
        .lines()
        .map(|line| line.chars().count().max(1).div_ceil(wrap_width))
        .sum();
    let height = (wrapped as u16 + 4).clamp(8, area.height.saturating_sub(2).max(8));
    let rect = centered(area, width, height);
    frame.render_widget(Clear, rect);
    let block = dialog(&format!("Plan — {}", HARNESSES[review.harness].1), AMBER);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(inner);

    let lines = review
        .text
        .lines()
        .map(|line| {
            let style = if line.starts_with("+++") || line.starts_with("---") {
                bold(theme::TEXT)
            } else if line.starts_with('+') {
                Style::default().fg(MINT)
            } else if line.starts_with('-') {
                Style::default().fg(RED)
            } else if line.starts_with("@@") {
                Style::default().fg(ACCENT)
            } else if line.starts_with("note:") || line.starts_with('⚠') {
                Style::default().fg(AMBER)
            } else if line.starts_with("TURNOUT AYARLARI") {
                bold(DIM)
            } else if line.ends_with("kaldırılacak") {
                Style::default().fg(RED)
            } else if line.ends_with("yeni") || line.ends_with("değişti") {
                Style::default().fg(MINT)
            } else if line.starts_with("  ") && line.contains(" → ") || line.starts_with("  fallback") {
                text()
            } else if line.starts_with("plan ") {
                bold(MINT)
            } else {
                dim()
            };
            Line::from(Span::styled(line.to_string(), style))
        })
        .collect::<Vec<_>>();
    let max_scroll = (wrapped as u16).saturating_sub(parts[0].height);
    review.scroll = review.scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((review.scroll, 0)),
        parts[0],
    );
    frame.render_widget(
        Paragraph::new(keys(&[
            ("y ⏎", "uygula"),
            ("n Esc", "vazgeç"),
            ("↑↓ PgUp PgDn", "kaydır"),
        ])),
        parts[1],
    );
}

// ---- connections --------------------------------------------------------------

fn conn_dialog(frame: &mut Frame, app: &App, conn: &Conn, area: Rect) {
    let extra = if matches!(conn.mode, ConnMode::Custom { .. }) {
        5
    } else {
        0
    };
    let rows = 7 + app.oauth.len().max(1) + app.providers.len().max(1) + 1 + extra;
    let rect = centered(
        area,
        84,
        (rows as u16 + 2).min(area.height.saturating_sub(2).max(10)),
    );
    frame.render_widget(Clear, rect);
    let block = dialog("Bağlantılar", ACCENT);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let mut lines = vec![Line::from(Span::styled("RESMÎ OTURUMLAR", bold(DIM)))];
    if app.oauth.is_empty() {
        lines.push(Line::from(Span::styled("  (okunuyor…)", dim())));
    }
    for item in &app.oauth {
        let (label, note, color) = match (item.id.as_str(), item.logged_in) {
            ("anthropic", true) => (
                "Anthropic / Claude",
                "yalnızca Claude Code içinde çalışır".to_string(),
                AMBER,
            ),
            ("openai", true) if item.gateway_capable => (
                "OpenAI / ChatGPT",
                "Codex ve Claude için hazır".to_string(),
                MINT,
            ),
            ("openai", true) => ("OpenAI / ChatGPT", "yalnızca Codex".to_string(), AMBER),
            (_, false) => (
                item.id.as_str(),
                "bağlı değil (turnout auth login)".to_string(),
                RED,
            ),
            (_, true) => (item.id.as_str(), "bağlı".to_string(), MINT),
        };
        let plan = if item.plan.is_empty() {
            String::new()
        } else {
            format!(" · {}", item.plan)
        };
        lines.push(Line::from(vec![
            Span::styled("  ● ", Style::default().fg(color)),
            Span::styled(label.to_string(), text()),
            Span::styled(format!("{plan}  {note}"), Style::default().fg(color)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("API SAĞLAYICILARI", bold(DIM))));
    if app.providers.is_empty() {
        lines.push(Line::from(Span::styled(
            "  henüz sağlayıcı eklenmedi — n ile ekleyin",
            dim(),
        )));
    }
    for (index, provider) in app.providers.iter().enumerate() {
        let selected = index == conn.cursor
            && matches!(conn.mode, ConnMode::List | ConnMode::ConfirmDelete(_));
        let (mark, color, note) = if provider.has_key {
            ("●", MINT, "anahtar hazır")
        } else {
            ("○", RED, "anahtar yok")
        };
        let mut line = Line::from(vec![
            Span::styled(
                if selected { " ▌" } else { "  " },
                Style::default().fg(ACCENT),
            ),
            Span::styled(format!("{mark} "), Style::default().fg(color)),
            Span::styled(
                provider_name(&provider.id),
                if selected { bold(theme::TEXT) } else { text() },
            ),
            Span::styled(format!("  {note}"), Style::default().fg(color)),
        ]);
        if selected {
            line = line.style(Style::default().bg(PANEL));
        }
        lines.push(line);
    }
    lines.push(Line::from(""));
    match &conn.mode {
        ConnMode::List => lines.push(keys(&[
            ("n", "yeni"),
            ("d", "kaldır"),
            ("r", "yenile"),
            ("Esc", "kapat"),
        ])),
        ConnMode::Preset => lines.push(keys(&[
            ("e", "EVREN"),
            ("a", "Anthropic API"),
            ("u", "özel uç nokta"),
            ("Esc", "geri"),
        ])),
        ConnMode::Custom {
            field,
            id,
            url,
            dialect,
            secret,
        } => {
            let row = |index: usize, label: &str, value: String| {
                let active = *field == index;
                Line::from(vec![
                    Span::styled(
                        if active { "▌ " } else { "  " },
                        Style::default().fg(ACCENT),
                    ),
                    Span::styled(
                        format!("{label:<10}"),
                        if active { bold(ACCENT) } else { dim() },
                    ),
                    Span::styled(value, text()),
                    Span::styled(if active { "▏" } else { "" }, Style::default().fg(ACCENT)),
                ])
            };
            lines.push(row(0, "ad", id.clone()));
            lines.push(row(1, "adres", url.clone()));
            lines.push(row(2, "protokol", format!("‹ {} ›", DIALECTS[*dialect])));
            lines.push(row(3, "anahtar", "•".repeat(secret.chars().count())));
            lines.push(Line::from(Span::styled(
                "Tab alan değiştir   ←→ protokol   ⏎ ilerle / kaydet   Esc iptal",
                dim(),
            )));
        }
        ConnMode::Key { preset, input } => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} anahtarı: ", provider_name(preset)),
                    bold(ACCENT),
                ),
                Span::styled("•".repeat(input.chars().count()), text()),
                Span::styled("▏", Style::default().fg(ACCENT)),
            ]));
            lines.push(Line::from(Span::styled(
                "Anahtar yalnızca senin okuyabileceğin bir dosyaya yazılır (şifre sorulmaz).   ⏎ kaydet   Esc iptal",
                dim(),
            )));
        }
        ConnMode::ConfirmDelete(id) => lines.push(Line::from(vec![
            Span::styled(
                format!("{} kaldırılsın mı? ", provider_name(id)),
                Style::default().fg(AMBER),
            ),
            Span::styled("y", bold(ACCENT)),
            Span::styled(" evet   ", dim()),
            Span::styled("herhangi bir tuş", bold(ACCENT)),
            Span::styled(" vazgeç", dim()),
        ])),
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// ---- small dialogs ---------------------------------------------------------------

fn message_dialog(frame: &mut Frame, message: &Message, area: Rect) {
    let color = if message.ok { MINT } else { RED };
    let width = 76.min(area.width);
    let lines = (message.text.chars().count() as u16 / width.saturating_sub(4).max(1)) + 5;
    let rect = centered(area, width, lines.clamp(6, area.height));
    frame.render_widget(Clear, rect);
    let block = dialog(&message.title, color);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Span::styled(message.text.clone(), text())).wrap(Wrap { trim: true }),
        parts[0],
    );
    frame.render_widget(
        Paragraph::new(Span::styled("Kapatmak için herhangi bir tuş", dim())),
        parts[1],
    );
}

fn busy_dialog(frame: &mut Frame, tick: u64, label: &str, area: Rect) {
    let rect = centered(area, (label.chars().count() as u16 + 8).max(30), 3);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", SPINNER[(tick as usize / 2) % SPINNER.len()]),
                bold(ACCENT),
            ),
            Span::styled(label.to_string(), text()),
        ]))
        .block(dialog("Bekleyin", ACCENT)),
        rect,
    );
}

fn palette_dialog(frame: &mut Frame, palette: &mut Palette, area: Rect) {
    let entries = App::palette_entries(&palette.query);
    let height = (entries.len().max(1) as u16 + 5).clamp(8, area.height.saturating_sub(2).max(8));
    let rect = centered(area, 84, height);
    frame.render_widget(Clear, rect);
    let block = dialog("Komutlar", ACCENT);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(": ", bold(ACCENT)),
            Span::styled(palette.query.clone(), text()),
            Span::styled("▏", Style::default().fg(ACCENT)),
        ])),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(PANEL),
        )),
        rows[1],
    );
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("Eşleşen komut yok", dim())),
            rows[2],
        );
    } else {
        let width = inner.width.saturating_sub(2);
        let items = entries
            .iter()
            .map(|(_, label, detail)| {
                ListItem::new(spread(
                    vec![Span::styled(format!(" {label}"), text())],
                    vec![Span::styled(
                        format!("{detail} "),
                        Style::default().fg(FAINT),
                    )],
                    width,
                ))
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .highlight_style(Style::default().bg(PANEL))
            .highlight_symbol("▌");
        frame.render_stateful_widget(list, rows[2], &mut palette.list);
    }
    frame.render_widget(
        Paragraph::new(keys(&[("⏎", "çalıştır"), ("yaz", "ara"), ("Esc", "kapat")])),
        rows[3],
    );
}

fn menu_dialog(frame: &mut Frame, menu: &mut Menu, area: Rect) {
    let height = (menu.items.len() as u16 + 4).clamp(7, area.height.saturating_sub(2).max(7));
    let rect = centered(area, 84, height);
    frame.render_widget(Clear, rect);
    let block = dialog(&menu.title, ACCENT);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let width = inner.width.saturating_sub(2);
    let items = menu
        .items
        .iter()
        .map(|item| {
            ListItem::new(spread(
                vec![Span::styled(format!(" {}", item.label), text())],
                vec![Span::styled(
                    format!("{} ", item.detail),
                    Style::default().fg(FAINT),
                )],
                width,
            ))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .highlight_style(Style::default().bg(PANEL))
        .highlight_symbol("▌");
    frame.render_stateful_widget(list, rows[0], &mut menu.list);
    frame.render_widget(
        Paragraph::new(keys(&[("↑↓", "seç"), ("⏎", "onayla"), ("Esc", "vazgeç")])),
        rows[1],
    );
}

fn input_dialog(frame: &mut Frame, input: &Input, area: Rect) {
    let rect = centered(area, 60, 6);
    frame.render_widget(Clear, rect);
    let block = dialog(&input.title, ACCENT);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let lines = vec![
        Line::from(Span::styled(input.prompt.clone(), dim())),
        Line::from(vec![
            Span::styled("› ", bold(ACCENT)),
            Span::styled(input.value.clone(), text()),
            Span::styled("▏", Style::default().fg(ACCENT)),
        ]),
        Line::from(""),
        keys(&[("⏎", "kaydet"), ("Esc", "vazgeç")]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn output_dialog(frame: &mut Frame, output: &mut Output, area: Rect) {
    let width = area.width.saturating_sub(6).min(110);
    let wrap_width = width.saturating_sub(2).max(1) as usize;
    let wrapped: usize = output
        .text
        .lines()
        .map(|line| line.chars().count().max(1).div_ceil(wrap_width))
        .sum();
    let height = (wrapped as u16 + 4).clamp(7, area.height.saturating_sub(2).max(7));
    let color = if output.ok { MINT } else { RED };
    let rect = centered(area, width, height);
    frame.render_widget(Clear, rect);
    let block = dialog(&output.title, color);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(2), Constraint::Length(1)])
        .split(inner);
    let lines = output
        .text
        .lines()
        .map(|line| {
            let style = if line.starts_with("[FAIL]")
                || line.starts_with("error")
                || line.contains(" stopped ")
            {
                Style::default().fg(RED)
            } else if line.starts_with("[ ok ]")
                || line.contains(" ready ")
                || line.trim_start().starts_with('+')
            {
                Style::default().fg(MINT)
            } else {
                text()
            };
            Line::from(Span::styled(line.to_string(), style))
        })
        .collect::<Vec<_>>();
    let max_scroll = (wrapped as u16).saturating_sub(parts[0].height);
    output.scroll = output.scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((output.scroll, 0)),
        parts[0],
    );
    frame.render_widget(
        Paragraph::new(keys(&[("↑↓", "kaydır"), ("Esc ⏎", "kapat")])),
        parts[1],
    );
}

fn confirm_dialog(frame: &mut Frame, confirm: &Confirm, area: Rect) {
    let width = 70.min(area.width);
    let lines = (confirm.text.chars().count() as u16 / width.saturating_sub(4).max(1)) + 5;
    let rect = centered(area, width, lines.clamp(6, area.height));
    frame.render_widget(Clear, rect);
    let block = dialog("Onay", AMBER);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Span::styled(confirm.text.clone(), text())).wrap(Wrap { trim: true }),
        parts[0],
    );
    frame.render_widget(
        Paragraph::new(keys(&[("y", "evet"), ("herhangi bir tuş", "vazgeç")])),
        parts[1],
    );
}

fn help_dialog(frame: &mut Frame, area: Rect) {
    let rect = centered(area, 78, 24);
    frame.render_widget(Clear, rect);
    let block = dialog("Yardım", ACCENT);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = [
        ("↑↓  j k", "araç seç"),
        (
            "⏎  m",
            "model seçici (sağlayıcılar arası geçerken seçimler korunur)",
        ),
        (
            "a",
            "değişiklikleri önizle ve uygula (yedek otomatik alınır)",
        ),
        (
            "x",
            "seçili aracı çalıştır (turnout run); bitince buraya dönülür",
        ),
        ("t", "varsayılan modelle bağlantı testi"),
        (
            "p",
            "seçimi başlatma profili olarak kaydet (config'e dokunmaz)",
        ),
        ("b", "yedekten geri al"),
        ("g", "Turnout Agent durumu"),
        ("d", "sağlık kontrolü (doctor)"),
        (
            "c",
            "bağlantılar: API anahtarı, özel uç nokta, resmî oturumlar",
        ),
        (":", "her komutun listesi (arama ile)"),
        ("r", "her şeyi yeniden oku"),
        ("q  Ctrl+C", "çık"),
    ];
    let mut lines = vec![
        Line::from(Span::styled("Kısayollar", bold(DIM))),
        Line::from(""),
    ];
    for (key, what) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("{key:<12}"), bold(ACCENT)),
            Span::styled(what, text()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Modeller seçilince config dosyalarına ancak 'a' ile yazılır; geri almak için 'b'.",
        dim(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Kapatmak için herhangi bir tuş",
        dim(),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
