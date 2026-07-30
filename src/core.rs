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

// One entry in the conversation history. Deliberately just User/
// Assistant for now — a tool-result-carrying variant arrives once the
// agent loop actually needs one. An enum, not a flat struct with
// optional fields, so an invalid combination (e.g. a tool result with
// no correlating id) isn't representable at all — matches every other
// enum decision in this codebase (ChatError, CredentialSource,
// ProviderLabel).
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    User { content: String },
    Assistant { content: String },
}

// Describes one tool the model may call. Minimal for now — a
// parameters JSON schema arrives once tool schema/dispatch actually
// need one; nothing constructs a non-empty ToolDefinition list yet.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
}

// One requested tool invocation. `arguments` stays a raw JSON string —
// parsing it into typed arguments is each tool's own job, not something
// LlmResponse itself should assume the shape of.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, PartialEq)]
pub enum LlmResponse {
    Text(String),
    ToolCalls(Vec<ToolCall>),
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
    fn send(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ChatError>;
}

// Tool-call dispatch isn't wired up yet — nothing produces
// LlmResponse::ToolCalls this step, so that arm is an explicit
// placeholder, not a real dispatch path.
fn to_core_event(send_result: Result<LlmResponse, ChatError>) -> CoreEvent {
    match send_result {
        Ok(LlmResponse::Text(text)) => CoreEvent::AssistantChunk(text),
        Ok(LlmResponse::ToolCalls(_)) => {
            CoreEvent::Error("tool calls not yet supported".to_string())
        }
        Err(e) => CoreEvent::Error(e.to_string()),
    }
}

pub struct Core {
    client: Arc<dyn LlmClient + Send + Sync>,
    history: Vec<Message>,
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
        Core {
            client,
            history: Vec::new(),
            tx,
            rx,
        }
    }

    pub fn submit_user_message(&mut self, text: String) {
        self.history.push(Message::User { content: text });

        let tx = self.tx.clone();
        let client = Arc::clone(&self.client);
        let history = self.history.clone();
        thread::spawn(move || {
            let event = to_core_event(client.send(&history, &[]));
            let _ = tx.send(event);
        });
    }

    pub fn poll_events(&mut self) -> Vec<CoreEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            if let CoreEvent::AssistantChunk(text) = &event {
                self.history.push(Message::Assistant {
                    content: text.clone(),
                });
            }
            events.push(event);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    // to_core_event() is the pure, provider-agnostic decision logic that
    // turns "did the client's send() succeed" into a CoreEvent. It
    // doesn't parse anything itself — parsing is each LlmClient impl's
    // job — so it only ever sees an LlmResponse or a ChatError.
    #[test]
    fn to_core_event_returns_assistant_chunk_for_a_text_response() {
        let event = to_core_event(Ok(LlmResponse::Text("hi there".to_string())));

        assert_eq!(event, CoreEvent::AssistantChunk("hi there".to_string()));
    }

    #[test]
    fn to_core_event_returns_error_for_any_chat_error() {
        let event = to_core_event(Err(ChatError::Connection("connection refused".to_string())));

        assert!(matches!(event, CoreEvent::Error(_)));
    }

    // Tool-call dispatch isn't wired up yet — this just needs
    // to_core_event to handle the variant exhaustively rather than not
    // compile. A real tool call can't reach this yet (nothing produces
    // LlmResponse::ToolCalls yet), so a clear "not yet supported" error
    // is a fine placeholder.
    #[test]
    fn to_core_event_returns_an_error_for_tool_calls_not_yet_supported() {
        let event = to_core_event(Ok(LlmResponse::ToolCalls(vec![ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: "{}".to_string(),
        }])));

        assert!(matches!(event, CoreEvent::Error(_)));
    }

    // Records every call it receives so tests can assert on exactly what
    // history Core threaded through, without any real network I/O.
    struct RecordingClient {
        calls: Mutex<Vec<Vec<Message>>>,
    }

    impl RecordingClient {
        fn new() -> Self {
            RecordingClient {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl LlmClient for RecordingClient {
        fn send(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<LlmResponse, ChatError> {
            let mut calls = self.calls.lock().unwrap();
            calls.push(messages.to_vec());
            let reply_number = calls.len();
            Ok(LlmResponse::Text(format!("reply {reply_number}")))
        }
    }

    // poll_events() is non-blocking, so it can race a reply that hasn't
    // arrived yet from the spawned thread. Same pattern as the
    // tests/real_*_smoke_test.rs integration tests.
    fn poll_until_nonempty(core: &mut Core, timeout: Duration) -> Vec<CoreEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            let events = core.poll_events();
            if !events.is_empty() {
                return events;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for a CoreEvent");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn history_accumulates_across_multiple_submit_user_message_calls() {
        let recorder = Arc::new(RecordingClient::new());
        let client: Arc<dyn LlmClient + Send + Sync> = recorder.clone();
        let mut core = Core::with_client(client);

        core.submit_user_message("first".to_string());
        poll_until_nonempty(&mut core, Duration::from_secs(1));

        core.submit_user_message("second".to_string());
        poll_until_nonempty(&mut core, Duration::from_secs(1));

        let calls = recorder.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            vec![Message::User {
                content: "first".to_string()
            }]
        );
        assert_eq!(
            calls[1],
            vec![
                Message::User {
                    content: "first".to_string()
                },
                Message::Assistant {
                    content: "reply 1".to_string()
                },
                Message::User {
                    content: "second".to_string()
                },
            ]
        );
    }
}
