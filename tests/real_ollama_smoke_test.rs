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

// Unlike poll_until_nonempty, keeps draining across the agent loop's
// possibly-multiple rounds until a final AssistantChunk or Error shows
// up — same helper as real_mistral_smoke_test.rs's identical fixture,
// duplicated rather than shared per this codebase's existing convention.
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

// The point of the whole increment: a real local Ollama model, given a
// prompt with an obvious tool to reach for, actually calls it and
// completes the round-trip — not just that our own request/response
// mapping is internally consistent (Steps 1-3's unit tests already
// proved that without touching a real model at all). "List the files
// in the current directory" is deliberately low-ambiguity, chosen so
// this test exercises real model behavior rather than real model
// creativity.
#[test]
fn submit_user_message_can_call_list_files_via_local_ollama() {
    let mut core = Core::new();

    core.submit_user_message("List the files in the current directory.".to_string());

    let events = poll_until_final(&mut core, Duration::from_secs(60));

    let list_files_result = events.iter().find_map(|event| match event {
        CoreEvent::ToolCall { name, result, .. } if name == "list_files" => Some(result),
        _ => None,
    });

    match list_files_result {
        Some(Ok(_)) => {}
        Some(Err(error)) => panic!("list_files call failed: {error}"),
        None => panic!("expected a list_files tool call, got: {events:?}"),
    }

    match events.last() {
        Some(CoreEvent::AssistantChunk(text)) => assert!(!text.is_empty()),
        other => panic!("expected a final assistant reply, got: {other:?}"),
    }
}
