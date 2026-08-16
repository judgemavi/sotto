# T080 — Settings as a sheet with four panes

**Status:** done

**Wave:** N7 — v2 workspace

**Depends on:** T069 (`in-review`), which grouped Settings into cards and pinned its disclosure
inventory. `docs/design/workspace-v2-mock.html` is normative and now specifies this surface.

**Owns:** `crates/app/src/settings/mod.rs`, the settings entry point in
`crates/app/src/workspace/layout.rs`, and this task

## What the mock specifies

Settings stops being a long scrolling page and becomes a **modal sheet** (`role="dialog"`,
`aria-modal`) over a scrim, with a titled head, a close control, and a **left nav of four panes**:

1. **Storage & privacy** — first, and default, because the product's claims live here
2. **Recording** — capture scope, include-your-microphone, screen frames, microphone input, start shortcut
3. **Transcription** — model, language, and the append-only statement
4. **Summaries & Ask** — the reasoning backend and what a summary is allowed to reach

Only the selected pane renders. The nav marks the current one (`aria-current`), and each pane opens
with a heading and a one-line lede saying what the pane is for.

The current implementation is one 1,086-line scrolling column with every control at the same depth.
T069 grouped it into cards, which helped; this is the structural change that card grouping was
standing in for.

## What must not be lost

**The disclosure set.** T069 pinned six privacy and disclosure statements with an exact inventory
regression, because losing one in a visual pass is the worst possible outcome of a task like this —
it is how "audio is never written to disk" survived past the point it was true. That regression
must still pass, or be deliberately updated with each change justified statement by statement.

Storage & privacy leads for a reason: it is where the reader checks what Sotto keeps, what it costs
on disk, and the one case where anything leaves the machine. Do not demote it to make room for
configuration.

## What is true today, and must stay true

- **Codex is the only reasoning backend.** Other providers exist in `crates/providers` but are
  deliberately not surfaced; see the board's standing decisions. The mock's model control offers
  Codex alone. Do not add a provider list.
- **Whisper is `base.en`.** The mock's transcription pane names other sizes; `small.en` and
  `medium.en` are provisionable but the default is unmeasured against real meeting audio (T065).
  Offer only what the app can actually resolve today, and do not invent a model list.
- **Codex requires an explicit acknowledgement** before it can be selected. That consent gate is
  ADR-0014's and must survive the redesign.
- Several mock controls have no implementation: a start shortcut (no key bindings are registered),
  and language selection. Render nothing for a control that does not exist, or state plainly that
  it is not built — the standard the Import entry point set.

## Plan

1. Build the sheet: scrim, dialog semantics, a close control, and Escape to dismiss.
2. Build the four-pane nav with only the selected pane rendering.
3. Move each existing control into its pane without changing its behaviour.
4. Keep every stateful control state-legible, per T069: it shows what is currently true before
   offering to change it.
5. Re-check the disclosure inventory and the narrow-width test.

## Acceptance

- Settings opens as a sheet, closes by its control and by Escape, and returns focus sensibly.
- Four panes, Storage & privacy default, only the selected one rendered, current one marked.
- Every existing setting still reachable and still works; no control is lost in the move.
- The disclosure inventory regression passes, or every change to it is justified individually.
- No control is offered for a capability that does not exist.
- Nothing clips at the stated minimum width.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

New settings, the reasoning backend list (Codex only for now), model provisioning changes (T065),
and the workspace columns.

## Notes

### What was built

`crates/app/src/settings/mod.rs` only. The 1,086-line scrolling column is now a scrim-backed sheet:
a head with the title and a Close control, a four-entry nav, and one pane rendered at a time.

- **Sheet.** Full-window scrim (`tokens.scrim`), sheet sized to the mock's
  `min(760px, 100vw - 48px)` / `min(600px, 100vh - 64px)`, rounded, bordered, `overflow_hidden`.
  Status notices (MCP, reasoning controller, last action) moved from the bottom of the scroll into
  a foot inside the sheet, so a message is visible from any pane.
- **Dialog semantics are structural, not declared.** GPUI has no ARIA. The sheet is a scrim-backed
  panel in its own window, it takes focus on open (`track_focus` + `window.focus` in the
  constructor), its close control is always visible, and Escape dismisses. Both close paths call
  `window.remove_window()`, which returns key status to the workspace window behind it.
  The focus handle is load-bearing: without a focused ancestor GPUI dispatches key events to the
  window root only, and Escape would be silently ignored.
- **Nav.** `aria-current` becomes accent wash + accent ink + semibold, per the mock's
  `.set-nav button[aria-current="true"]`. Below 640px the nav stacks above the pane as a wrapping
  row instead of eating a 168px rail out of a 372px sheet — a left rail at 420px leaves the pane
  too narrow for the Codex/OpenAI action rows, and a clipped button is the defect this redesign
  exists to remove.

### Pane layout

1. **Storage & privacy** (first, default) — `RECORDINGS_DISCLOSURE` as the leading claim, then
   `REASONING_DISCLOSURE` as the honest (warn-toned) egress claim, then the real recording
   directory, the storage-budget control, every retained recording with its Delete, and growing /
   failed-finalization rows.
2. **Recording** — statements only; see below.
3. **Transcription** — statements only; see below.
4. **Summaries & Ask** — "uncited claims fail closed", the unbuilt auto-summarize note, the
   reasoning backends group (default-for-new-work, the Codex card, the OpenAI card), the three
   role cards, and the MCP source group (add remote HTTPS, local-unavailable, bearer credential,
   configured servers).

MCP moved onto Summaries & Ask because the task defines that pane as "the reasoning backend **and
what a summary is allowed to reach**". It has no pane of its own in the mock.

### Mock controls with no implementation

Nothing was rendered as a control. Each is a statement row, warn-toned where the mock implied a
control that does not exist:

| Mock control | What shipped |
| --- | --- |
| Start shortcut (`⌘⇧R anywhere`) | "Start shortcut — not built". No key bindings are registered. |
| Microphone input picker | "Microphone input — not built". Sotto records the system default input; the row points at System Settings › Sound. |
| Language picker | "Language — not built". `base.en` is English-only and no language option is passed. |
| Whisper model list (`small`, `large-v3-turbo`) | One statement naming `base.en`, its size, its pinned digest and its cache path, and saying plainly that `small.en`/`medium.en` are provisionable but unranked (T065), so a picker would be a guess. A second row discloses the `SOTTO_WHISPER_MODEL` override honestly — it is neither downloaded nor digest-checked, and progress still reads `base.en`, so it is a developer tool, not a setting. |
| "Include your microphone" switch | Statement: it is a per-recording choice made at start, not a persisted setting. |
| Retention budget `<select>` | Kept as the existing raise-only GB input, which is what the app actually enforces. |
| "Reveal storage in Finder", "Delete all recordings" | Not added — new settings, out of scope. |
| "Summarize when a recording stops" switch | "— not built". Summaries are written when Summarize is pressed. |
| Summary model `<select>` (Codex alone) | No provider dropdown added. The existing Codex and OpenAI cards are unchanged. |
| Capture scope / screen frames / append-only / uncited-claims-fail-closed | Statements, as in the mock. Capture scope is stated honestly: audio scoping is per-target and is reported on the capture bar, not asserted globally. |

### Disclosure inventory, statement by statement

`privacy_disclosure_inventory_is_complete` passes **unchanged** — all six strings are byte-for-byte
identical. Placement:

1. `REASONING_DISCLOSURE` — Storage & privacy, as the honest egress claim.
2. `CODEX_DISCLOSURE` — Summaries & Ask, on the Codex card, above the acknowledgement button.
3. `OPENAI_DISCLOSURE` — Summaries & Ask, on the OpenAI card.
4. `RECORDINGS_DISCLOSURE` — Storage & privacy, leading claim.
5. `MCP_DISCLOSURE` — Summaries & Ask, heading the source group.
6. `LOCAL_MCP_DISCLOSURE` — Summaries & Ask, on the warn-toned "Local MCP process — unavailable"
   card.

`REASONING_DISCLOSURE` renders once, on Storage & privacy, rather than twice. The reasoning group
carries a short pointer instead, and both backend cards state their own egress, so no pane
under-discloses.

Because a string constant proves nothing about what is painted, each disclosure now carries its own
debug selector and a new mounted test (`every_pinned_disclosure_still_renders_on_a_pane`) asserts
all six are laid out on the pane they moved to. That is the regression a four-pane split actually
needs.

### Decisions a reviewer should check

- **OpenAI was kept surfaced.** The board's standing decision says Codex is the only backend and
  others "are not surfaced in Settings". Removing the OpenAI card would have deleted
  `OPENAI_DISCLOSURE` from the UI and removed a working Keychain-backed credential path, against
  this task's own acceptance ("no control is lost in the move"). Read narrowly — *do not add a
  provider list* — nothing was added. Deleting a shipped, working backend surface is a product
  decision that deserves its own task, not a side effect of a layout pass.
- **ADR-0014's Codex gate survives.** "Enable Codex — I understand" stays disabled until the probe
  reports ready, the Codex role button stays disabled unless `codex_enabled && codex_ready`, and
  the disclosure sits directly above the acknowledgement.
- **Scroll wrapper.** `Scrollable` lifts its element's style onto an outer div and clears it, so
  the pane's column, gap and padding are declared on a child of the scrolling wrapper rather than
  on the wrapper itself. The previous implementation put them on the scrolled element and silently
  lost its gaps.

### Verification

- `cargo test -p app` — 192 passed, 0 failed (11 in `settings`, up from 6).
- `cargo clippy -p app --all-targets --all-features -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.
- `git diff --check` scoped to the owned files — clean.

New tests: `every_pinned_disclosure_still_renders_on_a_pane`,
`settings_opens_on_storage_and_privacy_with_one_pane_rendered`,
`selecting_a_pane_renders_that_pane_and_no_other`, `escape_dismisses_the_sheet`,
`the_nav_switches_panes_and_close_dismisses_the_sheet`. The narrow-width test now checks the sheet,
the nav and every pane at 420px, not just the default one.

### NOT RUN

- **No signed-app visual check.** Nothing was launched; every claim above about appearance comes
  from layout assertions, not from looking at the sheet. Colour, spacing and dark-mode balance are
  unverified by eye.
- **Focus return after dismissal is asserted only as "the window is gone".** That the workspace
  window becomes key is macOS behaviour, not something the test harness observes.
- **`selecting_a_pane_renders_that_pane_and_no_other` cannot assert Storage & privacy is absent**
  after switching away. GPUI's `Frame::clear` does not clear `debug_bounds`, so entries survive
  across frames and `is_none()` only proves an element never rendered in that window. Storage
  renders on open by definition, so it is excluded from the absence check; single-pane rendering at
  open is pinned by the sibling test.
- **The settings entry point in `crates/app/src/workspace/layout.rs` was not touched** — there
  isn't one. Settings still opens from the `Sotto ▸ Settings…` menu item, whose `OpenSettings`
  action is declared in `crates/app/src/main.rs`, outside this task's ownership. Wiring the mock's
  status-bar and title-bar settings entries needs either that action moved into the lib or
  `MeetingWorkspace` to hold the sheet, and both files belong to other tasks.
- **Storage & privacy's budget card and recording rows still render only when the library snapshot
  succeeds.** Unchanged behaviour, but it means a failed snapshot leaves that pane with claims and
  a notice and no controls.

## Maintainer review — 2026-08-16

Closed. The visual pass was run in the built app: the sheet's colour, spacing and dark-mode balance
read correctly, the four panes switch with only the selected one shown and the current one marked,
every control survived the restructure into cards, and dismissal by Escape and by the sheet's own
control returns to a sensible focus. Those were the four things layout assertions could not reach —
in particular that **Storage & privacy** disappears after navigating away, which no test can prove
because `debug_bounds` never clears and the pane renders on open by definition.

Two of the recorded residuals were void rather than outstanding. This task noted there was no
in-window settings entry point and that `Sotto ▸ Settings…` dispatch was unproven; T081 added the
gear and T082 pinned the menu action with three regressions that mount the shell exactly as
`main.rs` does.

The failed-library-snapshot case stands as recorded: Storage & privacy then renders its claims and
a notice with no controls. Unchanged behaviour, not a regression introduced here.
