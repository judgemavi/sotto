# T066 — Let the reasoning layer pull a screenshot when it needs one

**Status:** in-review

**Wave:** M4 — recording

**Depends on:** T059 (`done`) for the decoder, T062 (`in-review`) for the durable recording
reference.

**Owns:** the reasoning runtime assembly in `crates/app/src/notes/**` and
`crates/app/src/reasoning/**`, `crates/app/src/workspace/transcript.rs`, and this task

## Supersedes the original T066

This task was first written to render the decoded frame inline in the app instead of opening
Preview. The maintainer redirected it on 2026-08-13, and the redirection is right: the product need
is not a nicer image viewer. It is that **the model receives the transcript, decides for itself
whether a moment needs visual context, and pulls that frame** — which is exactly the
`inspect_screen` contract ADR-0009 already defines and T059 already serves.

## The actual gap

Both halves exist and are not connected.

- `RecordingBackedScreenInspector` implements `screen::ScreenInspectionSource` over the session
  recording, with honest requested/actual timestamps and explicit missing states.
- `insight`'s summarizer and notes paths accept an inspector through
  `SummarizerBuilder::with_screen_inspector` and route it into
  `complete_with_optional_inspection`.

But `RecordingBackedScreenInspector` appears **only inside `crates/insight/src/context/mod.rs`'s
test module**. Nothing in `crates/app/src` ever calls `with_screen_inspector`. In the shipped
product the inspector is always `None`, so a reasoning pass cannot request a frame no matter what
the transcript contains. The capability is complete on both sides of a seam nobody joined.

## Plan

1. Assemble the recording-backed inspector into the app's reasoning runtime and pass it wherever a
   summarizer or notes run is constructed for a session that has a retained recording.
2. Resolve the recording per session at request time. A pruned, deleted or still-growing recording
   must yield the explicit unavailable state the contract defines — the reasoning run continues
   transcript-only rather than failing.
3. Keep the disclosure boundary exactly where it is. Local decode and local OCR are not consent to
   transport an image; an image still reaches a backend only under the existing separate opt-in.
   Assert this over the serialized request, not by inspection of the call site.
4. Make the inspection visible in the result. When a notes or Ask run consulted a frame, the user
   must be able to see that it did, at which moment, and with what decode precision. A model that
   silently looked at the screen is a worse product than one that did not look at all.
5. Prove no inspection happens on the default path. A run whose transcript needs no visual context
   must decode nothing and OCR nothing — the existing test asserting this must still hold once the
   inspector is actually present rather than `None`.

## On the existing `Show screen` button

The manual gesture stays for now, but it writes a decoded PNG to `std::env::temp_dir()` and opens
Preview. That frame outlives the session, is not counted by the retention budget, and is not removed
when the user deletes the recording — it escapes every contract ADR-0018 makes about recordings.

Either remove the button, or stop writing the frame to a location the product does not control.
Recommend one and do it; do not leave the leak in place because it predates this task.

## Acceptance

- A notes or Ask run over a session with a retained recording can request and receive a frame, and
  a test drives the whole path through the app's own runtime assembly rather than a constructed
  test double.
- A session whose recording is missing, pruned or deleted still completes transcript-only, with the
  unavailable state surfaced rather than swallowed.
- No image reaches a reasoning backend without the existing separate opt-in, asserted over the
  serialized request.
- A run that does not need visual context performs no decode and no OCR.
- When a frame was consulted, the user can see that it was, and at which moment.
- No decoded frame is written outside the product's retention contract.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The decoder and its provenance contract (T059, closed), the capture writer (T061), OCR engine
changes, video playback, and the image transport opt-in itself.

## What was built

**`crates/app/src/reasoning/inspection.rs` (new) — the joint.**

- `ScreenInspectorAssembly::assemble(session_id) -> (Arc<dyn ScreenInspectionSource>,
  ScreenConsultationLog)`. The product implementation is `RecordingScreenInspectors` over
  `StoredRecordings` (the meeting SQLite file), `PlatformFrameDecoder`, and local Apple Vision OCR.
- The recording is resolved **per request**, inside `inspect`, not captured at assembly time. A
  recording pruned or deleted between "generate notes" and the model's question is still honest.
  `Store::load_recording` deliberately excludes a still-growing recording, so a live capture
  resolves to `None` and becomes `ScreenUnavailableReason::RecordingMissing`; settled `Missing`
  rows map through the existing contract to `recording_deleted` / `recording_pruned`. In every
  case `insight` renders `availability=unavailable`, the second pass proceeds, and the run
  completes transcript-only.
- The image policy is hard-wired to `ImageInspectionPolicy::Deny`. An `image` request is refused
  *before* any decode. Phase 5 still owns the opt-in; nothing here anticipates it.
- Every request — served or refused — is appended to a run-scoped `ScreenConsultationLog` with the
  requested moment, evidence kind, the model's stated reason, and the outcome (requested and
  decoded media time, decode precision, OCR character count) or the unavailable token.

**`crates/app/src/notes/controller.rs` — the runtime that uses it.**

- `NotesController` owns an inspector assembly (`with_screen_inspectors` swaps it for tests).
  `start_generation` assembles one inspector per run and `run_generation` passes it to
  `MeetingNotesGenerator::with_screen_inspector`. This is the first and only production caller.
- Consultations land on the controller when the run is polled — including a failed run, because
  the model still looked — and are exposed by `NotesController::screen_consultations()` and on
  `NotesSnapshot::screen_consultations`. They are cleared when the selected meeting changes and
  when a new run starts, so a disclosure never follows the user to another meeting.

**`crates/app/src/workspace/transcript.rs` — visibility, and the leak.**

- The per-row `Show screen` button is **removed**, with `show_screen_at`, `show_screen_at_impl`,
  `recording_inspection_message`, `NoTranscriptOcr` and `open_local_frame`. See below.
- Selecting a transcript row now appends `screen_consultation_disclosure(...)` to the workspace
  message: which moment reasoning looked at, at what decoded media time, with what precision, and
  that no image was sent to a backend. A run that consulted nothing says nothing.

## Decision: the `Show screen` button is removed

Recommended and done. Reasons, in order:

1. It wrote a decoded PNG into `std::env::temp_dir()` and handed it to Preview. That frame outlived
   the session, was invisible to the retention budget, and survived recording deletion — it escaped
   every contract ADR-0018 makes. That alone required action.
2. The only way to keep the gesture *and* the contract is an in-app image viewer with its own
   bounded, deletable frame store. That is precisely the original T066 the maintainer redirected
   away from on 2026-08-13: "the product need is not a nicer image viewer."
3. ADR-0016: screen evidence is on demand, reached through cited evidence details; the default
   review surface does not reserve space for frames. A button on every transcript row is the
   noisy default surface the workspace design rejects.
4. The product need it was standing in for — someone looking at the screen when a moment needs it —
   is now served by the reasoning layer pulling the frame itself, under a disclosure the user sees.

The manual growing-recording probe (`capture::macos::probe_committed_recording`) went with it. If a
manual gesture returns, it belongs behind a retained, deletable frame store, not `temp_dir()`.

## Verification

Run and green:

- `cargo test -p app --lib` — 120 passed. New: four in `reasoning::inspection::tests`, four in
  `notes::controller::tests::screen`, one in `workspace::transcript::tests`.
- `cargo test --workspace` — all suites pass.
  `providers::codex::tests::timed_out_probe_kills_and_reaps_its_process_group` failed once on a
  loaded parallel run and passes in isolation on re-run; it is a pre-existing timing flake in a
  crate this task does not touch.
- `cargo clippy --workspace --all-targets` — clean under the workspace's deny set.
- `cargo fmt --all -- --check` — clean.
- `git diff --check` over the owned paths — clean; the new files are untracked, so they were also
  scanned directly for trailing whitespace and hard tabs.

The four end-to-end tests drive `NotesController::start_generation` through a real
`providers::Registry` resolution, so the assembly under test is the app's own, not a double:

- `a_notes_run_pulls_one_frame_through_the_apps_own_runtime_assembly` — model asks for
  `local_ocr` at an event id, exactly one decode and one OCR pass, the second turn carries
  `decoded_media_time=8.025s`, and the disclosed consultation reports the moment and
  `DecodedVideoFrame`.
- `a_pruned_recording_still_completes_the_run_from_the_transcript_alone` — for both a pruned row
  and no settled row: zero decodes, `availability=unavailable` in the request, notes still Ready.
- `an_image_request_is_refused_locally_and_no_image_reaches_the_backend` — `image_opt_in_required`
  before any decode.
- `a_run_needing_no_visual_context_decodes_nothing_with_the_inspector_present` — one turn, zero
  decodes, zero OCR, zero consultations, with the assembly counter proving an inspector really was
  wired rather than `None`.

The disclosure boundary is asserted over the serialized request, not the call site: every turn is
captured as `serde_json::to_string(&request.completion)` plus whether the transport received an
`AuthorizedReasoningImage` at all, and each of the four tests asserts no image was attached and the
serialized text contains no PNG magic, no `data:image`, no base64 payload, and no recording path.

## NOT RUN

- **Any live reasoning backend.** There is no configured Codex or OpenAI credential on this
  machine, so no real model has ever chosen to issue `inspect_screen` over a real meeting. Whether
  the prompt actually elicits an inspection at a useful moment is unmeasured.
- **Real video decode.** `PlatformFrameDecoder` calls the AVFoundation bridge; every test uses an
  injected decoder. The bridge itself is covered by T059, but the app's assembly has never decoded
  a frame out of a real `.mp4`.
- **Apple Vision OCR through this path.** `LocalOcr` is exercised only by a counting fake.
- **The workspace rendering of the disclosure was verified only through `select_annotation_anchor`
  and a unit test of the formatter**, not by driving a GPUI window and reading the message.

## Open gaps, deliberately not closed here

- **Ask cannot consult a frame at all.** `insight::AskEngine` has no `with_screen_inspector` seam —
  the acceptance item says "notes or Ask", and only notes has a seam to join. Giving Ask one is an
  `insight` change and belongs to whoever owns that crate.
- **The notes column does not render the consultation.** `NotesController` and `NotesSnapshot`
  carry it, and the transcript column discloses it on row selection, but
  `crates/app/src/workspace/notes.rs` matches `NotesState::Ready { .. }` exhaustively and is not
  owned by this task, so the consultation cannot be added to the notes receipt block here. A
  follow-up should render `snapshot.screen_consultations` beside the model/cache receipt.
- **A cached reopen loses the disclosure.** Consultations live for the run, not the artifact.
  Reopening an unchanged meeting serves cached notes with no provider call and no consultation
  list, even if the original run did look at the screen. Persisting it means extending the derived
  view in `insight`/`rag`.

## Ownership deviation

`crates/app/Cargo.toml` gained `screen = { path = "../screen" }` (and `Cargo.lock` followed).
`app` could not previously name `ScreenInspectionSource` or `ImageInspectionPolicy`:
`providers::inspection` re-exports only `RecordingBackedScreenInspector` and
`RecordingFrameUnavailableReason`, which is not enough to construct an inspector or implement the
trait. The alternative was widening the re-export in `crates/providers`, which this task does not
own. No other file outside the owned set was touched.
