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
  the attempted value. **Reachable via the library API and
  `tests/real_mistral_smoke_test.rs`** (`Core::with_client` +
  `MistralClient::new`), but not yet selectable from the `cargo run`
  binary itself — that still needs the `--provider`/`--model` CLI
  wiring described in `README.md`'s Phase 2 roadmap entry.

## Backlog (not yet implemented)

- **Provider transparency.** Whichever provider is active (local Ollama
  vs. Mistral, once added) must always be visible to the user in the
  TUI, not just configurable at startup. Not yet implemented — no
  provider selection exists yet for there to be transparent about; needs
  the `--provider` CLI flag to land first.
- **Local-first default.** A new install must default to local Ollama
  with a "friendly" default model (`mistral-nemo`), never to a remote
  provider. True today only because Ollama is the sole provider; once
  `--provider` CLI selection adds provider choice, this needs to be an
  explicit, preserved default rather than an accident of what's built so
  far.

## Out of scope / not applicable

- No plaintext secrets files, `.env` parsing, or config-file credential
  storage exist in this project, and none are planned — see parent
  `CLAUDE.md`'s "No secrets from plaintext" rule.
