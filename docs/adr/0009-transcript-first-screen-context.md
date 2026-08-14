# ADR-0009: Transcript-first reasoning and explicit screen inspection

- Status: Accepted
- Date: 2026-08-11
- Decision owners: Sotto maintainers

## Context

ADR-0006 provisionally placed capture metadata and eager local OCR beside every retained
screen snapshot in offline prompts. Its evidence came from one short synthetic fixture: OCR
added relevant slide text but did not change the recap's objections, competitor mention, or
next steps, while the image arm could not run through the text-only completion contract.

That provisional default spends local compute and prompt tokens on every sampled frame, widens
the sensitive context sent to a backend, and makes screen evidence look necessary even when the
timestamped conversation is sufficient. Sotto retains low-rate change frames for its local board,
so reasoning can instead ask for a specific moment only when visual evidence is material.

## Decision

Initial recap, clustering, and later advisor requests contain only:

- session-level capture-target metadata;
- final transcript text with event id, speaker, start/end timestamp, and local prosody.

They contain no `screen.snapshot` rows, OCR text, frame paths, or image bytes. Frame ingestion
performs change detection, content-addressed PNG retention, interval construction, and bounded
pruning, but does not run Vision OCR. The legacy timeline OCR field remains empty until a future
schema migration can remove it without breaking append-only consumers.

A first reasoning pass may return one typed action:

```json
{
  "action": "inspect_screen",
  "timestamp_seconds": 12.3,
  "evidence": "metadata|local_ocr|image",
  "reason": "why this moment is necessary"
}
```

`event_id` may replace `timestamp_seconds`, but exactly one selector is required. The resolver is
read-only and returns either explicit absence or provenance containing the requested coordinate,
sampled capture timestamp, visible interval, snapshot event id, local frame reference, and
`sampled_change_frame` precision. Timestamp intervals are half-open; a gap, out-of-range request,
unknown/non-screen event, pruned frame, or frame outside the session cache is reported explicitly.
The resolver never substitutes a nearby unrelated frame and never calls a sample an exact video
frame.

Local OCR runs only for a resolved `local_ocr` request. An `image` request requires user-owned
opt-in before even a local frame reference is released toward the reasoning integration. The
current provider-neutral completion primitive is text-only, so it does not attach image bytes;
backend capability metadata alone is not authorization. A second reasoning pass receives the
inspection provenance and OCR or explicit absence, and cannot request another inspection.

Inspection results are derived evidence and never append to or rewrite the session timeline.
Board thumbnails continue to read retained local `FrameRef`s independently of whether reasoning
is configured.

Derived-view caches use ADR-0008's `BackendFingerprint`, not model id alone. Insight requires an
explicit resolved fingerprint before reading or writing a clustering artifact, so Codex CLI and
direct OpenAI cannot share results merely because they expose the same model name.

## Consequences

- Most calls pay no OCR cost and disclose no screen content to a reasoning backend.
- Missing or pruned visual evidence remains visible as absence instead of being silently guessed.
- Retained frames remain bounded local artifacts rather than full-video recording.
- Reasoning prompts and timeline storage no longer treat OCR as an eager fact about the call.
- Actual multimodal attachment still needs a connector-level typed image seam and T029 coverage;
  this ADR defines the authorization and inspection contract without changing `core`.
- The old CLI screen-context options can remain source-compatible temporarily, but eager OCR is
  rejected rather than reviving ADR-0006 behavior.

## Supersedes

This decision supersedes ADR-0006's provisional metadata-plus-OCR prompt default. ADR-0006 remains
the historical record of the synthetic comparison that motivated testing screen value, not the
current implementation direction.

