# Architecture

Maintainer-facing design decisions and the reasoning behind them. Status
and open gaps live in `SECURITY.md`; increment-by-increment planning
lives locally under `docs/` (gitignored, not part of this record).

## `core` / `tui` / `cli` module split

`core` (`src/core.rs` plus
`src/core/{ollama,mistral,credentials,sandbox_path,tools}.rs`) holds
conversation state and provider round-trips; it has zero
`ratatui`/`crossterm` imports. It exposes exactly two entry points:
`submit_user_message(&mut self, text: String)` and `poll_events(&mut
self) -> Vec<CoreEvent>`. `tui` (`src/tui.rs`) owns all rendering/input
state — `InputBox` (typed text), `App` (owns `InputBox` plus the message
log and the one `Core` instance) — and talks to `core` only through that
pair of calls. Keeping the boundary this narrow is what lets `core` be
tested with zero terminal/rendering setup at all.

Internally, `core.rs` itself only holds what's genuinely shared across
providers — `Core`, `CoreEvent`, `ChatError`, the `LlmClient` trait, and
`run_agent_loop` — and re-exports each submodule's public items (`pub
use ollama::OllamaClient`, etc.) so nothing outside `core` needs to know
about this internal layout; `cli.rs`'s `crate::core::OLLAMA_MODEL`/
`crate::core::MISTRAL_MODEL` and `main.rs`'s `emed_code::core::{...}`
imports are unaffected by it. Split out once `core.rs` reached ~525
lines (two providers' request/response types, fetch/extract functions,
clients, and credential resolution all in one file) — a pure move, no
behavior change, done ahead of Phase 3 adding tool-calling/`SandboxPath`
to `core` too, which would have made the file considerably harder to
navigate by the time that landed.

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
`Arc<dyn LlmClient + Send + Sync>` and calls `client.send(&self, messages:
&[Message], tools: &[ToolDefinition]) -> Result<LlmResponse, ChatError>`
— a plain sync method, no `async-trait`, matching the rest of the
project's sync-loop-plus-threads concurrency model. `OllamaClient` and
`MistralClient` are the two implementations; adding a provider means
writing a new `LlmClient` impl, not touching `Core`.

`messages`/`tools`/`LlmResponse` (added when `Core` gained real
conversation history — see "Conversation history" below) let a single
method signature represent both a plain chat turn and a tool-calling
turn: `tools` may be empty (as it always is today — nothing constructs a
non-empty `ToolDefinition` list yet), and `LlmResponse` is `Text(String)`
for a final answer or `ToolCalls(Vec<ToolCall>)` for one or more
requested tool invocations. Each provider maps the shared `Message` enum
into its own private wire-format type (`OllamaMessage`/`MistralMessage`)
via a small `to_ollama_messages`/`to_mistral_messages` function — the
provider never sees `Message` directly beyond that mapping step.

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

## Conversation history: `Message` enum, mutated only on the polling thread

`Core` holds a `Vec<Message>` that grows for the whole session — every
user message, assistant reply, tool-call request, and tool result gets
appended, and the full history is sent with every request (each
provider maps it into its own wire format; see the `LlmClient` section
above). `Message` is an enum (`User`, `Assistant`, `ToolCalls { calls:
Vec<ToolCall> }`, `ToolResult { tool_call_id: String, content: String
}`), not a flat struct with optional fields, matching every other enum
decision in this codebase (`ChatError`, `CredentialSource`,
`ProviderLabel`) — a tool-result-carrying variant was always the plan
here (see the note below on when it actually arrived) rather than a
speculative addition.

`ToolCalls` holds one entry *per individual tool call*, not one entry
per LLM turn — Mistral can in principle request several tool calls in a
single turn (one assistant message, multiple `tool_calls`), and the
textbook-correct shape would be one `Message::ToolCalls` holding all of
them. This was a deliberate simplification, tried rather than researched
to a standstill first, with a real assumption that needed checking: does
Mistral's API tolerate several separate single-call turns as well as one
multi-call turn?

Confirmed working: a real end-to-end test
(`tests/real_mistral_smoke_test.rs`) that requires at least two tool
calls to complete (list a directory, then read a file in it) passes
against the real API — order and `tool_call_id` correlation being
preserved is what actually mattered, not whether the calls are grouped
into one turn or several.

Deliberately unbounded and uncompacted for now: nothing trims or
summarizes history as it grows, so a very long session sends a
correspondingly large request every time. This isn't an oversight —
conversation summarization has been a named future item since before
Phase 1 started; it just had nothing concrete to apply to until history
existed at all.

History is mutated only on the thread that calls `poll_events()`, never
on the spawned background thread. `submit_user_message` clones the
current history into the closure that thread runs (so the network call
sees an accurate snapshot), but `self.history` itself stays exclusively
owned by `Core`; `poll_events()` is what appends the assistant's reply
(or, since the agent loop landed, a tool call's request+result pair)
back into it as `CoreEvent`s get drained. This avoids needing any
shared-mutable-state primitive (a `Mutex` around history, say) — there's
effectively a single writer, the thread that drives the UI loop. The
agent loop's own *local* copy of history (used to keep talking to the
provider across loop iterations within one `submit_user_message` call)
and `Core`'s *persistent* copy (rebuilt incrementally from the
`CoreEvent` stream) are deliberately two separate `Vec<Message>` — see
"The agent loop" below for why, and why they're kept in sync by
convention rather than by sharing one data structure.

## `SandboxPath`: containment enforced by the type system, not convention

`SandboxPath::new(root: &Path, requested: &Path) -> Result<Self,
SandboxError>` is the only way to construct one, and tool functions take
`&SandboxPath`, never a raw `PathBuf`/`&str` — a call site can't forget
to check containment, because there's no path type it could pass
instead that would compile. This matters more here than in a typical
read-only-file scenario: tool output (file contents) feeds back into the
LLM conversation, so an escaped read is a real prompt-injection-adjacent
exfiltration path, not a hypothetical one.

Containment is checked against `std::fs::canonicalize`d paths — both
`root` and the joined candidate — not lexical-only `.`/`..`
normalization. This is the part that actually matters: a symlink placed
*inside* the sandbox root pointing *outside* it looks contained as a
literal string (`root/link`), and only resolving it (which
`canonicalize` does, as a side effect of also resolving `.`/`..`) reveals
where it really points. `SandboxPath::new` canonicalizes `root` itself on
every call rather than trusting a caller to have done it once — the cost
is one extra filesystem call, and it removes a caller-side contract that
would otherwise be easy to get subtly wrong (e.g. a root path with an
unresolved symlink component, which would break the `starts_with` prefix
check in a way that's hard to notice).

`SandboxError` (`NotFound`, `Escapes`) has hand-written `Display` text —
fixed per variant, never interpolating the actual resolved path. A
rejected symlink's error must not itself disclose where the symlink
really pointed; that would defeat the point of rejecting it.

Tests use a hand-rolled `Drop`-based `TempDir` guard (creates a real
temp directory, removes it when dropped, even if a test panics
partway through) rather than adding a `tempfile` dev-dependency — same
reasoning as `ChatError` over `thiserror`: a small enough amount of code
that writing it directly is more instructive than depending on it, for
a learning-focused project.

### `new_for_write`: validating a target that may not exist yet (Phase 4 Step 1)

`SandboxPath::new` requires the full target to already exist (it
canonicalizes the whole candidate path, which fails on a nonexistent
file) — fine for read-only tools, but `write_file` needs to accept a
path that doesn't exist yet. `new_for_write(root: &Path, requested:
&Path) -> Result<Self, SandboxError>` validates in two stages instead:

1. **The parent directory must already exist.** `requested.file_name()`
   pulls off the last path component (`None` for something like `"."`/
   `".."`, folded into `NotFound` rather than a new variant, since it's
   the same "nothing to identify here" condition); the parent is
   canonicalized and checked for containment the same way `new` checks
   its whole candidate. No `mkdir -p` — a missing parent directory is
   `Err(NotFound)`, not auto-created (directory creation is its own,
   not-yet-built tool with its own security posture; see
   `SECURITY.md`).
2. **If the target already exists, it's re-validated in full.**
   `candidate.canonicalize().unwrap_or(candidate)` tries to
   canonicalize the exact target; success means the target already
   exists (the overwrite case), so any symlink sitting at that name is
   resolved and re-checked for containment — the same guarantee reads
   already get. Failure means the target doesn't exist yet (the create
   case), so the parent-only-canonicalized candidate is used as-is,
   which is still safe: `file_name()` already guaranteed the last
   component has no `.`/`..` to exploit.

Not yet reachable from any tool — real and unit-tested, but unused
outside its own tests until `write_file` is built on top of it (Phase
4's later steps).

## Tool execution: `dispatch()` is the only thing the agent loop calls

`src/core/tools.rs` holds `read_file`/`list_files` and `dispatch(root:
&Path, tool_call: &ToolCall) -> Result<String, ToolError>` — the one
function the agent loop calls for every `ToolCall` it gets back from a
provider. `dispatch` matches on `tool_call.name`
*first*, then parses that specific tool's own argument shape — not the
other way around — so an unrecognized tool name never has to reason
about argument parsing at all, and adding a third tool means one new
match arm plus one new function here, nothing in the agent loop itself
changes. This is deliberately the "clearly-bounded module" a future
orchestrator-backed tool source (see the Nextcloud-MCP discussion that
shaped this phase's scope) would swap in behind, without touching
`Core`.

Both tools go through `SandboxPath::new` before touching the filesystem
at all — there's no code path in either function that calls
`std::fs::read_to_string`/`std::fs::read_dir` on an unvalidated path.
Errors are `ToolError`, not raw `std::io::Error`: `InvalidPath` wraps
the underlying `SandboxError` (whose `Display` is already proven to
never leak a resolved path — see `SandboxPath`'s section above),
`IoFailure` covers a validated path that still fails to read (e.g.
permission denied, deleted mid-flight), and `MalformedArguments`/
`UnknownTool` cover dispatch-level failures. None of these variants
carry a raw `std::io::Error` or any other type whose `Display` isn't
under this project's own control.

`tool_definitions() -> Vec<ToolDefinition>` (also in `tools.rs`, next to
`dispatch`, not off in `core.rs`) is the fixed list actually advertised
to a provider — kept beside `dispatch`'s match arms specifically so the
two can't silently drift apart; a test asserts the names match. Each
provider maps `ToolDefinition` into its own wire-format tool-schema type
(`MistralTool`/`MistralFunctionDef` in `mistral.rs`, confirmed against
Mistral's function-calling docs: `{"type": "function", "function":
{name, description, parameters}}`) via a `to_mistral_tools` function,
mirroring how `Message` gets mapped into each provider's own message
type. `MistralRequest.tools: Vec<MistralTool>` uses `#[serde(
skip_serializing_if = "Vec::is_empty")]` so a request with nothing to
advertise omits the field entirely rather than sending `"tools": []`
(harmless either way in practice, since `Core` always has at least the
two built-in tools to advertise today, but avoids relying on that).

On the response side, `extract_mistral_reply` returns
`Result<LlmResponse, ChatError>` directly (not a bare `String`) — Mistral
sends back either plain text or a tool-calling turn, so the function
branches on whether any tool calls came back, rather than needing a
separate provider-facing concept of "did the model call a tool."

### A real bug, caught by the local-gated smoke test, not by unit tests

`MistralResponseMessage.tool_calls` was first typed as a bare
`Vec<MistralToolCall>` with `#[serde(default)]`, on the assumption that
a plain-text reply simply omits the `tool_calls` key. `#[serde(default)]`
only substitutes a default when a field is *missing* — it does not run
when the field is present with an explicit JSON `null`. Mistral's real
API does send `"tool_calls": null` explicitly for at least some
plain-text replies once a request advertises tools (inconsistently —
a later request in the same test session omitted the key instead,
suggesting this varies), which serde then tried to deserialize directly
into a `Vec`, failing with "invalid type: null, expected a sequence."
The fixture-based unit tests never caught this because they were
written before any real advertised-tools response existed to model the
fixture on. Fixed by typing the field `Option<Vec<MistralToolCall>>`
instead: `Option<T>` deserializes a JSON `null` as `None` natively (no
special attribute needed for that case), and `#[serde(default)]` still
covers a genuinely missing field the same way. `extract_mistral_reply`
then does `.unwrap_or_default()` to treat both as "no tool calls."
Codified as its own regression test
(`extract_mistral_reply_treats_an_explicit_null_tool_calls_as_absent`)
so this is caught by a fast, deterministic test from now on, not only
by the slow, real-network one.

## Settings: `~/.config/emed-code/settings.toml`, fail-safe

`src/core/settings.rs` holds `Settings { file_access_security:
FileAccessSecurity }` and `FileAccessSecurity` (`Strict`/`Loose`,
`Strict` the `#[default]` variant) — an enum-of-kinds for the setting's
value, not a raw `String`, matching every other enum decision in this
codebase (`ChatError`, `CredentialSource`, `Message`): an unrecognized
string can't silently become a third, unintended state, because there
is no `String` field left to hold one.

Split into a pure function and a thin I/O shell, the same pattern as
`extract_mistral_reply`/`fetch_mistral_reply`: `Settings::parse(toml_str:
&str) -> Settings` does the actual `basic_toml::from_str` parse and is
what the unit tests exercise directly (no filesystem access needed);
`Settings::load() -> Settings` resolves the real path via
`dirs::config_dir()` (added specifically for this — it gets
`XDG_CONFIG_HOME`/`AppData`/`Library` conventions right across Linux,
Windows, and macOS for one dependency, cheaper than hand-rolling and
then getting it wrong the first time this runs somewhere that isn't
Linux), reads it, and hands the contents to `parse`. Not unit-tested
itself, same as `Core::with_client`'s real `std::env::current_dir()`
call — only the pure logic around it is.

Threaded into `Core` the same way as `root`: `Core::with_client` calls
`Settings::load()` once at construction and stores the result in a new
`settings: Settings` field; `submit_user_message` copies out
`settings.file_access_security` (a `Copy` enum, so no need to clone or
share `Settings` itself across the thread boundary) and passes it into
`run_agent_loop`, which threads it through to every `dispatch` call.

### Fails safe, never panics, never fails open

`Settings::parse` uses `.unwrap_or_default()` on the parse `Result` — a
missing file, an empty file, malformed TOML, and an unrecognized
`file_access_security` value (e.g. `"yolo"`) all collapse to the exact
same outcome: `Settings::default()`, i.e. `Strict`. There is
deliberately no partial-recovery logic that tries to salvage a
malformed file's other fields; with only one field today that would be
pure speculation, and the fail-safe direction (defaulting to the *more*
restrictive value) is what makes collapsing every failure mode into
one outcome safe to do at all — the alternative of failing *open*
would turn a typo in a config file into a silent security regression.

### Why `~/.config/emed-code/`, not project-root or `~/.local/share/`

This file drives which files `read_file`/`list_files` refuse to touch
(see "Content-sensitivity filtering" below) — it is deliberately
*outside* the sandbox `SandboxPath` enforces, so a future `write_file`
tool (Phase 4) can never reach it, even indirectly through
prompt-injection-driven self-modification. A security-relevant
guardrail that could be edited by the same tool it constrains would
not be much of a guardrail. This mirrors the reasoning behind resolving
Mistral's API key via `getfrompass` rather than a project-local file
(see "Credentials" below): anything that gates what the model can do
or see should live somewhere the model's own tool access structurally
cannot.

## Content-sensitivity filtering: blocking `read_file`/`list_files` on sensitive paths (Step 8b)

`SandboxPath` gained a `relative_path()` accessor — the canonicalized
(symlink-resolved), root-relative path, stored alongside the existing
absolute path at construction time (one `strip_prefix` call, since the
containment check already proves the relative path exists).
`tools.rs`'s `is_content_restricted(relative_path: &Path) -> bool`
matches this against a small, explicitly non-exhaustive starter
blocklist: `.env`/`.env.*` and `*.pem`/`*.key` by filename, `.ssh` as a
path component matched anywhere (not just at the root, so
`project/.ssh/id_rsa` is caught too), and `.git/config` specifically
(the last two path components, not all of `.git` — `.git/hooks/pre-commit`
is unaffected).

`read_file`/`list_files` each check this (only when
`FileAccessSecurity::Strict`; `Loose` skips the check entirely, its one
current distinct behavior) after `SandboxPath::new` succeeds but before
touching the filesystem, returning the new `ToolError::AccessDenied` (a
static message — unlike `SandboxError::Escapes`, there's nothing to
hide here, since the LLM already knows exactly which path it
requested).

### Why the canonicalized path, not the raw requested string

Checking the literal argument a tool call passed in (e.g.
`"innocuous.txt"`) would miss a symlink with an innocuous name that
actually resolves to a blocked file (e.g. `.env`) — exactly the same
class of bypass `SandboxPath`'s containment check already had to
account for. Matching against `relative_path()` instead means the
blocklist inherits that same symlink-safety guarantee for free, rather
than introducing a second, weaker path check right next to the
stronger one. Covered by
`read_file_blocks_a_symlink_that_resolves_to_a_blocked_file`.

### `list_files`'s scope, specifically

A blocked entry's *name* still appears in its parent directory's
listing (existence isn't hidden — low risk), but listing *into* a
blocked directory, or anything nested inside one, is refused the same
way `read_file` refuses a blocked file directly — checked before
`std::fs::read_dir` runs at all, so there's no code path where a
blocked directory's contents get enumerated even partially.

## Diff generation: `DiffLine`, a pure wrapper around `similar` (Phase 4 Step 2)

`src/core/diff.rs` holds `DiffLine` (`Added`/`Removed`/`Unchanged`,
each carrying one line's text) and `generate_diff(old: &str, new: &str)
-> Vec<DiffLine>`. Structured per-line data, not a pre-formatted
string — deliberately, so the TUI can render `Added`/`Removed` with
real color (Step 3) rather than relying on a text convention like
unified diff's `+`/`-` prefixes, which was the whole point of
overriding the initial plain-text recommendation for this feature (see
`docs/write-file.md`).

`generate_diff` is a thin, pure wrapper around
`similar::TextDiff::from_lines(old, new)` — the Myers-diff algorithm
itself isn't re-implemented or second-guessed here, only mapped into
this project's own `DiffLine` shape. One thing worth knowing:
`similar`'s `Change::value()` includes each line's own trailing
newline (`from_lines` splits on it but keeps it attached to the line
that precedes it), which gets trimmed off — `DiffLine` holds one
line's text, not the newline that separates it from the next one.

`similar` (`3.1.1`) has zero external dependencies with its default
features (`std` + `text`) — its heavier features (`bytes`, `unicode`)
that would pull in `bstr`/`unicode-segmentation` aren't enabled.
Justified back when this project's overall plan was first sketched
(`docs/project-plan.md`): a real algorithm, not boilerplate, so worth a
dependency rather than hand-rolling — unlike `thiserror`, there's a
genuine correctness cost to getting a diff algorithm wrong.

Not yet reachable from anything real — `generate_diff`/`DiffLine` are
unit-tested directly but unused outside their own tests until Step 3
(rendering) and Step 4 (the actual `write_file` tool) build on top of
them.

## Colored diff rendering: `render_diff_lines` (Phase 4 Step 3)

`tui.rs`'s `render_diff_lines(diff: &[DiffLine]) -> Vec<Line<'static>>`
turns diff data into styled ratatui `Line`s — the first per-line-styled
content anywhere in this TUI (everything else is plain, unstyled
text). Not wired into `App`'s log/`draw` pipeline yet — tested directly
against constructed `DiffLine` data via `TestBackend`, checking actual
rendered cell colors, not just text content.

**Background color, not text color** — corrected after an initial
implementation used `Style::fg` (colored text). The user wants this to
match Claude Code's own diff display: a colored line *band* (red for
removed, green for added), not colored text, specifically so a future
per-line syntax-highlighting pass (not built, not scheduled) has the
text-color channel free rather than competing with diff coloring for
it. Each line also keeps a `+`/`-`/` ` text prefix alongside its color
— a terminal without color support, or a color-blind user, still gets
a real signal, not just a color-only distinction that vanishes without
color.

**Known, deliberate limitation, tracked as its own future step**: the
background only covers the line's own text width, not the full render
width — a true edge-to-edge band (matching Claude Code's actual look)
needs the text padded out to the real terminal width, which isn't
knowable at this pure, `Vec<DiffLine> → Vec<Line>` stage (no `Frame`/
width available here). Explicitly deferred to its own step at the end
of Phase 4, once real wiring into `draw` (Step 5) makes the actual
width available to pad against — see `docs/write-file.md`.

## The agent loop: `run_agent_loop`, capped at `MAX_TOOL_CALLS`

Runs entirely on `submit_user_message`'s spawned background thread, in
a plain `loop`: call `client.send(&history, &tool_defs)`; `Text` means
done (send `CoreEvent::AssistantChunk`, return); `ToolCalls` means
dispatch each one (`tools::dispatch`), append a `Message::ToolCalls` +
`Message::ToolResult` pair to the loop's *local* history for each, send
one `CoreEvent::ToolCall { id, name, arguments, result }` per call (once
dispatch has already completed — not a separate proposed/finished pair,
since local file tools finish near-instantly and there's no
meaningful in-progress state worth a second event), then loop again
with the updated history; any `ChatError` means done (send
`CoreEvent::Error`, return).

`CoreEvent::ToolCall.result` is `Result<String, String>`, not a
flattened `String` — refined after manual testing surfaced a real
problem: asking the agent to read several files rendered their entire
contents straight into the chat log, which was unpleasant to actually
use, not just noisy. `Message::ToolResult`'s `content` (what the *model*
sees on the next turn) still carries the full success-or-error text
either way — only what reaches the `CoreEvent` (and therefore the TUI)
distinguishes them, so `tui.rs`'s `format_core_event` can show just
`"ok"` on success (the payload is for the model, not something the log
needs to echo back at the user) while still showing an error's actual
message in full (short and useful, unlike a whole file's contents).

`MAX_TOOL_CALLS` (40) bounds the running count of *individual* tool
calls across the whole loop, not rounds — a round-based cap wouldn't
actually bound the risk it's meant to (a model batching many calls into
one round would sail through a low round-count cap while still doing
all that work) and would also cut off legitimate work (reading a new
project's ~15-20 files, worst case one file per round, could exceed a
cap like 10 rounds). Checked per-batch: if a round's calls would push
the running total over the cap, the whole round is rejected with a
`CoreEvent::Error`, not partially executed.

`Core` gained a `root: PathBuf` field for this (`dispatch` needs a
sandbox root and nothing previously provided one) — defaults to
`std::env::current_dir()` in `with_client`, so `main.rs` needed no
changes.

The now-removed `to_core_event` free function used to do this Text/Err
→ `CoreEvent` mapping (with a placeholder "tool calls not yet
supported" arm for `ToolCalls`, since nothing produced that variant
yet). Once the loop needed to handle `ToolCalls` with real dispatch
logic — necessarily inline, since only the loop knows about the running
tool-call count and can decide whether to continue — keeping a
separate helper that only ever got called for its other two branches,
with a now-factually-wrong third one, stopped earning its keep; the
two one-line mappings it used to do are inlined directly into the
loop's `match` arms instead.

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

### Known limitation, deliberately deferred

`ChatError::Connection`
flattens whatever `ureq` produced (connection refused, timeout, DNS
failure, TLS error, ...) into one `String` via `.to_string()`. That's
fine today — nothing currently needs to tell those cases apart. It stops
being fine the day retry-vs-fail-fast logic is written (expected in
Phase 3's agent loop, once Ollama's tool-calling reliability needs more
than plain retry-with-backoff): a string can't be pattern-matched
on to decide "retry this" vs. "give up," so `Connection` would need to
become its own small enum (e.g. `Timeout`, `Refused`, `DnsFailure`,
`Other(String)`) capturing `ureq::Error`'s actual variants instead of
collapsing them. Revisit at that point, not before — this is exactly the
"don't design for hypothetical future requirements" call, made
explicitly rather than silently.

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
  `ureq`. Take the already-mapped wire-format message list (built by
  `to_ollama_messages`/`to_mistral_messages`) and collapse every failure
  mode (connection refused, timeout, non-2xx status) into
  `Result<String, ChatError>` (specifically `ChatError::Connection`).
  Neither has a unit test of its own; each has a `local`-feature-gated
  smoke test instead (see "The `local` Cargo feature" below).
- `to_ollama_messages`/`to_mistral_messages` — pure: map the shared
  `Message` history into each provider's own wire-format type. Tested
  directly (fixture `Message` list in, expected wire messages out) —
  this is what actually makes `Core`'s conversation history reach the
  provider, so it earns its own test rather than only being exercised
  indirectly.
- `extract_reply`/`extract_mistral_reply` — pure: parse a response body
  into the reply text or a `ChatError`. Tested with fixture JSON
  strings, no network involved. `OllamaClient::send`/`MistralClient::send`
  are each just their provider's map-then-fetch-then-extract calls
  chained.
- `run_agent_loop` — the one place a `Result<LlmResponse, ChatError>`
  a `LlmClient::send` call produced becomes a `CoreEvent`: `Text`
  becomes `AssistantChunk`, `ChatError` becomes `Error`, and
  `ToolCalls` means dispatching each call and emitting one
  `CoreEvent::ToolCall` per call (see "The agent loop" above). This
  used to be a separate pure `to_core_event` function, removed once
  handling `ToolCalls` needed real dispatch logic that only the loop
  itself can drive (see that section for why).

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
