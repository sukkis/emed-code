# emed-code

A minimal terminal AI coding assistant, aiming for Claude-Code-like
usability, operable inside tmux. Work in progress — see Roadmap below
for current status.

## Running it

```
cargo run -- [--provider ollama|mistral] [--model <name>]
```

Both flags are optional. With neither, it talks to a local Ollama
instance (requires Ollama running with the `mistral-nemo` model pulled
— `ollama list` to check) — local-first is the default regardless of
build order. `--model` overrides the provider's own default
(`mistral-nemo` for Ollama, `mistral-small-latest` for Mistral).

`--provider mistral` requires an API key, available via `getfrompass`
(key `emed-code/mistral/api_key`) or the `MISTRAL_API_KEY` env var —
`getfrompass` is checked first and preferred whenever both are present.
A startup line reports which source supplied the key (never the value
itself); if neither has one, the app exits with a clear error before
opening the TUI.

Type a message and press Enter to send it. The reply appears in the
chat log above once the provider responds (no streaming yet — one
request is sent and it waits for the complete reply). `Up`/`Down`/
`PageUp`/`PageDown` scroll the log; `Ctrl-C` or `Ctrl-Q` quits.

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
- **Phase 2 — Provider abstraction + Mistral**: `LlmClient` trait,
  `clap`-based provider/model selection, Mistral support.
- **Phase 3 — Agent loop + file tools**: tool-calling, sandboxed
  `read_file`/`write_file`.
- **Phase 4 — Diff preview + confirmation**: show a diff before any
  write, require confirmation.
