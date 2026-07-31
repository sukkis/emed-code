# Security

Current security posture — what's protected, how, and what isn't
addressed yet. See `ARCHITECTURE.md` for the reasoning behind these
choices; this document tracks position, not rationale.

## Overview

emed-code hands an LLM the ability to read, and eventually write, files
in your project, plus your Mistral API key if you use the cloud
provider. The threats that follow from that:

- **Credential leakage** — the Mistral API key ending up somewhere it
  shouldn't (logs, error messages, a request going to the wrong place).
- **Sandbox escape** — a tool call reading or writing outside the
  directory you launched emed-code from.
- **Sensitive-file exposure** — a tool reading (and thereby sending to
  a cloud provider) or overwriting a file like `.env` or an SSH key,
  whether the model asks for it directly or via a symlink.
- **Unsupervised writes** — any write happening without a human seeing
  a diff and approving it first.
- **Self-modifying security policy** — a compromised or
  prompt-injected model trying to loosen its own restrictions by
  editing the settings file that defines them.

Each is addressed below under its own heading.

## Credential handling

The Mistral API key is resolved via `getfrompass` first, falling back
to the `MISTRAL_API_KEY` environment variable only when `getfrompass`
has no value. Only the non-panicking `try_get_from_pass` is ever
called — never a write function. The key is held as `Zeroizing<String>`
regardless of source, used only to build the outgoing
`Authorization` header, and never appears in a `ChatError` or any log
line — a startup message reports *which source* supplied the key, never
the value.

A missing key produces a clear error before the terminal UI even opens,
rather than the app starting with no working provider.

## Local-first default and provider transparency

A plain `cargo run`, with no flags, always defaults to local Ollama —
never Mistral — so using a cloud provider is always an explicit choice.
The active provider is shown in the chat title for the entire session,
not just in a startup line that scrolls out of view once the terminal
UI takes over, so it's never ambiguous whether a cloud call is in play.

## Sandboxed file access

`SandboxPath` is the only way any tool touches the filesystem — reads,
listings, and writes are all validated against it before anything
happens on disk. Containment is checked against canonicalized
(symlink-resolved) paths, not lexical `.`/`..` normalization, so a
symlink placed inside the sandboxed directory but pointing outside it
is caught, both for existing targets (reads, overwrites) and for the
write-specific case of a not-yet-existing file's parent directory.
Rejection errors never include the actual resolved path, so a rejected
symlink's real target isn't itself disclosed by the refusal.

## Content-sensitivity filtering

Containment alone doesn't mean a file is appropriate to touch — a
`.env` sitting legitimately inside your project is still a secret. In
`strict` mode (the default), reads, listings, and writes all refuse a
small, deliberately non-exhaustive set of sensitive-by-convention
paths: `.env`/`.env.*`, the `.ssh` directory (anywhere in the path, not
just at the root), `.git/config` specifically, and `*.pem`/`*.key`.
This is checked against the canonicalized path, not the literal string
a tool call passed in, so a symlink with an innocuous name pointing at
a blocked file doesn't bypass it. A directory listing still shows a
blocked entry's *name* (existence isn't hidden) but refuses to
enumerate into it or read anything inside.

`loose` mode disables this filtering entirely — an explicit,
user-chosen tradeoff, not a default.

## Unsupervised-write protection

Writing to disk always requires a human to see a diff and explicitly
approve it first — there's no code path where a write happens without
that. A proposed write is validated (sandbox + content-sensitivity
filtering, identical to a read) *before* anything is even shown for
approval, so a forbidden path never reaches the confirmation prompt at
all. If the confirmation channel is ever interrupted (e.g. the app
closing mid-prompt) the write is treated as declined, never applied.

The write path exists in the codebase but isn't yet advertised to any
provider, so no model can currently trigger it — see `README.md`'s
Roadmap for when that's expected to change.

## Settings tamper-resistance

`~/.config/emed-code/settings.toml` (the file that controls
`strict`/`loose` filtering) lives outside the sandboxed directory
entirely, resolved via the OS's own config-directory convention rather
than a project-relative path. This means no file-writing tool this
project builds can ever reach or modify it, however it's invoked —
closing off a specific attack shape where a compromised or
prompt-injected model tries to loosen its own restrictions by editing
the policy that constrains it. The same reasoning is why the Mistral
API key goes through `getfrompass` rather than a project-local file:
anything that gates what the model can do must live somewhere its own
tool access structurally cannot reach.

A missing, empty, or malformed settings file always falls back to
`strict` — never a crash, never a silent loosening of policy.

## Runaway tool-call protection

A single user message is capped at 40 total individual tool calls,
counted across the whole exchange rather than per round-trip (a
round-based cap could be sailed through by a model batching many calls
into one round). Exceeding it ends the exchange with a clear error
rather than looping unboundedly.

## Out of scope

emed-code has no plaintext secrets files, `.env` parsing, or
config-file credential storage for its *own* runtime needs — none are
planned. This is separate from, and doesn't cover, what a file-reading
tool might expose from a *user's* project; see "Content-sensitivity
filtering" above for that.
