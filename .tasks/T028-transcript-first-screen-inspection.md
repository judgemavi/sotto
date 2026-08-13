# T028 — Transcript-first reasoning with on-demand screen inspection

**Status:** done

**Wave:** R1 — may run beside connector work after T024

**Depends on:** T024; T015; T017

**Owns:** `crates/insight/src/context/**`, `crates/insight/src/summarizer.rs`,
`crates/insight/src/clustering/mod.rs`, `crates/insight/src/lib.rs`,
`crates/insight/Cargo.toml`, `crates/insight/tests/**`, `crates/screen/src/inspection/**`,
`crates/screen/src/lib.rs`, `crates/screen/src/tests.rs`, `crates/screen/tests/**`,
associated prompt-context files,
`docs/adr/0009-transcript-first-screen-context.md`, `AGENTS.md` after T024 hands it off

## Goal

Use timestamped transcript, speaker, prosody, and capture-target metadata as the default
reasoning context. Retain bounded local change frames for the board and retrospective
inspection, but perform OCR or send an image only when the reasoning flow requests
screen evidence for a specific moment.

## Plan

1. Supersede provisional ADR-0006 with ADR-0009. Keep local board thumbnails/change
   frames; remove eager OCR/images from reasoning prompts.
   Update the living `AGENTS.md` screen-context and phase language in the same handoff.
2. Stop running Vision OCR during frame ingestion. Continue the bounded, content-addressed
   retention needed to resolve a historical interval without recording raw video.
3. Add read-only `inspect_screen(timestamp | event_id)` resolution. Return requested
   timestamp, captured timestamp, visible interval, snapshot event id, frame ref, and
   precision/availability. Never label a sampled frame as an exact video frame.
4. Make recap, clustering, and later advising first-pass prompts transcript-first. A
   structured request for screen evidence triggers a second pass that may run local OCR
   and may attach an image only when the selected backend supports it and the user opted in.
5. Missing, pruned, or out-of-range frames return explicit evidence absence. They do not
   silently select the nearest unrelated frame.
6. Add T024's backend fingerprint to derived-view cache identity; model id alone cannot
   distinguish Codex CLI from direct OpenAI.
7. Preserve append-only timeline semantics. Inspection results are derived evidence, not
   retroactive timeline facts.

## Contract for downstream tasks

T029 verifies identical transcript-first behavior through both backends. T013 uses this
typed two-pass action rather than embedding screen/OCR data in every watcher prompt.

## Acceptance

- The initial reasoning request contains no OCR text or image bytes.
- A fake first pass requesting a timestamp receives the correct retained interval and
  provenance on the second pass.
- No inspection request means no Vision OCR and no image-provider work.
- Missing and pruned timestamps are explicit and provenance-safe.
- Images never leave the device without per-user opt-in.
- Raw video is never accumulated; frame retention stays bounded.
- Board thumbnails remain available independently of reasoning.

## Out of scope

Full-video recording, continuous OCR, automatic image sending, UI design, and realtime
advisor policy.

## Notes

- Screen ingestion now retains bounded, content-addressed change frames without invoking OCR;
  the legacy `ScreenSnapshot.ocr_text` field is empty. `screen::inspection` is the canonical typed
  contract for timestamp or event-id requests, half-open interval resolution, sampled-frame
  provenance, local OCR, user-owned image opt-in, and explicit missing/pruned/out-of-range states.
- Summary and clustering first requests contain timestamped final utterances plus capture-target
  metadata represented once at session level; they contain no `ScreenSnapshot` lines. A valid
  `inspect_screen` action permits one derived-evidence second pass; repeated inspection is
  rejected and local frame paths are not sent as text.
- Clustering requires ADR-0008's `BackendFingerprint` through an additive builder before cache
  access. Tests prove the same model through different backend fingerprints does not collide.
- ADR-0009 supersedes ADR-0006's eager metadata-plus-OCR assumption, and the living architecture
  now documents transcript-first context and explicit screen inspection.
- Verification passed: `cargo test -p screen --lib --no-fail-fast` (6 tests),
  `cargo test -p insight --no-fail-fast` (7 tests), and
  `cargo clippy -p screen -p insight --all-targets -- -D warnings`.
- `cargo check -p cli` remains blocked in `whisper-rs-sys` before CLI compilation by the generated
  `whisper_full_params` size assertion (`1_usize - 296_usize`); setting
  `WHISPER_DONT_GENERATE_BINDINGS=1` against the existing target output produces the same failure.

Actual backend image attachment and caller-constrained schema transport remain T031 work: the
current core completion primitive is text-only, so T028 authorizes/resolves an opted-in local
image but never transmits bytes. Runtime
construction of `RetainedScreenInspector` and resolved-backend fingerprint wiring also remain for
T029; the contracts and deterministic tests are complete without changing `core`.

## Review

Accepted on 2026-08-11. The reviewer reran the complete screen and insight test suites plus strict
Clippy. Transcript-only first-pass assertions, sampled-frame provenance, on-demand OCR, consent
gating, explicit absence states, and backend-aware cache isolation all passed. Image transport is
assigned to T031 and runtime wiring to T029 rather than being implied complete here.
