# Architecture

Maintainer-facing design decisions and the reasoning behind them. Status
and open gaps live in `SECURITY.md`; increment-by-increment planning
lives locally under `docs/` (gitignored, not part of this record).

## `core` / `tui` / `cli` module split

`core` (`src/core.rs`) holds conversation state and provider round-trips;
it has zero `ratatui`/`crossterm` imports. It exposes exactly two entry
points: `submit_user_message(&mut self, text: String)` and
`poll_events(&mut self) -> Vec<CoreEvent>`. `tui` (`src/tui.rs`) owns all
rendering/input state — `InputBox` (typed text), `App` (owns `InputBox`
plus the message log and the one `Core` instance) — and talks to `core`
only through that pair of calls. Keeping the boundary this narrow is what
lets `core` be tested with zero terminal/rendering setup at all.

`cli` (`src/cli.rs`) is a third, smaller module for command-line flag
parsing — it's neither conversation state nor rendering/input state, so
it doesn't belong in either of the other two. It exposes `Cli`
(`clap::Parser`) and `Provider` (`clap::ValueEnum`); `main.rs` is the
only thing that constructs a `Cli` and acts on it, matching the existing
pattern of keeping pure, testable logic in a lib module and `main.rs`
itself thin (see "Quit-key detection" below for the precedent this
follows).

## Concurrency: thread + `mpsc`, no async runtime

Each `submit_user_message` call clones `Core`'s `mpsc::Sender` and spawns
a plain `std::thread` to do the provider round-trip; `poll_events` drains
the paired `Receiver` non-blockingly (`try_recv` in a loop), so it's safe
to call once per UI redraw without ever stalling on network I/O. No
`tokio`, matching the rest of this project's synchronous-main-loop
style. Revisit only if real concurrent-provider or cancellation needs
outgrow thread-per-request.

## `LlmClient` trait: `Core` talks to providers only through this

`Core` never calls a provider's HTTP shape directly. It holds an
`Arc<dyn LlmClient + Send + Sync>` and calls `client.send(&self, message:
&str) -> Result<String, ChatError>` — a plain sync method, no
`async-trait`, matching the rest of the project's sync-loop-plus-threads
concurrency model. `OllamaClient` is the first implementation (a
`MistralClient` is next); adding a provider means writing a new
`LlmClient` impl, not touching `Core`.

`Arc`, not `Box`: dynamic dispatch (a trait object, not a generic
`Core<C: LlmClient>`) was chosen because the concrete client isn't known
until a `--provider` CLI flag is parsed at startup — there's no
generics-only capability the trait's plain sync method needs that would
justify monomorphization instead. `Arc` specifically (rather than `Box`)
is required by the concurrency model above: `submit_user_message` spawns
a new thread per call, and that thread's `move` closure needs its own
usable handle to the client while `Core` keeps using its own handle for
every later call — `Box`'s exclusive ownership can only ever have one
owner, so the client would have to be moved permanently out of `Core`
into the first thread that used it. `Arc`'s reference-counted *shared*
ownership lets `Core` keep a handle while `Arc::clone` (an O(1) refcount
bump, not a copy of the client itself) hands the spawned thread an
independent one pointing at the same value. The `+ Send + Sync` bound on
the trait object is required because `Arc<T>` is only itself safe to
move/share across threads when `T: Send + Sync`; the compiler can't
infer that for an arbitrary `dyn LlmClient`, so it's stated explicitly.

## Error handling: a hand-written `ChatError` enum, no `thiserror`

Provider errors are `ChatError` (`Connection`, `MalformedResponse`,
`Auth`), with hand-written `Display`/`std::error::Error` impls rather
than `thiserror`-derived ones — per this project's Dependency Discipline
(parent `CLAUDE.md`), a handful of variants is a small, genuinely
instructive amount of code for a learning-focused project, not
boilerplate worth a dependency. `Auth` exists for Mistral's API-key
rejection case, which
`OllamaClient` has no way to hit (no credentials involved) but the enum
is shared across every `LlmClient` impl.

`extract_mistral_reply` is where `Auth` actually gets populated: Mistral
can send back one of two shapes on any given request — a success
envelope (`{"choices": [...]}`) or an error envelope (`{"message": ...,
"request_id": ...}`, e.g. on a 401), and there's no HTTP status code
available at this pure-parsing layer to tell them apart up front — that
information lives in the HTTP response `fetch_mistral_reply` receives,
one layer up, but the body text alone is all `extract_mistral_reply`
gets to work with. So it tries the success shape first; if that fails
to deserialize, it tries the error shape; if that also fails, it
surfaces the *original* success-shape parse error as
`ChatError::MalformedResponse` rather than the second attempt's (a
generic "wasn't valid JSON at all" is a more useful message than "also
didn't look like an error envelope"). This means a real Mistral auth
failure comes back as `ChatError::Auth("Unauthorized")`, not lumped in
with genuinely malformed responses.

## Credentials: `MistralClient` takes an already-resolved key

`MistralClient::new(api_key: Zeroizing<String>, model: String)` takes
the key directly rather than performing the `getfrompass`/env-var lookup
itself. Resolving *which* source supplies the key
(`resolve_mistral_api_key`/`lookup_mistral_api_key`) and deciding what
happens if neither source has one are startup-wiring concerns — that's
`main.rs`'s job (see "Startup wiring" below), not `MistralClient`'s. This
keeps `MistralClient` scoped to one responsibility: given a key and a
model, do the HTTP round-trip and map errors correctly.
`lookup_mistral_api_key` tries `getfrompass` first (key
`emed-code/mistral/api_key`), falling back to the `MISTRAL_API_KEY` env
var only when `getfrompass` yields no value — see `SECURITY.md` for the
credential-handling specifics (never logging the value, only ever
calling the non-panicking `try_get_from_pass`).

## Startup wiring: `main.rs` picks the `LlmClient`, `Core` stays agnostic

`main.rs` parses `Cli`, resolves the model (`Cli::resolved_model`, which
falls back to `OLLAMA_MODEL`/`MISTRAL_MODEL` — the same
`pub(crate)` constants `cli.rs`'s defaulting logic reads directly, so the
default model string exists in exactly one place, not duplicated between
`core.rs` and `cli.rs`), then constructs whichever client the
`--provider` flag selected before entering `ratatui::run`. For Mistral,
this is also where `lookup_mistral_api_key` actually gets called and its
result acted on: on success, `credential_log_message` is printed (to the
plain terminal, before the alternate screen takes over — same reasoning
as any other pre-TUI startup diagnostic); on failure, `main` returns an
`io::Error` before any TUI setup happens, rather than the app opening
with no working provider. `App::with_core(core: Core, provider_label:
ProviderLabel)` (alongside the existing zero-arg `App::new`) is what
lets `main.rs` hand in a specifically-constructed `Core` and the label
describing it.

## Provider label: a static, startup-time indicator in the chat title

`tui::ProviderLabel` (`Ollama`/`Mistral`) is `tui`'s own enum, not a
reuse of `cli::Provider` — `tui` has no reason to depend on `cli` for
what is, from its perspective, just display text (`"local (ollama)"` /
`"cloud (mistral)"`, rendered as `Block::bordered().title(format!("emed-code
— AI: {}", ...))` in `draw`). `main.rs` maps `cli::Provider` to
`ProviderLabel` when constructing `App`, since it's the one place that
already knows both. This is deliberately a one-time, startup-set value —
`App` stores it once in `with_core` and `draw` just reads it every
frame; there's no live-switching mechanism, matching the CLI selection
it reflects being parsed once at process start.

## Testing strategy: pure decision logic vs. a thin I/O shell

Provider integration code is split so the actual network call is as
small and dumb as possible, with everything else a plain function over
data:

- `fetch_ollama_reply`/`fetch_mistral_reply` — the only parts that touch
  `ureq`. Collapse every failure mode (connection refused, timeout,
  non-2xx status) into `Result<String, ChatError>` (specifically
  `ChatError::Connection`). Neither has a unit test of its own; each has
  a `local`-feature-gated smoke test instead (see "The `local` Cargo
  feature" below).
- `extract_reply`/`extract_mistral_reply` — pure: parse a response body
  into the reply text or a `ChatError`. Tested with fixture JSON
  strings, no network involved. `OllamaClient::send`/`MistralClient::send`
  are each just their provider's fetch-then-extract call chained.
- `to_core_event` — pure and now provider-agnostic: takes the
  `Result<String, ChatError>` a `LlmClient::send` call already produced
  (fetch *and* parse are done by then) and wraps it into a `CoreEvent`.
  It doesn't parse anything itself, unlike before this trait existed —
  parsing is each provider's own job, since that's the part that differs
  between them.

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

Some behavior can only be verified against a real external service —
Ollama locally, or the real Mistral API (`tests/real_ollama_smoke_test.rs`,
`tests/real_mistral_smoke_test.rs`). Those tests are never part of a
plain `cargo test` or CI run — they live in their own file under
`tests/`, gated with
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

## Main loop timing: poll with a timeout, not a blocking read

`main.rs`'s loop uses `event::poll(Duration::from_millis(100))` rather
than a blocking `event::read()`. A blocking read would mean `Core`
replies arriving while the user isn't typing go unnoticed until the
next keypress happens to trigger a redraw. `poll` returns immediately
the moment a real key event is ready — the timeout only bounds how long
it's willing to wait when *nothing* happens, so typing latency is
unaffected; only "is there anything new from `Core`" carries up to
100ms of lag, and only during idle time. Getting genuinely zero-latency
wakeups (no timeout at all) would require unifying keyboard input and
`Core`'s replies into one channel the loop blocks on indefinitely —
`std::sync::mpsc` can't select across two independently-typed channels,
so that would mean either reworking `Core` to report via an injected
callback/sender instead of owning its own internal channel, or adding
`crossbeam-channel` for real multi-channel `select!`. Not worth it for
100ms of harmless idle-only lag; revisit if that ever stops being true.

## Text wrapping: ported from emed, not a shared dependency

Long chat lines are wrapped with a standalone `wrap_line(line: &str,
width: usize) -> Vec<String>` (word-wrap on spaces, hard-break a single
word longer than the width, real `unicode-width` character
measurement) — the same approach as emed's `src/wrap.rs`, ported rather
than depended on. `emed`'s `wrapped_lines` is a method on `EditorState`
(its whole rope-backed buffer/cursor/undo state), not a standalone
function, so reusing it directly would mean constructing a full
`EditorState` for something conceptually self-contained.

Also considered and declined: adopting `ropey` more broadly, for
consistency with emed. `InputBox` (a single short line, edited only at
its end) and the log (an append-only transcript, never edited in
place) don't have the shape `ropey` is for — efficient arbitrary-
position edits in a large, actively-edited document. `String` and
`Vec<String>` already fit both. If a future piece genuinely needs that
(not currently planned — Phase 4's diff view is confirm-only, not
inline-editable), that's a fresh decision for that piece specifically,
not a reason to convert what's here now.

Confirmed while evaluating this: emed's `Lexer` trait
(`tokenize_line(&self, line: &str, in_comment: bool)`) takes a plain
`&str` with zero `ropey` involvement, so if syntax highlighting is ever
borrowed from emed (a V2+ item, not Phases 1–4), that reuse is
unaffected by any of the above either way.

`wrap_line` is now wired into `draw` via `wrap_text(text: &str, width:
usize) -> Vec<String>`, not called directly on a whole log entry.
`wrap_line` only expects a single already-`\n`-free line (matching its
origin: one rope buffer line); calling it straight on a multi-paragraph
entry stripped every embedded newline, collapsing paragraph breaks and
indentation into one continuous run before width-wrapping. `wrap_text`
splits on real line breaks first (`.lines()`, which preserves blank
lines and each line's own leading whitespace) and only sends individual
lines to `wrap_line` for width-wrapping. The resulting flat line count
now drives both rendering and `chat_scroll_skip`'s scroll math, so
counted and rendered lines can't disagree — this was the whole point of
doing the wrapping ourselves instead of splitting the responsibility
between our own line-counting and ratatui's separate `Paragraph::wrap`.

`scroll_up` now clamps against `App::max_scroll`, the true ceiling
(total wrapped lines minus visible height) as of the last render.
`draw` takes `&mut App` rather than `&App` specifically so it can feed
this back via `App::set_max_scroll` after computing `wrapped_lines`
each frame — the ceiling can't be known at key-press time otherwise,
since that depends on render width. It's at most one frame stale (the
gap between a resize and the next draw), which is harmless since the
next `draw` immediately recomputes it. Without this clamp,
`scroll_offset` could overshoot the true top with no ceiling, and
`scroll_down` would have to silently "pay off" that overshoot step by
step before the visible view started moving again — technically
correct but felt unresponsive.

Scrolling to the log's bottom (`scroll_offset == 0`) doubles as "the user
is following along" — `App::apply_core_events` relies on this and
deliberately leaves `scroll_offset` untouched when new content (e.g.
more of a streamed reply) arrives. Staying at `0` already tracks the
latest content automatically, since `chat_scroll_skip` recomputes the
skip from the current (growing) line count every render; forcing it
back to `0` on every event was only needed to *correct* a
non-`following` offset, and doing so unconditionally is what yanked a
manually-scrolled-up user back to the bottom mid-stream. This is a
distance-from-bottom representation, not an absolute anchor, so a
user scrolled up while content is still streaming will drift forward
slightly as new lines land below — not a full freeze, which was an
explicit, accepted trade-off (avoids needing an absolute-position
representation, which isn't knowable at key-press time for the same
reason `max_scroll` above isn't).
