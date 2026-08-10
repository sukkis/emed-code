//! [`AnthropicClient`], an [`LlmClient`] implementation talking to
//! Anthropic's Messages API.

use serde::{Deserialize, Serialize};

use super::{AnthropicThinking, ChatError, LlmResponse, Message, ToolCall, ToolDefinition};

// Anthropic's tool_use/tool_result content wants a real JSON
// object/id-based correlation, not Mistral's flat sibling-field shape —
// a genuine sum type on the request side too, same reasoning as
// AnthropicContentBlock below for the response side.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlockParam {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, PartialEq, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContentBlockParam>,
}

fn to_anthropic_messages(messages: &[Message]) -> Vec<AnthropicMessage> {
    messages
        .iter()
        .map(|message| match message {
            Message::User { content } => AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContentBlockParam::Text {
                    text: content.clone(),
                }],
            },
            Message::Assistant { content } => AnthropicMessage {
                role: "assistant".to_string(),
                content: vec![AnthropicContentBlockParam::Text {
                    text: content.clone(),
                }],
            },
            Message::ToolCalls { calls } => AnthropicMessage {
                role: "assistant".to_string(),
                content: calls
                    .iter()
                    .map(|call| AnthropicContentBlockParam::ToolUse {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        // Same invariant to_ollama_messages relies on:
                        // this string only ever comes from
                        // extract_anthropic_reply's own
                        // Value::to_string() a few calls earlier — a
                        // strict round-trip that cannot fail unless our
                        // own code already corrupted it.
                        input: serde_json::from_str(&call.arguments).expect(
                            "ToolCall.arguments must be the JSON this client itself serialized",
                        ),
                    })
                    .collect(),
            },
            Message::ToolResult {
                tool_call_id,
                content,
                ..
            } => AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContentBlockParam::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: content.clone(),
                }],
            },
        })
        .collect()
}

// Flat shape — no {"type": "function", "function": {...}} wrapper like
// Mistral/Ollama both use. Confirmed against Anthropic's own tool-use
// docs, not guessed.
#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

fn to_anthropic_tools(tools: &[ToolDefinition]) -> Vec<AnthropicTool> {
    tools
        .iter()
        .map(|tool| AnthropicTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.parameters.clone(),
        })
        .collect()
}

// Kept separate from AnthropicThinking itself rather than making one
// enum serialize two different ways — a bare string for TOML
// (settings.rs's own #[serde(rename_all = "lowercase")]) versus this
// tagged-object wire shape. Fighting serde attributes to unify two
// genuinely different shapes isn't simpler than two small types.
fn anthropic_thinking_param(thinking: AnthropicThinking) -> serde_json::Value {
    match thinking {
        AnthropicThinking::Disabled => serde_json::json!({"type": "disabled"}),
        AnthropicThinking::Adaptive => serde_json::json!({"type": "adaptive"}),
    }
}

// Anthropic's /v1/messages request body. system is its own top-level
// field, not a synthesized system-role message like Mistral/Ollama both
// need — see the research notes in docs/anthropic-provider.md. No
// "tools" key at all when there are none to advertise, matching
// Mistral/Ollama's identical convention.
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    system: String,
    messages: Vec<AnthropicMessage>,
    max_tokens: u32,
    thinking: serde_json::Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
}

// Anthropic's response content is an array of typed blocks, not
// Mistral/Ollama's flat content-or-tool_calls split. Internally tagged
// on "type", mirroring the wire shape directly. `Thinking` is parsed
// (Claude Sonnet 5 runs adaptive thinking by default — see
// docs/anthropic-provider.md design question 9) but its fields are never
// read; declaring it as an empty struct variant lets serde accept
// whatever's actually present (thinking text, a signature) without
// needing to name each field, since deny_unknown_fields isn't set.
// `Other` is a serde(other) catch-all so an unrecognized future block
// type (e.g. redacted_thinking) doesn't fail the whole response parse.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    Thinking {},
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    stop_reason: String,
}

// Anthropic's error envelope: {"type": "error", "error": {"type": ...,
// "message": ...}}. Folded into ChatError::Auth for any recognized
// error envelope, matching extract_mistral_reply's existing precedent —
// Mistral's own envelope has no type field to distinguish causes any
// further, so this doesn't regress anything by not doing more here.
#[derive(Debug, Deserialize)]
struct AnthropicErrorEnvelope {
    error: AnthropicErrorDetail,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetail {
    message: String,
}

fn extract_anthropic_reply(json: &str) -> Result<LlmResponse, ChatError> {
    match serde_json::from_str::<AnthropicResponse>(json) {
        Ok(response) => {
            if response.stop_reason == "max_tokens" {
                return Err(ChatError::ResponseTruncated);
            }
            if response.stop_reason == "refusal" {
                return Err(ChatError::Refused);
            }

            let tool_calls: Vec<ToolCall> = response
                .content
                .iter()
                .filter_map(|block| match block {
                    AnthropicContentBlock::ToolUse { id, name, input } => Some(ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: input.to_string(),
                    }),
                    _ => None,
                })
                .collect();

            if !tool_calls.is_empty() {
                return Ok(LlmResponse::ToolCalls(tool_calls));
            }

            let text: String = response
                .content
                .iter()
                .filter_map(|block| match block {
                    AnthropicContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();

            if text.is_empty() {
                Err(ChatError::MalformedResponse(
                    "response has neither text nor tool_use content".to_string(),
                ))
            } else {
                Ok(LlmResponse::Text(text))
            }
        }
        Err(parse_error) => match serde_json::from_str::<AnthropicErrorEnvelope>(json) {
            Ok(error) => Err(ChatError::Auth(error.error.message)),
            Err(_) => Err(ChatError::MalformedResponse(parse_error.to_string())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AnthropicThinking, Message, ToolCall, ToolDefinition};

    // extract_anthropic_reply: Anthropic's content is an array of typed
    // blocks (text/tool_use/thinking), not Mistral's flat content-or-
    // tool_calls split — see docs/anthropic-provider.md's research notes.

    #[test]
    fn extract_anthropic_reply_reads_text_content_from_a_well_formed_response() {
        let json = r#"{
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "hi there"}
            ],
            "stop_reason": "end_turn"
        }"#;

        let reply = extract_anthropic_reply(json).unwrap();

        assert_eq!(reply, LlmResponse::Text("hi there".to_string()));
    }

    // Claude Sonnet 5 runs adaptive thinking by default (docs/
    // anthropic-provider.md design question 9) — a thinking block ahead
    // of the real reply must be parsed and silently discarded, not
    // choke the whole response parse.
    #[test]
    fn extract_anthropic_reply_skips_a_thinking_block_ahead_of_the_text_reply() {
        let json = r#"{
            "content": [
                {"type": "thinking", "thinking": "let me consider...", "signature": "abc"},
                {"type": "text", "text": "hi there"}
            ],
            "stop_reason": "end_turn"
        }"#;

        let reply = extract_anthropic_reply(json).unwrap();

        assert_eq!(reply, LlmResponse::Text("hi there".to_string()));
    }

    #[test]
    fn extract_anthropic_reply_parses_tool_calls_skipping_a_thinking_block() {
        let json = r#"{
            "content": [
                {"type": "thinking", "thinking": "let me check...", "signature": "abc"},
                {"type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {"path": "notes.txt"}}
            ],
            "stop_reason": "tool_use"
        }"#;

        let reply = extract_anthropic_reply(json).unwrap();

        assert_eq!(
            reply,
            LlmResponse::ToolCalls(vec![ToolCall {
                id: "toolu_01".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path":"notes.txt"}"#.to_string(),
            }])
        );
    }

    // Unlike Mistral (content is null whenever tool_calls is populated),
    // Anthropic's real shape can carry both a preamble text block and a
    // tool_use block as siblings — e.g. "Let me check that file." next
    // to the actual tool_use. Same precedence rule as Mistral/Ollama
    // regardless: tool calls win, accompanying text is dropped.
    #[test]
    fn extract_anthropic_reply_prefers_tool_calls_over_accompanying_text() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Let me check that file."},
                {"type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {"path": "notes.txt"}}
            ],
            "stop_reason": "tool_use"
        }"#;

        let reply = extract_anthropic_reply(json).unwrap();

        assert_eq!(
            reply,
            LlmResponse::ToolCalls(vec![ToolCall {
                id: "toolu_01".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path":"notes.txt"}"#.to_string(),
            }])
        );
    }

    // The point of this test: a truncated reply must come back as a
    // distinct, actionable ChatError, not be silently parsed as if the
    // partial content were the whole answer.
    #[test]
    fn extract_anthropic_reply_returns_a_response_truncated_chat_error_for_max_tokens() {
        let json = r#"{
            "content": [{"type": "text", "text": "partial repl"}],
            "stop_reason": "max_tokens"
        }"#;

        let result = extract_anthropic_reply(json);

        assert_eq!(result, Err(ChatError::ResponseTruncated));
    }

    #[test]
    fn extract_anthropic_reply_returns_a_refused_chat_error_for_a_refusal() {
        let json = r#"{
            "content": [],
            "stop_reason": "refusal"
        }"#;

        let result = extract_anthropic_reply(json);

        assert_eq!(result, Err(ChatError::Refused));
    }

    #[test]
    fn extract_anthropic_reply_returns_a_malformed_response_chat_error_on_bad_json() {
        let json = r#"{ "content": [ { "type": "text" "#;

        let result = extract_anthropic_reply(json);

        assert!(matches!(result, Err(ChatError::MalformedResponse(_))));
    }

    #[test]
    fn extract_anthropic_reply_returns_a_malformed_response_chat_error_when_content_has_neither_text_nor_tool_use()
     {
        let json = r#"{
            "content": [],
            "stop_reason": "end_turn"
        }"#;

        let result = extract_anthropic_reply(json);

        assert!(matches!(result, Err(ChatError::MalformedResponse(_))));
    }

    // Anthropic's real error envelope shape: {"type": "error", "error":
    // {"type": "...", "message": "..."}}. Folded into ChatError::Auth
    // for any recognized error envelope, matching Mistral's existing
    // extract_mistral_reply precedent (its own error envelope has no
    // type field at all to distinguish causes further).
    #[test]
    fn extract_anthropic_reply_returns_an_auth_chat_error_for_an_error_envelope() {
        let json = r#"{
            "type": "error",
            "error": {
                "type": "authentication_error",
                "message": "invalid x-api-key"
            }
        }"#;

        let result = extract_anthropic_reply(json);

        assert!(matches!(result, Err(ChatError::Auth(_))));
    }

    // to_anthropic_messages / to_anthropic_tools: the request-building
    // half, complementing extract_anthropic_reply's response parsing
    // above. See docs/anthropic-provider.md's research notes for the
    // real wire-shape differences from Mistral/Ollama this reflects.

    #[test]
    fn to_anthropic_messages_maps_user_and_assistant_roles() {
        let messages = vec![
            Message::User {
                content: "hello".to_string(),
            },
            Message::Assistant {
                content: "hi there".to_string(),
            },
        ];

        let mapped = to_anthropic_messages(&messages);

        assert_eq!(
            mapped,
            vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: vec![AnthropicContentBlockParam::Text {
                        text: "hello".to_string(),
                    }],
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: vec![AnthropicContentBlockParam::Text {
                        text: "hi there".to_string(),
                    }],
                },
            ]
        );
    }

    // Anthropic's tool_use.input wants a real JSON object, not Mistral's
    // pre-stringified arguments — same parse-back round-trip
    // to_ollama_messages already uses for the identical reason.
    #[test]
    fn to_anthropic_messages_maps_tool_calls_to_the_real_wire_shape() {
        let messages = vec![Message::ToolCalls {
            calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path": "a.txt"}"#.to_string(),
            }],
        }];

        let mapped = to_anthropic_messages(&messages);

        assert_eq!(
            mapped,
            vec![AnthropicMessage {
                role: "assistant".to_string(),
                content: vec![AnthropicContentBlockParam::ToolUse {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "a.txt"}),
                }],
            }]
        );
    }

    // Anthropic is id-based like Mistral (tool_use_id ~ tool_call_id),
    // not name-based like Ollama — Message::ToolResult's name field is
    // ignored here too (docs/anthropic-provider.md design question 3,
    // confirmed against mistral.rs's identical existing pattern).
    #[test]
    fn to_anthropic_messages_maps_tool_result_to_the_real_wire_shape() {
        let messages = vec![Message::ToolResult {
            tool_call_id: "call_1".to_string(),
            name: "read_file".to_string(),
            content: "contents".to_string(),
        }];

        let mapped = to_anthropic_messages(&messages);

        assert_eq!(
            mapped,
            vec![AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContentBlockParam::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "contents".to_string(),
                }],
            }]
        );
    }

    // Flat shape — no {"type": "function", "function": {...}} wrapper
    // like Mistral/Ollama both use. Confirmed against Anthropic's own
    // tool-use docs, not guessed.
    #[test]
    fn to_anthropic_tools_maps_tool_definition_to_the_expected_schema() {
        let tools = vec![ToolDefinition {
            name: "read_file".to_string(),
            description: "Read a file's contents.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }];

        let mapped = to_anthropic_tools(&tools);
        let value = serde_json::to_value(&mapped).unwrap();

        assert_eq!(
            value,
            serde_json::json!([
                {
                    "name": "read_file",
                    "description": "Read a file's contents.",
                    "input_schema": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    }
                }
            ])
        );
    }

    // anthropic_thinking_param: maps Settings' AnthropicThinking to the
    // wire shape — kept as its own small function/tests rather than
    // trying to make AnthropicThinking itself serialize two different
    // ways (a bare string for TOML, a tagged object for the request).

    #[test]
    fn anthropic_thinking_param_maps_disabled_to_the_expected_wire_shape() {
        assert_eq!(
            anthropic_thinking_param(AnthropicThinking::Disabled),
            serde_json::json!({"type": "disabled"})
        );
    }

    #[test]
    fn anthropic_thinking_param_maps_adaptive_to_the_expected_wire_shape() {
        assert_eq!(
            anthropic_thinking_param(AnthropicThinking::Adaptive),
            serde_json::json!({"type": "adaptive"})
        );
    }

    // AnthropicRequest: Anthropic's /v1/messages request body. system is
    // its own top-level field (not a synthesized system-role message
    // like Mistral/Ollama both need) — see docs/anthropic-provider.md's
    // research notes. No "tools" key at all when there are none to
    // advertise, matching Mistral/Ollama's identical convention.
    #[test]
    fn anthropic_request_serializes_to_the_expected_shape() {
        let request = AnthropicRequest {
            model: "claude-sonnet-5".to_string(),
            system: "be helpful".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContentBlockParam::Text {
                    text: "hello".to_string(),
                }],
            }],
            max_tokens: 16000,
            thinking: serde_json::json!({"type": "disabled"}),
            tools: vec![],
        };

        let value = serde_json::to_value(&request).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "model": "claude-sonnet-5",
                "system": "be helpful",
                "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "hello"}]}
                ],
                "max_tokens": 16000,
                "thinking": {"type": "disabled"}
            })
        );
    }

    #[test]
    fn anthropic_request_includes_tools_when_present() {
        let request = AnthropicRequest {
            model: "claude-sonnet-5".to_string(),
            system: String::new(),
            messages: vec![],
            max_tokens: 16000,
            thinking: serde_json::json!({"type": "disabled"}),
            tools: to_anthropic_tools(&[ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file's contents.".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
        };

        let value = serde_json::to_value(&request).unwrap();

        assert_eq!(
            value["tools"],
            serde_json::json!([
                {
                    "name": "read_file",
                    "description": "Read a file's contents.",
                    "input_schema": {"type": "object"}
                }
            ])
        );
    }
}
