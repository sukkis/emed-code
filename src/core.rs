// Conversation state, LLM round-trips, tool execution.
// No ratatui/crossterm imports — tui-facing rendering/input state
// lives in the tui module instead.

use std::sync::mpsc;
use std::thread;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq)]
pub enum CoreEvent {
    AssistantChunk(String),
}

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

fn extract_reply(json: &str) -> Result<String> {
    let response: ChatResponse = serde_json::from_str(json)?;
    Ok(response.message.content)
}

pub struct Core {
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
        let (tx, rx) = mpsc::channel();
        Core { tx, rx }
    }

    pub fn submit_user_message(&mut self, text: String) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let reply = format!("echo: {text}");
            let _ = tx.send(CoreEvent::AssistantChunk(reply));
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
    use std::time::{Duration, Instant};

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
    fn extract_reply_errors_on_malformed_json() {
        let json = r#"{ "message": { "role": "assistant" "#;

        let result = extract_reply(json);

        assert!(result.is_err());
    }

    // poll_events() is non-blocking, so it can race a reply that hasn't
    // arrived yet. Retry on the test's side until something shows up.
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

    // Proves the thread + mpsc round-trip end to end: submitting a message
    // spawns a thread, and its reply shows up via poll_events() afterward.
    #[test]
    fn submit_user_message_replies_on_a_background_thread() {
        let mut core = Core::new();

        core.submit_user_message("hello".to_string());

        let events = poll_until_nonempty(&mut core, Duration::from_secs(1));

        assert_eq!(
            events,
            vec![CoreEvent::AssistantChunk("echo: hello".to_string())]
        );
    }
}
