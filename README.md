# emed-code

A minimal terminal AI coding assistant, aiming for Claude-Code-like
usability, operable inside tmux. Work in progress — see Roadmap below
for current status.

## Running it

```
cargo run
```

Currently shows a static two-region screen (chat log / input box) and
quits on any keypress. The input box doesn't accept typing yet, and
nothing is wired to an LLM from the UI yet — that's upcoming roadmap
work (see below). `core`'s Ollama integration exists and is covered by
tests, just not yet connected to what you see on screen.

### Running the full local test suite

```
just test
```

runs everything, including a test that talks to a real local Ollama
instance (model: `mistral-nemo`, see `ollama list` to check it's
pulled). `just ci` (or plain `cargo test`) skips that and runs only
what doesn't depend on anything outside the checkout.

## Roadmap

- **Phase 1 — Chat TUI + Ollama** (in progress): ratatui chat
  interface, scrollable log, input box, talking to a local Ollama
  model. No tools yet.
- **Phase 2 — Provider abstraction + Mistral**: `LlmClient` trait,
  `clap`-based provider/model selection, Mistral support.
- **Phase 3 — Agent loop + file tools**: tool-calling, sandboxed
  `read_file`/`write_file`.
- **Phase 4 — Diff preview + confirmation**: show a diff before any
  write, require confirmation.
