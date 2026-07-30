use serde::{Deserialize, Serialize};

use super::{ChatError, LlmClient, LlmResponse, Message, ToolDefinition};

const OLLAMA_URL: &str = "http://localhost:11434/api/chat";
pub(crate) const OLLAMA_MODEL: &str = "mistral-nemo";

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

fn to_ollama_messages(messages: &[Message]) -> Vec<OllamaMessage> {
    messages
        .iter()
        .map(|message| match message {
            Message::User { content } => OllamaMessage {
                role: "user".to_string(),
                content: content.clone(),
            },
            Message::Assistant { content } => OllamaMessage {
                role: "assistant".to_string(),
                content: content.clone(),
            },
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
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

fn fetch_ollama_reply(model: &str, messages: Vec<OllamaMessage>) -> Result<String, ChatError> {
    let request = OllamaRequest {
        model: model.to_string(),
        messages,
        stream: false,
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
    #[test]
    fn ollama_request_serializes_to_the_expected_shape() {
        let request = OllamaRequest {
            model: "llama3".to_string(),
            messages: vec![OllamaMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            stream: false,
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
                    content: "hello".to_string()
                },
                OllamaMessage {
                    role: "assistant".to_string(),
                    content: "hi there".to_string()
                },
            ]
        );
    }
}
