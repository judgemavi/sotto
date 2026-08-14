# T055 — One window and the workspace visual system

**Status:** done

**Wave:** N5 — workspace finish

**Depends on:** satisfied. T049, T051, T052, T053 and T054 were accepted on 2026-08-13 with their
visual acceptance narrowed and handed here; `crates/app/src/workspace/**` is released.

**Owns:** `crates/app/src/workspace/**`, `crates/app/src/main.rs`, `crates/app/src/settings/**`,
`docs/design/workspace-mock.html`, `docs/adr/0016-two-column-meeting-workspace.md` for an
amendment section, and this task

## Why this exists

T049 through T054 delivered the structure and the behaviour. Every acceptance criterion they were
given was structural — columns, docks, resize, citation focus, cancellation, retention — and every
one is met. None of them was given the visual half, so none of it was built. The running app has the
right bones and none of the design.

`docs/design/workspace-mock.html` is the reference implementation of the intended product and is
**normative for this task**. It is an interactive mock: open it in a browser, start a session, type
a note, generate notes, expand the Ask rail. Where this task's prose and the mock disagree, the mock
wins, except where a rule in `AGENTS.md` or an ADR overrides both.

## What is wrong today

Observed in the running signed-bundle build on 2026-08-13:

1. **Two windows.** `main.rs` opens the workspace and a separate `SettingsView` that carries session
   control, reasoning settings and MCP settings. The window you read is not the window telling you
   what is being captured.
2. **No design system.** Colours are ad-hoc hex literals at call sites — `rgb(0x171a20)`,
   `rgb(0x252a32)`, `rgb(0x9aa5b1)`, `rgb(0x5f8f7b)` and others, each chosen locally. There is no
   token module, no type scale, no spacing scale, and nothing is themeable.
3. **The transcript is not the mock's grid.** `render_committed_row` is a flat padded stack with a
   top border, not a timestamp / speaker / text row with tabular numerals.
4. **Raw epoch milliseconds in user-facing copy.** `layout.rs:50` formats
   `meeting.started_at_unix_ms` directly: *"started 1786625633040"*.
5. **The three registers are not visually distinct.** Captured fact, the user's typed note, and
   model-derived output are the product's central distinction and the mock encodes them in colour
   and form. The app renders them as similar boxes.

## The window decision

One window, as the mock shows.

- Session control moves into the workspace as a session bar above the columns: capture state, target
  name, scope chips reading screen / audio / mic separately and truthfully, elapsed time, Pause, and
  Stop. AGENTS.md requires that while a session runs the indicator names *what* is captured and that
  Stop is always one obvious action away — the split window undermines both.
- Settings — reasoning backends, the experimental Codex path, MCP sources, model provisioning — is
  not a second always-open window. It opens on demand from the menu and is closed by default.
- **Carry the close guard across.** `main.rs` currently stops capture when the control window is
  closed while `requires_visible_control()` holds. With one window that logic must move to it:
  closing the only window while recording stops the session first. A running capture must never
  outlive its visible indicator.

## Plan

1. Amend ADR-0016 with a short section: one window, the session bar, settings on demand, and the
   mock checked in as the design reference. Do not reopen the column ordering decision.
2. Add a token module under `crates/app/src/workspace/`: palette, type scale, spacing scale, and the
   three semantic registers, wired through `gpui_component`'s theme so light and dark both resolve.
   Port the values from the mock rather than inventing a second palette.
3. Replace every hardcoded colour in the workspace and settings modules with a token. This is
   mechanical and it is the acceptance-critical part: a stray literal is how the system rots.
4. Rebuild the transcript row as the mock's grid — timestamp, speaker label, wrapped text, with
   `tabular-nums` equivalent alignment on the timestamp column, prosody annotation inline and muted,
   and the typed-note pin beneath its anchored row.
5. Merge the windows per the decision above, including the close guard.
6. Sweep user-facing strings for machine values. No unix milliseconds, no raw ids, no debug
   formatting. Timestamps render as wall clock; durations render as elapsed time.
7. Apply the mock's empty and disabled states verbatim in tone: what is happening, why, and what the
   user can do. They are written to be honest about the no-key tier and about capture scope.
8. Fix the inherited per-frame render cost while rebuilding these paths. Two residuals were handed
   here at closure:
   - `annotations_by_anchor` calls `resolve_transcript_anchor` per annotation, and each call runs a
     full `replay_lenient` plus two whole-event scans. Build the transcript-id set, the successor
     map and the active set **once** per call and resolve every annotation against them.
   - `render` calls `selected_events(cx)`, cloning the entire event vector every frame.
   With T052's pacer releasing words every 40-60 ms, both repeat roughly twenty times a second over
   the whole session. This is the likeliest cause of any late-session scroll degradation, and it
   would be easy to blame list virtualization instead.

## Acceptance

- The app opens exactly one window at launch. Settings opens on demand and closes without ending the
  session; closing the last window while recording stops capture first.
- The session bar shows capture state, target, separate screen/audio/mic scope, elapsed time, and
  Stop, while a session runs.
- `rg 'rgb\(0x' crates/app/src/workspace crates/app/src/settings` returns nothing outside the token
  module.
- Transcript rows render as the mock's three-part grid with aligned timestamps.
- Captured fact, typed note, and derived output are distinguishable at a glance without reading a
  legend, in both light and dark.
- No user-facing string contains a unix timestamp, raw id, or debug-formatted value.
- A rendered frame performs at most one `replay_lenient` over the selected session and no full clone
  of the event vector, asserted by a test over the projection helpers rather than by inspection.
- **The app launches and renders all four panes.** An automated smoke check that constructs the
  window and renders one frame, so a double-lease or layout panic fails the build rather than
  reaching a human — this class of defect has already shipped once.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.
- A screenshot of the running signed bundle beside the mock is attached to `## Notes`, with any
  deliberate divergence named and justified.

## Out of scope

New product behaviour of any kind, changes to capture scope, note generation, ask semantics or
retention, and reopening ADR-0015/0016's column decisions.

## Notes

- 2026-08-13 implementation pass: launch now constructs only the meeting workspace; Settings is
  menu-triggered and closed by default. The workspace window owns the capture-stop close guard.
  The session bar carries target, separate screen/audio/mic scope, elapsed time, and Stop through
  provisioning, running, and finalization.
- The mock palette now resolves through one light/dark token module. Workspace and Settings render
  paths contain no colour literals outside that module. Transcript finals render as aligned
  timestamp / speaker / wrapped-text rows, with user-authored pins in the authored register.
- The render path now uses `project_frame_for`: one `replay_lenient`, no selected-event-vector
  clone, and one prebuilt transcript-id/successor/active index for all annotation and citation
  anchors. `one_frame_projection_resolves_every_register_without_event_clones` covers the combined
  projection.
- Automated evidence: all 93 app tests pass, including a real GPUI test-context construction and
  first render of the four-pane workspace; strict all-target/all-feature app Clippy, app formatting,
  owned-file diff checks, and the no-colour-literal grep pass. Signed-bundle visual comparison, the
  requested screenshot, real light/dark inspection, and manual close-while-recording acceptance are
  `NOT RUN` and remain required before this task can be accepted. Pause remains visibly disabled
  because a capture-pipeline pause contract would be new product behaviour and is outside this task.

## Planner review findings — 2026-08-13

Observed in the running single-window build. These are in scope for this task and must be closed
before it is accepted.

1. **Selecting a past meeting shows no transcript.** The read-only banner renders and the meeting is
   clearly selected, but the transcript column shows the live idle empty state. The data is present
   and correct — for session `1786625633040598000` the store holds 7 `utterance.final`, 373
   `utterance.partial`, and `sessions.id` matches `events.session_id`. The load path reads correctly
   on inspection (`select_meeting` → `notes.select` → `load_transcript` → `project_transcript` →
   `pacer.replace`), so the fault is between the projection and the rendered rows. Reproduce it with
   a test that drives the real persisted session through that path, rather than fixing by
   inspection; this is the highest-severity item on the board because reopening a meeting is the
   core of the no-key tier.
2. **Wrong empty state for a past meeting.** `transcript.rs` keys the empty copy on `live`, so a
   past meeting with no rows reads "Nothing is being captured. Start a scoped meeting or a
   microphone-only session when you are ready." A past meeting needs its own copy.
3. **Internal task ids in user-facing copy.** `library.rs:118` renders "Microphone-only capture is
   shown here but remains unavailable until T050 lands its capture path." This violates this task's
   own acceptance. Hide the control until T050 ships rather than explaining our backlog to the user,
   and stop advertising microphone sessions in the transcript empty state.
4. **Timestamps render in UTC.** "started 12:53 UTC" should be the user's local time.
5. **Meeting titles truncate from the front** in the session rail ("is is a gambling emergency"),
   which reads as corruption rather than elision.

Additional evidence worth carrying to T035: that session recorded `capture_target_kind = window`
with `capture_target_audio_scoped = 1`. If per-window audio scoping genuinely holds on this macOS
version, the system-wide-audio disclosure copy is more pessimistic than the truth and should be
re-checked rather than inherited.

### Planner findings resolved — 2026-08-13

- Reproduced the highest-severity failure before changing the load path. The regression persists
  seven finals whose ids survive the store round trip exactly, while deliberately reproducing the
  real session's divergent timestamp and append order; it then drives the real database through
  `MeetingWorkspace::new` and asserts all seven rows reach presentation state and the rendered
  virtual list. Before the repair it failed with `left: 0, right: 7`.
- Root cause: `Store::load_session` returns timestamp order, but the observed capture contains two
  timestamp domains. That placed replacements before their superseded targets, so lenient replay
  reported `UnknownSupersededEvent` and projected zero finals. The review workspace now sorts its
  loaded presentation copy by `EventId`, the append-only ordering authority, before replay. The
  durable store is not rewritten.
- A selected past meeting with no rows now says that no transcript was captured for that meeting;
  the cold idle copy remains capture-oriented. Neither path advertises microphone-only capture.
  The disabled microphone-only control and its T050 backlog explanation are no longer constructed.
- Read-only meeting start times now use the user's local timezone and a local 12-hour clock; `UTC`
  is no longer rendered.
- Session-rail buttons now give the title a bounded, left-aligned end-ellipsis child plus the full
  title as a tooltip, preserving the beginning instead of clipping it from the front.
- Automated recheck after these repairs: all 96 app tests pass; strict all-target/all-feature app
  Clippy, app formatting, owned-file diff checks, the internal-task-id/user-copy sweep, and the
  no-colour-literal gate pass. Signed-bundle/manual visual acceptance remains `NOT RUN`.

## Planner review findings, round 2 — 2026-08-13

Reopening a past meeting now renders its finals; the ordering repair is confirmed working against
the real persisted session. Two new defects, both visible in the same view.

6. **A closed session renders a live provisional strip.** Reopening the completed YouTube session
   shows "Meeting audio · Listening…" with a hypothesis under the transcript. The store explains it:
   event 394 is the last `utterance.partial`, it supersedes 393, and nothing supersedes it, so it
   stays active in replay and lands in the `unstable` register. Capture stopped over an hour ago and
   the app claims to be listening.
   Do not simply hide it — that text is real transcribed speech from the end of the call and is the
   only record of it. Render it in place, labelled as never finalized before capture stopped, and
   keep the live "Listening…" affordance for live sessions only.
7. **Transcript text clips instead of wrapping.** Rows are cut at the column edge — *"Their apps are
   telling them, "Ol"*. ADR-0015 requires wrapped text rows, T049 restated it, and the mock wraps.
   The three-part grid needs its text column to wrap and grow the row height.

Not a defect, recorded so it is not mistaken for one: the two rows at `00:00` are two genuine
persisted finals (ids 12 and 20) where the second restates and extends the first. The UI is
rendering the record faithfully. Whether the commit policy should emit that pair at all is an ASR
question for the T035 transcript-readability rubric, not a workspace fix, and the UI must never
silently deduplicate captured facts.

### Round 2 implementation evidence — 2026-08-13

- Completed-session projection now moves every trailing active hypothesis newer than its stream's
  latest final into the ordinary ordered transcript register and marks it `Not finalized before
  capture stopped`. It never enters the live provisional register, even when capture ended after a
  VAD speech-end event. Live projection remains silence-gated and retains at most one current
  `Listening…` hypothesis per stream.
- `completed_projection_preserves_trailing_partial_without_a_live_strip` asserts those registers and
  the unfinalized marker over projection output. Its two same-time finals also guard the explicit
  no-deduplication boundary.
- Transcript rows now let the flexible text cell shrink below its intrinsic width, so text wraps
  and the measured row grows while the 48 px timestamp and 92 px speaker cells remain fixed.
- Planner-supplied signed-bundle evidence in the review thread confirms the reopened 02:49 tail is
  an in-place unfinalized row with no live strip, and confirms wrapping/alignment at the narrow
  transcript width. A wide-column capture beside the normative mock is still `NOT RUN`; that single
  visual artifact remains before T055 acceptance.
- Final automated recheck after the round 2 repair: all 97 app tests pass; strict
  all-target/all-feature app Clippy, app formatting, owned-file diff checks, the user-copy colour
  gate, and signature verification pass. The release was rebuilt explicitly with
  `cargo build --release -p app` before `scripts/dev-bundle.sh`; `target/Sotto.app` is ad-hoc signed
  and satisfies its designated requirement. The repository-wide diff check still reports the
  pre-existing blank line at EOF in T048, outside T055 ownership.

## Ownership release — 2026-08-13 (planner)

All seven filed defects are implemented and its automated acceptance is green. The only remaining
item is the signed-bundle screenshot beside the mock, which is a manual observation and not a file
edit, so holding `crates/app/**` for it blocks live build work for no benefit.

This task therefore **releases** `crates/app/src/workspace/**` and `crates/app/src/settings/**`. It
retains only this task file and the outstanding visual verdict, and stays `in-review` until that
verdict is recorded here or by T035.

Released to:
- T057 — `crates/app/src/settings/mod.rs` for the recording library controls, and
  `crates/app/src/workspace/layout.rs` for the visible recording copy in the session bar.
- T056 — `crates/app/src/workspace/notes.rs` and `crates/app/src/workspace/transcript.rs`.

The two tasks touch disjoint files and may run concurrently. Neither may edit the other's.

## Planner visual verdict — 2026-08-13

Screenshot of the running signed bundle reviewed against `docs/design/workspace-mock.html`.

Verified fixed, six of seven:
- Transcript text wraps; the timestamp / speaker / text grid stays aligned at narrow width.
- A completed session's trailing hypotheses render in place, labelled "Not finalized before capture
  stopped" in the warn register. No live "Listening…" strip on a stopped session.
- Start times render in local time.
- The microphone-only control and its backlog copy are gone.
- Registers read correctly without a legend: authored notes in the accent register, unfinalized tail
  in warn, captured facts neutral.
- One window; Settings is menu-triggered.

**Defect 5 is NOT fixed and this task stays `in-review` for it alone.** The rail renders
`a gambling emergency - YouT` for a session titled `this is a gambling emergency - YouTube 🔊` —
clipped at both ends with no ellipsis. The idiom at `library.rs:159-167` is correct
(`w_full().min_w_0().whitespace_nowrap().text_ellipsis()`), so the cause is upstream: an ancestor
flex container without `min_w_0`, or a missing `overflow_hidden`, letting the child overflow and be
clipped instead of ellipsized. A mid-word cut with no ellipsis character is the signature of
clipping, not truncation. `crates/app/src/workspace/library.rs` remains owned by this task; it was
not released to T056 or T057.

New observation, not previously filed:

8. **A silence marker renders as record content.** Row 02:43 shows `You — [ Silence ]` labelled as
   unfinalized. VAD gating is meant to keep silence hypotheses out of the visible register, and an
   unfinalized tail whose whole content is a silence marker is noise rather than the last thing
   anyone said. Suppress it, or state why it is kept.

## Final-round handoff — 2026-08-13

- Defect 5: fixed only in `workspace/library.rs`. The title child already carried the correct
  end-ellipsis idiom. The actual cause was its `gpui-component` `Button` ancestor, whose library
  default is `flex_shrink_0`, plus rail ancestors that did not bound horizontal overflow. The
  meeting button now opts into shrinking and clips at its own width; the rail root and scrolling
  column now carry `min_w_0` plus bounded overflow. The title label itself was not changed.
- Focused library test, strict app-library Clippy with all features and `-D warnings`, workspace
  formatting, and the owned-file diff check pass. Signed-bundle visual confirmation of the repaired
  ellipsis remains `NOT RUN`.
- Defect 8 belongs in T056-owned `workspace/transcript.rs` and was not edited. Exact seam:
  `project_frame_for_mode` silence-gates live partials through the `speaking` condition, but
  `completed == true` bypasses that condition and promotes every newer active partial into the
  committed register. `transcript_row` rejects only blank strings, so the literal `[ Silence ]`
  becomes an unfinalized record row. T056 should reject silence-only marker hypotheses before the
  completed promotion while retaining substantive unfinalized speech and its existing label.

## Planner update — 2026-08-13

Defect 5 fixed at `library.rs:99`. Root cause was `gpui_component::Button` defaulting to
`flex_shrink_0`, so the button overflowed its rail regardless of correct ellipsis styling on the
child — the container hypothesis was right and the specific cause is worth remembering, since every
other rail control inherits the same default.

Defect 8 moved to T056, which owns `transcript.rs`. The implementer diagnosed it without editing the
file, which is the behaviour the parallelism rule is for.

**This task now closes on one thing only:** visual confirmation that the session rail shows a
readable, front-preserved, ellipsized title in the running signed bundle.

## Defect 5, round 3 — root cause confirmed in the component — 2026-08-13

Still reproducing on a verified-fresh build (source 11:07, binary 11:11): the rail renders
`a gambling emergency - YouT` for `this is a gambling emergency - YouTube 🔊`.

The surviving text is the *middle*, which is the tell. An end-ellipsis failure would preserve the
front. Losing both ends means the label is centered inside a clipped box.

`gpui_component`'s Button hardcodes its layout at `button.rs:460-462`:

    .flex_shrink_0()
    .items_center()
    .justify_center()

Bounding ancestors and adding `overflow_hidden` cannot fix this, and neither can `text_ellipsis()`
on the child: the text node is not the constrained element, it is a centered child overflowing a
clipped parent. Removing `flex_shrink_0` upstream left `justify_center` intact.

**Fix:** do not use `Button` for a variable-length text row. Render the session row as an
interactive `div` with `.id()` and `.on_click()`, laid out `justify_start`, with the title as a
`min_w_0` ellipsized child. This applies to every future rail control carrying variable-length text,
not only this one.

## Closure — 2026-08-13 (planner)

Accepted. One window, session bar, design tokens, the transcript grid with wrapping, local-time
stamps, the honest unfinalized-tail label, the hidden microphone control, and the per-frame render
cost are all implemented and visually verified against `docs/design/workspace-mock.html` on the
running signed bundle.

**Defect 5 is deferred by maintainer decision, not fixed.** Current state: the session rail title
preserves its beginning and clips at the end without a visible ellipsis character. Three attempts
were spent on it; the root cause is `gpui_component::Button`'s hardcoded `justify_center` at
`button.rs:460-462`, and the fix is to render the row as an interactive `div` instead of a `Button`.
That is written down above so a future attempt starts from the cause rather than the symptom. It is
a cosmetic divergence from the mock on one label and does not block ship.

Recorded as a known divergence in T035's readability rubric so the ship verdict sees it rather than
rediscovering it.

Defect 8 stays with T056, which owns `transcript.rs`. It was not edited by this task.
