# ADR-0012: Generic, provenance-stable proposal events

- Status: Accepted
- Date: 2026-08-12
- Decision owners: Sotto maintainers

## Context

The original timeline schema described a sales-call watcher and suggestions. ADR-0011 replaced
that product framing with meeting-general, optional proposals. The timeline must record exactly
what Sotto displayed without allowing model output or a UI interaction to become captured meeting
fact. Proposal anchors and evidence also need contextual validation: valid field syntax alone
cannot prove that an id names an earlier event in the same session.

## Decision

1. The one-way event names are `proposal.trigger`, `proposal.partial`, `proposal.final`, and
   `proposal.disposition`. The old sales event names and payload types are removed.
2. Proposal kinds are clarifying question, decision check, next step, follow-up, and relevant
   context. A proposal carries non-empty meeting anchors, typed meeting evidence event ids, and
   opaque external evidence references. `core` neither parses nor depends on MCP receipts.
3. Proposal values have private fields, fallible constructors, and validating deserializers.
   External evidence references are bounded opaque identifiers, never source URIs, titles,
   credentials, or excerpts.
4. The timeline builder is the contextual authority. Proposal anchors and meeting evidence must
   name earlier captured facts in its session. Proposal-specific methods are the only proposal
   append path. Strict and lenient replay repeat the same contextual validation for persisted data.
5. Streaming partials may supersede partials. A final may supersede only an active partial. Kind,
   anchors, meeting evidence, and external evidence remain identical across that chain; text alone
   may grow. No proposal event may supersede captured evidence, and ordinary payloads cannot
   supersede proposal output.
6. Proposed state is represented by the proposal event itself. Dismissed, copied, and explicitly
   accepted are append-only dispositions against an active final proposal. Acceptance records a
   user interaction with Sotto output; it does not assert that meeting participants accepted the
   proposal.
7. Event classification is explicit: captured utterance/VAD/prosody/screen events are meeting
   facts; proposal events are system output; dispositions and annotations are user interaction;
   errors are diagnostics.
8. `proposal.run_audit` records the provider-neutral terminal run outcome separately from proposal
   content: a bounded connector/backend fingerprint, optional token usage, outcome, proposal id,
   and matching meeting anchors. Cancellation is an outcome, not mutable runtime state. The audit
   contains no provider SDK values, credentials, endpoint, prompt, or upstream error text.
   The referenced active event is the terminal target: completed is valid only for a final;
   cancelled or failed is valid only for a trigger or partial. Each event accepts exactly one
   terminal audit, and an audited partial cannot later be superseded. A later same-anchor proposal
   event is a distinct run.

## Compatibility

This is a pre-release, one-way wire cutover. Current fixtures and call sites move to the new names;
there is no legacy reader or dual serialization. A database containing experimental old proposal
rows is not compatible and must be reset. Captured meeting rows use unchanged event names and
payloads. No released Sotto build produced the removed proposal rows.

## Consequences

- Downstream watcher and UI work cannot create unanchored or cross-session proposal records.
- Replaying serialized data cannot bypass the checks used by live append.
- Checkpoint validation retains compact proposal phase/provenance metadata, not generated proposal
  text. Ordinary captured-event producers keep their existing infallible append API.
- Rendering and proposal usefulness evaluation remain downstream work.

## Revisit if

- proposals need to anchor a derived artifact rather than a captured fact;
- a future external-evidence plane needs a provider-neutral reference shape beyond opaque ids; or
- a released database requires an explicit one-time migration rather than the pre-release reset.
