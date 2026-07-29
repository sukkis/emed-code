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
    Error(String),
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

fn extract_reply(json: &str) -> Result<String> {
    let response: ChatResponse = serde_json::from_str(json)?;
    Ok(response.message.content)
}

fn to_core_event(fetch_result: std::result::Result<String, String>) -> CoreEvent {
    match fetch_result {
        Ok(body) => match extract_reply(&body) {
            Ok(text) => CoreEvent::AssistantChunk(text),
            Err(e) => CoreEvent::Error(e.to_string()),
        },
        Err(message) => CoreEvent::Error(message),
    }
}

fn fetch_ollama_reply(text: &str) -> std::result::Result<String, String> {
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
        .map_err(|e| e.to_string())?;

    response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
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
            let event = to_core_event(fetch_ollama_reply(&text));
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
    fn extract_reply_errors_on_malformed_json() {
        let json = r#"{ "message": { "role": "assistant" "#;

        let result = extract_reply(json);

        assert!(result.is_err());
    }

    // to_core_event() is the pure decision logic that turns "did the
    // Ollama request succeed" into a CoreEvent, kept separate from the
    // actual ureq I/O so these three cases don't need a live server.
    #[test]
    fn to_core_event_returns_assistant_chunk_for_a_well_formed_response() {
        let body = r#"{ "message": { "role": "assistant", "content": "hi there" }, "done": true }"#
            .to_string();

        let event = to_core_event(Ok(body));

        assert_eq!(event, CoreEvent::AssistantChunk("hi there".to_string()));
    }

    #[test]
    fn to_core_event_returns_error_for_a_malformed_response_body() {
        let body = "{ not valid json".to_string();

        let event = to_core_event(Ok(body));

        assert!(matches!(event, CoreEvent::Error(_)));
    }

    #[test]
    fn to_core_event_returns_error_when_the_request_itself_failed() {
        let event = to_core_event(Err("connection refused".to_string()));

        assert!(matches!(event, CoreEvent::Error(_)));
    }
}
