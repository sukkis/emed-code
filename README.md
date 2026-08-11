# emed-code

A fast, keyboard-driven AI coding assistant that lives in your
terminal — pair with a local [Ollama](https://ollama.com) model or
Mistral's or Anthropic's cloud APIs without leaving your terminal or
tmux session.

## Why emed-code

- **Your choice of brain.** Talk to a local Ollama model — free and
  private — or Mistral's or Anthropic's cloud APIs for stronger
  answers, one flag apart. The active choice is always shown right in
  the chat title, so you're never surprised by a cloud call.
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
- **Creates directories, not just files.** Ask it to put something
  somewhere that doesn't exist yet, and it creates the whole path
  first — parent directories included — through the same
  itemized-preview-and-approve flow as every other write.
- **Learns your project's conventions.** Drop an `AGENTS.md` in your
  project — and another at `~/.config/emed-code/AGENTS.md` for
  preferences you want everywhere — and both are folded into every
  request automatically. Whether either was found is always shown
  right in the chat title, same as the active provider.
- **Built for the terminal.** A scrollable chat log and keyboard-only
  controls, equally at home in a tmux pane next to your editor or in a
  plain terminal window.

## Getting started

```
cargo run
```

talks to a local Ollama instance. Want a cloud model instead?

```
cargo run -- --provider mistral --model mistral-medium-latest
cargo run -- --provider anthropic
```

Both cloud providers need their own API key — either a `getfrompass`
entry (`emed-code/mistral/api_key` or `emed-code/anthropic/api_key`) or
the matching env var (`MISTRAL_API_KEY`/`ANTHROPIC_API_KEY`) works.
Ask it things like "what files are in this project?", "read Cargo.toml
and tell me the package name", or "add a `.gitignore` entry for
`target/`" — reads and writes are both sandboxed to wherever you
launched it from, and any write shows you a diff to approve first. This
works the same way no matter which of the three you're talking to.

Want to change which files it's willing to touch? Copy
`settings.toml.example` to `~/.config/emed-code/settings.toml` and edit
to your liking. Want it to know your project's own conventions? Add an
`AGENTS.md` at the project root; add one at
`~/.config/emed-code/AGENTS.md` for preferences that should apply
everywhere.

Run `cargo run -- --help` for the full flag reference. Type a message
and press Enter to send it; `Up`/`Down`/`PageUp`/`PageDown` scroll the
log; `Ctrl-C` or `Ctrl-Q` quits.

## Contributing

```
just test
```

runs everything, including tests against real external services: a
local Ollama instance (model: `mistral-nemo` — `ollama list` to check
it's pulled) and the real Mistral and Anthropic APIs (each needs its
own API key, same as above). `just ci` (or plain `cargo test`) skips
those and runs only what's self-contained. See `ARCHITECTURE.md` for
how it's built and why.

## Roadmap

- Token/context usage shown in the UI
- Independent read/write file-access strictness settings
- Syntax-highlighted diffs
- Shell command execution

## License

Licensed under GPL-3.0-or-later.
