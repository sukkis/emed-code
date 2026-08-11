// Opt-in smoke test against the REAL Anthropic API. Never runs in CI or
// a plain `cargo test` — only via `cargo test --features local` (see
// the Justfile's `test`/`anthropic` recipes).
//
// Requires: an Anthropic API key available via getfrompass
// (emed-code/anthropic/api_key) or the ANTHROPIC_API_KEY env var.
//
// Deliberately minimal — a smoke test proving the wire mapping works
// end-to-end against the real API, not a feature/reliability test.
// Manual testing already covered multi-tool-call round-trips, real
// transport errors (a genuine 429), and cost characteristics
// (2026-08-10) — see docs/anthropic-provider.md. Kept to two cheap,
// low-ambiguity prompts on purpose: this file runs on every
// `just anthropic`/`just test` invocation, and open-ended or looping
// prompts are exactly the shape that turned out expensive during manual
// testing.
#![cfg(feature = "local")]

use emed_code::core::{
    AnthropicClient, AnthropicThinking, Core, CoreEvent, lookup_anthropic_api_key,
};
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
fn submit_user_message_gets_a_real_reply_from_anthropic() {
    let (api_key, _source) = lookup_anthropic_api_key().expect(
        "no Anthropic API key found via getfrompass (emed-code/anthropic/api_key) or \
         ANTHROPIC_API_KEY",
    );
    let mut core = Core::with_client(Arc::new(AnthropicClient::new(
        api_key,
        "claude-sonnet-5".to_string(),
        AnthropicThinking::Disabled,
    )));

    core.submit_user_message("Say hello in exactly one short sentence.".to_string());

    let events = poll_until_nonempty(&mut core, Duration::from_secs(30));

    match &events[0] {
        CoreEvent::AssistantChunk(text) => assert!(!text.is_empty()),
        CoreEvent::Error(message) => panic!("expected a reply, got an error: {message}"),
        // A tool call here would be surprising model behavior worth
        // investigating, not silently allowed — this prompt gives no
        // reason to call one.
        CoreEvent::ToolCall { .. } => {
            panic!(
                "unexpected tool call for a prompt needing none: {:?}",
                events[0]
            )
        }
        CoreEvent::WriteProposed { .. } => panic!(
            "unexpected write proposal for a prompt needing none: {:?}",
            events[0]
        ),
    }
}

// The point of this test: a real Claude Sonnet 5 model, given a prompt
// with an obvious tool to reach for, actually calls it — through
// Anthropic's content-block tool_use/tool_result shape specifically —
// and completes the round-trip. Proves the wire mapping works for real,
// not just that its own fixture tests pass in isolation.
#[test]
fn submit_user_message_can_call_list_files_via_anthropic() {
    let (api_key, _source) = lookup_anthropic_api_key().expect(
        "no Anthropic API key found via getfrompass (emed-code/anthropic/api_key) or \
         ANTHROPIC_API_KEY",
    );
    let mut core = Core::with_client(Arc::new(AnthropicClient::new(
        api_key,
        "claude-sonnet-5".to_string(),
        AnthropicThinking::Disabled,
    )));

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
