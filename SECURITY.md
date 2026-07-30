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
  the error. **Not yet wired into a real tool** — `read_file`/
  `list_files` (Phase 3's next step) are what will actually call this.

## Backlog (not yet implemented)

- **Wire `SandboxPath` into real tools.** The validation logic above
  exists and is tested, but nothing calls it yet outside tests — Phase
  3's next step (`read_file`/`list_files`) is where it becomes
  reachable from a real tool call.
- **No content-sensitivity filtering on file tools (flagged
  2026-07-30).** `SandboxPath` only enforces *where* a path may resolve
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
  read, unconditionally. Proposed direction (not designed, not
  scheduled): a guardrail-strictness setting (e.g. `file_access:
  strict` blocking known-sensitive filename patterns) — deliberately
  deferred until there's a settings system to hang it on, rather than
  hardcoding a blocklist now with no way to configure it.

## Out of scope / not applicable

- **emed-code's own credentials**: no plaintext secrets files, `.env`
  parsing, or config-file credential storage exist for emed-code's
  *own* runtime needs (i.e. how it authenticates to Mistral), and none
  are planned — see parent `CLAUDE.md`'s "No secrets from plaintext"
  rule. This is unrelated to — and doesn't cover — what a file-reading
  tool might expose from a *user's* project; see the content-
  sensitivity-filtering backlog item above for that.
