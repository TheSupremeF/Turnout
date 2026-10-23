//! Lossless JSON / JSONC editing.
//!
//! Every patch is applied as a span replacement on the original text, so
//! comments, trailing commas, key order, formatting and unknown fields survive
//! untouched. This is what lets Relay edit a user's config without rewriting it.

use serde_json::Value;

use relay_core::{RelayError, Result};

pub(crate) type Span = (usize, usize);

#[derive(Clone, Copy, Debug, PartialEq)]
enum TokKind {
    Trivia,
    Punct,
    Str,
    Num,
    Bare,
}

#[derive(Clone, Debug)]
struct Tok {
    kind: TokKind,
    span: Span,
}

pub(crate) enum Node {
    Object(ObjNode),
    Array(ArrNode),
    Scalar(Span),
}

pub(crate) struct ObjNode {
    pub span: Span,
    pub entries: Vec<Entry>,
}

pub(crate) struct Entry {
    pub key: String,
    pub key_span: Span,
    pub value: Node,
}

pub(crate) struct ArrNode {
    pub span: Span,
    pub items: Vec<Node>,
}

impl Node {
    pub fn span(&self) -> Span {
        match self {
            Node::Object(obj) => obj.span,
            Node::Array(arr) => arr.span,
            Node::Scalar(span) => *span,
        }
    }
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

fn tokenize(text: &str) -> Result<Vec<Tok>> {
    let bytes = text.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => {
                while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                    i += 1;
                }
                toks.push(Tok {
                    kind: TokKind::Trivia,
                    span: (start, i),
                });
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                toks.push(Tok {
                    kind: TokKind::Trivia,
                    span: (start, i),
                });
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                toks.push(Tok {
                    kind: TokKind::Trivia,
                    span: (start, i),
                });
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                toks.push(Tok {
                    kind: TokKind::Str,
                    span: (start, i),
                });
            }
            b'0'..=b'9' | b'-' => {
                i += 1;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    i += 1;
                }
                toks.push(Tok {
                    kind: TokKind::Num,
                    span: (start, i),
                });
            }
            b'{' | b'}' | b'[' | b']' | b':' | b',' => {
                i += 1;
                toks.push(Tok {
                    kind: TokKind::Punct,
                    span: (start, i),
                });
            }
            _ => {
                while i < bytes.len() {
                    let ch = bytes[i] as char;
                    if ch.is_ascii_alphanumeric()
                        || ch == '_'
                        || ch == '$'
                        || ch == '-'
                        || ch == '.'
                    {
                        i += 1;
                    } else {
                        break;
                    }
                }
                if i == start {
                    return Err(RelayError::parse(
                        "<jsonc>",
                        format!("unexpected character '{}' at byte {start}", c as char),
                    ));
                }
                toks.push(Tok {
                    kind: TokKind::Bare,
                    span: (start, i),
                });
            }
        }
    }
    Ok(toks)
}

// ---------------------------------------------------------------------------
// Parser (tolerant: comments and trailing commas allowed)
// ---------------------------------------------------------------------------

struct Parser<'a> {
    text: &'a str,
    toks: Vec<Tok>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Result<Self> {
        Ok(Parser {
            text,
            toks: tokenize(text)?,
            pos: 0,
        })
    }

    fn peek(&mut self) -> Option<&Tok> {
        while let Some(tok) = self.toks.get(self.pos) {
            if tok.kind == TokKind::Trivia {
                self.pos += 1;
            } else {
                return Some(tok);
            }
        }
        None
    }

    fn bump(&mut self) -> Option<Tok> {
        self.peek()?;
        let tok = self.toks[self.pos].clone();
        self.pos += 1;
        Some(tok)
    }

    fn slice(&self, span: Span) -> &str {
        &self.text[span.0..span.1]
    }

    fn parse_document(&mut self) -> Result<Node> {
        let node = self.parse_value()?;
        if self.peek().is_some() {
            return Err(RelayError::parse("<jsonc>", "trailing content after value"));
        }
        Ok(node)
    }

    fn parse_value(&mut self) -> Result<Node> {
        let tok = self
            .bump()
            .ok_or_else(|| RelayError::parse("<jsonc>", "unexpected end of input"))?;
        match tok.kind {
            TokKind::Punct => match self.slice(tok.span).as_bytes()[0] {
                b'{' => {
                    let obj = self.parse_object(tok.span.0)?;
                    Ok(Node::Object(obj))
                }
                b'[' => {
                    let arr = self.parse_array(tok.span.0)?;
                    Ok(Node::Array(arr))
                }
                other => Err(RelayError::parse(
                    "<jsonc>",
                    format!("unexpected '{}'", other as char),
                )),
            },
            TokKind::Str | TokKind::Num | TokKind::Bare => Ok(Node::Scalar(tok.span)),
            TokKind::Trivia => unreachable!("peek skips trivia"),
        }
    }

    fn parse_object(&mut self, start: usize) -> Result<ObjNode> {
        let mut entries = Vec::new();
        loop {
            let (kind, span) = match self.peek() {
                Some(tok) => (tok.kind, tok.span),
                None => return Err(RelayError::parse("<jsonc>", "unterminated object")),
            };
            if kind == TokKind::Punct && self.slice(span) == "}" {
                let close = self.bump().unwrap();
                return Ok(ObjNode {
                    span: (start, close.span.1),
                    entries,
                });
            }
            // Key
            let key_tok = self
                .bump()
                .ok_or_else(|| RelayError::parse("<jsonc>", "missing object key"))?;
            let key_raw = self.slice(key_tok.span).to_string();
            let key = match key_tok.kind {
                TokKind::Str => unquote(&key_raw),
                TokKind::Bare => key_raw,
                _ => return Err(RelayError::parse("<jsonc>", "invalid object key")),
            };
            // Colon
            let colon = self
                .bump()
                .ok_or_else(|| RelayError::parse("<jsonc>", "missing ':' after key"))?;
            if colon.kind != TokKind::Punct || self.slice(colon.span) != ":" {
                return Err(RelayError::parse("<jsonc>", "missing ':' after key"));
            }
            let value = self.parse_value()?;
            entries.push(Entry {
                key,
                key_span: key_tok.span,
                value,
            });
            // Separator
            let (kind, span) = match self.peek() {
                Some(tok) => (tok.kind, tok.span),
                None => return Err(RelayError::parse("<jsonc>", "unterminated object")),
            };
            if kind == TokKind::Punct {
                match self.slice(span) {
                    "," => {
                        self.bump();
                    }
                    "}" => {}
                    _ => return Err(RelayError::parse("<jsonc>", "expected ',' or '}'")),
                }
            }
        }
    }

    fn parse_array(&mut self, start: usize) -> Result<ArrNode> {
        let mut items = Vec::new();
        loop {
            let (kind, span) = match self.peek() {
                Some(tok) => (tok.kind, tok.span),
                None => return Err(RelayError::parse("<jsonc>", "unterminated array")),
            };
            if kind == TokKind::Punct {
                match self.slice(span) {
                    "]" => {
                        let close = self.bump().unwrap();
                        return Ok(ArrNode {
                            span: (start, close.span.1),
                            items,
                        });
                    }
                    "," => {
                        self.bump();
                        continue;
                    }
                    _ => {}
                }
            }
            items.push(self.parse_value()?);
            let (kind, span) = match self.peek() {
                Some(tok) => (tok.kind, tok.span),
                None => return Err(RelayError::parse("<jsonc>", "unterminated array")),
            };
            if kind == TokKind::Punct {
                match self.slice(span) {
                    "," => {
                        self.bump();
                    }
                    "]" => {}
                    _ => return Err(RelayError::parse("<jsonc>", "expected ',' or ']'")),
                }
            }
        }
    }
}

fn unquote(raw: &str) -> String {
    let inner = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Ok(code) = u32::from_str_radix(&hex, 16) {
                    if let Some(c) = char::from_u32(code) {
                        out.push(c);
                    }
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Strip comments and trailing commas so `serde_json` can read JSONC.
pub fn to_plain_json(text: &str) -> Result<String> {
    let toks = tokenize(text)?;
    let mut out = String::with_capacity(text.len());
    for (i, tok) in toks.iter().enumerate() {
        match tok.kind {
            TokKind::Trivia => {
                let raw = &text[tok.span.0..tok.span.1];
                // Keep newlines so line numbers stay meaningful in errors.
                for c in raw.chars() {
                    if c == '\n' {
                        out.push('\n');
                    }
                }
            }
            TokKind::Punct => {
                let raw = &text[tok.span.0..tok.span.1];
                if raw == "," {
                    // Drop the comma when the next significant token closes the
                    // container (trailing comma).
                    let next_significant = toks[i + 1..]
                        .iter()
                        .find(|t| t.kind != TokKind::Trivia)
                        .map(|t| &text[t.span.0..t.span.1]);
                    if matches!(next_significant, Some("}") | Some("]") | None) {
                        continue;
                    }
                }
                out.push_str(raw);
            }
            _ => out.push_str(&text[tok.span.0..tok.span.1]),
        }
    }
    Ok(out)
}

pub fn to_value(text: &str) -> Result<Value> {
    let plain = to_plain_json(text)?;
    serde_json::from_str(&plain).map_err(|e| RelayError::parse("<jsonc>", e.to_string()))
}

// ---------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------

pub(crate) enum Lookup<'a> {
    Found(&'a Node),
    MissingIn {
        object: &'a ObjNode,
        missing: Vec<String>,
    },
    MissingInArray,
}

pub(crate) fn lookup<'a>(root: &'a Node, path: &[String]) -> Lookup<'a> {
    let mut node = root;
    let mut idx = 0;
    loop {
        if idx == path.len() {
            return Lookup::Found(node);
        }
        match node {
            Node::Object(obj) => match obj.entries.iter().find(|e| e.key == path[idx]) {
                Some(entry) => {
                    node = &entry.value;
                    idx += 1;
                }
                None => {
                    return Lookup::MissingIn {
                        object: obj,
                        missing: path[idx..].to_vec(),
                    }
                }
            },
            Node::Array(arr) => match path[idx].parse::<usize>() {
                Ok(index) if index < arr.items.len() => {
                    node = &arr.items[index];
                    idx += 1;
                }
                _ => return Lookup::MissingInArray,
            },
            Node::Scalar(_) => return Lookup::MissingInArray,
        }
    }
}

/// Replace or insert `path` with `value`, preserving everything else.
pub fn set(text: &str, path: &[String], value: &Value) -> Result<String> {
    if path.is_empty() {
        return Ok(render_root(value));
    }
    // A missing or empty file starts life as an empty object.
    let text = if text.trim().is_empty() { "{}\n" } else { text };
    let root = Parser::new(text)?.parse_document()?;
    match lookup(&root, path) {
        Lookup::Found(node) => {
            let span = node.span();
            let unit = detect_unit(text);
            let indent = line_indent(text, span.0);
            let replacement = render_pretty(value, &unit, &indent);
            let mut out = String::with_capacity(text.len() + replacement.len());
            out.push_str(&text[..span.0]);
            out.push_str(&replacement);
            out.push_str(&text[span.1..]);
            Ok(out)
        }
        Lookup::MissingIn { object, missing } => {
            let nested = nest_value(&missing[1..], value);
            let unit = detect_unit(text);
            let (insert_at, content) = insertion(text, object, &missing[0], &nested, &unit)?;
            let mut out = String::with_capacity(text.len() + content.len());
            out.push_str(&text[..insert_at]);
            out.push_str(&content);
            out.push_str(&text[insert_at..]);
            Ok(out)
        }
        Lookup::MissingInArray => Err(RelayError::KeyNotFound(path.join("."))),
    }
}

/// Remove `path` if present. Returns the new text and whether anything changed.
pub fn remove(text: &str, path: &[String]) -> Result<(String, bool)> {
    if text.trim().is_empty() {
        return Ok((text.to_string(), false));
    }
    let root = Parser::new(text)?.parse_document()?;
    // Find the parent object holding the final key.
    if path.is_empty() {
        return Ok((text.to_string(), false));
    }
    let (parent_path, key) = path.split_at(path.len() - 1);
    let key = &key[0];
    let (object, entry) = match lookup(&root, parent_path) {
        Lookup::Found(Node::Object(obj)) => match obj.entries.iter().find(|e| e.key == *key) {
            Some(entry) => (obj, entry),
            None => return Ok((text.to_string(), false)),
        },
        _ => return Ok((text.to_string(), false)),
    };
    let _ = object;
    // Remove from key start to value end, plus the separator comma.
    let mut start = entry.key_span.0;
    let mut end = entry.value.span().1;
    // Preceding whitespace
    while start > 0 && (text.as_bytes()[start - 1] as char).is_ascii_whitespace() {
        start -= 1;
    }
    // Following separator or preceding one
    let bytes = text.as_bytes();
    let mut probe = end;
    while probe < bytes.len() && (bytes[probe] as char).is_ascii_whitespace() {
        probe += 1;
    }
    if probe < bytes.len() && bytes[probe] == b',' {
        end = probe + 1;
    } else {
        // Remove the comma before the entry instead.
        let mut pre = entry.key_span.0;
        while pre > 0 && (bytes[pre - 1] as char).is_ascii_whitespace() {
            pre -= 1;
        }
        if pre > 0 && bytes[pre - 1] == b',' {
            start = pre - 1;
        }
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&text[end..]);
    Ok((out, true))
}

fn nest_value(missing: &[String], value: &Value) -> Value {
    let mut current = value.clone();
    for key in missing.iter().rev() {
        current = serde_json::json!({ key.clone(): current });
    }
    current
}

fn render_root(value: &Value) -> String {
    format!("{}\n", render_pretty(value, "  ", ""))
}

/// Render a value with hanging indentation matching the target container.
fn render_pretty(value: &Value, unit: &str, base_indent: &str) -> String {
    match value {
        Value::Object(map) if !map.is_empty() => {
            let mut out = String::from("{\n");
            let inner = format!("{base_indent}{unit}");
            for (i, (key, val)) in map.iter().enumerate() {
                let rendered = render_pretty(val, unit, &inner);
                out.push_str(&inner);
                out.push_str(&serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()));
                out.push_str(": ");
                out.push_str(&rendered);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(base_indent);
            out.push('}');
            out
        }
        Value::Array(items) if !items.is_empty() => {
            let mut out = String::from("[\n");
            let inner = format!("{base_indent}{unit}");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&inner);
                out.push_str(&render_pretty(item, unit, &inner));
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(base_indent);
            out.push(']');
            out
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_string()),
    }
}

fn render_inline(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn line_indent(text: &str, at: usize) -> String {
    let line_start = text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    text[line_start..at]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

fn detect_unit(text: &str) -> String {
    for line in text.lines().skip(1) {
        let indent: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        if !indent.is_empty() && line.trim().starts_with('"') {
            return indent;
        }
    }
    "  ".to_string()
}

/// Where and what to insert for a new key inside `object`.
fn insertion(
    text: &str,
    object: &ObjNode,
    key: &str,
    value: &Value,
    unit: &str,
) -> Result<(usize, String)> {
    let open = object.span.0;
    let close = object.span.1 - 1; // index of '}'
    let body = &text[open + 1..close];
    // An object with content decides its own style; an empty one follows the
    // surrounding file.
    let compact = if body.trim().is_empty() {
        !text.contains('\n')
    } else {
        !body.contains('\n')
    };
    let key_json = serde_json::to_string(key).map_err(|e| RelayError::other(e.to_string()))?;

    if object.entries.is_empty() {
        if compact {
            return Ok((open + 1, format!("{key_json}: {}", render_inline(value))));
        }
        let parent_indent = line_indent(text, open);
        let child_indent = format!("{parent_indent}{unit}");
        return Ok((
            open + 1,
            format!(
                "\n{child_indent}{key_json}: {}",
                render_pretty(value, unit, &child_indent)
            ) + &format!("\n{parent_indent}"),
        ));
    }

    // Indentation of existing children.
    let first_key_at = object.entries[0].key_span.0;
    let child_indent = line_indent(text, first_key_at);
    let last_value_end = object.entries.last().unwrap().value.span().1;

    if compact {
        // Trailing comma already present?
        let between = &text[last_value_end..close];
        let has_trailing_comma = between.contains(',');
        let sep = if has_trailing_comma { " " } else { ", " };
        return Ok((close, format!("{sep}{key_json}: {}", render_inline(value))));
    }

    // Does a trailing comma follow the last entry?
    let between = &text[last_value_end..close];
    let mut insert_at = last_value_end;
    let mut prefix = String::from(",");
    if let Some(offset) = between.find(',') {
        // Use the existing trailing comma instead of adding another one.
        let comma_at = last_value_end + offset;
        // Only if the comma is not inside a comment.
        let line_tail = &text[text[..comma_at].rfind('\n').map(|i| i + 1).unwrap_or(0)..comma_at];
        if !line_tail.trim_start().starts_with("//") && !line_tail.trim_start().starts_with('*') {
            insert_at = comma_at + 1;
            prefix = String::new();
        }
    }
    Ok((
        insert_at,
        format!(
            "{prefix}\n{child_indent}{key_json}: {}",
            render_pretty(value, unit, &child_indent)
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replaces_value_preserving_comments() {
        let text =
            "{\n  // keep this note\n  \"a\": 1, // trailing note\n  \"b\": {\"x\": true}\n}\n";
        let out = set(text, &["a".into()], &json!(2)).unwrap();
        assert!(out.contains("// keep this note"));
        assert!(out.contains("// trailing note"));
        assert!(out.contains("\"a\": 2"));
        assert!(out.contains("\"x\": true"));
    }

    #[test]
    fn inserts_nested_key_with_indent() {
        let text = "{\n  \"a\": 1\n}\n";
        let out = set(text, &["b".into(), "c".into()], &json!("hi")).unwrap();
        assert_eq!(
            out,
            "{\n  \"a\": 1,\n  \"b\": {\n    \"c\": \"hi\"\n  }\n}\n"
        );
    }

    #[test]
    fn uses_existing_trailing_comma() {
        let text = "{\n  \"a\": 1,\n}\n";
        let out = set(text, &["b".into()], &json!(true)).unwrap();
        assert_eq!(out, "{\n  \"a\": 1,\n  \"b\": true\n}\n");
    }

    #[test]
    fn inserts_into_compact_object() {
        let text = "{\"a\":1,\"b\":2}";
        let out = set(text, &["c".into()], &json!(3)).unwrap();
        assert_eq!(out, "{\"a\":1,\"b\":2, \"c\": 3}");
    }

    #[test]
    fn inserts_into_empty_object() {
        let text = "{\n  \"a\": {}\n}\n";
        let out = set(text, &["a".into(), "z".into()], &json!(1)).unwrap();
        assert_eq!(out, "{\n  \"a\": {\n    \"z\": 1\n  }\n}\n");
    }

    #[test]
    fn replaces_array_element() {
        let text = "{\n  \"list\": [1, 2, 3]\n}\n";
        let out = set(text, &["list".into(), "1".into()], &json!(9)).unwrap();
        assert!(out.contains("[1, 9, 3]"));
    }

    #[test]
    fn removes_key_keeping_comments() {
        let text = "{\n  // note\n  \"a\": 1,\n  \"b\": 2\n}\n";
        let (out, changed) = remove(text, &["a".into()]).unwrap();
        assert!(changed);
        assert!(out.contains("// note"));
        assert!(out.contains("\"b\": 2"));
        assert!(!out.contains("\"a\""));
    }

    #[test]
    fn creates_nested_keys_from_empty_text() {
        let out = set("", &["a".into(), "b".into()], &json!(1)).unwrap();
        assert_eq!(to_value(&out).unwrap(), json!({"a": {"b": 1}}));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn reads_jsonc_with_trailing_commas() {
        let value = to_value("{\n  // c\n  \"a\": [1, 2,],\n}").unwrap();
        assert_eq!(value, json!({"a": [1, 2]}));
    }

    #[test]
    fn pretty_renders_nested_values() {
        let out = render_pretty(&json!({"k": {"n": [1, 2]}}), "  ", "");
        assert_eq!(
            out,
            "{\n  \"k\": {\n    \"n\": [\n      1,\n      2\n    ]\n  }\n}"
        );
    }
}
