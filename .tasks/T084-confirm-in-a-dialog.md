# T084 — Destructive confirmations belong in a dialog

**Status:** in-review

**Planner note (2026-08-14):** status normalized from "done — awaiting the maintainer's hand
check" — the Status field takes exactly one vocabulary value, and awaiting the maintainer's check
*is* in-review. The implementation slice is complete (see *What was built*); closure needs the
hand check.

**Wave:** N7 — v2 workspace

**Depends on:** nothing. T083 closed and released `crates/app/src/workspace/layout.rs` and
`crates/app/src/workspace/mod.rs`.

**Owns:** `crates/app/src/workspace/layout.rs`, `crates/app/src/workspace/mod.rs`,
`crates/app/src/settings/mod.rs`, and this task

## Why

Deleting a recording is confirmed today by a **two-click arm**: the first click arms the control
and writes a sentence into the shell's message strip, the second performs the deletion. That was
the right stopgap when delete became an icon — it put the words back — but it is the wrong shape
for the decision.

Its problems are specific, not stylistic:

- **The prompt is nowhere near the control.** The message strip is at the top of the window; the
  trash icon may be in the settings sheet or the view bar. A person reads a warning in one place and
  clicks in another.
- **Nothing blocks.** An armed control is a normal window with a sentence in it. Clicking anywhere
  else silently disarms, so the confirmation can be missed rather than answered.
- **There is no Cancel.** Disarming is discovered, not offered.
- **Arming is invisible to anyone who does not look up.** For a destructive, irreversible action on
  a file the product promised to keep safe, the confirmation should be unmissable.

`gpui_component` provides what this needs: `Window::open_dialog` with `Dialog::confirm()`, OK and
Cancel, and an `on_ok` callback. The window is already mounted under `gpui_component::Root`, which
is the only precondition.

## What to change

1. **Deleting a retained recording** — from the view bar and from each row in the settings sheet.
   Both become a confirm dialog.
2. **Deleting the stored OpenAI API key** (`settings/mod.rs:417`) — destructive and currently
   unconfirmed entirely. It removes a credential from the Keychain on one click.

Keep every word the armed state already earned. The prompt must still name what dies:

> Delete "Sprint 41 planning" and its 412 MB? This removes the recording, its transcript and its
> notes from this Mac.

A recording still growing says its size is not known yet rather than guessing. A recording with no
retained media says so instead of naming a size. Those were deliberate and must survive.

## What not to change

- **Re-transcribe is not destructive.** It replaces a derived transcript while the captured
  timeline stays append-only, so it needs no dialog. Do not add one out of symmetry.
- **Stop is not destructive.** It ends a recording and keeps everything.
- Do not put a confirmation on anything reversible. A dialog on a harmless action teaches people to
  dismiss dialogs without reading, which is how the one that matters gets clicked through.

## Acceptance

- Deleting a recording, from either surface, opens a confirm dialog naming the recording and its
  size, with OK and Cancel.
- Cancel leaves the recording, its transcript and its notes untouched, asserted by test.
- Confirming deletes exactly the recording named and nothing else.
- Deleting the stored API key is confirmed the same way.
- The two-click arm and its message-strip prompt are gone; no armed state remains in either file.
- Escape cancels, and the dialog does not leave focus stranded.
- Nothing reversible gains a confirmation.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Undo, a trash or recycle bin for deleted recordings, bulk delete, and the retention budget's
automatic pruning — which is not a user action and must not start asking.

## What was built

One shape for all three, `workspace::confirm_delete_dialog`: `Dialog::confirm()` — OK and Cancel,
no close glyph, and an outside click that does not dismiss — with a danger-variant OK, the prompt
as the body, and `on_ok` doing the destructive work. Both `MeetingWorkspace::delete_open_session`
and `SettingsView::delete_recording` / `delete_key` now only *ask*; the work moved into
`confirm_delete_session`, `confirm_delete_recording` and `confirm_delete_key`, each taking the
session id the prompt was written about so confirming can only delete what the dialog named.

`gpui_component::Root` stores the open dialogs but draws nothing, so `MeetingWorkspace::render`
now ends with `Root::render_dialog_layer` — painted last, which is what puts a confirmation over
the settings sheet that raised it. Off a `Root` it yields nothing rather than panicking, so the
bare-shell layout tests are unaffected.

The arm is gone: no `delete_armed` in `workspace/mod.rs`, no `armed_recording` in
`settings/mod.rs`, and neither `select_pane` nor `reopen` nor `dismiss` has anything left to
disarm. `delete_icon_button` lost its `armed` parameter and is now only the settled trash glyph.

### How each dialog reads

| Site | Title | Body | OK |
| --- | --- | --- | --- |
| View bar, retained media | Delete recording | Delete “Sprint 41 planning” and its 412 MB? This removes the recording, its transcript and its notes from this Mac. | Delete |
| View bar, no retained media | Delete recording | Delete “Sprint 41 planning”? It has no retained media, and its transcript and notes are removed from this Mac. | Delete |
| Settings row, settled | Delete recording | Delete the recording for “Sprint 41 planning” and its 412.0 MB? Its transcript and notes stay on this Mac. | Delete |
| Settings row, still growing | Delete recording | Delete the still-growing recording for “Meeting 7”? Its final size is not known yet, and its transcript and notes stay on this Mac. | Delete |
| Stored API key | Delete API key | Delete the stored OpenAI API key? It is removed from this Mac's Keychain, and OpenAI reasoning stops resolving until you paste a key again. Sotto cannot show you the key it is about to remove, and cannot put it back. Your recordings, transcripts and notes are untouched. | Delete key |

Only the arm's own closing instruction — "Click Confirm delete to go ahead." — was dropped, because
the control it named no longer exists. Every other word survives, including the two deliberate
honesty cases: a still-growing recording says its final size is not known, and a recording with no
retained media says so instead of naming a size.

### Cancel and Escape

Cancel and Escape are the same path: `Dialog`'s `Cancel` action, bound to Escape inside the dialog's
own key context, runs `on_cancel` (default: close) and never `on_ok`. Nothing is deleted, no message
is written, and the sheet or shell underneath is exactly as it was. `Window::close_dialog` restores
the focus handle captured when the dialog opened, so a confirmation raised from settings hands focus
back to the sheet — a second Escape then dismisses settings, as it always did.

Clicking outside is *not* a third answer: `Dialog::confirm` sets `overlay_closable(false)`, so the
overlay swallows the click and the question stays on screen until it is answered.

### Not confirmed, deliberately

Re-transcribe, Stop, Reveal, Summarize, retention toggles, budget changes, and the retention
budget's automatic pruning all still act on one click. None of them destroys anything.

### Left for the maintainer

Return activates OK, because `Dialog` binds Enter to Confirm whenever a footer is present and
`gpui_component` 0.5.1 offers no way to disable Enter without also disabling Escape. Escape is
required here, so Enter stays. Worth a look on the real app: if a stray Return on a just-opened
delete dialog feels too easy, the fix is upstream (a separate `enter` opt-out) rather than a local
workaround that would cost Escape.
