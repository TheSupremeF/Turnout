//! Model ids compiled into the installed Claude Code.
//!
//! A Claude subscription (OAuth) cannot list models through `GET /v1/models`
//! without handing Turnout the login token, which Turnout never does. The
//! installed Claude Code binary (or `cli.js` for npm installs) carries every
//! model id it can select, so the picker reads them from there and follows
//! Claude Code updates without a Turnout release.

use std::path::Path;

const FAMILIES: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// Canonical model ids found in `bytes`, newest first within each family.
///
/// Dated snapshots (`-20250929`) and Bedrock variants (`-v1`) fold into their
/// alias, `X-0` folds into `X`, and anything with extra words
/// (`claude-fable-5-mythos-5`) is dropped as internal.
pub fn model_ids_in(bytes: &[u8]) -> Vec<String> {
    const PREFIX: &[u8] = b"claude-";
    let mut found = std::collections::BTreeSet::new();
    let mut i = 0;
    while i + PREFIX.len() <= bytes.len() {
        if &bytes[i..i + PREFIX.len()] != PREFIX {
            i += 1;
            continue;
        }
        // Must start a token, not sit inside `anthropic.claude-...`-like words.
        let starts_token = i == 0 || !is_id_byte(bytes[i - 1]) || bytes[i - 1] == b'.';
        let mut end = i + PREFIX.len();
        while end < bytes.len() && is_id_byte(bytes[end]) && end - i < 64 {
            end += 1;
        }
        if starts_token {
            if let Ok(token) = std::str::from_utf8(&bytes[i..end]) {
                if let Some(id) = canonical(token) {
                    found.insert(id);
                }
            }
        }
        i = end.max(i + 1);
    }
    let mut ids: Vec<String> = found.into_iter().collect();
    // `claude-opus-4-0` is the same model as `claude-opus-4`.
    let snapshot = ids.clone();
    ids.retain(|id| {
        !(id.ends_with("-0") && snapshot.contains(&id[..id.len() - 2].to_string()))
    });
    ids.sort_by_key(|id| sort_key(id));
    ids
}

/// Read the binary at `path` and list its model ids.
pub fn model_ids_in_binary(path: &Path) -> std::io::Result<Vec<String>> {
    Ok(model_ids_in(&std::fs::read(path)?))
}

/// "claude-opus-4-8" -> "Claude Opus 4.8".
pub fn model_label(id: &str) -> String {
    let mut parts = id.split('-');
    let _ = parts.next();
    let family = parts.next().unwrap_or_default();
    let version: Vec<&str> = parts.collect();
    let mut family_title = family.to_string();
    if let Some(first) = family_title.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    format!("Claude {family_title} {}", version.join("."))
}

fn is_id_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
}

fn canonical(token: &str) -> Option<String> {
    let mut parts: Vec<&str> = token.trim_end_matches('-').split('-').collect();
    if parts.len() < 3 || parts[0] != "claude" || !FAMILIES.contains(&parts[1]) {
        return None;
    }
    if parts.last().is_some_and(|p| p.len() >= 2 && p.starts_with('v') && p[1..].bytes().all(|b| b.is_ascii_digit())) {
        parts.pop();
    }
    if parts.last().is_some_and(|p| p.len() == 8 && p.bytes().all(|b| b.is_ascii_digit())) {
        parts.pop();
    }
    let version = &parts[2..];
    let numeric = |p: &&str| !p.is_empty() && p.len() <= 2 && p.bytes().all(|b| b.is_ascii_digit());
    if version.is_empty() || version.len() > 2 || !version.iter().all(numeric) {
        return None;
    }
    Some(parts.join("-"))
}

fn sort_key(id: &str) -> (usize, std::cmp::Reverse<Vec<u32>>) {
    let parts: Vec<&str> = id.split('-').collect();
    let family = FAMILIES.iter().position(|f| *f == parts[1]).unwrap_or(FAMILIES.len());
    let version = parts[2..].iter().filter_map(|p| p.parse().ok()).collect();
    (family, std::cmp::Reverse(version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_snapshots_and_drops_internal_names() {
        let blob = br#"x"claude-opus-4-8" claude-sonnet-4-5-20250929 claude-sonnet-4-5-20250929-v1
            claude-opus-4-0 claude-opus-4 claude-fable-5-mythos-5 claude-fable-5-1 anthropic.claude-haiku-4-5-20251001-v1:0
            xclaude-opus-9 claude-instant-1 claude-sonnet-3-7 claude-opus-5"#;
        assert_eq!(
            model_ids_in(blob),
            vec![
                "claude-fable-5-1",
                "claude-opus-5",
                "claude-opus-4-8",
                "claude-opus-4",
                "claude-sonnet-4-5",
                "claude-sonnet-3-7",
                "claude-haiku-4-5",
            ]
        );
    }

    #[test]
    fn labels_read_like_product_names() {
        assert_eq!(model_label("claude-opus-4-8"), "Claude Opus 4.8");
        assert_eq!(model_label("claude-sonnet-5"), "Claude Sonnet 5");
    }
}
