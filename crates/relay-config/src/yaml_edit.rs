//! Indentation-aware YAML editing for mapping keys.
//!
//! Scope: mappings and scalar/sequence values are patched in place. Sequence
//! *items* are replaced as whole values — item-level patching errors out rather
//! than guessing at anchors or complex keys.

use serde_json::Value;

use relay_core::{RelayError, Result};

pub fn to_value(text: &str) -> Result<Value> {
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| RelayError::parse("<yaml>", e.to_string()))?;
    serde_json::to_value(parsed).map_err(|e| RelayError::parse("<yaml>", e.to_string()))
}

#[derive(Debug, Clone)]
struct Line {
    start: usize,
    end: usize,
    indent: usize,
    key: Option<String>,
    key_raw: String,
    has_inline: bool,
    is_seq: bool,
}

impl Line {
    fn is_content(&self) -> bool {
        self.key.is_some() || self.is_seq
    }
}

fn parse_lines(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut offset = 0usize;
    for raw in text.split_inclusive('\n') {
        let start = offset;
        let end = offset + raw.len();
        offset = end;
        let content = raw.trim_end_matches('\n');
        let indent = content.chars().take_while(|c| *c == ' ').count();
        let trimmed = content.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" || trimmed == "..." {
            lines.push(Line {
                start,
                end,
                indent,
                key: None,
                key_raw: String::new(),
                has_inline: false,
                is_seq: false,
            });
            continue;
        }
        if trimmed.starts_with("- ") || trimmed == "-" {
            lines.push(Line {
                start,
                end,
                indent,
                key: None,
                key_raw: String::new(),
                has_inline: true,
                is_seq: true,
            });
            continue;
        }
        match find_colon(content) {
            Some(colon) => {
                let key_raw = content[..colon].trim().to_string();
                let key = unquote(&key_raw);
                let rest = content[colon + 1..].trim();
                let has_inline = !(rest.is_empty() || rest.starts_with('#'));
                lines.push(Line {
                    start,
                    end,
                    indent,
                    key: Some(key),
                    key_raw,
                    has_inline,
                    is_seq: false,
                });
            }
            None => lines.push(Line {
                start,
                end,
                indent,
                key: None,
                key_raw: String::new(),
                has_inline: true,
                is_seq: false,
            }),
        }
    }
    lines
}

/// Position of the mapping `:` (outside quotes), requiring space/EOL after it.
fn find_colon(content: &str) -> Option<usize> {
    let mut in_quote: Option<char> = None;
    for (i, c) in content.char_indices() {
        match in_quote {
            Some(q) => {
                if c == q {
                    in_quote = None;
                }
            }
            None => match c {
                '"' | '\'' => in_quote = Some(c),
                '#' => return None,
                ':' => {
                    let next = content[i + 1..].chars().next();
                    if matches!(next, None | Some(' ') | Some('\t')) {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

fn unquote(raw: &str) -> String {
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    if bytes.len() >= 2
        && bytes[0] == bytes[bytes.len() - 1]
        && (bytes[0] == b'"' || bytes[0] == b'\'')
    {
        return raw[1..raw.len() - 1].to_string();
    }
    raw.to_string()
}

/// Full key path for each keyed line, and the exclusive end of its block
/// (trailing blank/comment lines are not part of the block).
fn paths_for(lines: &[Line]) -> Vec<(Vec<String>, usize)> {
    let mut stack: Vec<(usize, Vec<String>)> = Vec::new();
    let mut out = vec![(Vec::new(), 0usize); lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if line.key.is_none() {
            continue;
        }
        while let Some((indent, _)) = stack.last() {
            if *indent >= line.indent {
                stack.pop();
            } else {
                break;
            }
        }
        let mut path = stack.last().map(|(_, p)| p.clone()).unwrap_or_default();
        path.push(line.key.clone().unwrap());
        out[i] = (path.clone(), 0);
        stack.push((line.indent, path));
    }
    for i in 0..lines.len() {
        if lines[i].key.is_none() {
            continue;
        }
        let mut cursor = i + 1;
        let mut last_content = i + 1;
        while cursor < lines.len() {
            let line = &lines[cursor];
            if line.is_content() {
                if line.indent <= lines[i].indent {
                    break;
                }
                last_content = cursor + 1;
            }
            cursor += 1;
        }
        out[i].1 = last_content;
    }
    out
}

/// Replace or insert `path` with `value`.
pub fn set(text: &str, path: &[String], value: &Value) -> Result<String> {
    if path.is_empty() {
        return Err(RelayError::KeyNotFound("(empty)".to_string()));
    }
    let lines = parse_lines(text);
    let indexed = paths_for(&lines);
    let unit = detect_unit(&lines);

    // Exact match: rewrite the line (and its block when the shape changes).
    if let Some((i, _)) = indexed
        .iter()
        .enumerate()
        .find(|(_, (p, _))| p.as_slice() == path)
    {
        return Ok(rewrite_line(text, &lines, &indexed, i, value, &unit));
    }

    // Deepest existing ancestor.
    let mut best: Option<(usize, Vec<String>)> = None;
    for (i, (p, _)) in indexed.iter().enumerate() {
        if p.len() < path.len() && path.starts_with(p.as_slice()) {
            let deeper = best.as_ref().map(|(_, bp)| bp.len()).unwrap_or(0) <= p.len();
            if deeper {
                best = Some((i, p.clone()));
            }
        }
    }

    let missing: &[String] = match &best {
        Some((_, parent_path)) => &path[parent_path.len()..],
        None => &path[..],
    };
    let value = nest(&missing[1..], value);

    match &best {
        Some((i, _)) => {
            let line = &lines[*i];
            let block_end = indexed[*i].1;
            let (insert_at, parent_indent) = if line.has_inline {
                // A scalar parent becomes a block header to hold the child.
                (line.end, line.indent)
            } else {
                (
                    lines[block_end.saturating_sub(1)].end.min(text.len()),
                    line.indent,
                )
            };
            let mut out = String::with_capacity(text.len() + 64);
            out.push_str(&text[..insert_at]);
            out.push_str(
                &block_lines(parent_indent + unit.len(), &missing[0], &value, &unit).join(""),
            );
            out.push_str(&text[insert_at..]);
            if line.has_inline {
                // `key: scalar` -> `key:` so the inserted block reads correctly.
                let content = &text[line.start..line.end];
                let colon = find_colon(content).unwrap_or(content.len() - 1);
                let header = format!("{}\n", &content[..=colon]);
                out = out.replacen(content, &header, 1);
            }
            Ok(out)
        }
        None => Ok(append_block(text, 0, &missing[0], &value, &unit)),
    }
}

/// Remove `path` (key line plus its block).
pub fn remove(text: &str, path: &[String]) -> Result<(String, bool)> {
    let lines = parse_lines(text);
    let indexed = paths_for(&lines);
    let Some((i, _)) = indexed
        .iter()
        .enumerate()
        .find(|(_, (p, _))| p.as_slice() == path)
    else {
        return Ok((text.to_string(), false));
    };
    let end = indexed[i].1;
    let start = lines[i].start;
    let tail = lines[end.saturating_sub(1)].end.min(text.len());
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&text[tail..]);
    Ok((out, true))
}

fn rewrite_line(
    text: &str,
    lines: &[Line],
    indexed: &[(Vec<String>, usize)],
    i: usize,
    value: &Value,
    unit: &str,
) -> String {
    let line = &lines[i];
    let block_end = indexed[i].1;
    let tail = lines[block_end.saturating_sub(1)].end.min(text.len());
    let mut out = String::with_capacity(text.len() + 64);
    out.push_str(&text[..line.start]);
    match value {
        Value::Object(map) if map.is_empty() => {
            out.push_str(&format!("{}: {{}}\n", line.key_raw));
        }
        Value::Array(items) if items.is_empty() => {
            out.push_str(&format!("{}: []\n", line.key_raw));
        }
        Value::Object(_) | Value::Array(_) => {
            out.push_str(&format!("{}:\n", line.key_raw));
            out.push_str(&render_block(value, line.indent + unit.len(), unit));
        }
        scalar => {
            out.push_str(&format!("{}: {}\n", line.key_raw, render_scalar(scalar)));
        }
    }
    out.push_str(&text[tail..]);
    out
}

fn append_block(text: &str, indent: usize, key: &str, value: &Value, unit: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    out.push_str(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&block_lines(indent, key, value, unit).join(""));
    out
}

fn nest(missing: &[String], value: &Value) -> Value {
    let mut current = value.clone();
    for key in missing.iter().rev() {
        current = serde_json::json!({ key.clone(): current });
    }
    current
}

/// One mapping entry as one or more lines (each ending with a newline).
fn block_lines(indent: usize, key: &str, value: &Value, unit: &str) -> Vec<String> {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(map) if map.is_empty() => vec![format!("{pad}{key}: {{}}\n")],
        Value::Array(items) if items.is_empty() => vec![format!("{pad}{key}: []\n")],
        Value::Object(_) | Value::Array(_) => {
            let mut out = vec![format!("{pad}{key}:\n")];
            out.push(render_block(value, indent + unit.len(), unit));
            out
        }
        scalar => vec![format!("{pad}{key}: {}\n", render_scalar(scalar))],
    }
}

/// Render a nested value as block lines at `indent`.
fn render_block(value: &Value, indent: usize, unit: &str) -> String {
    let pad = " ".repeat(indent);
    let mut out = String::new();
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                out.push_str(&block_lines(indent, key, item, unit).join(""));
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(map) if !map.is_empty() => {
                        let inner = render_block(item, indent + unit.len(), unit);
                        let first_line_end = inner.find('\n').map(|i| i + 1).unwrap_or(inner.len());
                        let first = inner[..first_line_end].trim_start().to_string();
                        out.push_str(&format!("{pad}- {first}"));
                        out.push_str(&inner[first_line_end..]);
                    }
                    scalar => out.push_str(&format!("{pad}- {}\n", render_scalar(scalar))),
                }
            }
        }
        scalar => out.push_str(&format!("{pad}{}\n", render_scalar(scalar))),
    }
    out
}

fn render_scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => quote_if_needed(s),
        _ => String::new(),
    }
}

fn quote_if_needed(value: &str) -> String {
    let needs = value.is_empty()
        || value != value.trim()
        || value.chars().next().is_some_and(|c| {
            matches!(
                c,
                '-' | '?'
                    | ':'
                    | ','
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '#'
                    | '&'
                    | '*'
                    | '!'
                    | '|'
                    | '>'
                    | '\''
                    | '"'
                    | '%'
                    | '@'
                    | '`'
                    | '~'
            )
        })
        || value.contains(": ")
        || value.contains(" #")
        || value.ends_with(':')
        || matches!(
            value,
            "true"
                | "false"
                | "null"
                | "yes"
                | "no"
                | "on"
                | "off"
                | "~"
                | "True"
                | "False"
                | "Null"
                | "Yes"
                | "No"
                | "On"
                | "Off"
        )
        || value.parse::<f64>().is_ok();
    if !needs {
        return value.to_string();
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

fn detect_unit(lines: &[Line]) -> String {
    let mut smallest = usize::MAX;
    let mut prev: Option<usize> = None;
    for line in lines {
        if !line.is_content() {
            continue;
        }
        if let Some(p) = prev {
            if line.indent > p {
                smallest = smallest.min(line.indent - p);
            }
        }
        prev = Some(line.indent);
    }
    if smallest == usize::MAX {
        "  ".to_string()
    } else {
        " ".repeat(smallest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replaces_inline_value_preserving_comments() {
        let text = "# header\nmodel: gpt-4o\nprovider:\n  openai:\n    baseURL: https://x\n";
        let out = set(text, &["model".into()], &json!("o3")).unwrap();
        assert!(out.contains("# header"));
        assert!(out.contains("model: o3"));
        assert!(out.contains("baseURL: https://x"));
    }

    #[test]
    fn inserts_nested_key_into_block() {
        let text = "provider:\n  openai:\n    models: {}\n";
        let out = set(
            text,
            &[
                "provider".into(),
                "openai".into(),
                "options".into(),
                "baseURL".into(),
            ],
            &json!("https://y"),
        )
        .unwrap();
        assert_eq!(
            out,
            "provider:\n  openai:\n    models: {}\n    options:\n      baseURL: https://y\n"
        );
    }

    #[test]
    fn quotes_values_that_need_it() {
        assert_eq!(quote_if_needed("plain"), "plain");
        assert_eq!(quote_if_needed("yes"), "\"yes\"");
        assert_eq!(quote_if_needed("a: b"), "\"a: b\"");
        assert_eq!(quote_if_needed("#tag"), "\"#tag\"");
        assert_eq!(quote_if_needed("https://x"), "https://x");
    }

    #[test]
    fn replaces_scalar_with_block() {
        let text = "a:\n  b: 1\n";
        let out = set(text, &["a".into()], &json!({ "c": 2 })).unwrap();
        assert_eq!(out, "a:\n  c: 2\n");
    }

    #[test]
    fn converts_scalar_parent_when_inserting_child() {
        let text = "model: gpt-4o\n";
        let out = set(text, &["model".into(), "fallback".into()], &json!("o3")).unwrap();
        assert_eq!(out, "model:\n  fallback: o3\n");
    }

    #[test]
    fn removes_key_with_block() {
        let text = "a: 1\nb:\n  c: 2\n  d: 3\ne: 4\n";
        let (out, changed) = remove(text, &["b".into()]).unwrap();
        assert!(changed);
        assert_eq!(out, "a: 1\ne: 4\n");
    }

    #[test]
    fn reads_yaml_values() {
        let value = to_value("a:\n  b: 1\n  c: two\n").unwrap();
        assert_eq!(value["a"]["b"], 1);
        assert_eq!(value["a"]["c"], "two");
    }
}
