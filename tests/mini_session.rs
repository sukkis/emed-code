// Opt-in mini-session smoke test against a REAL local Ollama instance,
// driven through App rather than just Core — the only place App's
// wiring to a real Core (not a hand-fed test event) gets exercised.
// Never runs in CI or a plain `cargo test` — only via
// `cargo test --features local` (see the Justfile's `test` recipe).
//
// Requires: Ollama running locally with the model Core is hardcoded to
// (see MODEL in src/core.rs; `ollama list` to check).
//
// Deliberately asks short, factual questions rather than open-ended
// ones (e.g. "explain Rust borrowing", which can take Ollama the
// better part of a minute) — this test exercises App's plumbing, not
// the model's reasoning, so there's no reason for it to be slow. A
// narrow TestBackend is used so the submitted text itself (fully under
// our control) is guaranteed to need scrolling, regardless of how
// terse the model's actual reply turns out to be.
#![cfg(feature = "local")]

use emed_code::tui::{App, LogEntry, draw};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn type_and_submit(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(press(KeyCode::Char(c)));
    }
    app.handle_key(press(KeyCode::Enter));
}

// poll_core_events() is non-blocking, so it can race a reply that
// hasn't arrived yet (same reasoning as real_ollama_smoke_test.rs's
// poll_until_nonempty). Retries on the test's side until the log grows
// past what it was right after submitting.
fn poll_until_reply_lands(app: &mut App, log_len_after_submit: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        app.poll_core_events();
        if app.log().len() > log_len_after_submit {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for a reply to land in the log");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Narrow enough that the submitted text alone (not the reply) forces
// wrapping past the visible height, so max_scroll is set regardless of
// how short the model's actual reply is. App::max_scroll is only ever
// updated here, by draw.
fn render(app: &mut App) {
    let backend = TestBackend::new(12, 8);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, app)).unwrap();
}

#[test]
fn mini_session_scroll_position_survives_a_real_reply_but_not_a_new_submit() {
    let mut app = App::new();

    // Submitting resets scroll to the bottom on its own (deliberate:
    // jump to the bottom to watch the new reply — see
    // ARCHITECTURE.md). Scroll up again *before* ever polling, so no
    // event is claimed yet regardless of how fast Ollama actually
    // replies — poll_events is non-blocking and only drains what's
    // already arrived, so nothing is lost by waiting to poll.
    type_and_submit(
        &mut app,
        "Reply with exactly one word: hello. Keep it short, this is an automated test.",
    );
    render(&mut app);
    app.handle_key(press(KeyCode::Up));
    let scrolled_offset = app.scroll_offset();
    assert!(
        scrolled_offset > 0,
        "expected Up to move the view — is there really nothing to scroll to?"
    );

    let log_len_after_submit = app.log().len();
    poll_until_reply_lands(&mut app, log_len_after_submit, Duration::from_secs(30));

    match &app.log()[log_len_after_submit] {
        LogEntry::Text(text) => assert!(
            !text.starts_with("error: "),
            "expected a real reply, got: {text}"
        ),
        LogEntry::Diff { .. } => panic!(
            "unexpected diff entry — these short factual prompts give no reason to call a \
             write tool"
        ),
    }
    assert_eq!(
        app.scroll_offset(),
        scrolled_offset,
        "expected the manually-scrolled position to survive the reply arriving"
    );

    // A second, deliberate submit *should* reset scroll to the bottom —
    // that's the user asking to see the new reply, unlike a reply just
    // arriving on its own.
    type_and_submit(
        &mut app,
        "Reply with exactly one word: goodbye. Keep it short, this is an automated test.",
    );
    assert_eq!(app.scroll_offset(), 0);

    let log_len_after_second_submit = app.log().len();
    poll_until_reply_lands(
        &mut app,
        log_len_after_second_submit,
        Duration::from_secs(30),
    );

    assert_eq!(
        app.log().len(),
        4,
        "expected two user entries and two replies"
    );
}
