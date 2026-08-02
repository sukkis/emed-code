use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{ChatError, LlmClient, LlmResponse, Message, ToolCall, ToolDefinition};

const MISTRAL_URL: &str = "https://api.mistral.ai/v1/chat/completions";
pub(crate) const MISTRAL_MODEL: &str = "mistral-small-latest";

// A flat struct, not an enum, deliberately: Mistral's own wire format
// really is one flat JSON object per message with mostly-optional
// sibling fields (content vs. tool_calls vs. tool_call_id depending on
// role) — this is the one place in the codebase where that shape is the
// most direct, correct representation, unlike core::Message (our own
// internal model), where an enum-of-kinds is the better fit.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct MistralMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<MistralToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

fn mistral_system_message(system: &str) -> MistralMessage {
    MistralMessage {
        role: "system".to_string(),
        content: Some(system.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }
}

fn to_mistral_messages(messages: &[Message]) -> Vec<MistralMessage> {
    messages
        .iter()
        .map(|message| match message {
            Message::User { content } => MistralMessage {
                role: "user".to_string(),
                content: Some(content.clone()),
                ..Default::default()
            },
            Message::Assistant { content } => MistralMessage {
                role: "assistant".to_string(),
                content: Some(content.clone()),
                ..Default::default()
            },
            Message::ToolCalls { calls } => MistralMessage {
                role: "assistant".to_string(),
                tool_calls: Some(
                    calls
                        .iter()
                        .map(|call| MistralToolCall {
                            id: call.id.clone(),
                            kind: "function".to_string(),
                            function: MistralFunctionCall {
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                            },
                        })
                        .collect(),
                ),
                ..Default::default()
            },
            Message::ToolResult {
                tool_call_id,
                content,
                ..
            } => MistralMessage {
                role: "tool".to_string(),
                content: Some(content.clone()),
                tool_call_id: Some(tool_call_id.clone()),
                ..Default::default()
            },
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct MistralRequest {
    model: String,
    messages: Vec<MistralMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<MistralTool>,
}

#[derive(Debug, Serialize)]
struct MistralFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct MistralTool {
    #[serde(rename = "type")]
    kind: String,
    function: MistralFunctionDef,
}

fn to_mistral_tools(tools: &[ToolDefinition]) -> Vec<MistralTool> {
    tools
        .iter()
        .map(|tool| MistralTool {
            kind: "function".to_string(),
            function: MistralFunctionDef {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        })
        .collect()
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct MistralFunctionCall {
    name: String,
    arguments: String,
}

// Serialize AND Deserialize: this same shape is used both when parsing
// a real tool-calling response (Deserialize) and when re-sending that
// same tool call back as history on the next round (Serialize) — the
// wire shape is identical either way. `kind` defaults to "function" on
// deserialize (Mistral always sends it, but nothing reads it back out
// of us, so the default is just a safety net, not load-bearing).
// PartialEq is needed transitively: MistralMessage derives it, and now
// carries an Option<Vec<MistralToolCall>> field.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct MistralToolCall {
    id: String,
    #[serde(rename = "type", default = "mistral_tool_call_type")]
    kind: String,
    function: MistralFunctionCall,
}

fn mistral_tool_call_type() -> String {
    "function".to_string()
}

// Mistral sends back either plain text (content: Some(...), tool_calls
// absent or explicitly null) or a tool-calling turn (content: null,
// tool_calls populated) — never both meaningfully at once. tool_calls is
// Option, not a bare Vec with #[serde(default)]: #[serde(default) only
// covers a *missing* field, but Mistral's real API sends "tool_calls":
// null explicitly for some plain-text responses rather than omitting
// the key — Option<Vec<_>> deserializes null as None natively, covering
// both cases. See extract_mistral_reply, which treats both as "no tool
// calls" via unwrap_or_default().
#[derive(Debug, Deserialize)]
struct MistralResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<MistralToolCall>>,
}

// One candidate completion. Mistral's API can in principle return more
// than one (the "choices" array), but we only ever read choices[0].
#[derive(Debug, Deserialize)]
struct MistralChoice {
    message: MistralResponseMessage,
}

#[derive(Debug, Deserialize)]
struct MistralResponse {
    choices: Vec<MistralChoice>,
}

// Mistral's error envelope for API-level failures (e.g. 401
// Unauthorized): {"message": "...", "request_id": "..."}. Has no
// "choices" field, so it's never mistaken for a successful MistralResponse.
#[derive(Debug, Deserialize)]
struct MistralErrorResponse {
    message: String,
}

fn extract_mistral_reply(json: &str) -> Result<LlmResponse, ChatError> {
    match serde_json::from_str::<MistralResponse>(json) {
        Ok(response) => {
            let message = response
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| ChatError::MalformedResponse("no choices in response".to_string()))?
                .message;
            let tool_calls = message.tool_calls.unwrap_or_default();

            if tool_calls.is_empty() {
                message.content.map(LlmResponse::Text).ok_or_else(|| {
                    ChatError::MalformedResponse(
                        "response has neither content nor tool_calls".to_string(),
                    )
                })
            } else {
                let tool_calls = tool_calls
                    .into_iter()
                    .map(|call| ToolCall {
                        id: call.id,
                        name: call.function.name,
                        arguments: call.function.arguments,
                    })
                    .collect();
                Ok(LlmResponse::ToolCalls(tool_calls))
            }
        }
        Err(parse_error) => match serde_json::from_str::<MistralErrorResponse>(json) {
            Ok(error) => Err(ChatError::Auth(error.message)),
            Err(_) => Err(ChatError::MalformedResponse(parse_error.to_string())),
        },
    }
}

fn fetch_mistral_reply(
    api_key: &str,
    model: &str,
    messages: Vec<MistralMessage>,
    tools: Vec<MistralTool>,
) -> Result<String, ChatError> {
    let request = MistralRequest {
        model: model.to_string(),
        messages,
        stream: false,
        tools,
    };

    let mut response = ureq::post(MISTRAL_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .send_json(&request)
        .map_err(|e| ChatError::Connection(e.to_string()))?;

    response
        .body_mut()
        .read_to_string()
        .map_err(|e| ChatError::Connection(e.to_string()))
}

pub struct MistralClient {
    api_key: Zeroizing<String>,
    model: String,
}

impl MistralClient {
    pub fn new(api_key: Zeroizing<String>, model: String) -> Self {
        MistralClient { api_key, model }
    }
}

impl LlmClient for MistralClient {
    fn send(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ChatError> {
        let mut mistral_messages = vec![mistral_system_message(system)];
        mistral_messages.extend(to_mistral_messages(messages));
        let mistral_tools = to_mistral_tools(tools);
        let body =
            fetch_mistral_reply(&self.api_key, &self.model, mistral_messages, mistral_tools)?;
        extract_mistral_reply(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The system-role wire-message prepended ahead of the mapped
    // dialogue when a request is built.
    #[test]
    fn mistral_system_message_builds_the_expected_wire_shape() {
        assert_eq!(
            mistral_system_message("be helpful"),
            MistralMessage {
                role: "system".to_string(),
                content: Some("be helpful".to_string()),
                tool_calls: None,
                tool_call_id: None,
            }
        );
    }

    // Mistral's /v1/chat/completions request body: OpenAI-style shape,
    // reply nested under choices[0].message rather than a top-level
    // message field like Ollama's. No "tools" key at all when there are
    // none to advertise — proves the field is actually omitted, not
    // serialized as an empty array (which some APIs treat differently
    // from an absent field).
    #[test]
    fn mistral_request_serializes_to_the_expected_shape() {
        let request = MistralRequest {
            model: "mistral-small-latest".to_string(),
            messages: vec![MistralMessage {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                ..Default::default()
            }],
            stream: false,
            tools: vec![],
        };

        let value = serde_json::to_value(&request).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "model": "mistral-small-latest",
                "messages": [
                    { "role": "user", "content": "hello" }
                ],
                "stream": false
            })
        );
    }

    #[test]
    fn mistral_request_includes_tools_when_present() {
        let request = MistralRequest {
            model: "mistral-small-latest".to_string(),
            messages: vec![],
            stream: false,
            tools: to_mistral_tools(&[ToolDefinition {
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
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "description": "Read a file's contents.",
                        "parameters": {"type": "object"}
                    }
                }
            ])
        );
    }

    // A real (non-streaming) Mistral chat completion response.
    #[test]
    fn extract_mistral_reply_reads_assistant_content_from_a_well_formed_response() {
        let json = r#"{
            "id": "cmpl-e5cc70bb28c444948073e77776eb30ef",
            "object": "chat.completion",
            "created": 1702256327,
            "model": "mistral-small-latest",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "hi there"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 2,
                "total_tokens": 7
            }
        }"#;

        let reply = extract_mistral_reply(json).unwrap();

        assert_eq!(reply, LlmResponse::Text("hi there".to_string()));
    }

    // Real Mistral tool-calling response shape, confirmed against
    // Mistral's own function-calling docs, not guessed: content is
    // null, the reply lives in a tool_calls array instead, each entry
    // wrapping a function.name/function.arguments pair (arguments as a
    // JSON string, not a nested object).
    #[test]
    fn extract_mistral_reply_parses_tool_calls_from_a_tool_calling_response() {
        let json = r#"{
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "D681PevKs",
                                "type": "function",
                                "function": {
                                    "name": "read_file",
                                    "arguments": "{\"path\": \"notes.txt\"}"
                                }
                            }
                        ]
                    }
                }
            ]
        }"#;

        let reply = extract_mistral_reply(json).unwrap();

        assert_eq!(
            reply,
            LlmResponse::ToolCalls(vec![ToolCall {
                id: "D681PevKs".to_string(),
                name: "read_file".to_string(),
                arguments: "{\"path\": \"notes.txt\"}".to_string(),
            }])
        );
    }

    // Real-world bug, caught via the local-gated smoke test against the
    // actual API: once a request advertises tools, Mistral sends back
    // "tool_calls": null explicitly for a plain-text reply, rather than
    // omitting the key — #[serde(default)] only covers a *missing*
    // field, not one present with an explicit null value.
    #[test]
    fn extract_mistral_reply_treats_an_explicit_null_tool_calls_as_absent() {
        let json = r#"{
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "hi there",
                        "tool_calls": null
                    }
                }
            ]
        }"#;

        let reply = extract_mistral_reply(json).unwrap();

        assert_eq!(reply, LlmResponse::Text("hi there".to_string()));
    }

    #[test]
    fn extract_mistral_reply_returns_a_malformed_response_chat_error_on_bad_json() {
        let json = r#"{ "choices": [ { "message": { "role": "assistant" "#;

        let result = extract_mistral_reply(json);

        assert!(matches!(result, Err(ChatError::MalformedResponse(_))));
    }

    // Mistral's actual 401 (invalid/missing API key) error body: no
    // "choices" field at all, just {"message": "...", "request_id":
    // "..."} — confirmed against real client bug reports, since Mistral's
    // own docs don't publish an error-schema example.
    #[test]
    fn extract_mistral_reply_returns_an_auth_chat_error_for_an_unauthorized_response() {
        let json = r#"{
            "message": "Unauthorized",
            "request_id": "c61f2f15feca71626a990e3740f99245"
        }"#;

        let result = extract_mistral_reply(json);

        assert!(matches!(result, Err(ChatError::Auth(_))));
    }

    // Compile-time proof that MistralClient satisfies LlmClient, same as
    // ollama_client_implements_llm_client in the ollama module. send()
    // needs a real network round-trip (and a real API key), which stays
    // out of scope for a unit test — see tests/real_mistral_smoke_test.rs.
    #[test]
    fn mistral_client_implements_llm_client() {
        fn assert_is_llm_client<C: LlmClient>() {}
        assert_is_llm_client::<MistralClient>();
    }

    // Same reasoning as to_ollama_messages_maps_user_and_assistant_roles
    // in the ollama module: Core threading real history through send()
    // is pointless if the concrete client discards all but the last turn.
    #[test]
    fn to_mistral_messages_maps_user_and_assistant_roles() {
        let messages = vec![
            Message::User {
                content: "hello".to_string(),
            },
            Message::Assistant {
                content: "hi there".to_string(),
            },
        ];

        let mapped = to_mistral_messages(&messages);

        assert_eq!(
            mapped,
            vec![
                MistralMessage {
                    role: "user".to_string(),
                    content: Some("hello".to_string()),
                    ..Default::default()
                },
                MistralMessage {
                    role: "assistant".to_string(),
                    content: Some("hi there".to_string()),
                    ..Default::default()
                },
            ]
        );
    }

    // The agent loop represents each tool call as its own history entry
    // rather than batching a whole round's calls into one — a deliberate
    // simplification with a real, unverified assumption about whether
    // Mistral's API tolerates several separate single-call turns as well
    // as one multi-call turn. This test pins down exactly what shape our
    // own code produces for that case, so any future change to it is
    // deliberate; it can't confirm Mistral's API actually accepts the
    // shape — only a real network round-trip against multiple tool
    // calls can do that.
    #[test]
    fn to_mistral_messages_maps_multiple_sequential_tool_call_rounds() {
        let messages = vec![
            Message::User {
                content: "read two files".to_string(),
            },
            Message::ToolCalls {
                calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    arguments: r#"{"path": "a.txt"}"#.to_string(),
                }],
            },
            Message::ToolResult {
                tool_call_id: "call_1".to_string(),
                name: "read_file".to_string(),
                content: "contents of a".to_string(),
            },
            Message::ToolCalls {
                calls: vec![ToolCall {
                    id: "call_2".to_string(),
                    name: "read_file".to_string(),
                    arguments: r#"{"path": "b.txt"}"#.to_string(),
                }],
            },
            Message::ToolResult {
                tool_call_id: "call_2".to_string(),
                name: "read_file".to_string(),
                content: "contents of b".to_string(),
            },
        ];

        let mapped = to_mistral_messages(&messages);
        let value = serde_json::to_value(&mapped).unwrap();

        assert_eq!(
            value,
            serde_json::json!([
                { "role": "user", "content": "read two files" },
                {
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "read_file", "arguments": "{\"path\": \"a.txt\"}" }
                        }
                    ]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "contents of a" },
                {
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": { "name": "read_file", "arguments": "{\"path\": \"b.txt\"}" }
                        }
                    ]
                },
                { "role": "tool", "tool_call_id": "call_2", "content": "contents of b" }
            ])
        );
    }

    // Confirmed against Mistral's own function-calling docs: a tool's
    // JSON schema goes under type: "function" / function: {name,
    // description, parameters}, not flattened at the top level.
    #[test]
    fn to_mistral_tools_maps_tool_definition_to_the_expected_schema() {
        let tools = vec![ToolDefinition {
            name: "read_file".to_string(),
            description: "Read a file's contents.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        }];

        let mapped = to_mistral_tools(&tools);
        let value = serde_json::to_value(&mapped).unwrap();

        assert_eq!(
            value,
            serde_json::json!([
                {
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "description": "Read a file's contents.",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" }
                            },
                            "required": ["path"]
                        }
                    }
                }
            ])
        );
    }
}
