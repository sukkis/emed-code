// Conversation state, LLM round-trips, tool execution.
// No ratatui/crossterm imports — tui-facing rendering/input state
// lives in the tui module instead.

use std::sync::mpsc;
use std::thread;

#[derive(Debug, PartialEq)]
pub enum CoreEvent {
    AssistantChunk(String),
}

pub struct Core {
    tx: mpsc::Sender<CoreEvent>,
    rx: mpsc::Receiver<CoreEvent>,
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
