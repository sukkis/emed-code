// Opt-in smoke test against the REAL Mistral API. Never runs in CI or a
// plain `cargo test` — only via `cargo test --features local` (see the
// Justfile's `test` recipe).
//
// Requires: a Mistral API key available via getfrompass
// (emed-code/mistral/api_key) or the MISTRAL_API_KEY env var.
#![cfg(feature = "local")]

use emed_code::core::{ConfirmationChoice, Core, CoreEvent, MistralClient, lookup_mistral_api_key};
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
        // Tools are advertised on every request, so this is reachable in
        // principle — this prompt just gives no reason to call one.
        CoreEvent::WriteProposed { .. } => panic!(
            "unexpected write proposal for a prompt needing none: {:?}",
            events[0]
        ),
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

// The known list of real files under src/core/, shared by both tests
// below — one checks the model actually finds them, the other checks it
// doesn't waste a call getting there.
const KNOWN_CORE_FILES: [&str; 8] = [
    "credentials.rs",
    "diff.rs",
    "mistral.rs",
    "ollama.rs",
    "sandbox_path.rs",
    "settings.rs",
    "system_prompt.rs",
    "tools.rs",
];

// Exercises the exact ambiguous-location failure docs/tool-descriptions.md
// Step 2 targets: "core" isn't a root-level directory in this repo (only
// src/core/ is), so answering requires confirming the real layout rather
// than guessing list_files(path: "core"). Task-success only — asserted on
// the final answer's content (real src/core/ filenames) and that
// list_files_recursive got called at some point, not on how cleanly it
// got there. See submit_user_message_avoids_guessing_at_a_nested_directorys_location
// below for the separate, stricter "did it waste a call" question — kept
// as two tests, not one, because they answer different questions and a
// model can pass one while failing the other (confirmed by manual
// testing, 2026-08-02: codestral-latest and mistral-medium-latest both
// eventually succeed even when they guess wrong first).
#[test]
fn submit_user_message_finds_a_nested_directory_from_an_ambiguous_request() {
    let (api_key, _source) = lookup_mistral_api_key().expect(
        "no Mistral API key found via getfrompass (emed-code/mistral/api_key) or MISTRAL_API_KEY",
    );
    let mut core = Core::with_client(Arc::new(MistralClient::new(
        api_key,
        "mistral-medium-latest".to_string(),
    )));

    core.submit_user_message("What files are under the core directory?".to_string());

    let events = poll_until_final(&mut core, Duration::from_secs(60));

    let used_list_files_recursive = events.iter().any(
        |event| matches!(event, CoreEvent::ToolCall { name, .. } if name == "list_files_recursive"),
    );
    assert!(
        used_list_files_recursive,
        "expected at least one list_files_recursive call, got: {events:?}"
    );

    match events.last() {
        Some(CoreEvent::AssistantChunk(text)) => assert!(
            KNOWN_CORE_FILES.iter().any(|file| text.contains(file)),
            "expected the reply to name real src/core/ files, got: {text:?}"
        ),
        other => panic!("expected a final assistant reply, got: {other:?}"),
    }
}

// The stricter, separate question from the task-success test above: does
// list_files's new redirect (docs/tool-descriptions.md Step 2) actually
// stop the model from guessing an unconfirmed path before falling back to
// list_files_recursive, or does it only ever get there after a failed
// guess first? Kept apart from task success on purpose — this is a
// "quality"/efficiency measure (avoiding a wasted round-trip), not
// correctness, and manual testing found both can vary independently.
// A failure here doesn't mean the feature is broken, only that the
// tightened wording isn't (yet, or reliably) preventing the guess.
//
// Checks both list_files AND list_files_recursive, not just list_files:
// manual testing (2026-08-02, mistral-medium-latest) found the guess can
// land on either tool — sometimes a failed list_files("core"), sometimes
// list_files_recursive correctly gets chosen but is still called with a
// guessed path ("core") instead of the project root ("."). Both are the
// same underlying failure (a guessed, unconfirmed path), just surfacing
// on a different tool call.
#[test]
fn submit_user_message_avoids_guessing_at_a_nested_directorys_location() {
    let (api_key, _source) = lookup_mistral_api_key().expect(
        "no Mistral API key found via getfrompass (emed-code/mistral/api_key) or MISTRAL_API_KEY",
    );
    let mut core = Core::with_client(Arc::new(MistralClient::new(
        api_key,
        "mistral-medium-latest".to_string(),
    )));

    core.submit_user_message("What files are under the core directory?".to_string());

    let events = poll_until_final(&mut core, Duration::from_secs(60));

    let failed_listing_calls = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                CoreEvent::ToolCall { name, result: Err(_), .. }
                    if name == "list_files" || name == "list_files_recursive"
            )
        })
        .count();
    assert_eq!(
        failed_listing_calls, 0,
        "expected no failed list_files/list_files_recursive guess before finding the real \
         path, got {failed_listing_calls}: {events:?}"
    );
}

// Like poll_until_final, but also stops the moment a write is proposed —
// used by the edit_file-vs-write_file test below, which needs to decline
// the proposal before the round can reach a final reply at all.
fn poll_until_write_proposed_or_final(core: &mut Core, timeout: Duration) -> Vec<CoreEvent> {
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    loop {
        events.extend(core.poll_events());
        if matches!(
            events.last(),
            Some(CoreEvent::WriteProposed { .. })
                | Some(CoreEvent::AssistantChunk(_))
                | Some(CoreEvent::Error(_))
        ) {
            return events;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for a write proposal or a final reply, got so far: {events:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// The point of docs/edit-file.md Step 6: given a small, targeted change
// to an existing file, does a real model actually reach for edit_file,
// or default to write_file just because it's the tool it already knew
// about? Declines every proposed write rather than applying any of
// them — this test is about which tool got chosen, not about actually
// mutating Cargo.toml in this repo.
//
// Loops declining rather than handling a single proposal: real testing
// (2026-08-03, mistral-medium-latest) found the model retries with
// another edit_file proposal immediately after a decline rather than
// giving up, so a fixed single round isn't enough to reach a final
// reply at all. The property that actually matters survives any number
// of retries — edit_file gets used and write_file never does, across
// the whole exchange — so that's what's asserted, not "the first
// attempt" specifically.
#[test]
fn submit_user_message_prefers_edit_file_over_write_file_for_a_targeted_change() {
    let (api_key, _source) = lookup_mistral_api_key().expect(
        "no Mistral API key found via getfrompass (emed-code/mistral/api_key) or MISTRAL_API_KEY",
    );
    let mut core = Core::with_client(Arc::new(MistralClient::new(
        api_key,
        "mistral-medium-latest".to_string(),
    )));

    core.submit_user_message(
        "In Cargo.toml, change the package version to \"0.1.1\" — just that one line, don't \
         touch anything else in the file."
            .to_string(),
    );

    let mut all_events = Vec::new();
    loop {
        let events = poll_until_write_proposed_or_final(&mut core, Duration::from_secs(60));
        let proposed = matches!(events.last(), Some(CoreEvent::WriteProposed { .. }));
        all_events.extend(events);
        if !proposed {
            break;
        }
        core.respond_to_confirmation(ConfirmationChoice::Decline);
    }

    let write_file_attempted = all_events
        .iter()
        .any(|event| matches!(event, CoreEvent::ToolCall { name, .. } if name == "write_file"));
    let edit_file_attempted = all_events
        .iter()
        .any(|event| matches!(event, CoreEvent::ToolCall { name, .. } if name == "edit_file"));

    assert!(
        edit_file_attempted && !write_file_attempted,
        "expected edit_file to be used and write_file never attempted for a targeted change, \
         got: {all_events:?}"
    );
}

// Regression guard for the real capability create_directory adds:
// write_file's new_for_write requires the parent directory to already
// exist (see sandbox_path.rs), so before create_directory existed there
// was no way for a model to satisfy "put a new file somewhere that
// doesn't exist yet" at all. Doesn't hint at create_directory by name —
// the prompt only implies the parent is missing, same "low-ambiguity but
// not spelled out" spirit as the other real-model tests in this file.
// Declines every proposal rather than applying any of them, same
// reasoning as the edit_file-vs-write_file test above — this is about
// which tool got reached for, not about actually creating files in this
// repo. Asserts create_directory was attempted at some point, not a
// strict ordering relative to write_file — a model that tries write_file
// first, sees it fail, then recovers via create_directory is still a
// pass; only never reaching for create_directory at all is the failure
// this guards against.
#[test]
fn submit_user_message_reaches_for_create_directory_for_a_file_in_a_missing_directory() {
    let (api_key, _source) = lookup_mistral_api_key().expect(
        "no Mistral API key found via getfrompass (emed-code/mistral/api_key) or MISTRAL_API_KEY",
    );
    let mut core = Core::with_client(Arc::new(MistralClient::new(
        api_key,
        "mistral-medium-latest".to_string(),
    )));

    core.submit_user_message(
        "Create a new file at examples/smoke-test/output.txt containing the text \"hello\"."
            .to_string(),
    );

    let mut all_events = Vec::new();
    loop {
        let events = poll_until_write_proposed_or_final(&mut core, Duration::from_secs(60));
        let proposed = matches!(events.last(), Some(CoreEvent::WriteProposed { .. }));
        all_events.extend(events);
        if !proposed {
            break;
        }
        core.respond_to_confirmation(ConfirmationChoice::Decline);
    }

    let create_directory_attempted = all_events.iter().any(
        |event| matches!(event, CoreEvent::ToolCall { name, .. } if name == "create_directory"),
    );

    assert!(
        create_directory_attempted,
        "expected create_directory to be used for a file inside a directory that doesn't \
         exist yet, got: {all_events:?}"
    );
}
