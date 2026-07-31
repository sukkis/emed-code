# Security

Current security posture and open gaps. This tracks *status*, not
rationale — see `ARCHITECTURE.md` for the why behind a given decision.

## Current status (implemented)

- **Phase 1 (Ollama, local-only):** no credentials, API keys, or secrets
  of any kind are read, stored, or transmitted anywhere in the codebase.
  The only network calls are to a local Ollama instance
  (`http://localhost`). Nothing here to leak.
- **Mistral credential resolution + a real request path (Phase 2).**
  `resolve_mistral_api_key`/`lookup_mistral_api_key` in
  `src/core.rs`: `getfrompass` (key `emed-code/mistral/api_key`) first,
  `MISTRAL_API_KEY` env var fallback if `pass` yields no value, `pass`
  preferred whenever both are present. Only
  `getfrompass::try_get_from_pass` is called — never the panicking
  `get_from_pass` or any write function. The resolved key is held as
  `Zeroizing<String>` regardless of which source supplied it, and passed
  into `MistralClient::new`, which uses it only to build the outgoing
  `Authorization: Bearer <key>` header in `fetch_mistral_reply`. A
  startup log line (`credential_log_message`) reports which source
  supplied the key, never the value, and deliberately names `getfrompass`
  rather than `pass` — see `ARCHITECTURE.md`. **Verified no key leakage
  into errors**: the key is never passed into any `ChatError` variant;
  even a malformed key that fails header-value construction surfaces
  only a static `"failed to parse header value"` message (confirmed
  against the `http` crate's `InvalidHeaderValue` `Display` impl), not
  the attempted value. **Now selectable from `cargo run`** via
  `--provider mistral` (see `README.md`); on startup, a missing key
  produces a clear error before the TUI opens, rather than the app
  starting with no working provider (`main.rs` returns an `io::Error`
  from `lookup_mistral_api_key`'s `None` case before any terminal setup
  happens).
- **Local-first default.** A new install (`cargo run`, no flags) defaults
  to local Ollama with `mistral-nemo` — enforced explicitly by
  `Cli`'s `default_value_t = Provider::Ollama`, not an accident of what's
  built so far, now that Mistral is also a real, selectable choice.
- **Provider transparency.** The active provider is shown in the chat
  block's title for the entire session (`emed-code — AI: local (ollama)`
  or `... cloud (mistral)`), not just in the startup-only credential log
  line, which scrolls out of view once the TUI's alternate screen takes
  over. A user can't lose track of whether a cloud provider is in use.
- **`SandboxPath` (Phase 3).** `src/core/sandbox_path.rs`:
  `SandboxPath::new(root, requested)` is the only way to construct one,
  and it's the only type Phase 3's file tools will accept — a raw
  `PathBuf`/`&str` can't be passed to a tool function once those land.
  Containment is checked against `std::fs::canonicalize`d paths (both
  `root` and the requested path), not lexical-only `.`/`..` normalization
  — verified via a real symlink test: a symlink placed *inside* the
  sandbox root pointing *outside* it is correctly rejected, which a
  string-only check would have missed. This matters because tool output
  (file contents) feeds back into the LLM conversation, making an
  escaped read a real prompt-injection-adjacent exfiltration path, not a
  theoretical one. Rejection errors (`SandboxError::Escapes`/`NotFound`)
  are fixed, hand-written strings that never include the actual resolved
  path, so a rejected symlink's real target isn't itself disclosed via
  the error. **Fully wired into a real, live code path, verified against
  the real API**: `Core`'s agent loop calls `tools::dispatch`, which
  calls `read_file`/`list_files`, which construct a `SandboxPath` before
  touching the filesystem at all. `MistralClient::send` now advertises
  `tools::tool_definitions()` in every real request and parses
  `tool_calls` from the real response — `cargo run -- --provider
  mistral` can genuinely read files for you as of this step. A
  local-gated end-to-end test against the real Mistral API (not just a
  fake test client) confirms a task requiring multiple tool calls
  (list a directory, then read a file in it) completes correctly.
- **The 40-tool-call cap and per-batch rejection are implemented and
  tested** — a scripted client that never stops requesting tool calls
  is proven to terminate with a clear error after exactly 40 individual
  calls, not hang or loop unboundedly.
- **Tool-call transparency in the running TUI, without echoing file
  contents into it (refined 2026-07-30, after manual testing).** Every
  tool invocation (name, arguments, success/failure) renders in the chat
  log with its own `"tool: "` prefix, distinct from `"emed-code: "`
  (assistant replies) and `"error: "` — extends the same transparency
  reasoning behind the provider indicator above to tool activity
  specifically: a user can see exactly which call ran and whether it
  succeeded, not just that something happened. On success, only `"ok"`
  is shown — not the actual result content (`Message::ToolResult` still
  carries the full content to the model regardless, per its own turn in
  the conversation; only what's *displayed* changes). A read file's
  contents are still sent to the LLM provider either way — this doesn't
  change that (see the content-sensitivity-filtering backlog item below
  for the actual exposure surface) — but it does mean file contents
  aren't *also* echoed into the user's own terminal scrollback/tmux pane,
  which is a real, if secondary, reduction in accidental-exposure
  surface (screen-sharing, terminal history, etc.), discovered as a
  usability rough edge during manual testing (dumping whole files into
  the log made using the feature genuinely unpleasant, not just a
  privacy nicety).

- **Settings system: `~/.config/emed-code/settings.toml`, fail-safe,
  structurally tamper-resistant (2026-07-31).** `src/core/settings.rs`'s
  `Settings::load()` reads one setting, `file_access_security`
  (`strict`/`loose`, defaulting to `strict`). A missing file, an empty
  file, malformed TOML, or an unrecognized value all fail safe to the
  same `strict` default — never a panic, never a fail-open state. **This
  file cannot be easily tampered with by anything emed-code's own tools
  can reach**: it lives outside the sandboxed project root that
  `SandboxPath` bounds, resolved via the OS's real config-directory
  convention (`dirs::config_dir()`), not a project-relative path — so
  even once Phase 4 adds a `write_file` tool, there is no path a
  prompt-injection-driven "edit your own settings to loosen file
  access" attempt could construct that `SandboxPath` would accept,
  short of the model already having arbitrary filesystem access outside
  emed-code entirely. Same reasoning as resolving Mistral's API key via
  `getfrompass` rather than a project-local file: anything that gates
  what the model can do or see must live somewhere the model's own tool
  access structurally cannot reach, not just somewhere it conventionally
  shouldn't. **Not yet consulted by any tool** — `Core` loads it, but
  `read_file`/`list_files` don't branch on it yet; see the backlog item
  below (tracked as Step 8b).

## Backlog (not yet implemented)

- **No content-sensitivity filtering on file tools (flagged
  2026-07-30; settings mechanism landed 2026-07-31, filtering itself
  still open).** `SandboxPath` only enforces *where* a path may resolve
  to — it says nothing about *which* files within that boundary are
  appropriate for an LLM-directed tool call to read. A `.env`, `.git/config`
  (can hold remote credentials), or similar sensitive-by-convention file
  sitting legitimately inside the sandboxed directory currently passes
  `read_file`'s containment check exactly like any other project file,
  and its plaintext would then be sent to the LLM provider on every
  subsequent turn, since `Core` sends the full conversation history with
  every request (see `ARCHITECTURE.md`'s "Conversation history"
  section). This is a
  distinct, tool-specific instance of the parent `CLAUDE.md`'s "No
  secrets from plaintext" rule — that rule constrains what *I* read on
  the user's behalf; this is about what `read_file` lets *the LLM*
  read, unconditionally. **The setting this will run on now exists**
  (see "Settings system" below) but isn't consulted by `read_file`/
  `list_files` yet — that wiring, plus the actual starter blocklist
  (`.env`/`.env.*`, `.ssh/`, `.git/config`, `*.pem`, `*.key`,
  explicitly non-exhaustive) and a new `ToolError::AccessDenied`
  variant, is tracked as Step 8b in `docs/agent-loop.md`.

## Out of scope / not applicable

- **emed-code's own credentials**: no plaintext secrets files, `.env`
  parsing, or config-file credential storage exist for emed-code's
  *own* runtime needs (i.e. how it authenticates to Mistral), and none
  are planned — see parent `CLAUDE.md`'s "No secrets from plaintext"
  rule. This is unrelated to — and doesn't cover — what a file-reading
  tool might expose from a *user's* project; see the content-
  sensitivity-filtering backlog item above for that.
