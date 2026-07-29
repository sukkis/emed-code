# Architecture

Maintainer-facing design decisions and the reasoning behind them. Status
and open gaps live in `SECURITY.md`; increment-by-increment planning
lives locally under `docs/` (gitignored, not part of this record).

## `core` / `tui` module split

`core` (`src/core.rs`) holds conversation state and provider round-trips;
it has zero `ratatui`/`crossterm` imports. It exposes exactly two entry
points: `submit_user_message(&mut self, text: String)` and
`poll_events(&mut self) -> Vec<CoreEvent>`. `tui` (`src/tui.rs`) owns all
rendering/input state — `InputBox` (typed text), `App` (owns `InputBox`
plus the message log and the one `Core` instance) — and talks to `core`
only through that pair of calls. Keeping the boundary this narrow is what
lets `core` be tested with zero terminal/rendering setup at all.

## Concurrency: thread + `mpsc`, no async runtime

Each `submit_user_message` call clones `Core`'s `mpsc::Sender` and spawns
a plain `std::thread` to do the provider round-trip; `poll_events` drains
the paired `Receiver` non-blockingly (`try_recv` in a loop), so it's safe
to call once per UI redraw without ever stalling on network I/O. No
`tokio`, matching the rest of this project's synchronous-main-loop
style. Revisit only if real concurrent-provider or cancellation needs
outgrow thread-per-request.

## Testing strategy: pure decision logic vs. a thin I/O shell

Provider integration code is split so the actual network call is as
small and dumb as possible, with everything else a plain function over
data:

- `fetch_ollama_reply` — the only part that touches `ureq`. Collapses
  every failure mode (connection refused, timeout, non-2xx status) into
  a plain `Result<String, String>`.
- `to_core_event` — pure: takes that `Result<String, String>`, returns a
  `CoreEvent`. Tested with plain fixture strings/`Err` values, no network
  involved.

This means the interesting behavior (what happens on a malformed
response vs. a failed request) is covered by fast, deterministic unit
tests, and the untestable-without-a-real-server sliver is kept as thin
as possible.

## `App`-level tests don't mock `Core`

`App::handle_key`'s tests (Enter submits, non-Enter forwards to
`InputBox`) only assert on `App`'s own observable state — log content,
input cleared — never on whether `Core::submit_user_message` "really"
ran. Introducing a trait/mock seam for `Core` purely to make that one
extra assertion possible isn't earning its keep: `Core`'s own behavior
is already covered by its own tests (see "Testing strategy" above), and
the full path (type → Enter → see a reply) still gets a real check by
running the app. Revisit if `App`-level logic grows complex enough that
"was `Core` engaged correctly" stops being obvious from reading the one
line that calls it.

## Quit-key detection is a pure predicate, not inline in `main.rs`

`is_quit_key(&KeyEvent) -> bool` (in `tui.rs`) decides whether a key
event should end the program (currently Ctrl-C or Ctrl-Q); `main.rs`
just calls it before forwarding anything else to `App`. The surrounding
loop (reading real events from a real terminal) can't be unit-tested,
but the decision itself doesn't need a terminal at all — same
pure-logic-vs-thin-shell split as `Core`'s provider code. This is also
why `main.rs` has stayed free of its own crossterm imports beyond
`event`/`Event`: the crossterm-type-level work happens in `tui.rs`.

## The `local` Cargo feature: gating tests that need a real external service

Some behavior can only be verified against a real local service (here:
Ollama). Those tests are never part of a plain `cargo test` or CI run —
they live in their own file under `tests/`, gated with
`#![cfg(feature = "local")]` at the top of the file, and only run via
`cargo test --features local` (or `just test`). This mirrors the same
convention already used in `emed` and `personal-cloud-mcp`, so it isn't
being invented fresh here. `just ci` runs plain `cargo test` (the
`local`-gated files compile to zero tests), `just test` runs with
`--features local` for full local coverage.

## No separate `crossterm` dependency — use ratatui's re-export

`tui` code imports crossterm types via `ratatui::crossterm::...`, not a
directly-added `crossterm` line in `Cargo.toml`. `ratatui`'s `crossterm`
feature (on by default) pulls in a specific crossterm version internally
and re-exports it at `ratatui::crossterm`. Adding our own separate
`crossterm` dependency risks Cargo resolving a *different* version than
the one `ratatui` itself was built against for the same functionality —
using the re-export guarantees they're always the same one.

## Terminal setup/teardown: `ratatui::run`, not hand-rolled

`main.rs` wraps its event loop in `ratatui::run(|terminal| { ... })`
rather than manually calling crossterm's raw-mode/alternate-screen
functions. `run` calls `ratatui::init()` (enables raw mode + alternate
screen, installs a panic hook that restores the terminal even if the
closure panics) before running the closure, then unconditionally calls
`ratatui::restore()` after — including on an early return via `?`. This
means panic-safe terminal restoration doesn't need any code of our own;
it comes from using this helper instead of managing terminal state
directly.

## Error handling: plain strings for now, no bespoke error type yet

Provider errors currently collapse to `String` (via `.to_string()` on
whatever `ureq`/`serde_json` produced) rather than a dedicated error
enum. There's no retry-vs-fail-fast or timeout-vs-auth-failure logic yet
that would need to distinguish failure kinds, so there's nothing for a
bespoke type to buy right now. A hand-written `ChatError` (no
`thiserror` — see Dependency Discipline) is expected once multi-provider
retry/fallback logic needs to tell those cases apart.
