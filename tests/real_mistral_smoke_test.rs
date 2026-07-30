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

// Unlike poll_until_nonempty, keeps draining across the agent loop's
// possibly-multiple rounds until a final AssistantChunk or Error shows
// up — the number of ToolCall events in between isn't known ahead of
// time here (that's up to the real model's behavior).
fn poll_until_final(core: &mut Core, timeout: Duration) -> Vec<CoreEvent> {
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    loop {
        events.extend(core.poll_events());
        if matches!(
            events.last(),
            Some(CoreEvent::AssistantChunk(_)) | Some(CoreEvent::Error(_))
        ) {
            return events;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for a final reply, got so far: {events:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
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
        // Not impossible for Mistral the way it is for Ollama (tools are
        // now advertised on every request), but this prompt gives no
        // reason to call one — a tool call here would be surprising
        // model behavior worth investigating, not silently allowed.
        CoreEvent::ToolCall { .. } => {
            panic!(
                "unexpected tool call for a prompt needing none: {:?}",
                events[0]
            )
        }
    }
}

// The concrete check for the agent loop's batching-simplification risk
// (one Message::ToolCalls entry per individual call, not per LLM turn —
// see ARCHITECTURE.md's "Conversation history" section): this task
// requires at least two tool calls (list_files, then read_file) to
// complete correctly. If Mistral's API rejects or misbehaves with our
// one-entry-per-call history shape, this is where it would surface —
// as a real ChatError or a clearly wrong/confused final answer — not
// silently pass because the only tested scenario never exercised more
// than one call at a time.
#[test]
fn submit_user_message_can_complete_a_task_requiring_multiple_tool_calls() {
    let (api_key, _source) = lookup_mistral_api_key().expect(
        "no Mistral API key found via getfrompass (emed-code/mistral/api_key) or MISTRAL_API_KEY",
    );
    let mut core = Core::with_client(Arc::new(MistralClient::new(
        api_key,
        "mistral-small-latest".to_string(),
    )));

    core.submit_user_message(
        "First list the files in the current directory. Then read the Cargo.toml file \
         and tell me the exact package name declared in it."
            .to_string(),
    );

    let events = poll_until_final(&mut core, Duration::from_secs(60));

    let tool_call_count = events
        .iter()
        .filter(|event| matches!(event, CoreEvent::ToolCall { .. }))
        .count();
    assert!(
        tool_call_count >= 2,
        "expected at least 2 tool calls (list_files, read_file), got {tool_call_count}: {events:?}"
    );

    match events.last() {
        Some(CoreEvent::AssistantChunk(text)) => assert!(
            text.contains("emed-code"),
            "expected the real package name in the reply, got: {text:?}"
        ),
        other => panic!("expected a final assistant reply, got: {other:?}"),
    }
}
