//! Anthropic Messages wire format. Reused by the gateway.

use std::collections::HashMap;

use relay_core::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, MessageContent, Model, ProviderId,
    RelayError, Result, Role, StopReason, StreamEvent, ToolCallDelta, Usage,
};
use serde_json::{json, Value};

pub const DEFAULT_MAX_TOKENS: u64 = 4096;

pub fn build_messages_body(req: &ChatRequest, stream: bool) -> Value {
    let mut system = req.system.clone().unwrap_or_default();
    let mut messages: Vec<Value> = Vec::new();
    for message in &req.messages {
        if message.role == Role::System {
            if !system.is_empty() {
                system.push('\n');
            }
            system.push_str(&message.content.text());
            continue;
        }
        messages.push(convert_message(message));
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
        "stream": stream,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if let Some(v) = req.temperature {
        body["temperature"] = json!(v);
    }
    if let Some(v) = req.top_p {
        body["top_p"] = json!(v);
    }
    if !req.stop.is_empty() {
        body["stop_sequences"] = json!(req.stop);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    let mut tool = json!({"name": t.name, "input_schema": t.input_schema});
                    if let Some(d) = &t.description {
                        tool["description"] = json!(d);
                    }
                    tool
                })
                .collect(),
        );
    }
    if let Some(choice) = &req.tool_choice {
        body["tool_choice"] = choice.clone();
    }
    body
}

fn convert_message(message: &ChatMessage) -> Value {
    let role = if message.role == Role::Assistant {
        "assistant"
    } else {
        "user"
    };
    let content = match &message.content {
        MessageContent::Text(text) => json!(text),
        MessageContent::Blocks(blocks) => Value::Array(
            blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
                    ContentBlock::Image { media_type, data } => Some(json!({
                        "type": "image",
                        "source": {"type": "base64", "media_type": media_type, "data": data}
                    })),
                    ContentBlock::ToolUse { id, name, input } => {
                        Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => Some(json!({
                        "type": "tool_result", "tool_use_id": tool_use_id,
                        "content": content, "is_error": is_error
                    })),
                    // Thinking blocks need provider signatures; never replayed.
                    ContentBlock::Reasoning { .. } => None,
                })
                .collect(),
        ),
    };
    json!({"role": role, "content": content})
}

pub fn map_stop(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Unknown(other.to_string()),
    }
}

fn usage_of(v: &Value) -> Usage {
    Usage {
        input_tokens: v["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["output_tokens"].as_u64().unwrap_or(0),
    }
}

pub fn parse_message_response(v: &Value) -> Result<ChatResponse> {
    let blocks = v["content"]
        .as_array()
        .ok_or_else(|| RelayError::parse("<messages>", "response has no content array"))?;
    let content = blocks
        .iter()
        .filter_map(|b| match b["type"].as_str()? {
            "text" => Some(ContentBlock::Text {
                text: b["text"].as_str()?.to_string(),
            }),
            "thinking" => Some(ContentBlock::Reasoning {
                text: b["thinking"].as_str()?.to_string(),
            }),
            "tool_use" => Some(ContentBlock::ToolUse {
                id: b["id"].as_str()?.to_string(),
                name: b["name"].as_str()?.to_string(),
                input: b["input"].clone(),
            }),
            _ => None,
        })
        .collect();
    Ok(ChatResponse {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        model: v["model"].as_str().unwrap_or_default().to_string(),
        content,
        stop_reason: v["stop_reason"].as_str().map(map_stop).unwrap_or_default(),
        usage: usage_of(&v["usage"]),
    })
}

/// Stateful parser for Anthropic SSE data payloads.
///
/// `Usage` events are partial (input tokens at start, output tokens at the
/// end); consumers add them together.
#[derive(Default)]
pub struct EventParser {
    tool_slots: HashMap<u64, usize>,
}

impl EventParser {
    pub fn parse(&mut self, v: &Value) -> Vec<StreamEvent> {
        match v["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let m = &v["message"];
                let mut out = vec![StreamEvent::Start {
                    id: m["id"].as_str().unwrap_or_default().to_string(),
                    model: m["model"].as_str().unwrap_or_default().to_string(),
                }];
                if m["usage"].is_object() {
                    out.push(StreamEvent::Usage {
                        usage: usage_of(&m["usage"]),
                    });
                }
                out
            }
            "content_block_start" => {
                let block = &v["content_block"];
                if block["type"] == "tool_use" {
                    let slot = self.tool_slots.len();
                    self.tool_slots
                        .insert(v["index"].as_u64().unwrap_or(0), slot);
                    vec![StreamEvent::ToolCallDelta {
                        delta: ToolCallDelta {
                            index: slot,
                            id: block["id"].as_str().map(str::to_string),
                            name: block["name"].as_str().map(str::to_string),
                            arguments_delta: None,
                        },
                    }]
                } else {
                    Vec::new()
                }
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match d["type"].as_str().unwrap_or_default() {
                    "text_delta" => vec![StreamEvent::TextDelta {
                        text: d["text"].as_str().unwrap_or_default().to_string(),
                    }],
                    "thinking_delta" => vec![StreamEvent::ReasoningDelta {
                        text: d["thinking"].as_str().unwrap_or_default().to_string(),
                    }],
                    "input_json_delta" => {
                        let slot = self
                            .tool_slots
                            .get(&v["index"].as_u64().unwrap_or(0))
                            .copied()
                            .unwrap_or(0);
                        vec![StreamEvent::ToolCallDelta {
                            delta: ToolCallDelta {
                                index: slot,
                                id: None,
                                name: None,
                                arguments_delta: d["partial_json"].as_str().map(str::to_string),
                            },
                        }]
                    }
                    _ => Vec::new(),
                }
            }
            "message_delta" => {
                let mut out = Vec::new();
                if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                    out.push(StreamEvent::Stop {
                        reason: map_stop(reason),
                    });
                }
                if v["usage"].is_object() {
                    out.push(StreamEvent::Usage {
                        usage: usage_of(&v["usage"]),
                    });
                }
                out
            }
            "ping" => vec![StreamEvent::Ping],
            "error" => vec![StreamEvent::Error {
                message: relay_core::redact_secrets(
                    v["error"]["message"]
                        .as_str()
                        .unwrap_or("provider stream error"),
                ),
                code: v["error"]["type"].as_str().map(str::to_string),
                retryable: v["error"]["type"] == "overloaded_error",
            }],
            _ => Vec::new(),
        }
    }
}

pub fn parse_models(provider: &ProviderId, v: &Value) -> Vec<Model> {
    v["data"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item["id"].as_str()?;
                    let mut model = Model::new(provider.clone(), id);
                    if let Some(name) = item["display_name"].as_str() {
                        model.display_name = name.to_string();
                    }
                    model.capabilities = relay_core::ModelCapabilities {
                        vision: true,
                        reasoning: true,
                        ..relay_core::ModelCapabilities::coding_defaults()
                    };
                    model.endpoint = relay_core::EndpointKind::Messages;
                    Some(model)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_folds_system_messages_and_defaults_max_tokens() {
        let req = ChatRequest {
            model: "m".into(),
            system: Some("a".into()),
            messages: vec![ChatMessage::system("b"), ChatMessage::user("hi")],
            ..Default::default()
        };
        let body = build_messages_body(&req, false);
        assert_eq!(body["system"], "a\nb");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn parses_stream_with_tool_use() {
        let mut p = EventParser::default();
        let mut all = Vec::new();
        for ev in [
            json!({"type": "message_start", "message": {"id": "m1", "model": "c", "usage": {"input_tokens": 5}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "ok"}}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "t1", "name": "ls"}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"a\":1}"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 9}}),
        ] {
            all.extend(p.parse(&ev));
        }
        assert!(matches!(&all[0], StreamEvent::Start { id, .. } if id == "m1"));
        assert!(all
            .iter()
            .any(|e| matches!(e, StreamEvent::TextDelta { text } if text == "ok")));
        assert!(all.iter().any(|e| matches!(e,
            StreamEvent::ToolCallDelta { delta } if delta.index == 0 && delta.arguments_delta.as_deref() == Some("{\"a\":1}"))));
        assert!(all.iter().any(|e| matches!(
            e,
            StreamEvent::Stop {
                reason: StopReason::ToolUse
            }
        )));
    }

    #[test]
    fn parses_non_streaming_response() {
        let v = json!({"id": "1", "model": "c", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "hi"}], "usage": {"input_tokens": 1, "output_tokens": 2}});
        let r = parse_message_response(&v).unwrap();
        assert_eq!(r.usage.total(), 3);
        assert_eq!(r.stop_reason, StopReason::EndTurn);
    }
}
