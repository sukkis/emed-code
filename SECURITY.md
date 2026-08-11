# Security

Current security posture — what's protected, how, and what isn't
addressed yet. See `ARCHITECTURE.md` for the reasoning behind these
choices; this document tracks position, not rationale.

## Overview

emed-code hands an LLM the ability to read, and eventually write, files
in your project, plus your Mistral or Anthropic API key if you use one
of the cloud providers. The threats that follow from that:

- **Credential leakage** — a cloud provider's API key ending up
  somewhere it shouldn't (logs, error messages, a request going to the
  wrong place).
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
- **Persisted prompt injection** — a compromised or prompt-injected
  model writing instructions into a file it later reads back as
  trusted context in a *future* session, not just the current one.

Each is addressed below under its own heading.

## Credential handling

Both cloud providers' API keys follow the identical pattern: resolved
via `getfrompass` first (`emed-code/mistral/api_key` /
`emed-code/anthropic/api_key`), falling back to the matching
environment variable (`MISTRAL_API_KEY` / `ANTHROPIC_API_KEY`) only
when `getfrompass` has no value. Only the non-panicking
`try_get_from_pass` is ever called — never a write function. Each key
is held as `Zeroizing<String>` regardless of source, used only to build
the outgoing auth header (`Authorization: Bearer` for Mistral,
`x-api-key` for Anthropic — different header, identical protection),
and never appears in a `ChatError` or any log line — a startup message
reports *which source* supplied the key, never the value.

A missing key produces a clear error before the terminal UI even opens,
rather than the app starting with no working provider.

## Local-first default and provider transparency

A plain `cargo run`, with no flags, always defaults to local Ollama —
never a cloud provider — so talking to Mistral or Anthropic is always
an explicit choice. The active provider is shown in the chat title for
the entire session, not just in a startup line that scrolls out of view
once the terminal UI takes over, so it's never ambiguous whether a
cloud call is in play.

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
enumerate into it or read anything inside — recursive listing applies
this same rule at every level of the tree, not just the top, so a
restricted directory found partway down is still named but never
descended into.

`loose` mode disables this filtering entirely — an explicit,
user-chosen tradeoff, not a default. Recursive listing's separate
skip-list for build/VCS noise (`target`, `.git`, `node_modules`) is not
part of this filtering — it isn't a security control, so it stays in
effect even in `loose` mode.

## Unsupervised-write protection

Writing to disk always requires a human to see a diff and explicitly
approve it first — there's no code path where a write happens without
that. A proposed write is validated (sandbox + content-sensitivity
filtering, identical to a read) *before* anything is even shown for
approval, so a forbidden path never reaches the confirmation prompt at
all. If the confirmation channel is ever interrupted (e.g. the app
closing mid-prompt) the write is treated as declined, never applied.
The diff itself stays fully truthful to the bytes about to be
written — even a difference as small as a missing trailing newline is
shown explicitly rather than ever being normalized away, since the
approval is only meaningful if the preview genuinely matches what
happens on disk.

This applies identically across all three providers — the confirmation
gate is routed by tool name, not by which `LlmClient` is in use, so a
local Ollama model reaches the exact same approval step a cloud Mistral
or Anthropic model does.

## Directory creation

`create_directory` supports nested (`mkdir -p`-style) creation, which
needs its own traversal check: symlink containment can normally only be
verified by canonicalizing a path that already exists, and a
naively-recursive creation would have no such check available for a
level that doesn't exist yet. Two things close that gap instead: any
`.`/`..` path component is rejected upfront, lexically, before touching
the filesystem; and each level that *does* already exist is
canonicalized and re-checked for containment as the requested path is
walked one component at a time, so a symlink planted at any depth — not
just the final one — is still caught. A not-yet-existing level is only
safe to trust because both of those hold: no traversal component to
exploit, and every shallower level already validated.

Content-sensitivity filtering (see above) applies to every level a
nested request would create, not only the final one — a restricted name
partway down a path (e.g. `.ssh` as an intermediate directory) is
refused just as it would be at the leaf.

Directory creation goes through the same unsupervised-write protection
as file writes: a human sees an itemized preview of every directory
about to be created and must explicitly approve it before anything
touches disk. The one difference from a file write: if the requested
path (and every parent it needs) already exists, there's nothing to
create, so it succeeds immediately with no confirmation prompt — there's
no write to review.

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

## AGENTS.md content trust

Global `AGENTS.md` (`~/.config/emed-code/AGENTS.md`) has the same
tamper-resistance as `settings.toml` — outside the sandboxed directory,
unreachable by any file-writing tool. Project `AGENTS.md` sits inside
the sandbox and isn't protected the same way: a write to it is
possible, and would change what a *later* session's system prompt
contains. The same-session risk is bounded, since it's read once at
startup, before any tool call runs — but a successful write today
could still shape a future one. Mitigated the same way any other
project file's integrity is: git visibility and the existing
write-confirmation gate, not a technical restriction specific to this
file.

## Runaway tool-call protection

A single user message is capped at 40 total individual tool calls,
counted across the whole exchange rather than per round-trip (a
round-based cap could be sailed through by a model batching many calls
into one round). Exceeding it ends the exchange with a clear error
rather than looping unboundedly.

A separate, independently-bounded mechanism caps retries of an empty
model response at a fixed small number of attempts before ending the
exchange with a clear error. This doesn't compound with the tool-call
cap above — an empty response never produces a tool call, so the two
counters never add to each other's cost.

**Known gap**: the cap bounds total call *count*, not call *rate* or
*continued relevance*. Manual testing against the Anthropic provider
found a model continuing to call tools well past an explicit user
correction ("don't implement, just note it") — comfortably under the
40-call cap, but enough consecutive calls in a row to hit the
provider's own rate limit. The cap doesn't detect "this task should
have stopped," only "this task made too many calls total." Not
addressed yet.

## Dependency vulnerability scanning

CI runs `cargo audit` against `Cargo.lock` on every push or pull request
that touches `Cargo.toml`/`Cargo.lock`, checking every dependency
against the RustSec advisory database. A known vulnerability fails the
build; informational advisories don't.

## Out of scope

emed-code has no plaintext secrets files, `.env` parsing, or
config-file credential storage for its *own* runtime needs — none are
planned. This is separate from, and doesn't cover, what a file-reading
tool might expose from a *user's* project; see "Content-sensitivity
filtering" above for that.
