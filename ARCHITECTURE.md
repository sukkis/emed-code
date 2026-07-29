# Architecture

Maintainer-facing design decisions and the reasoning behind them. Status
and open gaps live in `SECURITY.md`; increment-by-increment planning
lives locally under `docs/` (gitignored, not part of this record).

## `core` / `tui` module split

`core` (`src/core.rs`) holds conversation state and provider round-trips;
it has zero `ratatui`/`crossterm` imports. It exposes exactly two entry
points: `submit_user_message(&mut self, text: String)` and
`poll_events(&mut self) -> Vec<CoreEvent>`. The `tui` module (not yet
built) will own all rendering/input state and talk to `core` only
through that pair of calls. Keeping the boundary this narrow is what
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

## Error handling: plain strings for now, no bespoke error type yet

Provider errors currently collapse to `String` (via `.to_string()` on
whatever `ureq`/`serde_json` produced) rather than a dedicated error
enum. There's no retry-vs-fail-fast or timeout-vs-auth-failure logic yet
that would need to distinguish failure kinds, so there's nothing for a
bespoke type to buy right now. A hand-written `ChatError` (no
`thiserror` — see Dependency Discipline) is expected once multi-provider
retry/fallback logic needs to tell those cases apart.
