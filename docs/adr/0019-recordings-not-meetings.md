# ADR-0019: Sotto records anything, and the summary follows the content

- Status: Accepted
- Date: 2026-08-13
- Decision owners: Sotto maintainers

## Context

Sotto captures any recordable application, transcribes it from the retained recording, and reasons
over the transcript. Nothing in that pipeline is specific to meetings.

The product vocabulary is specific to meetings anyway. The rail says **Meetings**, the button says
**Start scoped meeting**, transcript rows are attributed to **Meeting audio**, notes are **Meeting
notes** appended to the **live meeting record**, and the reasoning artifacts are `meeting_notes.v1`
and `meeting_notes.v2`.

This is inherited, not accidental. ADR-0011 set the destination as "an AI-enabled meeting copilot"
after an earlier sales-call framing, and ADR-0013 genericised the local knowledge taxonomy while
keeping the meeting frame above it. The product has been widened once already; this widens the rest.

The cost became concrete on 2026-08-13. A maintainer captured a Brooklyn Nine-Nine clip, generated
notes, and the model returned:

> Transcript contains entertainment dialogue from a Brooklyn Nine-Nine video; no meeting content is
> present.

The pipeline worked perfectly. The product asked the wrong question, so a correct answer was
useless. Nothing was wrong with the capture, the transcript, the connector, or the model.

**The schema is the real constraint, not the words.** `GroundedMeetingNotes` fixes seven sections —
overview, topics, decisions, action items, open questions, risks, follow-ups — and action items
carry owners and due dates. For a lecture, a podcast, a debugging session, or a sitcom, most of
those are empty and the rest are forced. Renaming the labels would leave that behaviour intact.

## Decision

**A session is a recording of something. What Sotto produces from it is a summary shaped by the
content, not a meeting report.**

- The product vocabulary becomes recording-centric. A captured session is a **recording** or a
  **session**; its transcript is a **transcript**; what reasoning produces is **notes** or a
  **summary**. "Meeting" survives only where the content genuinely is one.
- Transcript attribution names the source, not the occasion: **captured audio** and **your
  microphone**, rather than "Meeting audio" and "You".
- **The summary adapts.** Sections appear because the content supports them, not because the schema
  declares them. A planning call yields decisions and action items; a lecture yields topics and
  explanations; a debugging session yields findings. An empty section is omitted rather than
  rendered blank.
- **Citations remain mandatory and unchanged.** Every claim still cites the transcript events that
  support it, and unsupported claims still fail closed. Adaptivity applies to which sections exist,
  never to whether a claim is evidenced. This is the line the widening must not cross.
- Sotto accepts recordings it did not capture. Importing an audio or video file produces a session
  with the same transcript, notes and retention behaviour as a captured one.
- Recording only the microphone is a first-class way to start a session, not a degraded capture.

## Consequences

- The notes contract changes. `meeting_notes.v1` and `v2` are meeting-shaped and their artifact
  kinds, schema ids and prompts must be superseded rather than edited, so existing cached notes stay
  interpretable and do not silently reinterpret under a new schema.
- **Meeting-specific value is not lost.** Decisions, action items, owners and due dates remain
  exactly as strong when the content is a meeting. This decision removes a floor, not a ceiling.
- Imported media has no capture-time provenance: no capture target, no scoped audio claim, no screen
  frames. Those absences must be explicit in the session record rather than defaulted, or an import
  will masquerade as a capture.
- The privacy claims are unchanged and still load-bearing. An imported file is copied into the same
  retention budget, is visible, and is deletable on the same terms.
- ADR-0011's meeting-copilot framing is superseded on vocabulary and notes shape. Its ordering
  ruling — dependable post-call summarization before realtime proposals — stands and is reinforced.
- ADR-0013 is unaffected; it already genericised local knowledge.

## Revisit if

- Adaptive sections prove less useful than fixed ones for meetings specifically, which would argue
  for a content-type hint rather than full adaptivity.
- Users import media large enough that the retention budget's defaults stop making sense.

## References

- ADR-0011 (superseded in part), ADR-0013, ADR-0018
- T050, T070, T071
