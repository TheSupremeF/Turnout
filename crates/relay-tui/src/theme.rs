//! Colors and the block-letter logo.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Product name shown in the window title. The identity itself lives in
/// `relay_core::brand`; this re-export keeps the UI import stable.
pub const APP_NAME: &str = relay_core::APP_NAME;
/// Uppercase wordmark drawn with [`glyph`]. Every letter must have a glyph.
pub const WORDMARK: &str = "TURNOUT";

pub const ACCENT: Color = Color::Rgb(122, 162, 255);
pub const ACCENT_2: Color = Color::Rgb(190, 130, 255);
pub const MINT: Color = Color::Rgb(84, 214, 170);
pub const AMBER: Color = Color::Rgb(230, 195, 106);
pub const RED: Color = Color::Rgb(240, 113, 120);
pub const TEXT: Color = Color::Rgb(220, 225, 240);
pub const DIM: Color = Color::Rgb(122, 131, 154);
pub const FAINT: Color = Color::Rgb(70, 78, 104);
pub const PANEL: Color = Color::Rgb(34, 40, 62);

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn dim() -> Style {
    Style::default().fg(DIM)
}

pub fn bold(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn lerp(from: Color, to: Color, t: f32) -> Color {
    let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (from, to) else {
        return from;
    };
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
}

/// Three-row, four-column glyphs in the same half-block style OpenCode uses.
fn glyph(letter: char) -> [&'static str; 3] {
    match letter {
        'A' => ["▄▀▀▄", "█▀▀█", "▀  ▀"],
        'E' => ["█▀▀▀", "█▀▀ ", "▀▀▀▀"],
        'K' => ["█ ▄▀", "██▄ ", "▀  ▀"],
        'L' => ["█   ", "█   ", "▀▀▀▀"],
        'N' => ["█▄ █", "█ ▀█", "▀  ▀"],
        'O' => ["█▀▀█", "█  █", "▀▀▀▀"],
        'R' => ["█▀▀█", "█▀▀▄", "▀  ▀"],
        'T' => ["▀██▀", " ██ ", " ▀▀ "],
        'U' => ["█  █", "█  █", " ▀▀ "],
        'Y' => ["█  █", "▀▄▄▀", "  ▀ "],
        _ => ["    ", "    ", "    "],
    }
}

pub fn logo_width(name: &str) -> u16 {
    (name.chars().count() * 5).saturating_sub(1) as u16
}

/// The logo as three gradient-colored lines.
pub fn logo(name: &str) -> Vec<Line<'static>> {
    let letters = name.chars().collect::<Vec<_>>();
    let total = logo_width(name).max(1) as f32;
    (0..3)
        .map(|row| {
            let mut spans = Vec::new();
            let mut column = 0f32;
            for (index, letter) in letters.iter().enumerate() {
                for ch in glyph(*letter)[row].chars() {
                    let color = lerp(ACCENT, ACCENT_2, column / total);
                    spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
                    column += 1.0;
                }
                if index + 1 < letters.len() {
                    spans.push(Span::raw(" "));
                    column += 1.0;
                }
            }
            Line::from(spans)
        })
        .collect()
}
