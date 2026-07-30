// Conversation state, LLM round-trips, tool execution.
// No ratatui/crossterm imports — tui-facing rendering/input state
// lives in the tui module instead.

mod credentials;
mod mistral;
mod ollama;

pub use credentials::{CredentialSource, credential_log_message, lookup_mistral_api_key};
pub use mistral::MistralClient;
pub use ollama::OllamaClient;

pub(crate) use mistral::MISTRAL_MODEL;
pub(crate) use ollama::OLLAMA_MODEL;

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

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

fn to_core_event(send_result: Result<String, ChatError>) -> CoreEvent {
    match send_result {
        Ok(text) => CoreEvent::AssistantChunk(text),
        Err(e) => CoreEvent::Error(e.to_string()),
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
        Self::with_client(Arc::new(OllamaClient::new(OLLAMA_MODEL.to_string())))
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

    // to_core_event() is the pure, provider-agnostic decision logic that
    // turns "did the client's send() succeed" into a CoreEvent. It
    // doesn't parse anything itself — parsing is each LlmClient impl's
    // job — so it only ever sees already-extracted reply text or a
    // ChatError.
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
}
