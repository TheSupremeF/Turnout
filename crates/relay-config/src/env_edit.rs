//! Line-level `.env` editing. Comments and unrelated keys are preserved.

use serde_json::{Map, Value};

/// Parse the file into a flat key → value map.
pub fn to_value(text: &str) -> Value {
    let mut map = Map::new();
    for line in text.lines() {
        if let Some((key, value)) = assignment(line) {
            map.insert(key, Value::String(unquote(value.trim())));
        }
    }
    Value::Object(map)
}

/// Set `KEY=value`, preserving `export ` prefixes and quote style.
pub fn set(text: &str, key: &str, value: &str) -> String {
    let mut out = String::with_capacity(text.len() + value.len());
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        match assignment(line) {
            Some((existing, previous)) if existing == key && !replaced => {
                replaced = true;
                let (prefix, _) = split_assignment(line);
                let quoted = if previous.trim().starts_with('"')
                    && previous.trim().ends_with('"')
                    && previous.trim().len() >= 2
                {
                    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
                } else if previous.trim().starts_with('\'')
                    && previous.trim().ends_with('\'')
                    && previous.trim().len() >= 2
                {
                    format!("'{}'", value.replace('\'', "'\\''"))
                } else {
                    value.to_string()
                };
                out.push_str(prefix);
                out.push('=');
                out.push_str(&quoted);
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            _ => out.push_str(line),
        }
    }
    if !replaced {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(key);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    }
    out
}

/// Remove `KEY`, returning the new text and whether it changed.
pub fn remove(text: &str, key: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    for line in text.split_inclusive('\n') {
        if !changed {
            if let Some((existing, _)) = assignment(line) {
                if existing == key {
                    changed = true;
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    (out, changed)
}

fn split_assignment(line: &str) -> (&str, &str) {
    let line = line.trim_end_matches('\n');
    let eq = line.find('=').unwrap_or(line.len());
    (&line[..eq], line.get(eq + 1..).unwrap_or(""))
}

/// Parse `KEY=VALUE` with optional `export `.
pub(crate) fn assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
    if line.starts_with('#') || line.is_empty() {
        return None;
    }
    let eq = line.find('=')?;
    let key = line[..eq].trim().to_string();
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    Some((key, line[eq + 1..].to_string()))
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        return value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        return value[1..value.len() - 1].to_string();
    }
    // Strip inline comments on unquoted values.
    match value.split_once(" #") {
        Some((value, _)) => value.trim_end().to_string(),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_existing_key_preserving_style() {
        let text = "# comment\nexport OPENAI_API_KEY=\"old\"\nOTHER=1\n";
        let out = set(text, "OPENAI_API_KEY", "new");
        assert!(out.contains("export OPENAI_API_KEY=\"new\""));
        assert!(out.contains("# comment"));
        assert!(out.contains("OTHER=1"));
    }

    #[test]
    fn appends_missing_key() {
        let out = set("A=1\n", "B", "2");
        assert_eq!(out, "A=1\nB=2\n");
    }

    #[test]
    fn removes_key() {
        let (out, changed) = remove("A=1\nB=2\n", "A");
        assert!(changed);
        assert_eq!(out, "B=2\n");
    }

    #[test]
    fn reads_values() {
        let value = to_value("A=1\nexport B=\"two\"\n# c\nC=three # note\n");
        assert_eq!(value["A"], "1");
        assert_eq!(value["B"], "two");
        assert_eq!(value["C"], "three");
    }
}
