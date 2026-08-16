# T097 — Choose a transcription model before you need it

**Status:** done

**Wave:** M4 — recording

**Depends on:** T078's home surface, which is where this lands. Coordinate with it — T078 owns
`layout.rs`, `mod.rs`, and `library.rs` and is `in-review`.

**Owns:** the launch-time model check and its home-surface presentation, the model-choice state in
`crates/app/src/settings/**` if a persisted choice needs a home, the progress plumbing in
`crates/app/src/session/**`, and this task. `crates/asr/**` is read-only here: `ModelSize::spec()`
already exposes everything this needs.

## Why this exists

Whisper weights are downloaded by exactly three code paths, and every one of them is triggered by
an action the user takes because they want something to happen *right now*:

- `crates/app/src/session/mod.rs:1077` — pressing record
- `crates/app/src/session/import.rs:105` — importing a file
- `crates/app/src/workspace/mod.rs:1087` — re-transcribing

Nothing provisions at launch. So the first recording anyone makes stalls behind a 488 MB download
at the exact moment they least want to wait — a meeting has started, they pressed record, and the
capture bar says "Preparing" for thirty seconds with no indication that half a gigabyte is moving.
Observed on 2026-08-15: `ggml-small.en.bin` landed at 19:29 during what looked like a hang.

The download is not the problem. Doing it at the worst possible moment, silently, and without ever
asking is the problem.

## What to build

**On launch, if no usable model is present, the home surface says so and offers the choice.** Not a
modal that blocks the app, and not a silent background fetch — a visible, dismissible state on the
surface the user already lands on, because a person who opens Sotto to read an old transcript should
not be forced through a download first.

**Present every option with what it costs.** `ModelSize::spec()` already carries the exact figures;
do not restate them as literals:

| Size | File | Download |
|---|---|---|
| `BaseEn` | `ggml-base.en.bin` | 147,964,211 bytes (~148 MB) |
| `SmallEn` | `ggml-small.en.bin` | 487,614,201 bytes (~488 MB) |
| `MediumEn` | `ggml-medium.en.bin` | 1,533,774,781 bytes (~1.53 GB) |

Download size is a fact and comes from `spec()`. **Runtime cost — memory and relative speed — is
not currently recorded anywhere**, and the task's own honesty bar applies: state what is measured,
not what sounds plausible. Either measure the resident cost of each model on this hardware and
record the figures with their method, or describe the tradeoff qualitatively without inventing
numbers. Do not ship a table of confident-looking megabytes nobody measured.

Say which one is chosen by default and why. `ModelSize::default()` resolves to `SmallEn` today.

**The choice is the user's and it persists.** Picking a model downloads that one. A user who wants
`MediumEn` should not have to take `SmallEn` first.

**Everything that needs Whisper is disabled until a model exists.** That is start-recording, import,
and re-transcribe — the same three paths listed above, which is not a coincidence: they are exactly
the actions that would otherwise trigger a silent download. Reading a transcript, browsing the
library, renaming, deleting, and Ask over existing material all stay available.

This deliberately contradicts a rule the codebase otherwise holds, so the distinction has to be
right. T079 removed the Pause control rather than ship it disabled, reasoning that *"a permanently
disabled control is worse than an absent one: a person reads it as a capability that is momentarily
unavailable and waits for it, where an absent control simply tells the truth."*

Here the person would be reading it correctly. Recording **is** momentarily unavailable, and waiting
**is** the right response — the wait is a download the same surface is offering. Pause was
permanently unavailable, which is why absence was honest there and would be dishonest here.

So follow the pattern T079 cites as the counter-example done correctly: present, inert, and saying
in place why. A disabled record button that explains nothing fails this as badly as a silent
download does — it must name the reason and point at the choice that resolves it.

## Two defects to fix while here

1. **The capture bar discards progress it already has.** `crates/app/src/workspace/layout.rs:545`
   matches `SessionLifecycle::ProvisioningModel { target, .. }` and drops the `progress` field,
   rendering a bare "Preparing". `progress_label` (`crates/app/src/session/mod.rs:1825`) already
   computes `"Downloading base.en… 40% (195 of 488 MB). Stop keeps a resumable partial."` A download
   that must still happen mid-session has to show that, not hide it.

2. **Those labels name the wrong model.** `progress_label` hardcodes `base.en` in all three phases
   while `ModelSize::default()` resolves to `SmallEn`. The message states a fact that is not true.
   It must name the model actually being fetched.

Also note `crates/app/src/session/import.rs:108` passes `|_| {}` as its progress callback, so an
import discards progress entirely.

## Acceptance

- Launching with no model present shows the choice on the home surface, and the app remains usable
  for everything that does not need transcription — reading an existing transcript, browsing the
  library, deleting a recording.
- With no model present, start-recording, import, and re-transcribe are inert and each says in place
  why, pointing at the choice that resolves it. None of them silently triggers a download, and none
  is merely greyed out without a reason.
- Once a model is present those three are live, with no relaunch required.
- Every offered model shows its real download size, sourced from `ModelSize::spec()` rather than
  restated.
- Any runtime-cost figure shown is measured, with its method recorded in this task. No invented
  numbers.
- The chosen model persists across a relaunch, and choosing one downloads that one.
- Launching with a model already present shows none of this.
- A download still triggered mid-session reports its phase and percentage, and names the model it is
  actually fetching.
- Cancelling a download leaves a resumable partial, as `progress_label` already promises.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Changing which models are offered or their pinned checksums (`crates/asr/**` owns that), the
transcription pipeline itself, and model eviction or disk reclamation.

## Notes

Raised on 2026-08-15 after a maintainer observed a thirty-second "Preparing" on first record and
asked why the app had not settled this at launch. No recorded design intent was found for the
current behaviour: T023 is cited elsewhere as the provisioning precedent — "first-run download,
integrity check, progress, lazy load and idle unload" — but that task file is no longer on the
board, and "first run" was implemented as first *recording* rather than first *launch*. This is an
open design question, not a regression against a recorded decision.

## Decisions

### Home owns the choice; launch never owns the network

The persisted selection is checked on launch, off GPUI's thread, but launch never downloads. A
missing or invalid selected artifact produces a dismissible Home panel; a person opening Sotto only
to read history can ignore it. An already-present artifact is SHA-256 checked against
`ModelSize::spec()` and the panel stays absent. The temporary `Checking` state also keeps the panel
absent, avoiding a false missing-model flash while a large cached file is hashed.

The default remains `small.en` to preserve Sotto's existing transcription behaviour. No accuracy
ranking is claimed: T065 lacked an independent reference. Home states runtime tradeoffs only as
smallest/lightest, current default, and largest/heaviest. **No runtime-cost figures are displayed
and none were measured for this task.** Download byte counts come exclusively from
`ModelSize::spec()`.

### Transcription actions consume availability; they do not provision

Start-recording, microphone-only recording, import, and re-transcription now require the session
controller's already-verified path. None calls `resolve_configured_or_download` itself. Their Home
cards remain present and state that the model choice on Home resolves the unavailable state;
re-transcription's disabled control itself reads `Choose model on Home` and carries the detailed
reason. Once the Home download verifies, the shared entity changes to `Ready` and every action is
live without relaunch.

The explicit Home download persists the selected size before requesting exactly that artifact.
Progress and completion are generation-fenced, so a cancelled or superseded download cannot
overwrite a newer choice. Cancel signals the provisioner's cancellation token; its existing
contract retains the `.partial` file, and Home returns to the missing state ready to resume.

### Progress names the selected model

`SessionLifecycle::ProvisioningModel` now carries `ModelSize`, and its status label derives
`base.en`, `small.en`, or `medium.en` from that value rather than claiming `base.en`. The capture bar
renders the lifecycle's full phase/percentage label instead of dropping progress behind
`Preparing`. Import no longer has a progress callback to discard because import cannot provision.

## Acceptance status

- Launch-time persisted selection and background availability check — implemented.
- Home choice with all three spec-derived download sizes and honest qualitative tradeoffs —
  implemented.
- Explicit, cancellable, resumable, generation-fenced download of the selected model —
  implemented.
- Start, import, and re-transcribe inert with an in-place route to Home until ready — implemented.
- Ready transition takes effect through the shared session entity without relaunch — implemented.
- Correct model-aware phase and percentage labels — implemented and covered by focused unit tests.

## Maintainer review — 2026-08-16

Validated in the built app: the choice appears on launch with no model, the download runs, and
**cancel and resume work** — a cancelled download leaves its partial and resumes from where it
stopped rather than restarting. That was the one acceptance line no test covered, because the
partial's retention belongs to `crates/asr/**`, which this task held read-only.

One defect found and fixed here: **the progress sentence was printed four times.** Home draws the
choice panel and then one card per way of starting, and every card was handed
`unavailable_reason` — so `Downloading small.en… 56% (276 of 488 MB). Cancelling keeps a resumable
partial.` appeared under Capture an app, Record just your microphone and Import audio or video as
well as in the panel itself. Three of those sit beside a control that can neither cancel nor resume
anything.

The two audiences now have two sentences. `unavailable_reason` is unchanged and still names Home,
because its readers — the re-transcribe control and the status line — are on other surfaces.
`home_card_reason` is for a card sitting directly under the choice panel: it names the model and
says it is still downloading, without the percentage, the byte counts, or the cancel instruction.
`a_start_card_does_not_repeat_the_downloads_own_progress_line` holds the two apart.

## Verification

- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo check -p app --lib` — passed.
- Focused model, missing-action, cancellation-fencing, cached-check no-flash, Home mounting, and
  model-aware progress regressions pass.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p app --lib` — 249 passed, 4 explicitly gated
  real-media tests ignored.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked` — passed across the workspace;
  only the repository's explicitly gated real-model, real-media, live-provider, and manual tests
  remain ignored.
- Strict workspace Clippy over all targets and features with `-D warnings` — passed.
- Repository-wide formatting and `git diff --check` — passed.
