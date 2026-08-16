# T069 — Settings and Ask look like debug panels

**Status:** done

**Wave:** N6 — UI primitives

**Depends on:** T062 landing, which releases `crates/app/src/workspace/**`.

**Ownership override (maintainer, 2026-08-13):** explicitly released T069 to proceed while T062
remains in review. Preserve T062's existing work and stay inside T069's owned files.

**Owns:** `crates/app/src/settings/mod.rs`, `crates/app/src/workspace/ask.rs`, and this task

## Why this exists

The maintainer's verdict on 2026-08-13: "settings look so stupid". That is a fair reading of the
screenshot, and Ask is worse — it is visibly broken, not merely plain.

`docs/design/workspace-mock.html` is normative for the shipped look. Neither of these surfaces was
built against it, and it shows.

### Ask — actual layout defects

- The heading **"Ask" renders three times** stacked: the panel toggle, a header, and a section title.
- **"Use every retained meeting" overlaps** the "Include this meeting in cross-session search" text
  beneath it.
- The panel is **clipped at the right edge** of the window; controls run past it.

These are the same family of defect T060 built its shrink-priority row primitive to prevent. Use
that primitive here rather than bounding widths at each site — that approach has failed four times
now, and Ask is the fourth.

### Settings — no hierarchy

Every element carries equal weight: section titles, explanatory paragraphs, inputs and buttons all
sit at the same size in one unbroken column. Consequences visible in the screenshot:

- **State is unreadable.** The Codex control reads "Disable experimental Codex", which is an action
  label describing the *opposite* of the current state. The maintainer concluded Codex could not be
  enabled while it was already enabled and already selected as Summarizer. A control must make the
  present state obvious before it offers to change it.
- **A recording failure card is wedged between unrelated sections**, styled like content rather than
  like a problem needing attention.
- **Long privacy paragraphs sit above the controls they qualify**, so the reader meets the caveat
  before knowing what it applies to.
- **Related controls are not grouped.** Model id, save, check and enable for one backend are strung
  in a line beside controls belonging to a different one.

Settings is where the product makes its privacy claims — what leaves the machine, what is stored,
what a backend can see. Those claims are load-bearing, and presenting them as an undifferentiated
wall means nobody reads them. That is a product failure, not a cosmetic one.

## Plan

1. Fix Ask's three layout defects first; it is broken, not just unstyled. Adopt T060's row primitive
   for its controls.
2. Give Settings a visual hierarchy using the existing design tokens: section headings distinct from
   body text, related controls grouped into cards, explanatory text subordinate to the control it
   qualifies rather than above it.
3. Make every stateful control state-legible. A toggle shows what is currently true, then offers the
   change. Apply this to the Codex acknowledgement, role overrides, and the API key controls.
4. Surface problems as problems. A failed recording, a missing key, or an unavailable backend should
   read as attention-needing, not as another paragraph.
5. Keep every privacy statement. Reword or reposition freely, but nothing about what leaves the
   machine may be dropped in the tidying — those sentences are the product's disclosure, and losing
   one in a visual pass would be the worst possible outcome of this task.
6. Add a narrow-width test for both surfaces, per T060's rule. Layout defects here have only ever
   been found by a human at one window width.

## Acceptance

- Ask renders one heading, no overlapping controls, and nothing clipped at any width down to the
  stated minimum.
- Settings has a legible hierarchy: a reader can find a section, see its state, and act without
  reading every paragraph.
- Every stateful control shows its current state before offering to change it.
- No privacy or disclosure statement is lost; a test or a recorded checklist proves the set is
  intact.
- A narrow-width test covers both surfaces and fails if a control escapes its bounds.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

New settings, removing any existing control, changing the theme tokens, and the recording
finalization defect itself (T068).

## Implementation status — 2026-08-13

- Ask now relies on the dock title as its single heading and uses T060's `ControlRow` for scope,
  retention, and submit controls. The input and descriptive labels yield before essential actions.
- Settings is grouped into reasoning-backend, role, recording, and meeting-source cards. Every
  stateful card leads with its current state before presenting actions; failed recordings and the
  unavailable local MCP path use the warning treatment.
- The six existing privacy and disclosure statements remain verbatim and are pinned by an exact
  inventory regression.
- Narrow-width GPUI tests cover Ask at 300 px and Settings at 420 px. Focused tests, the remaining
  126 app tests, strict app Clippy over all targets, scoped formatting, and scoped diff checks pass.
- Full app test status is not green because the unrelated
  `reasoning::tests::codex_requires_persisted_consent_then_resolves_without_api_key` assertion
  currently fails in isolation against the concurrent providers tree. Browser/owner visual
  acceptance remains **NOT RUN**.

## Closed — 2026-08-16

The three Ask defects this task was filed for are settled, each by the strongest evidence available
for its kind.

**Clipping** was already covered: `narrow_ask_panel_keeps_both_scopes_and_submit_inside_bounds`
proves the scope row, the selection row and the form all stay inside the minimum panel width.

**Overlap** was not, and is now. The narrow test stacks the three *control rows*, but the line the
controls overlapped — the sentence qualifying the scope — was not one of them and carried no
selector, and the defect was reported at an ordinary width rather than at the minimum. It now has a
selector, and `the_scope_controls_never_sit_on_the_line_that_qualifies_them` asserts the stack and
the horizontal bounds at both 420px and the minimum. Forcing a 20px negative offset makes it fail
with `the scope controls must stack rather than overlap at 420px`, so it is not vacuous.

**Three stacked headings** is fixed by construction and is deliberately *not* asserted by test. The
panel body renders no heading element at all — the dock's `Panel::title` is the only one — and the
only other occurrences of the word are the toolbar toggle and the submit button, both controls. A
bounds assertion that "no heading renders" would be exactly the vacuous kind T076 and T098 record:
`debug_bounds` never clears, so `is_none()` only ever proves a selector was never drawn in that
window, and it would pass whether or not the fix held. Proven by reading the render rather than by
a test that cannot fail.

Settings hierarchy and state legibility remain the maintainer's judgment and were not held for;
T080–T084 have since overtaken much of that surface. This closes T091, whose ownership override
waited on this task and T077.
