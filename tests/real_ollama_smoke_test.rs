// Opt-in smoke test against a REAL local Ollama instance, using whatever
// model Core is currently hardcoded to (see MODEL in src/core.rs).
// Never runs in CI or a plain `cargo test` — only via
// `cargo test --features local` (see the Justfile's `test` recipe).
//
// Requires: Ollama running locally with that model pulled
// (`ollama list` to check).
#![cfg(feature = "local")]

use emed_code::core::{Core, CoreEvent};
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

#[test]
fn submit_user_message_gets_a_real_reply_from_local_ollama() {
    let mut core = Core::new();

    core.submit_user_message("Say hello in exactly one short sentence.".to_string());

    let events = poll_until_nonempty(&mut core, Duration::from_secs(30));

    match &events[0] {
        CoreEvent::AssistantChunk(text) => assert!(!text.is_empty()),
        CoreEvent::Error(message) => panic!("expected a reply, got an error: {message}"),
        // Not impossible any more (tools are now advertised to Ollama
        // too), but this prompt gives no reason to call one — a tool
        // call here would be surprising model behavior worth
        // investigating, not silently allowed.
        CoreEvent::ToolCall { .. } => panic!("unexpected tool call from Ollama: {:?}", events[0]),
        CoreEvent::WriteProposed { .. } => {
            panic!("unexpected write proposal from Ollama: {:?}", events[0])
        }
    }
}
