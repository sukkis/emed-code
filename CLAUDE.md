# emed — Learning-Focused AI Instructions

## Purpose
emed exists so the user learns Rust and terminal UI programming — not to
ship features quickly. The measure of success for any session is whether
the user understands every line that got written, not how much roadmap
got covered.

Inherit all general standards from the parent `CLAUDE.md` (simplicity
first, dependency discipline, git branching, TDD). The rules below are
additive and specific to how we work in *this* project.

## Documentation Structure
Four places hold project knowledge, each with a distinct job. Don't mix
their content — a fact belongs in exactly one of these, and everything
below points at the one that applies.

- **`README.md`** — user-facing. What this is, how to build/run it, how
  to configure it (API keys, provider selection), troubleshooting.
  Written for someone using the tool, not building it. Also holds the
  live roadmap (see "Suggested Rhythm" below).
- **`ARCHITECTURE.md`** — maintainer-facing design decisions and their
  rationale (the *why*): dependency choices, error-handling strategy,
  security-relevant design (e.g. how API keys are read and passed
  around). Updated as each increment's design is settled. This is the
  durable record — if a future session needs to know why something was
  built a certain way, it should be answerable from here without digging
  through history.
- **`SECURITY.md`** — living checklist of current security posture (what's
  implemented, tied to the step that delivered it) plus a backlog of
  known gaps not yet addressed. Tracks *status*, not rationale — see
  ARCHITECTURE.md for the why behind a given decision. Update the
  checklist and backlog as soon as something changes; this file should
  always describe the real current state, not an aspirational one.
- **`docs/<topic>.md`** — internal planning/spec docs, one per increment
  or roadmap item. **Gitignored — local-only scratch material between us,
  not part of the repo.** Holds: increment scope, what's explicitly out
  of scope and why (including "Future: X" notes preserving design
  reasoning for deferred work, so it isn't rediscovered from scratch
  later), dependency justification, and the step-by-step breakdown
  (test-first plan + review focus per step). Once an increment lands,
  whatever's durable moves into README/ARCHITECTURE/SECURITY — `docs/`
  itself is the trail that got us there, not the destination. It's fine
  for it to go stale once an increment closes.
- **Never point at `docs/<topic>.md` (or anything else under `docs/`)
  from code comments, doc comments, or README/ARCHITECTURE/SECURITY.**
  Those files aren't in the repo, so the reference is dangling for
  anyone else who checks it out. If a comment needs the *why*, either
  say it inline or put the durable version in ARCHITECTURE.md/SECURITY.md
  and reference that instead.

## Pace: Human Speed, Not Machine Speed
- Work in the smallest increment that is still a coherent step — one
  test, one function, one concept. Not a whole feature in one pass.
- A roadmap item may reasonably span several sessions. That is correct,
  not a failure to be efficient.
- **Hard rule:** after explaining a completed increment — or, once an
  increment is broken into steps, a completed step (see "Step Overviews
  and Step Size" below) — stop and wait for the user's go-ahead before
  starting the next one. Do not treat "I explained it" as license to
  keep going in the same turn. The parent `CLAUDE.md`'s requirement to
  stop after Phase 1 (failing test) is one instance of this; it applies
  at every increment/step boundary, not only there.
- **An increment isn't finished until README.md, ARCHITECTURE.md, and
  SECURITY.md (whichever apply) say the same thing the code now does.**
  This includes doc comments (e.g. a struct field comment that described
  the old behavior). If the increment changed an API, a design decision,
  introduced a known shortcut/gap, or touched anything security-relevant,
  that update happens in the same increment, not filed away as future
  cleanup — stale docs are exactly the kind of thing that makes a future
  session reconstruct the wrong *why*. `docs/<topic>.md` gets updated too
  while the increment is still open (mark resolved items, note deferred
  reasoning) but, being scratch, it doesn't need to be "finished" the way
  the other three do.

## Step Overviews and Step Size
- Within an increment, break the work into small steps, tracked in the
  increment's `docs/<topic>.md` planning doc (once an increment is more
  than one step — trivial increments don't need this ceremony). Each
  step's diff must be small enough that it can be read in full and
  actually understood — not just confirmed to pass tests.
- **At the start of each step**, before doing anything else — including
  before TDD Phase 1's failing test — give a short overview: which step
  this is, what it accomplishes, and how it fits the increment's plan.
  This is a checkpoint to orient before reading code, not a summary
  after the fact.
- Each step still goes through its own TDD Phase 1 (failing test, stop,
  wait for confirmation) → Phase 2 (minimal implementation, full suite
  green) per the parent `CLAUDE.md` — steps don't relax TDD, they just
  give it a smaller unit to apply to.
- Don't collapse multiple planned steps into a single diff to save time
  — reviewability is the point, not throughput. If a step turns out
  bigger than expected once underway, stop and propose splitting it
  rather than pushing through.

## Explain and Discuss
- Before introducing anything new to the codebase — a crate, a Rust
  pattern not yet used, a data structure — explain it briefly and why
  it's the right tool, before writing the code.
- After writing code, walk through what it does, especially anything
  Rust-specific that isn't obvious at a glance: ownership, borrowing,
  lifetimes, trait bounds. These are the actual point of the project.
- When there's a real design decision (e.g. rope vs. gap buffer, how
  ownership of the buffer should work, error-handling strategy), raise
  it as a discussion with options — don't silently pick one and move on.
  This can take a whole session with little or no code landing, if
  that's what understanding it requires.

## What Not to Do
- Don't skip explanation because the code is short or "self-explanatory."
  Readable code still hides *why*, and *why* is what's being learned.

## Suggested Rhythm
1. Pick the next roadmap item (see `README.md`) and agree on the
   smallest useful slice of it as an increment. If it's more than one
   step, sketch the step breakdown in `docs/<topic>.md`.
2. For the next step: give the step overview (see "Step Overviews and
   Step Size"), then write the failing test only. Stop. Explain what it
   checks and why.
3. Wait for the user to run it and confirm the failure.
4. Implement minimally. Stop. Walk through the implementation.
5. Run the full suite, confirm green. Update `README.md`,
   `ARCHITECTURE.md`, and/or `SECURITY.md` for whatever this step made
   stale or newly true; update `docs/<topic>.md`'s status if it's
   tracking this step.
6. Offer a one-line commit message for the step's diff, unprompted —
   don't wait to be asked. Never run `git commit` (or `git add`)
   yourself; committing is always the user's action, on their own
   schedule, even right after offering the message.
7. Stop. Discuss what's next — the next step, or what's deferred and
   why — before starting another one.
