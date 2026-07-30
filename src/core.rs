// Conversation state, LLM round-trips, tool execution.
// No ratatui/crossterm imports — tui-facing rendering/input state
// lives in the tui module instead.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq)]
pub enum CoreEvent {
    AssistantChunk(String),
    Error(String),
}

// Hand-written, not `thiserror` — see project-plan.md's Dependency
// Discipline note: a handful of variants isn't worth a dependency.
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
struct ChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: OllamaMessage,
}

fn extract_reply(json: &str) -> Result<String, ChatError> {
    let response: ChatResponse =
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
    let request = ChatRequest {
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
    fn chat_request_serializes_to_ollama_shape() {
        let request = ChatRequest {
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
}
