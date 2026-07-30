# emed-code

A minimal terminal AI coding assistant, aiming for Claude-Code-like
usability, operable inside tmux. Work in progress — see Roadmap below
for current status.

## Running it

Requires a local Ollama instance running with the `mistral-nemo` model
pulled (`ollama list` to check; `core.rs`'s `MODEL` constant is where
that's set — no provider/model selection yet, see Roadmap).

```
cargo run
```

Type a message and press Enter to send it. The reply appears in the
chat log above once Ollama responds (no streaming yet — Phase 1 sends
one request and waits for the complete reply). `Up`/`Down`/`PageUp`/
`PageDown` scroll the log; `Ctrl-C` or `Ctrl-Q` quits.

### Running the full local test suite

```
just test
```

runs everything, including tests that talk to a real local Ollama
instance (model: `mistral-nemo`, see `ollama list` to check it's
pulled) — a request/response smoke test plus a short mini-session
(send a message, scroll, send a follow-up). `just ci` (or plain `cargo
test`) skips those and runs only what doesn't depend on anything
outside the checkout.

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
