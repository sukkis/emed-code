// Conversation state, LLM round-trips, tool execution.
// No ratatui/crossterm imports — tui-facing rendering/input state
// lives in the tui module instead.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq)]
pub enum CoreEvent {
    AssistantChunk(String),
    Error(String),
}

// Hand-written, not `thiserror` — per this project's Dependency
// Discipline (parent CLAUDE.md), a handful of variants isn't worth a
// dependency.
#[derive(Debug, PartialEq)]
pub enum ChatError {
    Connection(String),
    MalformedResponse(String),
    Auth(String),
}

impl fmt::Display for ChatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChatError::Connection(message) => write!(f, "connection error: {message}"),
            ChatError::MalformedResponse(message) => {
                write!(f, "malformed response: {message}")
            }
            ChatError::Auth(message) => write!(f, "authentication error: {message}"),
        }
    }
}

impl std::error::Error for ChatError {}

// Implemented by each provider's client; Core talks to whichever one is
// active only through this, never through a provider-specific type.
pub trait LlmClient {
    fn send(&self, message: &str) -> Result<String, ChatError>;
}

const OLLAMA_URL: &str = "http://localhost:11434/api/chat";
const MODEL: &str = "mistral-nemo";

#[derive(Debug, Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
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

fn to_core_event(send_result: Result<String, ChatError>) -> CoreEvent {
    match send_result {
        Ok(text) => CoreEvent::AssistantChunk(text),
        Err(e) => CoreEvent::Error(e.to_string()),
    }
}

fn fetch_ollama_reply(text: &str) -> Result<String, ChatError> {
    let request = OllamaRequest {
        model: MODEL.to_string(),
        messages: vec![OllamaMessage {
            role: "user".to_string(),
            content: text.to_string(),
        }],
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

pub struct OllamaClient;

impl LlmClient for OllamaClient {
    fn send(&self, message: &str) -> Result<String, ChatError> {
        let body = fetch_ollama_reply(message)?;
        extract_reply(&body)
    }
}

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

const MISTRAL_URL: &str = "https://api.mistral.ai/v1/chat/completions";
const MISTRAL_MODEL: &str = "mistral-small-latest";

fn fetch_mistral_reply(api_key: &str, text: &str) -> Result<String, ChatError> {
    let request = MistralRequest {
        model: MISTRAL_MODEL.to_string(),
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
}

impl MistralClient {
    pub fn new(api_key: Zeroizing<String>) -> Self {
        MistralClient { api_key }
    }
}

impl LlmClient for MistralClient {
    fn send(&self, message: &str) -> Result<String, ChatError> {
        let body = fetch_mistral_reply(&self.api_key, message)?;
        extract_mistral_reply(&body)
    }
}

const MISTRAL_PASS_KEY: &str = "emed-code/mistral/api_key";
const MISTRAL_API_KEY_ENV_VAR: &str = "MISTRAL_API_KEY";

#[derive(Debug, PartialEq)]
pub enum CredentialSource {
    Pass,
    EnvVar,
}

// pass wins when both are present — the more trustworthy source. None
// covers "pass had no value for any reason" (try_get_from_pass can't
// distinguish "no entry" from other failures), not specifically "no
// entry".
fn resolve_mistral_api_key(
    pass_value: Option<Zeroizing<String>>,
    env_var_value: Option<String>,
) -> Option<(Zeroizing<String>, CredentialSource)> {
    if let Some(key) = pass_value {
        return Some((key, CredentialSource::Pass));
    }
    env_var_value.map(|key| (Zeroizing::new(key), CredentialSource::EnvVar))
}

pub fn credential_log_message(source: &CredentialSource) -> &'static str {
    match source {
        CredentialSource::Pass => "Mistral key: from getfrompass",
        CredentialSource::EnvVar => {
            "Mistral key: from env var (no value emed-code/mistral/api_key from getfrompass)"
        }
    }
}

// The real getfrompass/env var lookup. Not unit tested directly — same
// as fetch_ollama_reply — see resolve_mistral_api_key for the tested
// decision logic this just feeds real values into.
pub fn lookup_mistral_api_key() -> Option<(Zeroizing<String>, CredentialSource)> {
    let pass_value = getfrompass::try_get_from_pass(MISTRAL_PASS_KEY);
    let env_var_value = std::env::var(MISTRAL_API_KEY_ENV_VAR).ok();
    resolve_mistral_api_key(pass_value, env_var_value)
}

pub struct Core {
    client: Arc<dyn LlmClient + Send + Sync>,
    tx: mpsc::Sender<CoreEvent>,
    rx: mpsc::Receiver<CoreEvent>,
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Core {
    pub fn new() -> Self {
        Self::with_client(Arc::new(OllamaClient))
    }

    pub fn with_client(client: Arc<dyn LlmClient + Send + Sync>) -> Self {
        let (tx, rx) = mpsc::channel();
        Core { client, tx, rx }
    }

    pub fn submit_user_message(&mut self, text: String) {
        let tx = self.tx.clone();
        let client = Arc::clone(&self.client);
        thread::spawn(move || {
            let event = to_core_event(client.send(&text));
            let _ = tx.send(event);
        });
    }

    pub fn poll_events(&mut self) -> Vec<CoreEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
        }
        events
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

    // to_core_event() is the pure, provider-agnostic decision logic that
    // turns "did the client's send() succeed" into a CoreEvent. It no
    // longer parses anything itself — parsing is each LlmClient impl's
    // job (see extract_reply) — so it only ever sees already-extracted
    // reply text or a ChatError.
    #[test]
    fn to_core_event_returns_assistant_chunk_for_a_successful_send() {
        let event = to_core_event(Ok("hi there".to_string()));

        assert_eq!(event, CoreEvent::AssistantChunk("hi there".to_string()));
    }

    #[test]
    fn to_core_event_returns_error_for_any_chat_error() {
        let event = to_core_event(Err(ChatError::Connection("connection refused".to_string())));

        assert!(matches!(event, CoreEvent::Error(_)));
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

    // getfrompass's try_get_from_pass returns Option<Zeroizing<String>>,
    // not Result — None covers "no entry" and every other failure mode
    // (pass not installed, gpg-agent locked, etc.) indistinguishably.
    // resolve_mistral_api_key takes that Option as a parameter rather
    // than calling getfrompass directly, so the "pass succeeded/failed"
    // and "env var present/absent" cases can be tested without a real
    // pass store.
    #[test]
    fn resolve_mistral_api_key_prefers_pass_when_both_are_available() {
        let pass_value = Some(Zeroizing::new("from-pass".to_string()));
        let env_var_value = Some("from-env".to_string());

        let (key, source) = resolve_mistral_api_key(pass_value, env_var_value).unwrap();

        assert_eq!(*key, "from-pass");
        assert_eq!(source, CredentialSource::Pass);
    }

    #[test]
    fn resolve_mistral_api_key_falls_back_to_env_var_when_pass_has_no_value() {
        let pass_value = None;
        let env_var_value = Some("from-env".to_string());

        let (key, source) = resolve_mistral_api_key(pass_value, env_var_value).unwrap();

        assert_eq!(*key, "from-env");
        assert_eq!(source, CredentialSource::EnvVar);
    }

    #[test]
    fn resolve_mistral_api_key_returns_none_when_neither_is_available() {
        let result = resolve_mistral_api_key(None, None);

        assert!(result.is_none());
    }

    #[test]
    fn credential_log_message_for_pass_names_getfrompass_not_pass() {
        assert_eq!(
            credential_log_message(&CredentialSource::Pass),
            "Mistral key: from getfrompass"
        );
    }

    #[test]
    fn credential_log_message_for_env_var_names_getfrompass_not_pass() {
        assert_eq!(
            credential_log_message(&CredentialSource::EnvVar),
            "Mistral key: from env var (no value emed-code/mistral/api_key from getfrompass)"
        );
    }

    // Compile-time proof that MistralClient satisfies LlmClient, same as
    // ollama_client_implements_llm_client above. send() needs a real
    // network round-trip (and a real API key), which stays out of scope
    // for a unit test — see tests/real_mistral_smoke_test.rs.
    #[test]
    fn mistral_client_implements_llm_client() {
        fn assert_is_llm_client<C: LlmClient>() {}
        assert_is_llm_client::<MistralClient>();
    }
}
