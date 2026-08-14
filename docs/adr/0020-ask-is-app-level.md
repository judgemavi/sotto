# ADR-0020: Ask is an app-level surface over a library that is searchable by default

- Status: Accepted
- Date: 2026-08-14
- Decision owners: Sotto maintainers
- Amends: ADR-0016 (workspace shape), ADR-0017 (Ask surface)

## Context

ADR-0017 scoped Ask to "exactly one selected session" and left an all-sessions scope to a later
task. ADR-0016 placed its dock on the right, collapsed by default, subordinate to the record.

Shipped, that produced a control that is dead more often than it is alive. Ask required a **stopped
recording to be selected**, so it was unavailable on Home, unavailable while a recording ran, and —
because `start_ask` returned early without that selection — unavailable even with the panel's own
"Use all recordings" toggle switched on. The one scope the toggle promised could not be reached
from the state a person is in when they want it.

Cross-recording search needed a second, separate opt-in on top of that. Each recording carried a
`session_search_policy` row, default excluded, flipped by an Include/Exclude control **inside the
Ask panel**. A person who wanted to ask across their library had to visit every recording and opt
each one in, through a control that read as a property of the note they were looking at.

Two consequences, both observed:

- Ask read as a feature *of the open note* rather than of the app. It was scoped by, gated on, and
  configured from whichever recording happened to be selected.
- "Every retained recording" almost always meant *none of them*, because nothing was retained until
  someone opted it in one at a time. The scope existed and was empty.

## Decision

**Ask answers from the whole library by default, narrows to one recording only because the person
chose to, and every completed recording is in the library index.**

- **The library is the default scope.** With nothing open, Ask is live and answers across every
  recording. Choosing "this recording" is a narrowing the person performs, not a precondition they
  must satisfy.
- **Both scopes are always visible, and the current one is selected.** A single control that
  renames itself ("Use all recordings") never states which scope is *active* — it states the one
  you would switch to. Two peer controls state it.
- **A running recording is a valid scope.** Asking about the call you are in is the most natural
  question there is. A live scope reads from the in-memory timeline, since the persisted log trails
  what is on screen and nothing is indexed yet, and the panel says **"This recording, so far"** so
  an answer is never mistaken for one drawn from a finished call.
- **Every completed recording is indexed. There is no per-recording search opt-in.** Indexing
  happens at stop, is idempotent by content hash, and is backfilled for libraries that predate this.
  `session_search_policy` is dropped (schema v12). The presence of a recording's `prior_meeting`
  document is the only state; nothing can disagree with it.
- ADR-0017's answer shape is unchanged: explicit questions only, cancellable, every factual claim
  carries valid `EventId` citations, an unanswerable question gets an explicit refusal, and no
  reasoning backend means a disabled control that says why.

### Why removing the opt-in does not weaken the privacy posture

The differentiator is that **audio never leaves the device**, and it still does not. The index is
local, lives in the same SQLite file as the timeline it was derived from, is never sent anywhere,
and cascades away when the recording is deleted. The opt-in was not protecting a boundary — nothing
crossed one — it was gating a local index behind a per-recording chore. The meaningful choice a
person has is whether to keep a recording at all, and that choice is unchanged: deleting the
recording is the way out of search.

What does still cross a boundary is unchanged and still explicit: a reasoning request's contents
(ADR-0010), a screen inspection (ADR-0009), and an MCP query disclosure (ADR-0011). Ask's library
scope retrieves bounded local excerpts and puts them in a request the person initiated, the same
way single-recording Ask always has.

## Consequences

- Ask's dock is still collapsed by default and still only responds to explicit questions. What
  changed is what it is *about*, not how loudly it behaves. ADR-0016's "Ask stays subordinate to the
  record" survives: it makes no unsolicited output, and it is still a derived view.
- Stopping a recording now does embedding work. It is deliberately the last step of finalization
  and non-fatal: the timeline and media are durable before it runs, so a failed index degrades
  search rather than losing a call, and the next library-scoped question repairs it.
- A first library-wide question on an old library pays the backfill cost once, on the Ask worker
  thread with progress on screen. Paying it at launch for a person who never opens Ask would be
  the wrong trade.
- A recording that captured no speech is skipped rather than failed. Silence is a legitimate
  recording. Once it carries a typed note, that note is indexed on its own.
