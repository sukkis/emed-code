//! [`AnthropicClient`], an [`LlmClient`] implementation talking to
//! Anthropic's Messages API.

use serde::Deserialize;

use super::{ChatError, LlmResponse, ToolCall};

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
    use crate::core::ToolCall;

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
}
