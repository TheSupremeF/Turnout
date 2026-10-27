//! Protocol conversion between the two dialects clients speak to the gateway
//! (Anthropic Messages, OpenAI Chat) and the neutral `ChatRequest` /
//! `StreamEvent` model that provider adapters understand.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use relay_core::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, MessageContent, ProtocolDialect,
    RelayError, Result, Role, StopReason, StreamEvent, ToolDef, Usage,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// tool_choice
// ---------------------------------------------------------------------------

enum Choice {
    Auto,
    Any,
    Tool(String),
}

fn render_choice(choice: Choice, upstream: ProtocolDialect) -> Value {
    match (choice, upstream) {
        (Choice::Auto, ProtocolDialect::AnthropicMessages) => json!({"type": "auto"}),
        (Choice::Any, ProtocolDialect::AnthropicMessages) => json!({"type": "any"}),
        (Choice::Tool(name), ProtocolDialect::AnthropicMessages) => {
            json!({"type": "tool", "name": name})
        }
        (Choice::Tool(name), ProtocolDialect::OpenAiResponses) => {
            json!({"type": "function", "name": name})
        }
        (Choice::Auto, _) => json!("auto"),
        (Choice::Any, _) => json!("required"),
        (Choice::Tool(name), _) => {
            json!({"type": "function", "function": {"name": name}})
        }
    }
}

fn choice_from_anthropic(v: &Value) -> Option<Choice> {
    match v["type"].as_str()? {
        "auto" => Some(Choice::Auto),
        "any" => Some(Choice::Any),
        "tool" => Some(Choice::Tool(v["name"].as_str()?.to_string())),
        _ => None,
    }
}

fn choice_from_openai(v: &Value) -> Option<Choice> {
    match v {
        Value::String(s) if s == "auto" => Some(Choice::Auto),
        Value::String(s) if s == "required" => Some(Choice::Any),
        Value::Object(_) => Some(Choice::Tool(
            v["function"]["name"]
                .as_str()
                .or_else(|| v["name"].as_str())?
                .to_string(),
        )),
        _ => None, // "none" cannot be expressed upstream; tools are simply omitted
    }
}

// ---------------------------------------------------------------------------
// Anthropic Messages  ->  neutral
// ---------------------------------------------------------------------------

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

pub fn anthropic_to_chat(
    v: &Value,
    upstream: ProtocolDialect,
    model_override: Option<&str>,
) -> Result<ChatRequest> {
    let model = model_override
        .map(str::to_string)
        .or_else(|| v["model"].as_str().map(str::to_string))
        .ok_or_else(|| RelayError::Validation("request has no model".into()))?;
    let raw = v["messages"]
        .as_array()
        .ok_or_else(|| RelayError::Validation("request has no messages".into()))?;

    let messages = raw
        .iter()
        .map(|m| {
            let role = if m["role"] == "assistant" {
                Role::Assistant
            } else {
                Role::User
            };
            let content = match &m["content"] {
                Value::String(s) => MessageContent::Text(s.clone()),
                Value::Array(blocks) => {
                    MessageContent::Blocks(blocks.iter().filter_map(anthropic_block).collect())
                }
                _ => MessageContent::Text(String::new()),
            };
            ChatMessage { role, content }
        })
        .collect();

    let tools = v["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                // Server-side tools (web search, …) carry no schema and cannot be relayed.
                .filter(|t| t["input_schema"].is_object())
                .map(|t| ToolDef {
                    name: t["name"].as_str().unwrap_or_default().to_string(),
                    description: t["description"].as_str().map(str::to_string),
                    input_schema: t["input_schema"].clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let tool_choice = if tools.is_empty() {
        None
    } else {
        choice_from_anthropic(&v["tool_choice"]).map(|c| render_choice(c, upstream))
    };

    Ok(ChatRequest {
        model,
        messages,
        system: Some(text_of(&v["system"])).filter(|s| !s.is_empty()),
        max_tokens: v["max_tokens"].as_u64(),
        temperature: v["temperature"].as_f64().map(|x| x as f32),
        top_p: v["top_p"].as_f64().map(|x| x as f32),
        stop: v["stop_sequences"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        tools,
        tool_choice,
        reasoning_effort: None,
        stream: v["stream"].as_bool().unwrap_or(false),
        metadata: Default::default(),
    })
}

fn anthropic_block(b: &Value) -> Option<ContentBlock> {
    match b["type"].as_str()? {
        "text" => Some(ContentBlock::Text {
            text: b["text"].as_str()?.to_string(),
        }),
        "image" if b["source"]["type"] == "base64" => Some(ContentBlock::Image {
            media_type: b["source"]["media_type"].as_str()?.to_string(),
            data: b["source"]["data"].as_str()?.to_string(),
        }),
        "tool_use" => Some(ContentBlock::ToolUse {
            id: b["id"].as_str()?.to_string(),
            name: b["name"].as_str()?.to_string(),
            input: b["input"].clone(),
        }),
        "tool_result" => Some(ContentBlock::ToolResult {
            tool_use_id: b["tool_use_id"].as_str()?.to_string(),
            content: text_of(&b["content"]),
            is_error: b["is_error"].as_bool().unwrap_or(false),
        }),
        // Provider-signed thinking cannot be replayed to a different provider.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// OpenAI Chat  ->  neutral
// ---------------------------------------------------------------------------

pub fn openai_to_chat(
    v: &Value,
    upstream: ProtocolDialect,
    model_override: Option<&str>,
) -> Result<ChatRequest> {
    let model = model_override
        .map(str::to_string)
        .or_else(|| v["model"].as_str().map(str::to_string))
        .ok_or_else(|| RelayError::Validation("request has no model".into()))?;
    let raw = v["messages"]
        .as_array()
        .ok_or_else(|| RelayError::Validation("request has no messages".into()))?;

    let mut system = String::new();
    let mut messages = Vec::new();
    for m in raw {
        match m["role"].as_str().unwrap_or("user") {
            "system" | "developer" => {
                if !system.is_empty() {
                    system.push('\n');
                }
                system.push_str(&text_of(&m["content"]));
            }
            "assistant" => {
                let mut blocks = Vec::new();
                let text = text_of(&m["content"]);
                if !text.is_empty() {
                    blocks.push(ContentBlock::Text { text });
                }
                for call in m["tool_calls"].as_array().into_iter().flatten() {
                    blocks.push(ContentBlock::ToolUse {
                        id: call["id"].as_str().unwrap_or_default().to_string(),
                        name: call["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        input: call["function"]["arguments"]
                            .as_str()
                            .and_then(|a| serde_json::from_str(a).ok())
                            .unwrap_or_else(|| json!({})),
                    });
                }
                messages.push(ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Blocks(blocks),
                });
            }
            "tool" => messages.push(ChatMessage {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: m["tool_call_id"].as_str().unwrap_or_default().to_string(),
                    content: text_of(&m["content"]),
                    is_error: false,
                }]),
            }),
            _ => {
                let content = match &m["content"] {
                    Value::Array(parts) => {
                        MessageContent::Blocks(parts.iter().filter_map(openai_part).collect())
                    }
                    other => MessageContent::Text(text_of(other)),
                };
                messages.push(ChatMessage {
                    role: Role::User,
                    content,
                });
            }
        }
    }

    let tools: Vec<ToolDef> = v["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter(|t| t["type"] == "function")
                .map(|t| ToolDef {
                    name: t["function"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    description: t["function"]["description"].as_str().map(str::to_string),
                    input_schema: t["function"]["parameters"].clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let tool_choice = if tools.is_empty() {
        None
    } else {
        choice_from_openai(&v["tool_choice"]).map(|c| render_choice(c, upstream))
    };

    Ok(ChatRequest {
        model,
        messages,
        system: Some(system).filter(|s| !s.is_empty()),
        max_tokens: v["max_completion_tokens"]
            .as_u64()
            .or_else(|| v["max_tokens"].as_u64()),
        temperature: v["temperature"].as_f64().map(|x| x as f32),
        top_p: v["top_p"].as_f64().map(|x| x as f32),
        stop: match &v["stop"] {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a
                .iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        },
        tools,
        tool_choice,
        // Chat-completions clients name it directly; OpenAI's own newer models
        // also accept `reasoning.effort`, so read both.
        reasoning_effort: v["reasoning_effort"]
            .as_str()
            .or_else(|| v["reasoning"]["effort"].as_str())
            .map(str::to_string),
        stream: v["stream"].as_bool().unwrap_or(false),
        metadata: Default::default(),
    })
}

fn openai_part(p: &Value) -> Option<ContentBlock> {
    match p["type"].as_str()? {
        "text" => Some(ContentBlock::Text {
            text: p["text"].as_str()?.to_string(),
        }),
        "image_url" => {
            let url = p["image_url"]["url"].as_str()?;
            let rest = url.strip_prefix("data:")?;
            let (media_type, data) = rest.split_once(";base64,")?;
            Some(ContentBlock::Image {
                media_type: media_type.to_string(),
                data: data.to_string(),
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// neutral  ->  responses (non-streaming)
// ---------------------------------------------------------------------------

fn anthropic_stop(reason: &StopReason, had_tool: bool) -> &'static str {
    match reason {
        StopReason::ToolUse => "tool_use",
        StopReason::EndTurn if had_tool => "tool_use",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        _ => "end_turn",
    }
}

fn openai_finish(reason: &StopReason, had_tool: bool) -> &'static str {
    match reason {
        StopReason::ToolUse => "tool_calls",
        StopReason::EndTurn if had_tool => "tool_calls",
        StopReason::MaxTokens => "length",
        StopReason::ContentFilter => "content_filter",
        _ => "stop",
    }
}

pub fn chat_to_anthropic(resp: &ChatResponse) -> Value {
    let had_tool = resp
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    let content: Vec<Value> = resp
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentBlock::Reasoning { text } => {
                Some(json!({"type": "thinking", "thinking": text, "signature": ""}))
            }
            ContentBlock::ToolUse { id, name, input } => {
                Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
            }
            _ => None,
        })
        .collect();
    json!({
        "id": resp.id, "type": "message", "role": "assistant", "model": resp.model,
        "content": content,
        "stop_reason": anthropic_stop(&resp.stop_reason, had_tool),
        "stop_sequence": null,
        "usage": {"input_tokens": resp.usage.input_tokens, "output_tokens": resp.usage.output_tokens},
    })
}

pub fn chat_to_openai(resp: &ChatResponse) -> Value {
    let had_tool = resp
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    let text: String = resp
        .content
        .iter()
        .filter_map(|b| {
            if let ContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    let reasoning: String = resp
        .content
        .iter()
        .filter_map(|b| {
            if let ContentBlock::Reasoning { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    let calls: Vec<Value> = resp
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "id": id, "type": "function",
                "function": {"name": name, "arguments": input.to_string()}
            })),
            _ => None,
        })
        .collect();
    let mut message = json!({"role": "assistant", "content": if text.is_empty() && !calls.is_empty() { Value::Null } else { json!(text) }});
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    json!({
        "id": resp.id, "object": "chat.completion", "created": now(), "model": resp.model,
        "choices": [{"index": 0, "message": message,
            "finish_reason": openai_finish(&resp.stop_reason, had_tool)}],
        "usage": {"prompt_tokens": resp.usage.input_tokens, "completion_tokens": resp.usage.output_tokens,
            "total_tokens": resp.usage.total()},
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// neutral stream  ->  SSE (both dialects)
// ---------------------------------------------------------------------------

/// Turns neutral stream events into the SSE text a client expects.
pub trait StreamEncoder: Send {
    /// Frames sent immediately, before the upstream produces anything.
    fn begin(&mut self) -> Vec<String>;
    fn push(&mut self, event: StreamEvent) -> Vec<String>;
    fn heartbeat(&mut self) -> String;
    /// Closing frames; idempotent.
    fn finish(&mut self) -> Vec<String>;
}

fn sse(event: &str, data: Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

#[derive(PartialEq, Clone, Copy)]
enum Open {
    Nothing,
    Text,
    Thinking,
    Tool(usize),
}

pub struct AnthropicEncoder {
    model: String,
    next_index: usize,
    open: Open,
    tool_slots: HashMap<usize, usize>,
    stop: Option<StopReason>,
    usage: Usage,
    finished: bool,
}

impl AnthropicEncoder {
    pub fn new(model: impl Into<String>) -> Self {
        AnthropicEncoder {
            model: model.into(),
            next_index: 0,
            open: Open::Nothing,
            tool_slots: HashMap::new(),
            stop: None,
            usage: Usage::default(),
            finished: false,
        }
    }

    fn current(&self) -> usize {
        self.next_index.saturating_sub(1)
    }

    fn close_open(&mut self, out: &mut Vec<String>) {
        if self.open != Open::Nothing {
            out.push(sse(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": self.current()}),
            ));
            self.open = Open::Nothing;
        }
    }

    fn open_block(&mut self, kind: Open, block: Value, out: &mut Vec<String>) {
        self.close_open(out);
        out.push(sse(
            "content_block_start",
            json!({"type": "content_block_start", "index": self.next_index, "content_block": block}),
        ));
        self.next_index += 1;
        self.open = kind;
    }

    fn delta(&self, delta: Value) -> String {
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": self.current(), "delta": delta}),
        )
    }
}

impl StreamEncoder for AnthropicEncoder {
    fn begin(&mut self) -> Vec<String> {
        vec![sse(
            "message_start",
            json!({"type": "message_start", "message": {
            "id": format!("msg_turnout_{}", now()), "type": "message", "role": "assistant",
            "model": self.model, "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0}}}),
        )]
    }

    fn push(&mut self, event: StreamEvent) -> Vec<String> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        match event {
            StreamEvent::Start { .. } | StreamEvent::Ping => {}
            StreamEvent::TextDelta { text } => {
                if self.open != Open::Text {
                    self.open_block(Open::Text, json!({"type": "text", "text": ""}), &mut out);
                }
                out.push(self.delta(json!({"type": "text_delta", "text": text})));
            }
            StreamEvent::ReasoningDelta { text } => {
                if self.open != Open::Thinking {
                    self.open_block(
                        Open::Thinking,
                        json!({"type": "thinking", "thinking": "", "signature": ""}),
                        &mut out,
                    );
                }
                out.push(self.delta(json!({"type": "thinking_delta", "thinking": text})));
            }
            StreamEvent::ToolCallDelta { delta } => {
                let slot = delta.index;
                if !self.tool_slots.contains_key(&slot) {
                    let id = delta
                        .id
                        .clone()
                        .unwrap_or_else(|| format!("toolu_turnout_{slot}"));
                    let name = delta.name.clone().unwrap_or_default();
                    self.open_block(
                        Open::Tool(slot),
                        json!({"type": "tool_use", "id": id, "name": name, "input": {}}),
                        &mut out,
                    );
                    self.tool_slots.insert(slot, self.current());
                }
                // Tool calls stream sequentially; fragments for a block that is
                // no longer open cannot be expressed and are dropped.
                if self.open == Open::Tool(slot) {
                    if let Some(args) = delta.arguments_delta.filter(|a| !a.is_empty()) {
                        out.push(
                            self.delta(json!({"type": "input_json_delta", "partial_json": args})),
                        );
                    }
                }
            }
            StreamEvent::Stop { reason } => self.stop = Some(reason),
            StreamEvent::Usage { usage } => {
                self.usage.input_tokens += usage.input_tokens;
                self.usage.output_tokens += usage.output_tokens;
            }
            StreamEvent::Error { message, code, .. } => {
                self.close_open(&mut out);
                out.push(sse(
                    "error",
                    json!({"type": "error", "error": {
                    "type": code.unwrap_or_else(|| "api_error".into()), "message": message}}),
                ));
                self.finished = true;
            }
        }
        out
    }

    fn heartbeat(&mut self) -> String {
        sse("ping", json!({"type": "ping"}))
    }

    fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        self.finished = true;
        self.close_open(&mut out);
        let reason = self.stop.clone().unwrap_or_default();
        out.push(sse("message_delta", json!({"type": "message_delta",
            "delta": {"stop_reason": anthropic_stop(&reason, !self.tool_slots.is_empty()), "stop_sequence": null},
            "usage": {"input_tokens": self.usage.input_tokens, "output_tokens": self.usage.output_tokens}})));
        out.push(sse("message_stop", json!({"type": "message_stop"})));
        out
    }
}

pub struct OpenAiEncoder {
    id: String,
    model: String,
    stop: Option<StopReason>,
    had_tool: bool,
    usage: Usage,
    finished: bool,
}

impl OpenAiEncoder {
    pub fn new(model: impl Into<String>) -> Self {
        OpenAiEncoder {
            id: format!("chatcmpl-turnout-{}", now()),
            model: model.into(),
            stop: None,
            had_tool: false,
            usage: Usage::default(),
            finished: false,
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> String {
        format!(
            "data: {}\n\n",
            json!({
            "id": self.id, "object": "chat.completion.chunk", "created": now(), "model": self.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        )
    }
}

impl StreamEncoder for OpenAiEncoder {
    fn begin(&mut self) -> Vec<String> {
        vec![self.chunk(json!({"role": "assistant", "content": ""}), None)]
    }

    fn push(&mut self, event: StreamEvent) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        match event {
            StreamEvent::Start { .. } | StreamEvent::Ping => Vec::new(),
            StreamEvent::TextDelta { text } => vec![self.chunk(json!({"content": text}), None)],
            StreamEvent::ReasoningDelta { text } => {
                vec![self.chunk(json!({"reasoning_content": text}), None)]
            }
            StreamEvent::ToolCallDelta { delta } => {
                self.had_tool = true;
                let mut call = json!({"index": delta.index, "function": {"arguments": delta.arguments_delta.clone().unwrap_or_default()}});
                if let Some(id) = &delta.id {
                    call["id"] = json!(id);
                    call["type"] = json!("function");
                }
                if let Some(name) = &delta.name {
                    call["function"]["name"] = json!(name);
                }
                vec![self.chunk(json!({"tool_calls": [call]}), None)]
            }
            StreamEvent::Stop { reason } => {
                self.stop = Some(reason);
                Vec::new()
            }
            StreamEvent::Usage { usage } => {
                self.usage.input_tokens += usage.input_tokens;
                self.usage.output_tokens += usage.output_tokens;
                Vec::new()
            }
            StreamEvent::Error { message, code, .. } => {
                self.finished = true;
                vec![format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({"error": {"message": message, "type": "api_error", "code": code}})
                )]
            }
        }
    }

    fn heartbeat(&mut self) -> String {
        ": ping\n\n".to_string()
    }

    fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let reason = self.stop.clone().unwrap_or_default();
        let mut out = vec![self.chunk(json!({}), Some(openai_finish(&reason, self.had_tool)))];
        if self.usage.total() > 0 {
            out.push(format!("data: {}\n\n", json!({
                "id": self.id, "object": "chat.completion.chunk", "created": now(), "model": self.model,
                "choices": [],
                "usage": {"prompt_tokens": self.usage.input_tokens, "completion_tokens": self.usage.output_tokens,
                    "total_tokens": self.usage.total()}})));
        }
        out.push("data: [DONE]\n\n".to_string());
        out
    }
}

// ---------------------------------------------------------------------------
// OpenAI Responses (what Codex speaks)  <->  neutral
// ---------------------------------------------------------------------------

fn responses_part(p: &Value) -> Option<ContentBlock> {
    match p["type"].as_str()? {
        "input_text" | "output_text" | "text" => Some(ContentBlock::Text {
            text: p["text"].as_str()?.to_string(),
        }),
        "input_image" => {
            let rest = p["image_url"].as_str()?.strip_prefix("data:")?;
            let (media_type, data) = rest.split_once(";base64,")?;
            Some(ContentBlock::Image {
                media_type: media_type.to_string(),
                data: data.to_string(),
            })
        }
        _ => None,
    }
}

pub fn responses_to_chat(
    v: &Value,
    upstream: ProtocolDialect,
    model_override: Option<&str>,
) -> Result<ChatRequest> {
    let model = model_override
        .map(str::to_string)
        .or_else(|| v["model"].as_str().map(str::to_string))
        .ok_or_else(|| RelayError::Validation("request has no model".into()))?;
    let mut system = text_of(&v["instructions"]);
    let items: Vec<Value> = match &v["input"] {
        Value::String(s) => vec![json!({"type": "message", "role": "user", "content": s})],
        Value::Array(items) => items.clone(),
        _ => Vec::new(),
    };

    let mut messages: Vec<ChatMessage> = Vec::new();
    for item in &items {
        match item["type"].as_str().unwrap_or("message") {
            "message" => {
                let blocks: Vec<ContentBlock> = match &item["content"] {
                    Value::String(s) => vec![ContentBlock::Text { text: s.clone() }],
                    Value::Array(parts) => parts.iter().filter_map(responses_part).collect(),
                    _ => Vec::new(),
                };
                match item["role"].as_str().unwrap_or("user") {
                    "system" | "developer" => {
                        let text: String = blocks
                            .iter()
                            .filter_map(|b| {
                                if let ContentBlock::Text { text } = b {
                                    Some(text.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        if !system.is_empty() {
                            system.push('\n');
                        }
                        system.push_str(&text);
                    }
                    role if !blocks.is_empty() => messages.push(ChatMessage {
                        role: if role == "assistant" {
                            Role::Assistant
                        } else {
                            Role::User
                        },
                        content: MessageContent::Blocks(blocks),
                    }),
                    _ => {}
                }
            }
            "function_call" => {
                let block = ContentBlock::ToolUse {
                    id: item["call_id"].as_str().unwrap_or_default().to_string(),
                    name: item["name"].as_str().unwrap_or_default().to_string(),
                    input: item["arguments"]
                        .as_str()
                        .and_then(|a| serde_json::from_str(a).ok())
                        .unwrap_or_else(|| json!({})),
                };
                // Tool calls belong to the assistant turn that precedes them.
                match messages.last_mut() {
                    Some(ChatMessage {
                        role: Role::Assistant,
                        content: MessageContent::Blocks(b),
                    }) => b.push(block),
                    _ => messages.push(ChatMessage {
                        role: Role::Assistant,
                        content: MessageContent::Blocks(vec![block]),
                    }),
                }
            }
            "function_call_output" => {
                let output = &item["output"];
                let content = if output.is_object() {
                    text_of(&output["content"])
                } else {
                    text_of(output)
                };
                messages.push(ChatMessage {
                    role: Role::User,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: item["call_id"].as_str().unwrap_or_default().to_string(),
                        content,
                        is_error: false,
                    }]),
                });
            }
            // Encrypted reasoning, local shell items, … cannot cross providers.
            _ => {}
        }
    }

    let tools: Vec<ToolDef> = v["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter(|t| t["type"] == "function")
                .map(|t| ToolDef {
                    name: t["name"].as_str().unwrap_or_default().to_string(),
                    description: t["description"].as_str().map(str::to_string),
                    input_schema: if t["parameters"].is_object() {
                        t["parameters"].clone()
                    } else {
                        json!({"type": "object"})
                    },
                })
                .collect()
        })
        .unwrap_or_default();
    let tool_choice = if tools.is_empty() {
        None
    } else {
        match &v["tool_choice"] {
            Value::String(s) if s == "auto" => Some(Choice::Auto),
            Value::String(s) if s == "required" => Some(Choice::Any),
            Value::Object(_) => v["tool_choice"]["name"]
                .as_str()
                .map(|n| Choice::Tool(n.to_string())),
            _ => None,
        }
        .map(|c| render_choice(c, upstream))
    };

    Ok(ChatRequest {
        model,
        messages,
        system: Some(system).filter(|s| !s.is_empty()),
        max_tokens: v["max_output_tokens"].as_u64(),
        temperature: v["temperature"].as_f64().map(|x| x as f32),
        top_p: v["top_p"].as_f64().map(|x| x as f32),
        stop: Vec::new(),
        tools,
        tool_choice,
        reasoning_effort: v["reasoning"]["effort"].as_str().map(str::to_string),
        stream: v["stream"].as_bool().unwrap_or(false),
        metadata: Default::default(),
    })
}

fn responses_usage(u: &Usage) -> Value {
    json!({"input_tokens": u.input_tokens, "output_tokens": u.output_tokens, "total_tokens": u.total(),
        "input_tokens_details": {"cached_tokens": 0}, "output_tokens_details": {"reasoning_tokens": 0}})
}

pub fn chat_to_responses(resp: &ChatResponse) -> Value {
    let mut output = Vec::new();
    for (i, block) in resp.content.iter().enumerate() {
        match block {
            ContentBlock::Text { text } => output.push(json!({"type": "message", "id": format!("msg_{i}"),
                "status": "completed", "role": "assistant",
                "content": [{"type": "output_text", "text": text, "annotations": []}]})),
            ContentBlock::ToolUse { id, name, input } => output.push(json!({"type": "function_call",
                "id": format!("fc_{i}"), "call_id": id, "name": name, "arguments": input.to_string(),
                "status": "completed"})),
            _ => {}
        }
    }
    json!({"id": resp.id, "object": "response", "created_at": now(), "status": "completed",
        "model": resp.model, "output": output, "usage": responses_usage(&resp.usage)})
}

enum ROpen {
    Nothing,
    Text {
        item_id: String,
        index: usize,
        text: String,
    },
    Reasoning {
        item_id: String,
        index: usize,
        text: String,
    },
    Tool {
        slot: usize,
        item_id: String,
        index: usize,
        call_id: String,
        name: String,
        args: String,
    },
}

pub struct ResponsesEncoder {
    id: String,
    model: String,
    seq: u64,
    next_output: usize,
    open: ROpen,
    seen_slots: std::collections::HashSet<usize>,
    output: Vec<Value>,
    stop: Option<StopReason>,
    usage: Usage,
    finished: bool,
}

impl ResponsesEncoder {
    pub fn new(model: impl Into<String>) -> Self {
        ResponsesEncoder {
            id: format!("resp_turnout_{}", now()),
            model: model.into(),
            seq: 0,
            next_output: 0,
            open: ROpen::Nothing,
            seen_slots: Default::default(),
            output: Vec::new(),
            stop: None,
            usage: Usage::default(),
            finished: false,
        }
    }

    fn ev(&mut self, name: &str, mut data: Value) -> String {
        data["type"] = json!(name);
        data["sequence_number"] = json!(self.seq);
        self.seq += 1;
        format!("event: {name}\ndata: {data}\n\n")
    }

    fn response_obj(&self, status: &str) -> Value {
        json!({"id": self.id, "object": "response", "created_at": now(), "status": status,
            "model": self.model, "output": self.output, "usage": responses_usage(&self.usage)})
    }

    fn close_open(&mut self, out: &mut Vec<String>) {
        match std::mem::replace(&mut self.open, ROpen::Nothing) {
            ROpen::Nothing => {}
            ROpen::Text {
                item_id,
                index,
                text,
            } => {
                out.push(self.ev("response.output_text.done", json!({"item_id": item_id, "output_index": index, "content_index": 0, "text": text})));
                let part = json!({"type": "output_text", "text": text, "annotations": []});
                out.push(self.ev("response.content_part.done", json!({"item_id": item_id, "output_index": index, "content_index": 0, "part": part})));
                let item = json!({"type": "message", "id": item_id, "status": "completed", "role": "assistant", "content": [part]});
                out.push(self.ev(
                    "response.output_item.done",
                    json!({"output_index": index, "item": item}),
                ));
                self.output.push(item);
            }
            ROpen::Reasoning {
                item_id,
                index,
                text,
            } => {
                let item = json!({"type": "reasoning", "id": item_id, "summary": [],
                    "content": [{"type": "reasoning_text", "text": text}]});
                out.push(self.ev(
                    "response.output_item.done",
                    json!({"output_index": index, "item": item}),
                ));
                self.output.push(item);
            }
            ROpen::Tool {
                item_id,
                index,
                call_id,
                name,
                args,
                ..
            } => {
                let args = if args.trim().is_empty() {
                    "{}".to_string()
                } else {
                    args
                };
                out.push(self.ev(
                    "response.function_call_arguments.done",
                    json!({"item_id": item_id, "output_index": index, "arguments": args}),
                ));
                let item = json!({"type": "function_call", "id": item_id, "call_id": call_id, "name": name,
                    "arguments": args, "status": "completed"});
                out.push(self.ev(
                    "response.output_item.done",
                    json!({"output_index": index, "item": item}),
                ));
                self.output.push(item);
            }
        }
    }

    fn open_item(&mut self, prefix: &str, item: Value, out: &mut Vec<String>) -> (String, usize) {
        self.close_open(out);
        let index = self.next_output;
        self.next_output += 1;
        let item_id = format!("{prefix}_{}_{index}", self.id);
        let mut item = item;
        item["id"] = json!(item_id);
        out.push(self.ev(
            "response.output_item.added",
            json!({"output_index": index, "item": item}),
        ));
        (item_id, index)
    }
}

impl StreamEncoder for ResponsesEncoder {
    fn begin(&mut self) -> Vec<String> {
        let created = json!({"response": {"id": self.id, "object": "response", "created_at": now(),
            "status": "in_progress", "model": self.model, "output": []}});
        vec![self.ev("response.created", created)]
    }

    fn push(&mut self, event: StreamEvent) -> Vec<String> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        match event {
            StreamEvent::Start { .. } | StreamEvent::Ping => {}
            StreamEvent::TextDelta { text } => {
                if !matches!(self.open, ROpen::Text { .. }) {
                    let (item_id, index) = self.open_item("msg",
                        json!({"type": "message", "status": "in_progress", "role": "assistant", "content": []}), &mut out);
                    out.push(self.ev("response.content_part.added", json!({"item_id": item_id, "output_index": index,
                        "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}})));
                    self.open = ROpen::Text {
                        item_id,
                        index,
                        text: String::new(),
                    };
                }
                if let ROpen::Text {
                    item_id,
                    index,
                    text: acc,
                } = &mut self.open
                {
                    acc.push_str(&text);
                    let (item_id, index) = (item_id.clone(), *index);
                    out.push(self.ev("response.output_text.delta", json!({"item_id": item_id, "output_index": index, "content_index": 0, "delta": text})));
                }
            }
            StreamEvent::ReasoningDelta { text } => {
                if !matches!(self.open, ROpen::Reasoning { .. }) {
                    let (item_id, index) = self.open_item(
                        "rs",
                        json!({"type": "reasoning", "summary": [], "content": []}),
                        &mut out,
                    );
                    self.open = ROpen::Reasoning {
                        item_id,
                        index,
                        text: String::new(),
                    };
                }
                if let ROpen::Reasoning {
                    item_id,
                    index,
                    text: acc,
                } = &mut self.open
                {
                    acc.push_str(&text);
                    let (item_id, index) = (item_id.clone(), *index);
                    out.push(self.ev("response.reasoning_text.delta", json!({"item_id": item_id, "output_index": index, "content_index": 0, "delta": text})));
                }
            }
            StreamEvent::ToolCallDelta { delta } => {
                let slot = delta.index;
                if self.seen_slots.insert(slot) {
                    let call_id = delta
                        .id
                        .clone()
                        .unwrap_or_else(|| format!("call_turnout_{slot}"));
                    let name = delta.name.clone().unwrap_or_default();
                    let (item_id, index) = self.open_item(
                        "fc",
                        json!({"type": "function_call", "status": "in_progress",
                        "call_id": call_id, "name": name, "arguments": ""}),
                        &mut out,
                    );
                    self.open = ROpen::Tool {
                        slot,
                        item_id,
                        index,
                        call_id,
                        name,
                        args: String::new(),
                    };
                }
                if let ROpen::Tool {
                    slot: open_slot,
                    item_id,
                    index,
                    args,
                    ..
                } = &mut self.open
                {
                    if *open_slot == slot {
                        if let Some(fragment) = delta.arguments_delta.filter(|a| !a.is_empty()) {
                            args.push_str(&fragment);
                            let (item_id, index) = (item_id.clone(), *index);
                            out.push(self.ev("response.function_call_arguments.delta", json!({"item_id": item_id, "output_index": index, "delta": fragment})));
                        }
                    }
                }
            }
            StreamEvent::Stop { reason } => self.stop = Some(reason),
            StreamEvent::Usage { usage } => {
                self.usage.input_tokens += usage.input_tokens;
                self.usage.output_tokens += usage.output_tokens;
            }
            StreamEvent::Error { message, code, .. } => {
                self.close_open(&mut out);
                let response = json!({"response": {"id": self.id, "object": "response", "status": "failed",
                    "model": self.model, "output": [], "error": {"code": code.unwrap_or_else(|| "server_error".into()), "message": message}}});
                out.push(self.ev("response.failed", response));
                self.finished = true;
            }
        }
        out
    }

    fn heartbeat(&mut self) -> String {
        ": ping\n\n".to_string()
    }

    fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        self.finished = true;
        self.close_open(&mut out);
        let truncated = matches!(self.stop, Some(StopReason::MaxTokens));
        let mut response = self.response_obj(if truncated { "incomplete" } else { "completed" });
        if truncated {
            response["incomplete_details"] = json!({"reason": "max_output_tokens"});
        }
        let name = if truncated {
            "response.incomplete"
        } else {
            "response.completed"
        };
        out.push(self.ev(name, json!({"response": response})));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core::ToolCallDelta;

    fn events(frames: &[String]) -> Vec<(String, Value)> {
        frames
            .iter()
            .map(|f| {
                let event = f
                    .lines()
                    .next()
                    .unwrap()
                    .trim_start_matches("event: ")
                    .to_string();
                let data = f.lines().nth(1).unwrap().trim_start_matches("data: ");
                (event, serde_json::from_str(data).unwrap())
            })
            .collect()
    }

    #[test]
    fn anthropic_request_maps_tools_results_and_choice() {
        let v = json!({
        "model": "claude-x", "max_tokens": 100, "stream": true,
        "system": [{"type": "text", "text": "be brief"}],
        "tools": [{"name": "ls", "description": "d", "input_schema": {"type": "object"}},
                  {"type": "web_search_20250305", "name": "web_search"}],
        "tool_choice": {"type": "tool", "name": "ls"},
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [{"type": "thinking", "thinking": "x", "signature": "s"},
                {"type": "tool_use", "id": "t1", "name": "ls", "input": {"p": "."}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1",
                "content": [{"type": "text", "text": "a.txt"}]}]}
        ]});
        let req = anthropic_to_chat(&v, ProtocolDialect::OpenAiChat, Some("glm-5.3")).unwrap();
        assert_eq!(
            req.model, "glm-5.3",
            "override wins over the Claude model name"
        );
        assert_eq!(req.system.as_deref(), Some("be brief"));
        assert_eq!(req.tools.len(), 1, "schema-less server tools are dropped");
        assert_eq!(
            req.tool_choice,
            Some(json!({"type": "function", "function": {"name": "ls"}}))
        );
        assert!(req.stream);
        let MessageContent::Blocks(b) = &req.messages[1].content else {
            panic!()
        };
        assert_eq!(b.len(), 1, "thinking block dropped");
        let MessageContent::Blocks(b) = &req.messages[2].content else {
            panic!()
        };
        assert!(matches!(&b[0], ContentBlock::ToolResult { content, .. } if content == "a.txt"));

        let responses = anthropic_to_chat(&v, ProtocolDialect::OpenAiResponses, None).unwrap();
        assert_eq!(
            responses.tool_choice,
            Some(json!({"type": "function", "name": "ls"}))
        );
    }

    #[test]
    fn openai_request_maps_back_to_neutral_with_anthropic_choice() {
        let v = json!({"model": "m", "stream": false,
            "messages": [{"role": "system", "content": "s"},
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function",
                    "function": {"name": "ls", "arguments": "{\"p\":\".\"}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "a.txt"}],
            "tools": [{"type": "function", "function": {"name": "ls", "parameters": {"type": "object"}}}],
            "tool_choice": "required"});
        let req = openai_to_chat(&v, ProtocolDialect::AnthropicMessages, None).unwrap();
        assert_eq!(req.system.as_deref(), Some("s"));
        assert_eq!(req.tool_choice, Some(json!({"type": "any"})));
        let MessageContent::Blocks(b) = &req.messages[1].content else {
            panic!()
        };
        assert!(
            matches!(&b[0], ContentBlock::ToolUse { input, .. } if input == &json!({"p": "."}))
        );
        assert!(matches!(&req.messages[2].content, MessageContent::Blocks(b)
            if matches!(&b[0], ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "c1")));
    }

    #[test]
    fn anthropic_stream_has_valid_block_structure() {
        let mut enc = AnthropicEncoder::new("m");
        let mut frames = enc.begin();
        for e in [
            StreamEvent::ReasoningDelta {
                text: "think".into(),
            },
            StreamEvent::TextDelta { text: "Hel".into() },
            StreamEvent::TextDelta { text: "lo".into() },
            StreamEvent::ToolCallDelta {
                delta: ToolCallDelta {
                    index: 0,
                    id: Some("c1".into()),
                    name: Some("ls".into()),
                    arguments_delta: Some(String::new()),
                },
            },
            StreamEvent::ToolCallDelta {
                delta: ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    arguments_delta: Some("{\"p\":".into()),
                },
            },
            StreamEvent::ToolCallDelta {
                delta: ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    arguments_delta: Some("\".\"}".into()),
                },
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                },
            },
        ] {
            frames.extend(enc.push(e));
        }
        frames.extend(enc.finish());
        assert!(enc.finish().is_empty(), "finish is idempotent");

        let evs = events(&frames);
        let names: Vec<&str> = evs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop", // thinking
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop", // text
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop", // tool
                "message_delta",
                "message_stop",
            ]
        );
        let indexes: Vec<u64> = evs
            .iter()
            .filter(|(n, _)| n == "content_block_start")
            .map(|(_, d)| d["index"].as_u64().unwrap())
            .collect();
        assert_eq!(indexes, [0, 1, 2]);
        let tool_json: String = evs
            .iter()
            .filter(|(_, d)| d["delta"]["type"] == "input_json_delta")
            .map(|(_, d)| d["delta"]["partial_json"].as_str().unwrap())
            .collect();
        assert_eq!(tool_json, "{\"p\":\".\"}");
        let md = &evs[evs.len() - 2].1;
        assert_eq!(
            md["delta"]["stop_reason"], "tool_use",
            "tool calls force tool_use even if upstream said stop"
        );
        assert_eq!(md["usage"]["output_tokens"], 3);
    }

    #[test]
    fn openai_stream_ends_with_finish_usage_and_done() {
        let mut enc = OpenAiEncoder::new("m");
        let mut frames = enc.begin();
        frames.extend(enc.push(StreamEvent::TextDelta { text: "hi".into() }));
        frames.extend(enc.push(StreamEvent::Stop {
            reason: StopReason::MaxTokens,
        }));
        frames.extend(enc.push(StreamEvent::Usage {
            usage: Usage {
                input_tokens: 1,
                output_tokens: 2,
            },
        }));
        frames.extend(enc.finish());
        let text = frames.concat();
        assert!(text.contains("\"finish_reason\":\"length\""));
        assert!(text.contains("\"total_tokens\":3"));
        assert!(text.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn responses_request_merges_tool_calls_into_the_assistant_turn() {
        let v = json!({"model": "gpt-x", "instructions": "base", "stream": true, "tool_choice": "auto",
            "tools": [{"type": "function", "name": "shell", "description": "run", "parameters": {"type": "object"}},
                      {"type": "web_search"}],
            "input": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "dev rules"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "list files"}]},
                {"type": "reasoning", "encrypted_content": "zzz"},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]},
                {"type": "function_call", "call_id": "c1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}"},
                {"type": "function_call", "call_id": "c2", "name": "shell", "arguments": "{\"cmd\":\"pwd\"}"},
                {"type": "function_call_output", "call_id": "c1", "output": "a.txt"},
                {"type": "function_call_output", "call_id": "c2", "output": {"content": "/w", "success": true}}]});
        let req = responses_to_chat(&v, ProtocolDialect::OpenAiChat, Some("glm")).unwrap();
        assert_eq!(req.system.as_deref(), Some("base\ndev rules"));
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tool_choice, Some(json!("auto")));
        assert_eq!(
            req.messages.len(),
            4,
            "user, assistant(text+2 calls), 2 results"
        );
        let MessageContent::Blocks(b) = &req.messages[1].content else {
            panic!()
        };
        assert_eq!(b.len(), 3);
        assert!(matches!(&req.messages[3].content, MessageContent::Blocks(b)
            if matches!(&b[0], ContentBlock::ToolResult { content, .. } if content == "/w")));
    }

    #[test]
    fn reasoning_effort_survives_every_dialect() {
        let responses = json!({"model": "glm", "input": "hi",
            "reasoning": {"effort": "xhigh", "summary": "auto"}});
        assert_eq!(
            responses_to_chat(&responses, ProtocolDialect::OpenAiChat, None)
                .unwrap()
                .reasoning_effort
                .as_deref(),
            Some("xhigh")
        );
        // Codex always sends a summary; without an effort there is nothing to map.
        let bare = json!({"model": "glm", "input": "hi", "reasoning": {"summary": "auto"}});
        assert_eq!(
            responses_to_chat(&bare, ProtocolDialect::OpenAiChat, None)
                .unwrap()
                .reasoning_effort,
            None
        );
        let chat = json!({"model": "glm", "messages": [], "reasoning_effort": "high"});
        assert_eq!(
            openai_to_chat(&chat, ProtocolDialect::OpenAiChat, None)
                .unwrap()
                .reasoning_effort
                .as_deref(),
            Some("high")
        );
        let chat_nested = json!({"model": "glm", "messages": [], "reasoning": {"effort": "max"}});
        assert_eq!(
            openai_to_chat(&chat_nested, ProtocolDialect::OpenAiChat, None)
                .unwrap()
                .reasoning_effort
                .as_deref(),
            Some("max")
        );
    }

    #[test]
    fn responses_stream_is_well_formed_for_text_and_tool_calls() {
        use relay_core::ToolCallDelta;
        let mut enc = ResponsesEncoder::new("m");
        let mut frames = enc.begin();
        for e in [
            StreamEvent::TextDelta { text: "Hi".into() },
            StreamEvent::ToolCallDelta {
                delta: ToolCallDelta {
                    index: 0,
                    id: Some("c1".into()),
                    name: Some("shell".into()),
                    arguments_delta: Some("{\"a\":".into()),
                },
            },
            StreamEvent::ToolCallDelta {
                delta: ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    arguments_delta: Some("1}".into()),
                },
            },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 3,
                    output_tokens: 4,
                },
            },
        ] {
            frames.extend(enc.push(e));
        }
        frames.extend(enc.finish());
        let evs = events(&frames);
        let names: Vec<&str> = evs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "response.created",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let seqs: Vec<u64> = evs
            .iter()
            .map(|(_, d)| d["sequence_number"].as_u64().unwrap())
            .collect();
        assert!(
            seqs.windows(2).all(|w| w[1] == w[0] + 1),
            "sequence numbers are contiguous"
        );
        let done = &evs.last().unwrap().1["response"];
        assert_eq!(done["output"][1]["arguments"], "{\"a\":1}");
        assert_eq!(done["output"][1]["call_id"], "c1");
        assert_eq!(done["usage"]["total_tokens"], 7);
    }

    #[test]
    fn non_streaming_responses_round_trip_tool_calls() {
        let resp = ChatResponse {
            id: "1".into(),
            model: "m".into(),
            content: vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "ls".into(),
                input: json!({"p": "."}),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        };
        assert_eq!(chat_to_anthropic(&resp)["stop_reason"], "tool_use");
        let o = chat_to_openai(&resp);
        assert_eq!(o["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            o["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"p\":\".\"}"
        );
        assert_eq!(o["choices"][0]["message"]["content"], Value::Null);
    }
}
