# Security

Current security posture and open gaps. This tracks *status*, not
rationale — see `ARCHITECTURE.md` for the why behind a given decision.

## Current status (implemented)

### Phase 1 — Ollama, local-only

No credentials, API keys, or secrets of any kind are read, stored, or
transmitted anywhere in the codebase. The only network calls are to a
local Ollama instance (`http://localhost`). Nothing here to leak.

### Mistral credential resolution + a real request path (Phase 2)

`resolve_mistral_api_key`/`lookup_mistral_api_key` in `src/core.rs`:
`getfrompass` (key `emed-code/mistral/api_key`) first, `MISTRAL_API_KEY`
env var fallback if `pass` yields no value, `pass` preferred whenever
both are present. Only `getfrompass::try_get_from_pass` is called —
never the panicking `get_from_pass` or any write function.

The resolved key is held as `Zeroizing<String>` regardless of which
source supplied it, and passed into `MistralClient::new`, which uses it
only to build the outgoing `Authorization: Bearer <key>` header in
`fetch_mistral_reply`. A startup log line (`credential_log_message`)
reports which source supplied the key, never the value, and
deliberately names `getfrompass` rather than `pass` — see
`ARCHITECTURE.md`.

Verified no key leakage into errors: the key is never passed into any
`ChatError` variant; even a malformed key that fails header-value
construction surfaces only a static `"failed to parse header value"`
message (confirmed against the `http` crate's `InvalidHeaderValue`
`Display` impl), not the attempted value.

Now selectable from `cargo run` via `--provider mistral` (see
`README.md`); on startup, a missing key produces a clear error before
the TUI opens, rather than the app starting with no working provider
(`main.rs` returns an `io::Error` from `lookup_mistral_api_key`'s
`None` case before any terminal setup happens).

### Local-first default

A new install (`cargo run`, no flags) defaults to local Ollama with
`mistral-nemo` — enforced explicitly by `Cli`'s `default_value_t =
Provider::Ollama`, not an accident of what's built so far, now that
Mistral is also a real, selectable choice.

### Provider transparency

The active provider is shown in the chat block's title for the entire
session (`emed-code — AI: local (ollama)` or `... cloud (mistral)`),
not just in the startup-only credential log line, which scrolls out of
view once the TUI's alternate screen takes over. A user can't lose
track of whether a cloud provider is in use.

### `SandboxPath` (Phase 3)

`src/core/sandbox_path.rs`: `SandboxPath::new(root, requested)` is the
only way to construct one, and it's the only type the file tools
accept — a raw `PathBuf`/`&str` can't be passed to a tool function.

Containment is checked against `std::fs::canonicalize`d paths (both
`root` and the requested path), not lexical-only `.`/`..`
normalization — verified via a real symlink test: a symlink placed
*inside* the sandbox root pointing *outside* it is correctly rejected,
which a string-only check would have missed. This matters because tool
output (file contents) feeds back into the LLM conversation, making an
escaped read a real prompt-injection-adjacent exfiltration path, not a
theoretical one.

Rejection errors (`SandboxError::Escapes`/`NotFound`) are fixed,
hand-written strings that never include the actual resolved path, so a
rejected symlink's real target isn't itself disclosed via the error.

Fully wired into a real, live code path, verified against the real
API: `Core`'s agent loop calls `tools::dispatch`, which calls
`read_file`/`list_files`, which construct a `SandboxPath` before
touching the filesystem at all. `MistralClient::send` advertises
`tools::tool_definitions()` in every real request and parses
`tool_calls` from the real response — `cargo run -- --provider
mistral` can genuinely read files for you. A local-gated end-to-end
test against the real Mistral API (not just a fake test client)
confirms a task requiring multiple tool calls (list a directory, then
read a file in it) completes correctly.

### `SandboxPath::new_for_write` (Phase 4 Step 1, 2026-07-31)

A second constructor, for `write_file`'s eventual use: validates a
target that may not exist yet (creating a new file), requiring only
that its *parent* directory already exists and resolves inside the
sandbox. If the target itself already exists (the overwrite case), it
is still fully canonicalized and re-checked, so a symlink sitting at
that name pointing outside the sandbox is caught — the same guarantee
reads already get. No `mkdir -p`: a missing parent directory is
rejected, not auto-created. See `ARCHITECTURE.md` for the full
two-stage validation logic.

Directory auto-creation is deliberately out of scope here — flagged as
its own future tool with its own security posture (sketched: gated by
`file_access_security`, where `strict` never auto-creates, a future
`medium` level would auto-create with the same confirmation gate
`write_file` itself uses, and `loose` would auto-create instantly
within the sandbox). Not built, noted so it isn't rediscovered from
scratch later.

**Not yet reachable from any tool** — real and unit-tested (including
the symlink-escape and overwrite-symlink-escape cases), but unused
outside its own tests until `write_file` is built on top of it later
in Phase 4.

### 40-tool-call cap

The 40-tool-call cap and per-batch rejection are implemented and
tested — a scripted client that never stops requesting tool calls is
proven to terminate with a clear error after exactly 40 individual
calls, not hang or loop unboundedly.

### Tool-call transparency in the running TUI (refined 2026-07-30, after manual testing)

Every tool invocation (name, arguments, success/failure) renders in the
chat log with its own `"tool: "` prefix, distinct from `"emed-code: "`
(assistant replies) and `"error: "` — extends the same transparency
reasoning behind the provider indicator above to tool activity
specifically: a user can see exactly which call ran and whether it
succeeded, not just that something happened.

On success, only `"ok"` is shown — not the actual result content
(`Message::ToolResult` still carries the full content to the model
regardless, per its own turn in the conversation; only what's
*displayed* changes). A read file's contents are still sent to the LLM
provider either way — this doesn't change that (see the
content-sensitivity-filtering section below for the actual exposure
surface) — but it does mean file contents aren't *also* echoed into
the user's own terminal scrollback/tmux pane, which is a real, if
secondary, reduction in accidental-exposure surface (screen-sharing,
terminal history, etc.), discovered as a usability rough edge during
manual testing (dumping whole files into the log made using the
feature genuinely unpleasant, not just a privacy nicety).

### Settings system: `~/.config/emed-code/settings.toml` (2026-07-31)

`src/core/settings.rs`'s `Settings::load()` reads one setting,
`file_access_security` (`strict`/`loose`, defaulting to `strict`). A
missing file, an empty file, malformed TOML, or an unrecognized value
all fail safe to the same `strict` default — never a panic, never a
fail-open state.

Structurally tamper-resistant: this file cannot be easily tampered with
by anything emed-code's own tools can reach. It lives outside the
sandboxed project root that `SandboxPath` bounds, resolved via the OS's
real config-directory convention (`dirs::config_dir()`), not a
project-relative path — so even with Phase 4's `write_file` tool, there
is no path a prompt-injection-driven "edit your own settings to loosen
file access" attempt could construct that `SandboxPath` would accept,
short of the model already having arbitrary filesystem access outside
emed-code entirely. Same reasoning as resolving Mistral's API key via
`getfrompass` rather than a project-local file: anything that gates
what the model can do or see must live somewhere the model's own tool
access structurally cannot reach, not just somewhere it conventionally
shouldn't.

### Content-sensitivity filtering for `read_file`/`list_files` (Step 8b, 2026-07-31)

`file_access_security` is now consulted on every tool call. In
`strict` mode (the default), `read_file`/`list_files` refuse a small,
explicitly non-exhaustive starter set of sensitive-by-convention paths:
`.env`/`.env.*`, the `.ssh` directory (matched anywhere in the path,
not just at the root), `.git/config` specifically (not all of `.git`),
and `*.pem`/`*.key`. `loose` mode skips this check entirely — its one
distinct behavior so far.

The check matches against the canonicalized, symlink-resolved path
(`SandboxPath::relative_path()`), not the raw requested string, so a
symlink with an innocuous name pointing at a blocked file can't bypass
it — covered by its own regression test.

A blocked path produces the new `ToolError::AccessDenied` (a static
message; unlike a rejected symlink, there's nothing to hide here, since
the LLM already knows exactly which path it asked for). `list_files`
still shows a blocked entry's *name* in its parent directory's listing
(existence isn't hidden), but refuses to enumerate into a blocked
directory or return anything inside one.

This closes the content-sensitivity-filtering gap tracked in this
file's backlog since Step 3 (2026-07-30).

### `write_file`'s tool logic (Phase 4 Step 4, 2026-07-31) — real, but not yet reachable

`tools.rs`'s `write_file` reuses the exact same two guards
`read_file`/`list_files` already have before it touches disk:
`SandboxPath::new_for_write` (containment, including a parent-directory
symlink escape or an overwrite-target symlink escape) and the same
content-sensitivity blocklist (only in `strict` mode). A rejected
write — whether from sandbox escape or the blocklist — never calls
`std::fs::write` at all; verified directly (a rejected write leaves no
file on disk, and an escape attempt leaves the real target's contents
unchanged).

**Not yet advertised to any provider or callable from a live agent
loop** — deliberately so. Phase 4's whole premise is that `write_file`
never exists without its confirmation gate (diff preview + y/n)
already in place; this step only builds and proves the write logic
itself. See `docs/write-file.md` for the remaining steps (the
confirmation gate, then the wiring that finally makes this reachable).

### The confirmation gate (Phase 4 Step 5a, 2026-07-31) — real, tested, still not reachable from a real provider

`write_file` calls are now routed through
`tools::write_file_with_confirmation`, which validates the path
(sandbox + content-sensitivity blocklist, identical to `write_file`
itself) *before* ever proposing a write — a forbidden path is rejected
immediately with no confirmation dialog, so there's nothing shown to
approve for a request that was never going to be allowed. Only a
validated write generates a diff and blocks for a real user answer via
a new `Core`↔TUI channel. Verified end-to-end (not just at the
function level) via a scripted `LlmClient`: the real agent loop
genuinely pauses — the target file is confirmed absent from disk right
after the proposal event arrives, before any confirmation is sent —
and only proceeds once `Core::respond_to_confirmation` is called.

A dropped/errored confirmation receive (e.g. the app exiting
mid-confirmation) fails safe to declining the write, never to a silent
apply — same fail-safe-not-fail-open posture as `Settings::load`.

Still not reachable from a real provider or a live agent loop driven by
an actual model — `write_file` isn't advertised via
`tool_definitions()` yet (Step 6). The TUI also can't yet actually
answer a confirmation prompt from a real key press — `App`'s
pending-confirmation input handling and the real diff/menu rendering
are Step 5b.

## Backlog (not yet implemented)

Nothing currently tracked here — the content-sensitivity-filtering gap
open since Step 3 (2026-07-30) was closed by Step 8b above
(2026-07-31). See `ARCHITECTURE.md` for the starter blocklist's exact
contents and why it's deliberately non-exhaustive rather than an
attempt at a complete list.

## Out of scope / not applicable

### emed-code's own credentials

No plaintext secrets files, `.env` parsing, or config-file credential
storage exist for emed-code's *own* runtime needs (i.e. how it
authenticates to Mistral), and none are planned — see parent
`CLAUDE.md`'s "No secrets from plaintext" rule. This is unrelated to —
and doesn't cover — what a file-reading tool might expose from a
*user's* project; see the settings/content-sensitivity-filtering
sections above for that.
