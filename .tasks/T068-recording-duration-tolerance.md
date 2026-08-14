# T068 — Stop failing finalization on capture startup overhead

**Status:** todo

**Wave:** M4 — recording

**Depends on:** nothing. T062 is `in-review` and holds `crates/app/src/workspace/**`; this task needs
only `crates/app/src/session/**`.

**Owns:** `crates/app/src/session/mod.rs` and this task

## The defect

Observed 2026-08-13 in Settings, on a normally stopped meeting:

    Recording finalization failed: Recording finalization failed: Recording duration
    67.177333333s disagreed with session elapsed time 69.476324333s beyond 2s.

`RECORDING_DURATION_TOLERANCE` is a fixed two seconds (`crates/app/src/session/mod.rs:38`) compared
against wall-clock `elapsed` at line 1201. But `session_started` is stamped **before** the capture
pipeline starts — before `start_gate`, before `capture.record_to`, before ScreenCaptureKit
negotiates a stream. Elapsed therefore always includes capture startup and shutdown drain, neither
of which produces media.

The 2.3-second gap here is not clock drift. It is the overhead the measurement was never supposed to
count, and it will exceed two seconds routinely — more so on a cold start, when a model is being
provisioned, or when the picker takes a moment.

The consequence is not cosmetic. Failing this check aborts finalization, so the recording never
becomes `Available`: it stays a growing reference carrying a failure string, is excluded from
`load_recording`, and cannot be re-transcribed or inspected. This is the mechanism behind every
"no retained screen recording" observation this week, and it is still happening after T062's repair.

The message is also emitted with its prefix twice — "Recording finalization failed: Recording
finalization failed:" — which means one layer is wrapping a string that already carries it.

## What to fix

1. Measure the right interval. Compare media duration against the wall-clock span **capture
   actually ran**, not the span the session object existed. If capture start and stop instants are
   not currently recorded, record them; that is the honest fix and it makes the tolerance mean
   something.
2. Then choose a tolerance for what remains — segment boundary rounding and finalization flush.
   T061's five-second segment interval is the natural floor to reason from. State the reasoning
   where the constant is defined so the next reader does not have to rediscover it.
3. Do not simply widen the constant until the failure stops. A number chosen to make an error go
   away silently stops catching the corruption this check exists to catch — the mixed-clock defect
   that produced a 223,555-second timeline against a 19.9-second session is exactly what it caught
   before.
4. Fix the doubled message prefix.
5. Decide what a genuine mismatch should do. Losing the entire recording reference over a duration
   disagreement is disproportionate when the media is present and playable. Prefer recording the
   discrepancy on the reference and keeping the recording usable, over discarding it — consistent
   with the ordering principle T062 established.

## Acceptance

- A normally stopped session whose capture startup exceeds two seconds finalizes to an available
  recording.
- The tolerance is documented at its definition with the reasoning for its value.
- A genuinely mismatched duration is still detected, asserted by a test that would fail if the check
  were removed or trivially widened.
- A detected mismatch leaves the recording referenced and usable, with the discrepancy stated.
- No failure message contains its own prefix twice.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The capture writer (T061), the transcription reader, retention policy, and the Settings presentation
of failed recordings (T069).
