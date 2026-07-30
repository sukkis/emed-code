// Opt-in smoke test against the REAL Mistral API. Never runs in CI or a
// plain `cargo test` — only via `cargo test --features local` (see the
// Justfile's `test` recipe).
//
// Requires: a Mistral API key available via getfrompass
// (emed-code/mistral/api_key) or the MISTRAL_API_KEY env var.
#![cfg(feature = "local")]

use emed_code::core::{Core, CoreEvent, MistralClient, lookup_mistral_api_key};
use std::sync::Arc;
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
fn submit_user_message_gets_a_real_reply_from_mistral() {
    let (api_key, _source) = lookup_mistral_api_key().expect(
        "no Mistral API key found via getfrompass (emed-code/mistral/api_key) or MISTRAL_API_KEY",
    );
    let mut core = Core::with_client(Arc::new(MistralClient::new(
        api_key,
        "mistral-small-latest".to_string(),
    )));

    core.submit_user_message("Say hello in exactly one short sentence.".to_string());

    let events = poll_until_nonempty(&mut core, Duration::from_secs(30));

    match &events[0] {
        CoreEvent::AssistantChunk(text) => assert!(!text.is_empty()),
        CoreEvent::Error(message) => panic!("expected a reply, got an error: {message}"),
    }
}
