# ADR-0007: Persist model-derived views outside the timeline

- Status: Accepted
- Date: 2026-07-29
- Decision owners: Sotto maintainers

## Context

Topical regions, links, and open threads are model opinions over a session. They must be cheap to
reopen, recomputable with another model, and removable without changing the factual append-only
record. Storing them as timeline events would make an interpretation look like something that
happened and would break byte-stability across clustering runs.

## Decision

SQLite schema version 3 adds `derived_views`, separate from `events`. A row is keyed by session,
versioned artifact kind, model, and a stable hash of the exact rendered timeline content. It
stores the serialized artifact and reported usage. Inserts are idempotent; timeline rows are
never updated during reasoning.

## Consequences

Reopening an unchanged session with the same model costs no provider tokens. Changing relevant
timeline content, model, or artifact version creates a new recomputable projection. Old
projections may coexist and can later be pruned independently of the canonical log.

The current hash is a deterministic FNV-1a cache key, not a security primitive. Artifact JSON is
validated against current `EventId`s on both provider output and cache load.

## Revisit if

- Derived artifacts need user edits or provenance beyond model, usage, and content identity.
- Cache-key collisions become a practical concern and require a cryptographic digest.
- A retention policy needs to select or prune multiple historical projections.
