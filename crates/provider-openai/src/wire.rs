//! OpenAI Chat Completions wire format: request building, response parsing and
//! SSE chunk parsing. Reused by the gateway for protocol conversion.

use relay_core::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, EndpointKind, MessageContent, Model,
    ModelCapabilities, ProviderId, RelayError, Result, Role, StopReason, StreamEvent,
    ToolCallDelta, Usage,
};
use serde_json::{json, Value};

pub fn build_chat_body(req: &ChatRequest, stream: bool) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = &req.system {
        messages.push(json!({"role": "system", "content": system}));
    }
    for message in &req.messages {
        convert_message(message, &mut messages);
    }
    let mut body = json!({"model": req.model, "messages": messages, "stream": stream});
    if let Some(v) = req.max_tokens {
        body["max_tokens"] = json!(v);
    }
    if let Some(v) = req.temperature {
        body["temperature"] = json!(v);
    }
    if let Some(v) = req.top_p {
        body["top_p"] = json!(v);
    }
    if !req.stop.is_empty() {
        body["stop"] = json!(req.stop);
    }
    if let Some(effort) = &req.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({"type": "function", "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }})
                })
                .collect(),
        );
    }
    if let Some(choice) = &req.tool_choice {
        body["tool_choice"] = choice.clone();
    }
    if stream {
        body["stream_options"] = json!({"include_usage": true});
    }
    body
}

pub fn build_responses_body(req: &ChatRequest, stream: bool) -> Value {
    let mut instructions = req.system.clone().unwrap_or_default();
    let mut input = Vec::new();
    for message in &req.messages {
        if message.role == Role::System {
            if !instructions.is_empty() {
                instructions.push('\n');
            }
            instructions.push_str(&message.content.text());
            continue;
        }
        let role = if message.role == Role::Assistant {
            "assistant"
        } else {
            "user"
        };
        let blocks = message.content.blocks();
        let mut parts = Vec::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text } => parts.push(json!({
                    "type": if role == "assistant" { "output_text" } else { "input_text" },
                    "text": text
                })),
                ContentBlock::Image { media_type, data } if role == "user" => parts.push(json!({
                    "type": "input_image", "image_url": format!("data:{media_type};base64,{data}")
                })),
                ContentBlock::ToolUse {
                    id,
                    name,
                    input: arguments,
                } => input.push(json!({
                    "type": "function_call", "call_id": id, "name": name,
                    "arguments": arguments.to_string()
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => input.push(json!({
                    "type": "function_call_output", "call_id": tool_use_id, "output": content
                })),
                ContentBlock::Reasoning { .. } | ContentBlock::Image { .. } => {}
            }
        }
        if !parts.is_empty() {
            input.push(json!({"type": "message", "role": role, "content": parts}));
        }
    }
    let mut body = json!({
        "model": req.model,
        "input": input,
        "stream": stream,
        "store": false
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(value) = req.max_tokens {
        body["max_output_tokens"] = json!(value);
    }
    if let Some(value) = req.temperature {
        body["temperature"] = json!(value);
    }
    if let Some(value) = req.top_p {
        body["top_p"] = json!(value);
    }
    if let Some(effort) = &req.reasoning_effort {
        body["reasoning"] = json!({"effort": effort});
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function", "name": tool.name,
                        "description": tool.description, "parameters": tool.input_schema
                    })
                })
                .collect(),
        );
    }
    if let Some(choice) = &req.tool_choice {
        body["tool_choice"] = choice.clone();
    }
    body
}

fn convert_message(message: &ChatMessage, out: &mut Vec<Value>) {
    let role = match message.role {
        Role::System => "system",
        Role::User | Role::Tool => "user",
        Role::Assistant => "assistant",
    };
    let blocks = match &message.content {
        MessageContent::Text(text) => {
            out.push(json!({"role": role, "content": text}));
            return;
        }
        MessageContent::Blocks(blocks) => blocks,
    };

    let mut parts: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut text_only = true;
    for block in blocks {
        match block {
            ContentBlock::Text { text } => parts.push(json!({"type": "text", "text": text})),
            ContentBlock::Image { media_type, data } => {
                text_only = false;
                parts.push(json!({"type": "image_url", "image_url": {
                    "url": format!("data:{media_type};base64,{data}")}}));
            }
            ContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                "id": id, "type": "function",
                "function": {"name": name, "arguments": input.to_string()}
            })),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => out.push(json!({
                "role": "tool", "tool_call_id": tool_use_id, "content": content
            })),
            ContentBlock::Reasoning { .. } => {}
        }
    }
    if parts.is_empty() && tool_calls.is_empty() {
        return;
    }
    let content = if text_only {
        let joined: String = parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        if joined.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            Value::String(joined)
        }
    } else {
        Value::Array(parts)
    };
    let mut msg = json!({"role": role, "content": content});
    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
    }
    out.push(msg);
}

pub fn map_finish(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::ContentFilter,
        other => StopReason::Unknown(other.to_string()),
    }
}

fn parse_usage(v: &Value) -> Usage {
    Usage {
        input_tokens: v["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["completion_tokens"].as_u64().unwrap_or(0),
    }
}

pub fn parse_chat_response(v: &Value) -> Result<ChatResponse> {
    let choice = v["choices"]
        .get(0)
        .ok_or_else(|| RelayError::parse("<chat>", "response has no choices"))?;
    let message = &choice["message"];
    let mut content = Vec::new();
    if let Some(reasoning) = message["reasoning_content"]
        .as_str()
        .or_else(|| message["reasoning"].as_str())
    {
        if !reasoning.is_empty() {
            content.push(ContentBlock::Reasoning {
                text: reasoning.to_string(),
            });
        }
    }
    if let Some(text) = message["content"].as_str() {
        if !text.is_empty() {
            content.push(ContentBlock::Text {
                text: text.to_string(),
            });
        }
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        for call in calls {
            let args = call["function"]["arguments"].as_str().unwrap_or("{}");
            content.push(ContentBlock::ToolUse {
                id: call["id"].as_str().unwrap_or_default().to_string(),
                name: call["function"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                input: serde_json::from_str(args).unwrap_or(Value::Object(Default::default())),
            });
        }
    }
    Ok(ChatResponse {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        model: v["model"].as_str().unwrap_or_default().to_string(),
        content,
        stop_reason: choice["finish_reason"]
            .as_str()
            .map(map_finish)
            .unwrap_or_default(),
        usage: parse_usage(&v["usage"]),
    })
}

pub fn parse_responses_response(v: &Value) -> Result<ChatResponse> {
    let mut content = Vec::new();
    for item in v["output"].as_array().into_iter().flatten() {
        match item["type"].as_str().unwrap_or_default() {
            "message" => {
                for part in item["content"].as_array().into_iter().flatten() {
                    if let Some(text) = part["text"].as_str() {
                        if !text.is_empty() {
                            content.push(ContentBlock::Text { text: text.into() });
                        }
                    }
                }
            }
            "function_call" => content.push(ContentBlock::ToolUse {
                id: item["call_id"].as_str().unwrap_or_default().to_string(),
                name: item["name"].as_str().unwrap_or_default().to_string(),
                input: item["arguments"]
                    .as_str()
                    .and_then(|value| serde_json::from_str(value).ok())
                    .unwrap_or_else(|| json!({})),
            }),
            _ => {}
        }
    }
    let used_tool = content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }));
    Ok(ChatResponse {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        model: v["model"].as_str().unwrap_or_default().to_string(),
        content,
        stop_reason: if used_tool {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        },
        usage: Usage {
            input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        },
    })
}

#[derive(Default)]
pub struct ResponsesChunkParser {
    started: bool,
    model: String,
    id: String,
    tools: std::collections::BTreeMap<usize, (Option<String>, Option<String>)>,
    used_tool: bool,
}

impl ResponsesChunkParser {
    pub fn parse(&mut self, value: &Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let kind = value["type"].as_str().unwrap_or_default();
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            events.push(StreamEvent::Error {
                message: error["message"]
                    .as_str()
                    .unwrap_or("provider stream error")
                    .to_string(),
                code: error["code"].as_str().map(str::to_string),
                retryable: false,
            });
            return events;
        }
        if kind == "response.created" {
            self.id = value["response"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            self.model = value["response"]["model"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        if !self.started && !self.id.is_empty() {
            self.started = true;
            events.push(StreamEvent::Start {
                id: self.id.clone(),
                model: self.model.clone(),
            });
        }
        match kind {
            "response.output_text.delta" => {
                if let Some(text) = value["delta"].as_str().filter(|text| !text.is_empty()) {
                    events.push(StreamEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(text) = value["delta"].as_str().filter(|text| !text.is_empty()) {
                    events.push(StreamEvent::ReasoningDelta {
                        text: text.to_string(),
                    });
                }
            }
            "response.output_item.added" if value["item"]["type"] == "function_call" => {
                self.used_tool = true;
                let index = value["output_index"].as_u64().unwrap_or(0) as usize;
                let id = value["item"]["call_id"].as_str().map(str::to_string);
                let name = value["item"]["name"].as_str().map(str::to_string);
                self.tools.insert(index, (id.clone(), name.clone()));
                events.push(StreamEvent::ToolCallDelta {
                    delta: ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta: None,
                    },
                });
            }
            "response.function_call_arguments.delta" => {
                let index = value["output_index"].as_u64().unwrap_or(0) as usize;
                let (id, name) = self.tools.get(&index).cloned().unwrap_or_default();
                events.push(StreamEvent::ToolCallDelta {
                    delta: ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta: value["delta"].as_str().map(str::to_string),
                    },
                });
            }
            "response.completed" => {
                let usage = &value["response"]["usage"];
                events.push(StreamEvent::Usage {
                    usage: Usage {
                        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                    },
                });
                events.push(StreamEvent::Stop {
                    reason: if self.used_tool {
                        StopReason::ToolUse
                    } else {
                        StopReason::EndTurn
                    },
                });
            }
            "response.failed" | "response.incomplete" => {
                let response = &value["response"];
                let error = &response["error"];
                events.push(StreamEvent::Error {
                    message: error["message"]
                        .as_str()
                        .unwrap_or(if kind == "response.incomplete" {
                            "OpenAI response was incomplete"
                        } else {
                            "OpenAI response failed"
                        })
                        .to_string(),
                    code: error["code"].as_str().map(str::to_string),
                    retryable: false,
                });
            }
            _ => {}
        }
        events
    }
}

/// Stateful parser turning OpenAI SSE chunks into neutral stream events.
#[derive(Default)]
pub struct ChunkParser {
    started: bool,
}

impl ChunkParser {
    pub fn parse(&mut self, v: &Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            events.push(StreamEvent::Error {
                message: err["message"]
                    .as_str()
                    .map(relay_core::redact_secrets)
                    .unwrap_or_else(|| "provider stream error".to_string()),
                code: err["code"].as_str().map(str::to_string),
                retryable: false,
            });
            return events;
        }
        if !self.started {
            self.started = true;
            events.push(StreamEvent::Start {
                id: v["id"].as_str().unwrap_or_default().to_string(),
                model: v["model"].as_str().unwrap_or_default().to_string(),
            });
        }
        if let Some(choice) = v["choices"].get(0) {
            let delta = &choice["delta"];
            if let Some(text) = delta["reasoning_content"]
                .as_str()
                .or_else(|| delta["reasoning"].as_str())
            {
                if !text.is_empty() {
                    events.push(StreamEvent::ReasoningDelta {
                        text: text.to_string(),
                    });
                }
            }
            if let Some(text) = delta["content"].as_str() {
                if !text.is_empty() {
                    events.push(StreamEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for call in calls {
                    events.push(StreamEvent::ToolCallDelta {
                        delta: ToolCallDelta {
                            index: call["index"].as_u64().unwrap_or(0) as usize,
                            id: call["id"].as_str().map(str::to_string),
                            name: call["function"]["name"].as_str().map(str::to_string),
                            arguments_delta: call["function"]["arguments"]
                                .as_str()
                                .map(str::to_string),
                        },
                    });
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                events.push(StreamEvent::Stop {
                    reason: map_finish(reason),
                });
            }
        }
        if v["usage"].is_object() {
            events.push(StreamEvent::Usage {
                usage: parse_usage(&v["usage"]),
            });
        }
        events
    }
}

/// Parse a `/models` response (`{"data": [...]}` or a bare array).
pub fn parse_models(provider: &ProviderId, v: &Value) -> Vec<Model> {
    let items = v["data"]
        .as_array()
        .or_else(|| v["models"].as_array())
        .or_else(|| v.as_array());
    let Some(items) = items else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            if item["visibility"]
                .as_str()
                .is_some_and(|value| value != "list")
            {
                return None;
            }
            let id = item["id"]
                .as_str()
                .or_else(|| item["slug"].as_str())
                .or_else(|| item.as_str())?;
            let mut model = Model::new(provider.clone(), id);
            if let Some(name) = item["name"]
                .as_str()
                .or_else(|| item["display_name"].as_str())
            {
                model.display_name = name.to_string();
            }
            model.context_window = item["context_length"]
                .as_u64()
                .or_else(|| item["context_window"].as_u64());
            model.capabilities = infer_capabilities(id);
            if model.capabilities.embeddings && !model.capabilities.chat {
                model.endpoint = EndpointKind::Completions;
            }
            Some(model)
        })
        .collect()
}

/// Capability heuristics from the model id; the catalog can override these.
pub fn infer_capabilities(model: &str) -> ModelCapabilities {
    let id = model.to_lowercase();
    if id.contains("embed") {
        return ModelCapabilities {
            embeddings: true,
            ..Default::default()
        };
    }
    let mut caps = ModelCapabilities::coding_defaults();
    caps.reasoning = ["o1", "o3", "o4", "reason", "think", "r1"]
        .iter()
        .any(|m| id.contains(m));
    caps.vision = ["vision", "4o", "gpt-5", "-vl", "gemini"]
        .iter()
        .any(|m| id.contains(m));
    caps
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core::ToolDef;

    #[test]
    fn reasoning_effort_reaches_the_upstream_in_both_dialects() {
        let req = ChatRequest {
            model: "glm".into(),
            messages: vec![ChatMessage::user("hi")],
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        assert_eq!(build_chat_body(&req, true)["reasoning_effort"], "high");
        assert_eq!(
            build_responses_body(&req, true)["reasoning"]["effort"],
            "high"
        );
        // A model without the knob must not receive a parameter it rejects.
        let plain = ChatRequest {
            reasoning_effort: None,
            ..req
        };
        assert!(build_chat_body(&plain, true).get("reasoning_effort").is_none());
        assert!(build_responses_body(&plain, true)
            .get("reasoning")
            .is_none());
    }

    #[test]
    fn builds_body_with_tools_and_tool_results() {
        let req = ChatRequest {
            model: "m".into(),
            system: Some("be brief".into()),
            messages: vec![
                ChatMessage::user("hi"),
                ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "ls".into(),
                        input: json!({"p": "."}),
                    }]),
                },
                ChatMessage {
                    role: Role::User,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: "t1".into(),
                        content: "a.txt".into(),
                        is_error: false,
                    }]),
                },
            ],
            tools: vec![ToolDef {
                name: "ls".into(),
                description: None,
                input_schema: json!({"type": "object"}),
            }],
            ..Default::default()
        };
        let body = build_chat_body(&req, true);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "ls");
        assert_eq!(msgs[2]["content"], Value::Null);
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "t1");
        assert_eq!(body["tools"][0]["function"]["name"], "ls");
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn parses_response_with_tool_call() {
        let v = json!({"id": "x", "model": "m", "choices": [{"finish_reason": "tool_calls",
            "message": {"content": null, "tool_calls": [{"id": "c1", "type": "function",
            "function": {"name": "ls", "arguments": "{\"p\":\".\"}"}}]}}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4}});
        let r = parse_chat_response(&v).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.usage.total(), 7);
        assert!(matches!(&r.content[0], ContentBlock::ToolUse { name, .. } if name == "ls"));
    }

    #[test]
    fn builds_responses_body_with_tools_and_no_storage() {
        let req = ChatRequest {
            model: "gpt-test".into(),
            system: Some("be brief".into()),
            messages: vec![
                ChatMessage::user("hi"),
                ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                        id: "call_1".into(),
                        name: "shell".into(),
                        input: json!({"command": "pwd"}),
                    }]),
                },
                ChatMessage {
                    role: Role::User,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: "call_1".into(),
                        content: "/tmp".into(),
                        is_error: false,
                    }]),
                },
            ],
            tools: vec![ToolDef {
                name: "shell".into(),
                description: Some("Run a command".into()),
                input_schema: json!({"type": "object"}),
            }],
            ..Default::default()
        };
        let body = build_responses_body(&req, true);
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["name"], "shell");
        assert!(body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call"));
        assert!(body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call_output"));
    }

    #[test]
    fn parses_responses_output_and_stream_events() {
        let response = json!({
            "id": "resp_1", "model": "gpt-test",
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "hello"}]},
                {"type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{\"command\":\"pwd\"}"}
            ],
            "usage": {"input_tokens": 3, "output_tokens": 4}
        });
        let parsed = parse_responses_response(&response).unwrap();
        assert_eq!(parsed.id, "resp_1");
        assert_eq!(parsed.stop_reason, StopReason::ToolUse);
        assert_eq!(parsed.usage.total(), 7);

        let mut parser = ResponsesChunkParser::default();
        let created = parser.parse(&json!({
            "type": "response.created",
            "response": {"id": "resp_1", "model": "gpt-test"}
        }));
        assert!(matches!(created[0], StreamEvent::Start { .. }));
        let text = parser.parse(&json!({
            "type": "response.output_text.delta", "delta": "hello"
        }));
        assert!(matches!(&text[0], StreamEvent::TextDelta { text } if text == "hello"));
        let completed = parser.parse(&json!({
            "type": "response.completed",
            "response": {"usage": {"input_tokens": 3, "output_tokens": 4}}
        }));
        assert!(matches!(completed[0], StreamEvent::Usage { .. }));
        assert!(matches!(completed[1], StreamEvent::Stop { .. }));
    }

    #[test]
    fn parses_chatgpt_plan_model_catalog() {
        let provider = ProviderId::new("openai");
        let models = parse_models(
            &provider,
            &json!({"models": [
                {"slug": "gpt-visible", "display_name": "GPT Visible", "visibility": "list"},
                {"slug": "gpt-hidden", "display_name": "GPT Hidden", "visibility": "hide"}
            ]}),
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-visible");
        assert_eq!(models[0].display_name, "GPT Visible");
    }

    #[test]
    fn chunk_parser_emits_start_once_then_deltas() {
        let mut p = ChunkParser::default();
        let a =
            p.parse(&json!({"id": "1", "model": "m", "choices": [{"delta": {"content": "he"}}]}));
        assert!(matches!(a[0], StreamEvent::Start { .. }));
        assert!(matches!(&a[1], StreamEvent::TextDelta { text } if text == "he"));
        let b = p.parse(
            &json!({"choices": [{"delta": {"reasoning_content": "hm"}, "finish_reason": "stop"}]}),
        );
        assert!(matches!(b[0], StreamEvent::ReasoningDelta { .. }));
        assert!(matches!(
            b[1],
            StreamEvent::Stop {
                reason: StopReason::EndTurn
            }
        ));
    }

    #[test]
    fn parses_models_in_both_shapes() {
        let p = ProviderId::new("x");
        assert_eq!(
            parse_models(
                &p,
                &json!({"data": [{"id": "a"}, {"id": "text-embedding-3"}]})
            )
            .len(),
            2
        );
        assert_eq!(parse_models(&p, &json!([{"id": "a"}])).len(), 1);
        assert!(
            parse_models(&p, &json!({"data": [{"id": "text-embedding-3"}]}))[0]
                .capabilities
                .embeddings
        );
    }
}
