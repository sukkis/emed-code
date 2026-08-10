// Opt-in smoke test against a REAL local Ollama instance, using whatever
// model Core is currently hardcoded to (see MODEL in src/core.rs).
// Never runs in CI or a plain `cargo test` — only via
// `cargo test --features local` (see the Justfile's `test` recipe).
//
// Requires: Ollama running locally with that model pulled
// (`ollama list` to check).
#![cfg(feature = "local")]

use emed_code::core::{ConfirmationChoice, Core, CoreEvent};
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

// The known list of real files under src/core/ — see the identical
// constant in real_mistral_smoke_test.rs; duplicated rather than shared
// per this codebase's existing convention for test fixtures in these
// two files (see poll_until_final's own comment above).
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

// Ollama/mistral-nemo equivalents of
// real_mistral_smoke_test.rs's ambiguous-directory pair
// (docs/tool-descriptions.md Step 2). Manual testing (2026-08-02) found
// mistral-nemo already reaches zero-guess success on this exact prompt,
// both quoted and unquoted — these codify that as a regression guard,
// not to drive new implementation (no code change accompanies them; the
// tool descriptions are provider-agnostic, see tool_definitions()).
//
// Known flaky (2026-08-02): 5 runs, 3 passed, 2 failed — once with an
// empty AssistantChunk and no tool call at all (looks like an
// inference-level glitch, not specific to this prompt: the unrelated
// submit_user_message_can_call_list_files_via_local_ollama test hit the
// same empty-response shape once), once with the model correctly
// reasoning the real path was likely under src/ but asking the user to
// confirm instead of calling list_files_recursive itself — a real
// instruction-following miss (violates both the base system prompt's
// "don't pause to ask, proceed on your best assumption" and this
// tool's own "instead of asking the user to clarify"), not something
// more description wording is expected to fix — see
// docs/tool-descriptions.md's status for why this is being left as
// documented, known flakiness rather than chased further here.
#[test]
fn submit_user_message_finds_a_nested_directory_from_an_ambiguous_request_via_local_ollama() {
    let mut core = Core::new();

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

// See submit_user_message_avoids_guessing_at_a_nested_directorys_location
// in real_mistral_smoke_test.rs for why this is a separate test from
// task success above, not folded into one assertion.
#[test]
fn submit_user_message_avoids_guessing_at_a_nested_directorys_location_via_local_ollama() {
    let mut core = Core::new();

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
// see the identical helper's comment in real_mistral_smoke_test.rs;
// duplicated rather than shared per this codebase's existing convention
// for these two files.
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

// Ollama/mistral-nemo equivalent of real_mistral_smoke_test.rs's
// edit_file-vs-write_file test — see its comment for the full
// reasoning, including why this loops declining rather than handling
// one proposal (real testing against Mistral found a retry-after-decline
// happens in practice). mistral-nemo's documented flakiness elsewhere in
// this file (see
// submit_user_message_finds_a_nested_directory_from_an_ambiguous_request_via_local_ollama's
// comment) means this one may turn out flaky too — not assumed here,
// left for real observation to confirm or rule out.
#[test]
fn submit_user_message_prefers_edit_file_over_write_file_for_a_targeted_change_via_local_ollama() {
    let mut core = Core::new();

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

// Ollama/mistral-nemo equivalent of real_mistral_smoke_test.rs's
// create_directory regression guard — see its comment for the full
// reasoning, including why only "attempted at some point" is asserted
// rather than a strict ordering relative to write_file.
//
// Known flaky (2026-08-10): manual testing found this isn't a
// create_directory-specific issue — mistral-nemo is inconsistently
// unreliable on the *second* tool call of any two-step confirmable-write
// sequence (a control case, write_file then edit_file with no
// create_directory involved, hit the same general shape of failure). See
// docs/create-directory-regression-check.md for the full investigation.
// If this flakes, it's this general gap, not a create_directory bug.
#[test]
fn submit_user_message_reaches_for_create_directory_for_a_missing_directory_via_local_ollama() {
    let mut core = Core::new();

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
