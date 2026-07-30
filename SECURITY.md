# Security

Current security posture and open gaps. This tracks *status*, not
rationale — see `ARCHITECTURE.md` for the why behind a given decision.

## Current status (implemented)

- **Phase 1 (Ollama, local-only):** no credentials, API keys, or secrets
  of any kind are read, stored, or transmitted anywhere in the codebase.
  The only network calls are to a local Ollama instance
  (`http://localhost`). Nothing here to leak.

## Backlog (not yet implemented)

- **Mistral API key retrieval (Phase 2 — provider abstraction).** Access
  the key via `getfrompass`, per the sanctioned Rust secrets-access
  convention (parent `CLAUDE.md`), with an environment-variable fallback
  for users without `pass`/`getfrompass` set up. `getfrompass` is the
  default and the nudged path; the env var is a documented escape hatch,
  not an equal alternative.
- **Provider transparency.** Whichever provider is active (local Ollama
  vs. Mistral, once added) must always be visible to the user in the
  TUI, not just configurable at startup. Not yet implemented — Phase 1
  has no provider selection to be transparent about.
- **Local-first default.** A new install must default to local Ollama
  with a "friendly" default model (`mistral-nemo`), never to a remote
  provider. True today only because Ollama is the sole provider; once
  Phase 2 adds provider choice, this needs to be an explicit, preserved
  default rather than an accident of what's built so far.
- **No secret material in logs/transcript/errors.** Once Mistral auth
  errors start flowing through `Core`'s error path, they must not
  surface the key (or any part of it) in the chat log, error strings, or
  anywhere else user- or log-visible. Not yet applicable — no such error
  path exists until Phase 2.

## Out of scope / not applicable

- No plaintext secrets files, `.env` parsing, or config-file credential
  storage exist in this project, and none are planned — see parent
  `CLAUDE.md`'s "No secrets from plaintext" rule.
