# Security

Current security posture and open gaps. This tracks *status*, not
rationale — see `ARCHITECTURE.md` for the why behind a given decision.

## Current status (implemented)

- **Phase 1 (Ollama, local-only):** no credentials, API keys, or secrets
  of any kind are read, stored, or transmitted anywhere in the codebase.
  The only network calls are to a local Ollama instance
  (`http://localhost`). Nothing here to leak.
- **Mistral credential resolution logic (Phase 2 step 3).**
  `resolve_mistral_api_key`/`lookup_mistral_api_key` in `src/core.rs`:
  `getfrompass` (key `emed-code/mistral/api_key`) first, `MISTRAL_API_KEY`
  env var fallback if `pass` yields no value, `pass` preferred whenever
  both are present. Only `getfrompass::try_get_from_pass` is called —
  never the panicking `get_from_pass` or any write function. The
  resolved key is held as `Zeroizing<String>` regardless of which source
  supplied it. A startup log line (`credential_log_message`) reports
  which source supplied the key, never the value, and deliberately names
  `getfrompass` rather than `pass` — see `ARCHITECTURE.md`. **Not yet
  wired into a live request path** — nothing calls
  `lookup_mistral_api_key` outside tests until Step 4's `MistralClient`
  exists, so none of this is reachable from the running app yet.

## Backlog (not yet implemented)

- **Wire the Mistral credential lookup into a real request (Phase 2
  step 4).** `MistralClient` needs to actually call
  `lookup_mistral_api_key` and use the result (or surface a clear error
  if neither source has a key) — the resolution logic above exists and
  is tested, but isn't reachable yet.
- **Provider transparency.** Whichever provider is active (local Ollama
  vs. Mistral, once added) must always be visible to the user in the
  TUI, not just configurable at startup. Not yet implemented — no
  provider selection exists yet for there to be transparent about
  (Phase 2 step 6, once step 5's `--provider` flag lands).
- **Local-first default.** A new install must default to local Ollama
  with a "friendly" default model (`mistral-nemo`), never to a remote
  provider. True today only because Ollama is the sole provider; once
  Phase 2 step 5 adds provider choice, this needs to be an explicit,
  preserved default rather than an accident of what's built so far.
- **No secret material in logs/transcript/errors.** Once Mistral auth
  errors start flowing through `Core`'s error path (Phase 2 step 4), they
  must not surface the key (or any part of it) in the chat log, error
  strings, or anywhere else user- or log-visible. `ChatError::Auth`
  currently only ever carries Mistral's own error message text (e.g.
  `"Unauthorized"`), never the key — worth re-checking once step 4 wires
  real auth failures through, since that's the point this stops being
  hypothetical.

## Out of scope / not applicable

- No plaintext secrets files, `.env` parsing, or config-file credential
  storage exist in this project, and none are planned — see parent
  `CLAUDE.md`'s "No secrets from plaintext" rule.
