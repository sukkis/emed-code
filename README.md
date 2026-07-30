# emed-code

A minimal terminal AI coding assistant, aiming for a fast, keyboard-
driven, chat-based coding workflow, operable inside tmux. Work in
progress — see Roadmap below for current status.

## Features

- Chat with a local [Ollama](https://ollama.com) model or Mistral's
  cloud API — your choice, one flag apart
- Remembers the whole conversation for the session, so follow-up
  questions work
- Always shows which provider is active right in the chat title — no
  surprise cloud calls
- Scrollable chat log, built for living inside tmux
- Credentials via `getfrompass` or a plain env var, whichever you have

## Running it

```
cargo run
```

talks to a local Ollama instance. Want Mistral instead?

```
cargo run -- --provider mistral --model codestral-latest
```

Mistral needs an API key — either a `getfrompass` entry
(`emed-code/mistral/api_key`) or the `MISTRAL_API_KEY` env var works.

Run `cargo run -- --help` for the full flag reference.

Type a message and press Enter to send it. `Up`/`Down`/`PageUp`/
`PageDown` scroll the log; `Ctrl-C` or `Ctrl-Q` quits.

### Running the full local test suite

```
just test
```

runs everything, including tests that talk to real external services:
a local Ollama instance (model: `mistral-nemo`, see `ollama list` to
check it's pulled) — a request/response smoke test plus a short
mini-session (send a message, scroll, send a follow-up) — and the real
Mistral API, which needs an API key available either via `getfrompass`
(`emed-code/mistral/api_key`) or the `MISTRAL_API_KEY` env var. `just
ci` (or plain `cargo test`) skips those and runs only what doesn't
depend on anything outside the checkout.

## Roadmap

- **Phase 1 — Chat TUI + Ollama** (done): ratatui chat interface,
  scrollable log, input box, talking to a local Ollama model. No tools
  yet.
- **Phase 2 — Provider abstraction + Mistral** (done): `LlmClient`
  trait, `OllamaClient`/`MistralClient`, `clap`-based provider/model
  selection, `getfrompass`+env-var Mistral credentials, active-provider
  indicator in the chat title.
- **Phase 3 — Agent loop + read-only file tools (Mistral only)**:
  multi-turn tool-calling, sandboxed `read_file`/`list_files`. Ollama
  tool-calling is its own later phase, not part of this one.
- **Phase 4 — write_file + diff preview + confirmation**: `write_file`
  ships together with a diff shown before any write and y/n
  confirmation gating it — never before that gate exists.
