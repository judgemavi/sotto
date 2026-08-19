# ADR-0023: Providers are added once; Notes and Ask pick among them

- Status: Accepted
- Date: 2026-08-18
- Decision owners: Sotto maintainers

## Context

ADR-0008 modeled reasoning as three independent roles — Watcher, Suggester, and
Summarizer — each able to pin a different backend. That matched an earlier plan where
live proposals and post-call notes were separate product jobs with different quality
and latency bars. The shipped product has two user-facing LLM surfaces, Notes and Ask,
and no live proposal engine. Assigning a Watcher or Suggester in Settings was leftover
machinery: it asked people to configure jobs that do not exist, and it implied that
Notes and Ask needed different *kinds* of provider rather than a provider they already
added.

Zed and similar tools treat a provider as something you add (API key, or later an ACP
agent). Individual surfaces then pick from what is ready. Sotto should do the same.
ACP is not implemented; the v1 product connectors remain the OpenAI Responses API and
the experimental Codex CLI from ADR-0008 and ADR-0014.

The empty `crates/advisor` placeholder is removed from the workspace until T013 is
unparked. Historical Board/Scene spike UI is not part of the product shell.

## Decision

Sotto has a list of providers, not a list of LLM jobs.

- **Settings adds or removes providers.** OpenAI is ready when a key is stored.
  Codex is ready after the ADR-0014 experimental acknowledgement and a successful
  login probe. Settings does not assign Watcher, Suggester, or Summarizer.
- **Notes and Ask each pick one ready provider**, or none. The two surfaces may
  differ. Convenience actions may set both at once ("Use OpenAI for all",
  "Use Codex for all", "Use no reasoning") without restoring per-job roles.
- **No ready provider, or none picked for that surface, disables that LLM
  feature.** Capture, transcript, and review stay available. Summarize and Ask
  do not call a backend in that state.
- **Codex may be picked for Ask as well as Notes**, once it is experimentally
  enabled. ADR-0014's restriction of Codex to the notes Summarizer is lifted for
  these two surfaces. Live proposals remain unshipped; this decision does not
  authorize a Watcher or Suggester path.
- Registry selection is keyed by `ReasoningSurface { Notes, Ask }`. Persisted
  v1 `roles.summarizer` migrates onto both surfaces; Watcher and Suggester
  selections are discarded.
- ACP is a future transport for adding a provider, not a second product concept
  beside API keys. It is out of scope until a dedicated contract exists.
- The sales-shaped `insight::Summarizer` / recap types remain for the headless
  CLI and historical evals. The app continues to use `MeetingNotesGenerator`.
  Collapsing those two stacks is follow-up, not required to ship this selection
  model. Historical HTTP adapters in `providers` (Anthropic, Google, OpenRouter,
  Ollama chat-completions) stay crate-internal test/eval transports; they are
  not product modules and are not selectable in Settings.

## Consequences

- A user with no configured provider never sees a working Summarize or Ask
  control. Adding a provider in Settings is not enough: Notes and Ask must
  pick it, unless they used a "for all" convenience action.
- Independent role selection in ADR-0008 is superseded for the product UI.
  Connector identity, cache fingerprints, Codex isolation, and "no reasoning
  is ordinary" are unchanged.
- ADR-0014 still requires explicit Codex consent and still discloses T030.
  Consent no longer implies a Summarizer-only assignment.
- Unparking T013 must reintroduce an advisor crate and a new surface (or an
  ADR) rather than silently reviving Watcher/Suggester settings.

## Revisit if

- ACP or another local-agent transport is added as a first-class way to add a
  provider.
- Live proposals ship and need a distinct surface with a different backend
  than Notes or Ask.
- Eval evidence justifies collapsing the CLI recap summarizer into
  `MeetingNotesGenerator`.

## Supersedes

This ADR amends ADR-0008's independent Watcher/Suggester/Summarizer selection
and ADR-0014's notes-Summarizer-only Codex assignment. Connector set, Codex
hardening, and OpenAI-as-supported-BYOK-path are unchanged.
