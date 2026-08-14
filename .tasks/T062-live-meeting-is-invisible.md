# T062 — The live meeting is invisible to the app's own surfaces

**Status:** in-review

**Wave:** M4 — recording

**Depends on:** nothing. T058 and T059 closed on 2026-08-13, releasing `crates/app/src/session/**`
and `crates/app/src/workspace/transcript.rs`.

**Concurrency (planner, 2026-08-13):** T063 runs alongside and holds `crates/asr/**` and
`crates/cli/src/pipeline.rs`. Do not edit them. `crates/capture/**` remains T061's.

**Owns:** `crates/app/src/workspace/library.rs`, `crates/app/src/workspace/transcript.rs`,
`crates/app/src/workspace/mod.rs`, `crates/app/src/settings/mod.rs`, `crates/app/src/session/**`,
the recording-reference persistence path in `crates/rag/**`, and this task

**Ownership extended (planner, 2026-08-13):** the task stopped correctly at `workspace/mod.rs` and
`settings/mod.rs`, which it needs for the rail refresh and the re-transcription gesture. Both are
free — T060 closed `workspace/mod.rs` and T057 closed `settings/mod.rs` — so both are granted.
Nothing else changes: `crates/asr/**` and `crates/cli/src/pipeline.rs` were T063's and are now
released; `crates/capture/**` remains T061's.

**Probe ownership amended (maintainer, 2026-08-13):** T062 owns the narrowly traced fix inside
`sotto_recording_probe` plus the growing-file committed-duration query and its app bridge. The
writer, segment interval, and all other capture behavior remain T061's.

The first slice is accepted on review. `save_growing_recording` at `crates/app/src/session/mod.rs:890`
now runs before `capture.record_to`, so the reference exists before anything that can fail;
`preserve_failed_finalization` retains it with a stated reason rather than orphaning the file; and
`assert_timeline_within_session` was corrected to compare against `probe.duration` instead of
wall-clock elapsed — the right repair, since the check was measuring the wrong clock rather than
being wrong to exist. Items 3, 4 and 4a remain.

## Why this exists

Observed by the maintainer on 2026-08-13 during the first real lagged-transcription run. The
transcript itself was good — near-verbatim against the meeting platform's own transcript, which is
the ADR-0018 payoff. Three surfaces around it were not.

### 0. The recording reference is written last, behind seven fallible steps, and its loss is silent

This is the serious one, confirmed on 2026-08-13 against a **stopped** meeting: the session ended
normally, the transcript locked, the meeting appears in the rail — and there is still no recording.

`save_recording` sits at `crates/app/src/session/mod.rs:1074`, after all of:

    wait_for_recording_finalization → probe_recording → transcribe_complete
    → append_final_utterances → ingress.send → load_session
    → assert_timeline_within_session → save_recording

Every one of those carries `?`. Any single failure returns from the worker and the recording
reference is never written. The MP4 remains on disk with nothing pointing at it: absent from the
library, uncounted by the retention budget, never pruned, not deletable by the user, and
permanently unreachable from `Show screen`. A privacy-sensitive file the product promised to make
visible and deletable becomes an orphan that only the filesystem knows about.

`assert_timeline_within_session` (line 1123) is the prime suspect. It rejects any event whose
coordinate exceeds **wall-clock** `elapsed`, and T058 has just changed utterance timestamps to
**media** time. That invariant was written for the previous clock and may now fire on correct data.
Confirm from the run's stderr before fixing, and fix the invariant on its merits — do not delete a
check that has caught real clock corruption before.

The ordering is wrong regardless of which step failed. The reference must exist before anything
that can fail, so that a partial or failed finalization degrades to a recording with incomplete
metadata rather than to no recording at all.

### 1. `Show screen` cannot work while a meeting is live

`show_screen_at_impl` resolves the recording through `store.load_recording(session_id)`
(`crates/app/src/workspace/transcript.rs:527`). That row is written by `save_recording` at
`crates/app/src/session/mod.rs:1074` — **after** the session stops, after `probe_recording`, after
the elapsed-time reconciliation. So during a live session no row exists and every row's button
reports "This meeting has no retained screen recording."

The button is rendered on every transcript row *while the meeting runs*, which is precisely when it
cannot succeed. A control that is always visible and never works is worse than one that is absent.

This is not a decode problem. T058 already proved an `AVAssetReader` can reopen the MP4 and consume
committed media while the writer is active, so the frame is there — only the durable reference to
it is missing.

### 2. The rail only lists a meeting after the app is restarted

Confirmed by the maintainer: the meeting appeared neither during the session nor after stopping it,
and showed up only after relaunching the app. `Store::list_sessions`
(`crates/rag/src/store.rs:390`) has no `ended_at` predicate, so this is a stale snapshot in the
workspace, not a query defect. Refresh the rail when a session starts and when it ends. A meeting
the user cannot select until they restart the app is, for that whole window, lost work.

### 3. Re-transcription is implemented and unreachable

`RecordingLibrary::retranscribe` (`crates/app/src/session/recordings.rs:98`) has **zero callers in
the workspace**. T058 built it correctly and could not surface it, because the planner's ownership
boundary put `crates/app/src/workspace/**` out of its reach. That is a planner error, not an
implementer one, and it is why this task holds both sides.

Re-transcription is the whole payoff of retention: today's misrecognitions become re-derivable with
a larger model. Shipping it as dead code forfeits that.

## Plan

1. Persist the session's recording reference **when capture starts**, with the path known and the
   recording explicitly marked as still growing. Keep the existing stop-time write as the
   transition to a finalized reference carrying duration, byte size and the verified identity time
   mapping. Do not let a growing reference claim a duration it has not yet reached. This fixes
   defect 0 and defect 1 together: the reference exists before any step that can fail, and it
   exists while the meeting is live.
1a. Make a failed finalization **loud**. Today a recording can be lost with no user-visible
   explanation while the transcript locks normally, which reads as success. Surface the real cause,
   and leave the recording referenced rather than orphaned.
2. Make `Show screen` work against a growing recording for any moment already committed. If the
   requested moment is inside the uncommitted tail, say so specifically — that moment has not been
   written yet — rather than reusing the deleted/pruned/missing copy, which would be untrue.
3. List the live meeting in the rail as soon as it starts, marked as in progress.
4. Give re-transcription a reachable gesture on a completed meeting, and report its result. Minimal
   is acceptable; unreachable is not.
4a. Stop rendering silence as transcript content. The observed run showed `[ Silence ]` and
   `[ Pause ]` rows attributed to "You" — the microphone channel is silence-filled by T061's stereo
   layout whenever the user is not speaking, so in a listening-only meeting these rows accumulate
   with no information in them. Suppress them, or fold them into the row they qualify. Do not
   change the prosody annotator itself; this is a presentation decision.
5. Re-check the retention contract still holds: a growing reference must be counted, pruned and
   deleted by the same rules as a finalized one, and an abandoned growing reference from a crashed
   session must not linger as a permanent phantom row.

## Acceptance

- `Show screen` returns a frame for a committed moment during a live meeting, and gives a distinct,
  truthful message for a moment in the uncommitted tail.
- The rail lists a meeting from the moment it starts.
- A completed meeting can be re-transcribed from the UI, and the resulting transcript replaces the
  derived one without mutating the original captured events.
- A session whose finalization fails at any step still leaves a referenced recording and a stated
  reason. Assert this by injecting a failure into a step before `save_recording` and proving the
  recording is still listed, still counted against the budget, and still deletable.
- A session killed mid-capture leaves no recording row claiming bytes or duration it does not have.
- Per the verification rule, all four are exercised against a real signed capture run, not simulated
  at the type level.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The transcription reader itself (T058), the frame decoder and OCR (T059), the capture writer (T061),
and any change to the retention budget policy.

## Resolved question

The maintainer confirmed the meeting **had** been stopped, and that it appeared in the rail only
after restarting the app. That answer promoted the recording loss from "live-only limitation" to
defect 0 above: a normally-completed session can lose its recording entirely and report nothing.

## Implementation status — 2026-08-13

Implemented within this task's owned files:

- capture now persists a `growing` recording reference before starting the file writer; it carries
  no invented duration, byte size, or time mapping, and finalization atomically settles it to the
  existing `available` shape;
- finalization failures retain that reference, persist the actual reason, end the session record,
  and surface a recording failure instead of silently returning with an orphaned MP4;
- growing references use their current filesystem size for retention accounting and follow the
  same pruning/deletion path as finalized recordings;
- live `Show screen` reads the live timeline, probes the committed media prefix, and distinguishes
  an uncommitted-tail request from deleted, pruned, or missing media;
- transcript projection suppresses final and partial `[ Silence ]` / `[ Pause ]` marker rows;
- the recording library exposes growing/failed references and rejects re-transcription until a
  recording has finalized.

Still blocked by the declared ownership boundary: the mounted rail refresh and re-transcription
gesture are both wired in `crates/app/src/workspace/mod.rs` (and the existing recording settings UI
is in `crates/app/src/settings/mod.rs`), neither of which this task owns. The required integration is
to refresh the notes catalogue and rebuild the search index in the newly-active branch of
`MeetingWorkspace::refresh_after_session`, and to call `RecordingLibrary::retranscribe` from a
completed-meeting action using the selected managed model path, then reload that transcript and
report the returned utterance count.

Automated evidence: focused RAG recording tests, app session tests, transcript projection tests,
app library compilation, strict Clippy for app/RAG, formatting, and diff checks pass. Signed real
capture acceptance remains **NOT RUN**.

## Defect found in the first slice — 2026-08-13

Live `Show screen` now resolves the reference — the message carries the real path, so item 1's
persistence fix works — but fails with:

    The recording has not committed that moment yet: capture stream failed:
    recording is not playable: …/recordings/1786652195282693000.mp4

Two separate problems.

### The wrong probe is being used for a growing file

`show_screen_at_impl` calls `capture::macos::probe_recording` (`transcript.rs:549`) to find the
committed duration. That probe was built by T061 for a **finalized or crash-truncated** file: it
decodes the first video frame and seeks to one second before the reported end, and returns
`StreamFailed("recording is not playable")` (`crates/capture/src/macos.rs:670`) whenever the native
probe returns false.

The file is almost certainly fine. Transcription was streaming from that same MP4 at the moment of
the failure — T058's reader reopens it through `sotto_asr_read_stereo` and reads committed audio
without trouble. So a growing recording is readable; it just does not satisfy a probe designed to
validate a completed one.

**Narrow ownership grant (planner):** T062 may add a growing-recording probe to
`crates/capture/**` — a committed-duration query that does not require a finalized, fully seekable
asset — and its native counterpart in the Swift bridge. This is the minimum needed and nothing more.
`crates/capture/**` otherwise remains T061's; T061 has no active implementer, and its outstanding
work is the manual acceptance now held by T064. The later maintainer trace supersedes the original
restriction on finalize-time probe behavior: its invalid sync-sample timestamp check is T062's to
repair. Do not change the writer or segment interval.

### The message states the opposite of what happened

"The recording has not committed that moment yet" tells the user to wait. The wrapped cause says the
file is not playable — a hard failure. Whichever is true, the outer sentence asserts the other one,
and the user cannot tell which. This is the same defect class as the "the disk is full" message that
collapsed five distinct causes during T057: an outer copy that misrepresents the cause it wraps.

A not-yet-committed moment and an unreadable recording are different situations with different user
actions. Say which one occurred. Do not wrap a failure in a reassurance.

## Confirmed: the writer is sound, the probe is the bug — 2026-08-13

The maintainer stopped the session and no `Recording finalization failed:` line appeared; the last
output was `ggml_metal_free: deallocating`, the transcriber being dropped at session end. So the
finalization path completed, which means `probe_recording` **succeeded on the same file at stop that
it had called "not playable" while growing**.

That settles it. T061's stereo delegate-segment writer produces a valid recording; the failure is
entirely that a finalize-time probe was pointed at a live file. Fix the probe, not the writer.

### Silence is not a success signal

A successful finalization prints nothing. There is no way for the user — or for a reviewer reading a
terminal — to distinguish "the recording settled correctly" from "the recording was silently lost",
which is the exact ambiguity that consumed two review rounds on 2026-08-13. Item 1a is extended:
make the settled outcome observable too, not only the failure. A one-line statement of the retained
recording's duration and size at stop is enough, and it doubles as the evidence T064 needs.

## Probe root cause repaired — 2026-08-13

The maintainer confirmed the retained MP4 plays in QuickTime and traced the rejection to
`sotto_recording_probe`: an `AVAssetReader` whose `timeRange` starts near the media end may decode
from the preceding sync sample. Sparse screen content makes an earlier returned PTS normal, but the
probe incorrectly required that PTS to be at or after the requested range start.

The finalized probe now accepts any finite, nonnegative decoded PTS while retaining its first-frame
and near-end decode checks. Live `Show screen` no longer calls that finalized playability probe: a
separate `sotto_recording_committed_duration` query reads only the growing asset's committed
duration and current byte size. Failure to read that metadata is reported as a query failure;
only a requested timestamp beyond the returned duration is described as an uncommitted tail.

Automated evidence: the Swift bridge regression accepts a preceding-sync-sample timestamp and
rejects negative/non-finite timestamps; all 3 Swift bridge tests, all 8 capture unit tests, all 8
focused app transcript tests, strict Clippy for capture/app all targets, formatting, and the scoped
diff check pass. The real signed growing/finalized `Show screen` cases remain part of T064 and are
**NOT RUN** in this implementation pass.

## Workspace reachability completed — 2026-08-13

The two previously blocked workspace seams are now wired:

- every observed live-session transition refreshes the persisted meeting catalogue and search
  index before showing the live transcript, so the rail can list the in-progress session without an
  app restart; a later `Running` notification retries the refresh if the first identity notification
  raced the durable `save_session` write;
- completed-meeting chrome exposes a single `Re-transcribe` action, provisions the configured
  managed Whisper model off the UI thread, calls `RecordingLibrary::retranscribe`, reloads the
  resulting derived projection, rebuilds search, and reports the returned row count or exact error;
- reopening a meeting now prefers its latest stored derived transcript while retaining captured
  event anchors for citations and screen navigation. The append-only captured timeline is not
  changed;
- growing and failed recording references are visible and deletable in Settings, including the
  recorded finalization failure, instead of being counted by retention but absent from its UI.

Focused app evidence: `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p app --lib --locked` passed
all 110 tests, including the new derived-projection immutability/anchor regression. Strict app
Clippy over all targets passed with warnings denied, and the scoped diff check passed.

The real signed capture cases are still **NOT RUN** here and remain assigned to T064: live
committed `Show screen`, truthful uncommitted-tail copy, live rail visibility, UI re-transcription,
and an injected finalization failure verified through the signed product surface. They are not part
of this final code handoff.

## Final presentation and observability handoff — 2026-08-13

- Transcript projection suppresses final, live partial, completed trailing partial, and retained-
  media-derived `[ Silence ]` / `[ Pause ]` marker rows without changing prosody production.
- A successfully finalized recording now prints its measured duration, byte size, and retained path
  after budget enforcement. It does not call a recording "retained" if that same enforcement pass
  pruned it.
- Focused transcript tests pass 9/9; the successful-finalization message has a direct regression.
  T064 continues to own the real signed capture observations listed above.

`crates/app/src/workspace/transcript.rs` is released to T066.
