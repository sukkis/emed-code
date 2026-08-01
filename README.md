# emed-code

A fast, keyboard-driven AI coding assistant that lives in your
terminal — pair with a local [Ollama](https://ollama.com) model or
Mistral's cloud API without leaving your terminal or tmux session.

## Why emed-code

- **Your choice of brain.** Talk to a local Ollama model — free and
  private — or Mistral's cloud API for stronger answers, one flag
  apart. The active choice is always shown right in the chat title, so
  you're never surprised by a cloud call.
- **Remembers the conversation.** Follow-up questions just work — no
  need to repeat context turn after turn.
- **Understands your project.** It can read and list files to answer
  real questions about your code, whichever provider you're talking
  to — sandboxed to the directory you launched it from, every file
  access shown transparently in the chat log, and sensitive files
  (`.env`, `.ssh`, private keys) refused by default.
- **Never writes blind.** It can create or edit files too, but always
  shows a colored diff and waits for your explicit approval first —
  nothing is written to disk without you seeing it.
- **Built for the terminal.** A scrollable chat log and keyboard-only
  controls, equally at home in a tmux pane next to your editor or in a
  plain terminal window.

## Getting started

```
cargo run
```

talks to a local Ollama instance. Want Mistral instead?

```
cargo run -- --provider mistral --model codestral-latest
```

Mistral needs an API key — either a `getfrompass` entry
(`emed-code/mistral/api_key`) or the `MISTRAL_API_KEY` env var works.
Ask it things like "what files are in this project?", "read Cargo.toml
and tell me the package name", or "add a `.gitignore` entry for
`target/`" — reads and writes are both sandboxed to wherever you
launched it from, and any write shows you a diff to approve first. This
works the same way whether you're talking to a local Ollama model or
Mistral's cloud API.

Want to change which files it's willing to touch? Copy
`settings.toml.example` to `~/.config/emed-code/settings.toml` and edit
to your liking.

Run `cargo run -- --help` for the full flag reference. Type a message
and press Enter to send it; `Up`/`Down`/`PageUp`/`PageDown` scroll the
log; `Ctrl-C` or `Ctrl-Q` quits.

## Contributing

```
just test
```

runs everything, including tests against real external services: a
local Ollama instance (model: `mistral-nemo` — `ollama list` to check
it's pulled) and the real Mistral API (needs an API key, same as
above). `just ci` (or plain `cargo test`) skips those and runs only
what's self-contained. See `ARCHITECTURE.md` for how it's built and
why.

## Roadmap

- A directory-creation tool, with its own confirmation and security
  posture
- An append/insert tool distinct from `write_file`'s whole-file
  replace, so adding a line doesn't depend on the model correctly
  reconstructing the entire file's contents
- Token/context usage shown in the UI
- Independent read/write file-access strictness settings
- Syntax-highlighted diffs
- Shell command execution
