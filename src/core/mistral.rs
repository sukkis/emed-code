use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{ChatError, LlmClient};

const MISTRAL_URL: &str = "https://api.mistral.ai/v1/chat/completions";
pub(crate) const MISTRAL_MODEL: &str = "mistral-small-latest";

#[derive(Debug, Serialize, Deserialize)]
struct MistralMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct MistralRequest {
    model: String,
    messages: Vec<MistralMessage>,
    stream: bool,
}

// One candidate completion. Mistral's API can in principle return more
// than one (the "choices" array), but we only ever read choices[0].
#[derive(Debug, Deserialize)]
struct MistralChoice {
    message: MistralMessage,
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

fn extract_mistral_reply(json: &str) -> Result<String, ChatError> {
    match serde_json::from_str::<MistralResponse>(json) {
        Ok(response) => response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| ChatError::MalformedResponse("no choices in response".to_string())),
        Err(parse_error) => match serde_json::from_str::<MistralErrorResponse>(json) {
            Ok(error) => Err(ChatError::Auth(error.message)),
            Err(_) => Err(ChatError::MalformedResponse(parse_error.to_string())),
        },
    }
}

fn fetch_mistral_reply(api_key: &str, model: &str, text: &str) -> Result<String, ChatError> {
    let request = MistralRequest {
        model: model.to_string(),
        messages: vec![MistralMessage {
            role: "user".to_string(),
            content: text.to_string(),
        }],
        stream: false,
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
    fn send(&self, message: &str) -> Result<String, ChatError> {
        let body = fetch_mistral_reply(&self.api_key, &self.model, message)?;
        extract_mistral_reply(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mistral's /v1/chat/completions request body: OpenAI-style shape,
    // reply nested under choices[0].message rather than a top-level
    // message field like Ollama's.
    #[test]
    fn mistral_request_serializes_to_the_expected_shape() {
        let request = MistralRequest {
            model: "mistral-small-latest".to_string(),
            messages: vec![MistralMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            stream: false,
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

        assert_eq!(reply, "hi there");
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
}
