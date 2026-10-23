//! Surgical TOML editing: only the touched key/value spans change, so
//! comments, ordering and unknown keys stay exactly as the user wrote them.

use serde_json::Value;

use relay_core::{RelayError, Result};

type Span = (usize, usize);

pub fn to_value(text: &str) -> Result<Value> {
    let parsed: toml::Value =
        toml::from_str(text).map_err(|e| RelayError::parse("<toml>", e.to_string()))?;
    serde_json::to_value(parsed).map_err(|e| RelayError::parse("<toml>", e.to_string()))
}

#[derive(Debug)]
struct Entry {
    path: Vec<String>,
    value_span: Span,
    line_start: usize,
    /// End of the whole entry, including continuation lines of multi-line values.
    line_end: usize,
    /// 0 = top level, n = the n-th table header in the file.
    section: usize,
}

#[derive(Debug)]
struct Section {
    path: Vec<String>,
    header_start: usize,
    header_end: usize,
}

#[derive(Debug, Default)]
struct Index {
    sections: Vec<Section>,
    entries: Vec<Entry>,
}

fn scan(text: &str) -> Index {
    let bytes = text.as_bytes();
    let mut index = Index::default();
    let mut current: Vec<String> = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let line_end = text[offset..]
            .find('\n')
            .map(|i| offset + i + 1)
            .unwrap_or(bytes.len());
        let line = &text[offset..line_end];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            offset = line_end;
            continue;
        }
        if trimmed.starts_with("[[") {
            if let Some(end) = trimmed.find("]]") {
                current = split_dotted(&trimmed[2..end]);
                index.sections.push(Section {
                    path: current.clone(),
                    header_start: offset,
                    header_end: line_end,
                });
            }
            offset = line_end;
            continue;
        }
        if trimmed.starts_with('[') {
            if let Some(end) = trimmed.rfind(']') {
                current = split_dotted(&trimmed[1..end]);
                index.sections.push(Section {
                    path: current.clone(),
                    header_start: offset,
                    header_end: line_end,
                });
            }
            offset = line_end;
            continue;
        }
        // key = value
        if let Some(eq) = find_assignment(trimmed) {
            let key_part = trimmed[..eq].trim();
            let mut path = current.clone();
            path.extend(split_dotted(key_part));
            // Value span in absolute coordinates.
            let trimmed_start = offset + (line.len() - line.trim_start().len());
            let value_rel = trimmed[eq + 1..].len() - trimmed[eq + 1..].trim_start().len();
            let value_start = trimmed_start + eq + 1 + value_rel;
            let value_end = scan_value_end(text, value_start, line_end);
            // Multi-line arrays / strings continue past the first line; skip
            // their continuation lines so `]` is never read as a table header.
            let entry_end = text[value_end..]
                .find('\n')
                .map(|i| value_end + i + 1)
                .unwrap_or(text.len());
            index.entries.push(Entry {
                path,
                value_span: (value_start, value_end),
                line_start: offset,
                line_end: entry_end,
                section: index.sections.len(),
            });
            offset = entry_end;
            continue;
        }
        offset = line_end;
    }
    index
}

/// Find the top-level `=` in a trimmed `key = value` line (outside quotes).
fn find_assignment(line: &str) -> Option<usize> {
    let mut in_string: Option<char> = None;
    let mut depth = 0i32;
    for (i, c) in line.char_indices() {
        match in_string {
            Some(q) => {
                if c == q {
                    in_string = None;
                }
            }
            None => match c {
                '"' | '\'' => in_string = Some(c),
                '[' | '{' => depth += 1,
                ']' | '}' => depth -= 1,
                '=' if depth <= 0 => return Some(i),
                '#' => return None,
                _ => {}
            },
        }
    }
    None
}

/// End of a value, handling multi-line arrays, inline tables and multi-line
/// strings. Stops before a trailing comment or the line break at top level.
fn scan_value_end(text: &str, start: usize, line_end: usize) -> usize {
    let bytes = text.as_bytes();
    for quote in ["\"\"\"", "'''"] {
        if text[start..].starts_with(quote) {
            let search = start + 3;
            return match text[search..].find(quote) {
                Some(offset) => search + offset + 3,
                None => text.len(),
            };
        }
    }
    let mut depth = 0i32;
    let mut in_string: Option<char> = None;
    let mut i = start;
    let mut end = start;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match in_string {
            Some(q) => {
                if c == '\\' {
                    i += 1;
                } else if c == q {
                    in_string = None;
                    end = i + 1;
                }
            }
            None => match c {
                '"' | '\'' => {
                    in_string = Some(c);
                    end = i + 1;
                }
                '[' | '{' => {
                    depth += 1;
                    end = i + 1;
                }
                ']' | '}' => {
                    depth -= 1;
                    end = i + 1;
                }
                '#' if depth <= 0 => return end.max(start),
                '\n' if depth <= 0 => return end.max(start),
                '\n' => {}
                _ => {
                    if !c.is_whitespace() {
                        end = i + 1;
                    }
                }
            },
        }
        i += 1;
    }
    let _ = line_end;
    end.max(start)
}

fn split_dotted(raw: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;
    for c in raw.trim().chars() {
        match in_quote {
            Some(q) => {
                if c == q {
                    in_quote = None;
                } else {
                    current.push(c);
                }
            }
            None => match c {
                '"' | '\'' => in_quote = Some(c),
                '.' => {
                    parts.push(current.trim().to_string());
                    current = String::new();
                }
                _ => current.push(c),
            },
        }
    }
    parts.push(current.trim().to_string());
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// Replace or insert `path` with `value`.
pub fn set(text: &str, path: &[String], value: &Value) -> Result<String> {
    if path.is_empty() {
        return Err(RelayError::KeyNotFound("(empty)".to_string()));
    }
    let index = scan(text);
    let rendered = render(value)?;

    if let Some(entry) = index.entries.iter().find(|e| e.path == path) {
        let (start, end) = entry.value_span;
        let mut out = String::with_capacity(text.len() + rendered.len());
        out.push_str(&text[..start]);
        out.push_str(&rendered);
        out.push_str(&text[end..]);
        return Ok(out);
    }

    // Insert `key = value` into the parent section, creating it when needed.
    let (parent, key) = path.split_at(path.len() - 1);
    let key = &key[0];
    let key_text = format_key(key);

    // Insert into the table that exactly matches the parent (or the top level).
    // Top-level keys must stay above the first table header, otherwise they
    // would silently become members of the last table.
    let line = format!("{key_text} = {rendered}\n");
    let target = if parent.is_empty() {
        Some(0)
    } else {
        index
            .sections
            .iter()
            .position(|s| s.path == parent)
            .map(|p| p + 1)
    };
    if let Some(id) = target {
        let last_entry = index
            .entries
            .iter()
            .filter(|e| e.section == id)
            .map(|e| e.line_end)
            .max();
        let (insert_at, blank_after) = match (last_entry, id) {
            (Some(end), _) => (end, false),
            (None, 0) => match index.sections.first() {
                Some(first) => (first.header_start, true),
                None => (text.len(), false),
            },
            (None, id) => (index.sections[id - 1].header_end, false),
        };
        let mut out = String::with_capacity(text.len() + line.len() + 2);
        out.push_str(&text[..insert_at]);
        if insert_at > 0 && !text[..insert_at].ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&line);
        if blank_after {
            out.push('\n');
        }
        out.push_str(&text[insert_at..]);
        return Ok(out);
    }

    // Append a new section at EOF.
    let mut out = String::with_capacity(text.len() + 64);
    out.push_str(text);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !parent.is_empty() {
        out.push('\n');
        out.push_str(&format!("[{}]\n", parent.join(".")));
    }
    out.push_str(&format!("{key_text} = {rendered}\n"));
    Ok(out)
}

/// Remove `path` if present (whole line, comments elsewhere untouched).
pub fn remove(text: &str, path: &[String]) -> Result<(String, bool)> {
    let index = scan(text);
    if let Some(entry) = index.entries.iter().find(|e| e.path == path) {
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..entry.line_start]);
        out.push_str(&text[entry.line_end..]);
        return Ok((out, true));
    }
    // A table (`[model_providers.relay]`) is not an entry: remove its header,
    // its keys and every sub-table (`[model_providers.relay.auth]`).
    let Some(position) = index.sections.iter().position(|s| s.path == path) else {
        return Ok((text.to_string(), false));
    };
    let start = index.sections[position].header_start;
    let end = index.sections[position + 1..]
        .iter()
        .find(|section| !section.path.starts_with(path))
        .map(|section| section.header_start)
        .unwrap_or(text.len());
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&text[end..]);
    Ok((out, true))
}

fn format_key(key: &str) -> String {
    if key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !key.is_empty()
    {
        key.to_string()
    } else {
        format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn render(value: &Value) -> Result<String> {
    match value {
        Value::Null => Err(RelayError::other("TOML cannot represent null")),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) => Ok(format!(
            "\"{}\"",
            s.replace('\\', "\\\\").replace('"', "\\\"")
        )),
        Value::Array(items) => {
            let parts = items
                .iter()
                .map(render)
                .collect::<Result<Vec<_>>>()?
                .join(", ");
            Ok(format!("[{parts}]"))
        }
        Value::Object(map) => {
            let parts = map
                .iter()
                .map(|(k, v)| Ok(format!("{} = {}", format_key(k), render(v)?)))
                .collect::<Result<Vec<_>>>()?
                .join(", ");
            Ok(format!("{{ {parts} }}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replaces_value_in_section_preserving_comments() {
        let text = "# top\nmodel = \"a\"\n\n[model_providers.relay]\n# keep\nbase_url = \"https://x\"\nenv_key = \"RELAY_API_KEY\"\n";
        let out = set(
            text,
            &["model_providers".into(), "relay".into(), "base_url".into()],
            &json!("https://y"),
        )
        .unwrap();
        assert!(out.contains("# keep"));
        assert!(out.contains("# top"));
        assert!(out.contains("base_url = \"https://y\""));
        assert!(out.contains("env_key = \"RELAY_API_KEY\""));
    }

    #[test]
    fn inserts_key_into_existing_section() {
        let text = "[a]\nx = 1\n";
        let out = set(text, &["a".into(), "y".into()], &json!(2)).unwrap();
        assert_eq!(out, "[a]\nx = 1\ny = 2\n");
    }

    #[test]
    fn creates_missing_section_at_eof() {
        let text = "x = 1\n";
        let out = set(text, &["a".into(), "b".into()], &json!("v")).unwrap();
        assert_eq!(out, "x = 1\n\n[a]\nb = \"v\"\n");
    }

    #[test]
    fn handles_multiline_values() {
        let text = "[a]\nlist = [\n  1,\n  2,\n]\nother = 3\n";
        let out = set(text, &["a".into(), "other".into()], &json!(4)).unwrap();
        assert!(out.contains("list = [\n  1,\n  2,\n]"));
        assert!(out.contains("other = 4"));
    }

    #[test]
    fn top_level_key_stays_above_the_first_table() {
        let text = "model = \"a\"\n\n[projects.x]\ntrust = true\n";
        let out = set(text, &["model_provider".into()], &json!("relay")).unwrap();
        let value = to_value(&out).unwrap();
        assert_eq!(value["model_provider"], "relay", "must be top level: {out}");
        assert_eq!(value["projects"]["x"].get("model_provider"), None);
    }

    #[test]
    fn top_level_key_into_file_that_starts_with_a_table() {
        let text = "[projects.x]\ntrust = true\n";
        let out = set(text, &["model".into()], &json!("m")).unwrap();
        let value = to_value(&out).unwrap();
        assert_eq!(value["model"], "m", "{out}");
        assert_eq!(value["projects"]["x"]["trust"], true);
    }

    #[test]
    fn multiline_array_is_not_mistaken_for_a_table_header() {
        let text = "[a]\nlist = [\n  1,\n]\n[b]\nz = 1\n";
        let out = set(text, &["a".into(), "extra".into()], &json!(true)).unwrap();
        let value = to_value(&out).unwrap();
        assert_eq!(value["a"]["extra"], true, "{out}");
        assert_eq!(value["a"]["list"], json!([1]));
        assert_eq!(value["b"]["z"], 1);
    }

    #[test]
    fn insert_into_parent_table_skips_subtables() {
        let text = "[a]\nx = 1\n[a.b]\ny = 2\n";
        let out = set(text, &["a".into(), "z".into()], &json!(3)).unwrap();
        let value = to_value(&out).unwrap();
        assert_eq!(value["a"]["z"], 3, "{out}");
        assert_eq!(value["a"]["b"].get("z"), None);
    }

    #[test]
    fn removes_entry_line() {
        let text = "[a]\nx = 1\n# note\ny = 2\n";
        let (out, changed) = remove(text, &["a".into(), "x".into()]).unwrap();
        assert!(changed);
        assert!(out.contains("# note"));
        assert!(!out.contains("x ="));
    }

    #[test]
    fn removes_a_whole_table_with_its_sub_tables() {
        let text = "# top\nmodel = \"m\"\n\n[model_providers.relay]\nname = \"Relay\"\nbase_url = \"http://x\"\n\n[model_providers.relay.auth]\ncommand = \"/old\"\n\n[model_providers.keep]\nname = \"keep\"\n";
        let (out, changed) = remove(text, &["model_providers".into(), "relay".into()]).unwrap();
        assert!(changed);
        assert!(!out.contains("model_providers.relay"), "{out}");
        assert!(out.contains("[model_providers.keep]"), "{out}");
        assert!(
            out.contains("# top") && out.contains("model = \"m\""),
            "{out}"
        );
        let value = to_value(&out).unwrap();
        assert!(value["model_providers"].get("relay").is_none());
        assert_eq!(value["model_providers"]["keep"]["name"], "keep");
    }

    #[test]
    fn removing_a_missing_table_is_a_no_op() {
        let text = "[a]\nx = 1\n";
        let (out, changed) = remove(text, &["b".into()]).unwrap();
        assert!(!changed);
        assert_eq!(out, text);
    }

    #[test]
    fn reads_toml_values() {
        let value = to_value("model = \"m\"\n[a.b]\nc = true\n").unwrap();
        assert_eq!(value["model"], "m");
        assert_eq!(value["a"]["b"]["c"], true);
    }
}
