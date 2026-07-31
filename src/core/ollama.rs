use serde::{Deserialize, Serialize};

use super::{ChatError, LlmClient, LlmResponse, Message, ToolCall, ToolDefinition};

const OLLAMA_URL: &str = "http://localhost:11434/api/chat";
pub(crate) const OLLAMA_MODEL: &str = "mistral-nemo";

// A flat struct with mostly-optional sibling fields, same reasoning as
// MistralMessage: role/content are always present, tool_calls/tool_name
// only apply to specific roles (assistant tool-calls, tool results).
// `#[serde(default)]` on the Option fields matters, not just their
// Option-ness: it covers a completely *missing* key (Ollama omits
// tool_calls/tool_name entirely on ordinary replies) — Option alone only
// covers an explicit `null`. Same distinction MistralResponseMessage's
// tool_calls field already documents.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
}

// Shared between requests (re-sending our own past tool calls) and
// responses (a real tool-calling reply) — Ollama's wire format is
// symmetric here, unlike Mistral's: no id, no "type" tag, and arguments
// as a real JSON object rather than a pre-stringified string.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct OllamaToolCall {
    function: OllamaFunctionCall,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct OllamaFunctionCall {
    name: String,
    arguments: serde_json::Value,
}

fn to_ollama_messages(messages: &[Message]) -> Vec<OllamaMessage> {
    messages
        .iter()
        .map(|message| match message {
            Message::User { content } => OllamaMessage {
                role: "user".to_string(),
                content: content.clone(),
                ..Default::default()
            },
            Message::Assistant { content } => OllamaMessage {
                role: "assistant".to_string(),
                content: content.clone(),
                ..Default::default()
            },
            Message::ToolCalls { calls } => OllamaMessage {
                role: "assistant".to_string(),
                content: String::new(),
                tool_calls: Some(
                    calls
                        .iter()
                        .map(|call| OllamaToolCall {
                            function: OllamaFunctionCall {
                                name: call.name.clone(),
                                // Invariant, not a fallible input: in an
                                // Ollama session, this string only ever
                                // comes from extract_ollama_reply's own
                                // Value::to_string() a few calls earlier —
                                // re-parsing it here is a strict round-trip
                                // that cannot fail unless our own code
                                // already corrupted it, which .expect()
                                // surfaces loudly rather than masking.
                                arguments: serde_json::from_str(&call.arguments).expect(
                                    "ToolCall.arguments must be the JSON this client itself serialized",
                                ),
                            },
                        })
                        .collect(),
                ),
                ..Default::default()
            },
            Message::ToolResult { name, content, .. } => OllamaMessage {
                role: "tool".to_string(),
                content: content.clone(),
                tool_name: Some(name.clone()),
                ..Default::default()
            },
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct OllamaFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct OllamaTool {
    #[serde(rename = "type")]
    kind: String,
    function: OllamaFunctionDef,
}

fn to_ollama_tools(tools: &[ToolDefinition]) -> Vec<OllamaTool> {
    tools
        .iter()
        .map(|tool| OllamaTool {
            kind: "function".to_string(),
            function: OllamaFunctionDef {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OllamaTool>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
}

fn extract_reply(json: &str) -> Result<String, ChatError> {
    let response: OllamaResponse =
        serde_json::from_str(json).map_err(|e| ChatError::MalformedResponse(e.to_string()))?;
    Ok(response.message.content)
}

// The tool-calling-aware counterpart to extract_reply above — not yet
// reachable from send() (see docs/ollama-tool-calling.md's Step 3),
// proven correct here in isolation first. Mirrors Mistral's
// extract_mistral_reply: empty/absent tool_calls means a plain-text
// reply, otherwise each call gets a synthesized id (Ollama's own wire
// format has none) and its arguments re-serialized from an object back
// to the String shape ToolCall.arguments expects.
fn extract_ollama_reply(json: &str) -> Result<LlmResponse, ChatError> {
    let response: OllamaResponse =
        serde_json::from_str(json).map_err(|e| ChatError::MalformedResponse(e.to_string()))?;
    let tool_calls = response.message.tool_calls.unwrap_or_default();

    if tool_calls.is_empty() {
        Ok(LlmResponse::Text(response.message.content))
    } else {
        let tool_calls = tool_calls
            .into_iter()
            .enumerate()
            .map(|(index, call)| ToolCall {
                id: format!("ollama_call_{index}"),
                name: call.function.name,
                arguments: call.function.arguments.to_string(),
            })
            .collect();
        Ok(LlmResponse::ToolCalls(tool_calls))
    }
}

fn fetch_ollama_reply(model: &str, messages: Vec<OllamaMessage>) -> Result<String, ChatError> {
    let request = OllamaRequest {
        model: model.to_string(),
        messages,
        stream: false,
        tools: vec![],
    };

    let mut response = ureq::post(OLLAMA_URL)
        .send_json(&request)
        .map_err(|e| ChatError::Connection(e.to_string()))?;

    response
        .body_mut()
        .read_to_string()
        .map_err(|e| ChatError::Connection(e.to_string()))
}

pub struct OllamaClient {
    model: String,
}

impl OllamaClient {
    pub fn new(model: String) -> Self {
        OllamaClient { model }
    }
}

impl LlmClient for OllamaClient {
    fn send(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ChatError> {
        let ollama_messages = to_ollama_messages(messages);
        let body = fetch_ollama_reply(&self.model, ollama_messages)?;
        let text = extract_reply(&body)?;
        Ok(LlmResponse::Text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ollama's /api/chat request body: model name, chat history, and
    // stream: false so the response arrives as one JSON object rather
    // than a series of chunked ones (streaming is out of scope for now).
    // No "tools" key at all when there are none to advertise, matching
    // Mistral's identical convention.
    #[test]
    fn ollama_request_serializes_to_the_expected_shape() {
        let request = OllamaRequest {
            model: "llama3".to_string(),
            messages: vec![OllamaMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
                ..Default::default()
            }],
            stream: false,
            tools: vec![],
        };

        let value = serde_json::to_value(&request).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "model": "llama3",
                "messages": [
                    { "role": "user", "content": "hello" }
                ],
                "stream": false
            })
        );
    }

    #[test]
    fn ollama_request_includes_tools_when_present() {
        let request = OllamaRequest {
            model: "llama3".to_string(),
            messages: vec![],
            stream: false,
            tools: to_ollama_tools(&[ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file's contents.".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
        };

        let value = serde_json::to_value(&request).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "model": "llama3",
                "messages": [],
                "stream": false,
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "description": "Read a file's contents.",
                            "parameters": {"type": "object"}
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn to_ollama_tools_maps_tool_definition_to_the_expected_schema() {
        let tools = vec![ToolDefinition {
            name: "list_files".to_string(),
            description: "List files in a directory.".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];

        let mapped = to_ollama_tools(&tools);
        let value = serde_json::to_value(&mapped).unwrap();

        assert_eq!(
            value,
            serde_json::json!([
                {
                    "type": "function",
                    "function": {
                        "name": "list_files",
                        "description": "List files in a directory.",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }
            ])
        );
    }

    // A real (non-streaming) Ollama /api/chat response. We only care
    // about pulling the assistant's reply text back out of it.
    #[test]
    fn extract_reply_reads_assistant_content_from_a_well_formed_response() {
        let json = r#"{
            "model": "llama3",
            "created_at": "2023-08-04T08:52:19.385406455-07:00",
            "message": {
                "role": "assistant",
                "content": "hi there"
            },
            "done": true
        }"#;

        let reply = extract_reply(json).unwrap();

        assert_eq!(reply, "hi there");
    }

    #[test]
    fn extract_reply_returns_a_malformed_response_chat_error_on_bad_json() {
        let json = r#"{ "message": { "role": "assistant" "#;

        let result = extract_reply(json);

        assert!(matches!(result, Err(ChatError::MalformedResponse(_))));
    }

    // extract_ollama_reply is the tool-calling-aware counterpart to
    // extract_reply above — not yet reachable from send() (that's
    // Step 3 in docs/ollama-tool-calling.md), proven correct here in
    // isolation first. Plain-text replies map the same way either
    // function is used.
    #[test]
    fn extract_ollama_reply_reads_assistant_content_from_a_well_formed_response() {
        let json = r#"{
            "model": "llama3",
            "message": {
                "role": "assistant",
                "content": "hi there"
            },
            "done": true
        }"#;

        let reply = extract_ollama_reply(json).unwrap();

        assert_eq!(reply, LlmResponse::Text("hi there".to_string()));
    }

    // Ollama's own documented tool-calling response shape: arguments
    // arrive as a parsed JSON object per function, and there's no id at
    // all — extract_ollama_reply synthesizes one per call via a simple
    // counter (ollama_call_0, ollama_call_1, ...) and re-serializes
    // arguments back to a JSON string to fit ToolCall.arguments: String.
    // Two calls to the same tool, deliberately: proves the counter (not
    // the tool name) is what keeps the synthesized ids distinct.
    #[test]
    fn extract_ollama_reply_parses_tool_calls_synthesizing_ids_and_stringifying_arguments() {
        let json = r#"{
            "model": "llama3",
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {
                        "function": {
                            "name": "read_file",
                            "arguments": {"path": "a.txt"}
                        }
                    },
                    {
                        "function": {
                            "name": "read_file",
                            "arguments": {"path": "b.txt"}
                        }
                    }
                ]
            },
            "done": true
        }"#;

        let reply = extract_ollama_reply(json).unwrap();

        assert_eq!(
            reply,
            LlmResponse::ToolCalls(vec![
                ToolCall {
                    id: "ollama_call_0".to_string(),
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"a.txt"}"#.to_string(),
                },
                ToolCall {
                    id: "ollama_call_1".to_string(),
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"b.txt"}"#.to_string(),
                },
            ])
        );
    }

    #[test]
    fn extract_ollama_reply_returns_a_malformed_response_chat_error_on_bad_json() {
        let json = r#"{ "message": { "role": "assistant" "#;

        let result = extract_ollama_reply(json);

        assert!(matches!(result, Err(ChatError::MalformedResponse(_))));
    }

    // Compile-time proof that OllamaClient actually satisfies the
    // LlmClient trait Core depends on. Not a behavioral test — send()
    // needs a real network round-trip, which stays out of scope for a
    // unit test (see tests/real_ollama_smoke_test.rs) — just that the
    // trait binding holds.
    #[test]
    fn ollama_client_implements_llm_client() {
        fn assert_is_llm_client<C: LlmClient>() {}
        assert_is_llm_client::<OllamaClient>();
    }

    // The whole point of Core threading real conversation history
    // through send() is lost if the concrete client then only looks at
    // the last message — this proves the full history actually reaches
    // Ollama's wire format, not just the latest turn.
    #[test]
    fn to_ollama_messages_maps_user_and_assistant_roles() {
        let messages = vec![
            Message::User {
                content: "hello".to_string(),
            },
            Message::Assistant {
                content: "hi there".to_string(),
            },
        ];

        let mapped = to_ollama_messages(&messages);

        assert_eq!(
            mapped,
            vec![
                OllamaMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                    ..Default::default()
                },
                OllamaMessage {
                    role: "assistant".to_string(),
                    content: "hi there".to_string(),
                    ..Default::default()
                },
            ]
        );
    }

    // Ollama's real assistant-tool-call shape: no id, no "type" tag, and
    // arguments as a JSON object rather than our own ToolCall's raw
    // string — the request-side mirror of what a tool-calling response
    // sends us (see extract_ollama_reply above).
    #[test]
    fn to_ollama_messages_maps_tool_calls_to_the_real_wire_shape() {
        let messages = vec![Message::ToolCalls {
            calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path": "a.txt"}"#.to_string(),
            }],
        }];

        let mapped = to_ollama_messages(&messages);

        assert_eq!(
            mapped,
            vec![OllamaMessage {
                role: "assistant".to_string(),
                content: String::new(),
                tool_calls: Some(vec![OllamaToolCall {
                    function: OllamaFunctionCall {
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": "a.txt"}),
                    }
                }]),
                ..Default::default()
            }]
        );
    }

    // Ollama correlates a tool result back to its request by name, not
    // id (it has no tool_call_id concept at all) — tool_name carries
    // that, sourced from Message::ToolResult's own name field.
    #[test]
    fn to_ollama_messages_maps_tool_result_to_the_real_wire_shape() {
        let messages = vec![Message::ToolResult {
            tool_call_id: "call_1".to_string(),
            name: "read_file".to_string(),
            content: "contents".to_string(),
        }];

        let mapped = to_ollama_messages(&messages);

        assert_eq!(
            mapped,
            vec![OllamaMessage {
                role: "tool".to_string(),
                content: "contents".to_string(),
                tool_name: Some("read_file".to_string()),
                ..Default::default()
            }]
        );
    }
}
