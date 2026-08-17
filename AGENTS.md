# Project: Sotto — Local-First AI Meeting Copilot

*Name: from "sotto voce" — under the voice. The quiet prompt beneath the conversation.*

## What we are building

A desktop app for people in meetings that captures an explicitly selected call, transcribes it locally, and turns it into a reviewable meeting record. With a configured reasoning backend, Sotto produces structured meeting notes and can optionally propose useful next moves grounded in both the meeting and user-authorized resources exposed through MCP.

**Sessions are explicitly started and explicitly scoped.** The user turns Sotto on and picks what to capture — an application or a window — through the system picker. Nothing outside that scope is ever captured. Sotto is not ambient, does not run in the background waiting for a call, and has no always-on mode.

## Two layers: the map, and the reasoning over it

Sotto is built as a **deterministic local layer** with an **optional reasoning layer** on top. This is not an implementation detail — it is the shape of the product.

**The local layer needs no API key and no network.** It captures the chosen target's audio and screen, transcribes on-device, and assembles a *map* of the session: a chronological, append-only timeline of who said what, when, with what prosody, against what was on screen. This layer is deterministic, private, and complete on its own. A user with no credentials configured still gets a reviewable record of their call.

**The reasoning layer makes the map useful as a meeting copilot.** Users may explicitly opt into an installed, ChatGPT-authenticated Codex CLI as an experimental no-API-key path, or use direct OpenAI Responses API access with their own key. Both sit behind an open backend contract so later runtimes can be added without changing `core` or reasoning consumers. The minimum AI feature is structured meeting notes: overview, topics, decisions, action items, open questions, risks, and follow-ups, all cited to the meeting record. Optional proposals can suggest a clarifying question, decision check, next step, follow-up, or relevant resource. MCP extends the evidence available to both notes and proposals through sources the user explicitly enabled for that session.

The distinction that matters, because it decides what belongs where:

- **Chronological structure is deterministic.** Who spoke, when, for how long, over which screen. No semantics required — local tools produce it exactly, every time.
- **Notes, topics, and proposals require meaning.** "These exchanges produced this decision." "This action remains unassigned." "This project document may answer the open question." That is the model's job, and each result is a *derived view* over the append-only log, never a mutation of it.

Add a model and the meeting view gains a cited synthesis beneath the transcript. Remove it and the chronological transcript is still correct and complete. Nothing breaks; capability degrades.

Two consequences to hold onto:

1. **Notes are the minimum AI product; proposals are optional.** A meeting copilot earns trust by producing a faithful, cited artifact before it tries to intervene live. Proposals default off and quiet; their usefulness is measured separately from note quality.
2. **`core` must never depend on `providers`.** The crate graph is what enforces the tier boundary. If reasoning code lands in `core`, the no-key tier stops being real.

Core differentiators (do not compromise these):

1. **Local-first.** Audio never leaves the device. Transcription runs on-device (whisper.cpp). A reasoning-backend request may contain redacted transcript text, explicitly granted external evidence excerpts, and—only after a separate opt-in—one inspected screen image. Separately, a user may grant disclosure of a bounded meeting-derived query to a named MCP server; that disclosure is visible, audited, and off by default.
2. **No forced API key.** The local record works without reasoning. An installed Codex CLI may use its existing ChatGPT login after explicit experimental consent; Sotto never reads Codex credentials or asks for an API key on that path. Direct OpenAI API access is optional BYOK and its key lives in the OS keychain. We never proxy inference through our servers. T030 still establishes that Codex has no supported empty-built-in-tools contract, so the UI must disclose that residual risk rather than presenting the experimental path as tool-free.
3. **Bring your own context.** MCP lets users make project documents, knowledge bases, tickets, CRM records, and prior-meeting resources available as explicitly selected evidence. V1 access is application-controlled and read-only: Sotto retrieves bounded resources, records provenance, and never exposes arbitrary MCP actions to the model.
4. **Transparent by design.** NO stealth features. We are an enablement tool (Gong/Balto category), not a concealment tool (Cluely category). Never implement screen-share invisibility, capture-evasion, or anything designed to hide the app from other call participants. Consent handling is a first-class feature.
   **Consent is structural, not a policy.** Capture scope is enforced by the OS content filter, not by us filtering afterwards: if the user picked one window, nothing else is ever in the buffer, so there is nothing to redact, prune, or be trusted about. Prefer the system picker — it is the affordance users already know from screen sharing, and macOS draws its own indicator around the captured window. An allowlist chosen by the user beats any blocklist we maintain.
5. **Lightweight by construction.** Sotto is a single native Rust binary. Small footprint is part of the product's identity ("local-first" should *feel* local-first) — resist dependencies and architecture choices that bloat it.
6. **The record, not a toast.** Notes and proposals live in the same top-down meeting view as the transcript. Meeting and external-source citations make clear why each derived item exists.
7. **Useful before it is smart.** The app works with no API key configured: capture, transcribe, and map. A model upgrades that record into cited notes and optional proposals; it never gates access to the meeting itself.

## The session timeline (core abstraction)

The canonical data structure of the entire product is the **session timeline**: an append-only, timestamped, heterogeneous event log per call. Everything is a producer into it or a consumer of it.

Event kinds (extend deliberately; all share `{id, session_id, ts, kind, payload}`):
- `utterance.partial` / `utterance.final` — per audio stream (local participant = mic, remote meeting audio = captured target), text + prosody annotations
- `vad` — speech start/stop per stream
- `prosody` — pauses, interruptions, speech rate, talk-time ratio deltas
- `screen.snapshot` — low-rate retained change-frame reference + active-app metadata and visible interval; OCR is derived only by an explicit later inspection
- `proposal.trigger` — optional watcher classification that an assist may be useful
- `proposal.partial` / `proposal.final` — optional copilot output, **anchored to the event(s) that triggered it**; external claims also carry source evidence ids
- `annotation.user` — the user's own marks on the meeting record

The **session record** carries what the timeline is *of*: start and end wall-clock, and the capture target the user chose (bundle id, window title). A timeline without a recorded scope is not reproducible and cannot be explained to the person in it.

Rules:
- **Append-only.** Corrections (partial → final) are new events referencing the superseded id, never mutations. Layout and consumers rely on this.
- **Both partials and finals are first-class** from day one. The map tier only needs finals; the live copilot needs partials. The schema never assumes batch.
- **Persistence:** timelines land in SQLite (same file as RAG). Every completed recording is indexed into that same local file so notes, proposals and Ask can reference prior recordings (ADR-0020). There is no per-recording search opt-in: the index never leaves the device, and deleting the recording is what removes it.
- Consumers: (a) live transcript UI, (b) notes generator, (c) optional proposal layer, (d) topical clustering, (e) RAG ingester. All read the same spine.
- **Meeting facts and system output are distinct.** Captured utterance, VAD, prosody, screen, and user-annotation events form the factual meeting record. Notes and topical views are recomputable derived artifacts stored alongside it. A proposal may be appended to the same audit timeline so Sotto can reproduce what it displayed, but it is typed as model output, never treated as a meeting fact, and never rewrites earlier events.

## UI model: one meeting workspace

**Starting a session is a deliberate, two-step act:** turn Sotto on, pick the target. The picker is the system's, not ours. While a session runs, the indicator shows *what* is being captured — the target's name, and audio versus screen distinctly — not merely that something is. Stopping is always one obvious action away, and the app never resumes a session on its own.

The product workspace is one meeting record with a session rail, transcript and notes side by side,
and a collapsible Ask dock. ADR-0016 amends ADR-0015's vertical ordering only:

- **Transcript and notes are peers.** Final utterances and at most one current partial per audio stream render as ordinary wrapped text rows with timestamp and neutral `You` / `Meeting audio` labels. Overview, decisions, action items, open questions, risks, and follow-ups render in the adjacent notes column. Every factual item links directly to its transcript row; MCP-grounded items also show their external source receipt.
- **The session rail preserves scope.** Persisted meetings are grouped and filterable, a past session opens read-only, and reviewing it never interrupts a running capture. The live session remains marked and one visible action returns to it.
- **Ask is app-level, and still subordinate to the record.** Its dock is collapsed by default and only responds to explicit questions — it never speaks unasked. But it belongs to the app, not to whichever recording is open: it answers from the whole library by default, works on Home and while a capture runs, and narrows to one recording only because the person picked that scope (ADR-0020). Asking about a running recording reads the live timeline and is labelled *so far*. Disabling reasoning leaves transcript and notes review intact.
- **Proposals, when enabled, are inline.** They appear as clearly typed system output near their cited meeting evidence, never as floating toasts and never as meeting fact.
- **Screen evidence is on demand.** Retained frames and inspection provenance are available through cited evidence details; the default review surface does not reserve a spatial canvas for thumbnails.

The shipped product has no Board lens, zoom controls, spatial canvas, or canvas-specific overlay. Historical canvas code and spike evidence may remain until removed deliberately, but the product shell must not construct or expose them.

**Visual calm is a hard requirement.** "Quiet by default" means stable text rows, no duplicate rolling hypotheses, no reflowing card field, and no automatic scroll after the user scrolls away. A visible Follow live action restores automatic movement.

## Stack (decided — pure Rust)

One language, one shipped binary. No Electron, no webview, and no bundled sidecar/IPC boundary.

- **UI: GPUI** (Zed's GPU-accelerated UI framework) + `gpui-component` for the meeting transcript, notes, settings screens, lists, and inputs.
  - GPUI is pre-1.0 with breaking changes between versions: **pin the exact version** in Cargo.toml, upgrade deliberately with an ADR per upgrade, never `*`.
  - Prefer the official crates.io release; fall back to a pinned git revision of the Zed repo if a needed fix isn't released. Avoid unofficial forks unless unavoidable (record as ADR).
  - When docs run out, the reference is the Zed source code — reading it is the expected workflow, not a workaround.
- **Async runtime:** tokio for the pipeline and network. Bridge carefully to GPUI's own executor at the UI boundary (single, well-defined seam: timeline events → UI entities).
- **Capture scope:** every session begins with the user choosing a target — an application or a window — via `SCContentSharingPicker`. That choice builds the `SCContentFilter` for both video and audio, so scope is enforced by the OS rather than by us discarding data afterwards. The chosen target (bundle id, window title) is recorded on the session and is useful context downstream: knowing the session is Zoom versus Keynote is free signal for notes and proposals.
  - **Open question for Spike A:** ScreenCaptureKit scopes *video* per-window/per-application natively. Whether *audio* can be scoped to the target application on our minimum macOS version must be verified, not assumed. If audio remains system-wide, say so plainly in the UI and the ADR — the scope guarantee is then video-only, and Slack pings and Spotify are in the recording.
- **Audio capture** (two separate streams: mic = local participant, target-app audio = remote meeting audio):
  - macOS: ScreenCaptureKit via a small Swift bridge (static lib, FFI). Mic via `cpal` or the same bridge.
  - Windows (later): WASAPI loopback via `cpal`, Windows.Graphics.Capture via `windows` crate, behind the same capture trait.
  - Two streams remain two speaker channels: the mic is the local participant and the captured target is remote meeting audio. This gives deterministic channel attribution without a diarization model and is a reason to keep sessions scoped rather than ambient.
- **Screen capture:** same ScreenCaptureKit session and the same content filter, sampled on significant change with a low-rate fallback; emits `screen.snapshot` timeline events referencing bounded, content-addressed local frames. The initial reasoning context is timestamped transcript, speaker/prosody, and session capture-target metadata — never eager OCR or image bytes. A reasoning pass may request `inspect_screen(timestamp | event_id)` once; only then may Apple Vision OCR run locally, and an image may proceed toward a capable backend only under explicit user opt-in. Inspection returns the requested coordinate, sampled capture time, visible interval, snapshot event id, frame reference, precision, and explicit missing/pruned status. A sampled frame is never described as an exact video frame. Because capture is scoped to a chosen window, there is no exclusion list to maintain — the password manager was never in frame.
- **VAD:** Silero via ONNX Runtime (`ort` crate), per-frame on both streams.
- **ASR:** whisper.cpp via `whisper-rs`, Metal acceleration. Sliding-window partial transcripts: ring buffer, re-transcribe last ~10s every ~500ms, emit partials + finals.
  - **Weights are downloaded on first run, not bundled.** Default to `base.en`; offer `small.en` and `medium.en` for users who will trade latency for accuracy. Confirm the default against the CLI latency bench rather than assuming — the observed misrecognition of "Acme" as "acne" on fixture audio is the kind of thing a larger model fixes and a latency budget may not afford.
  - **Be honest that first launch needs the network.** Everything else about this product is offline, so the one download is worth stating plainly in the UI rather than discovering. Verify integrity, show progress, and make it resumable. After that first fetch the map tier is fully local and needs no key and no connection.
- **Prosody extraction:** pause lengths, interruptions, speech rate, talk-time ratio — emitted as annotations alongside transcript text.
- **Reasoning backend layer:** a narrow Sotto-owned contract with an explicitly enabled experimental Codex-subscription path and optional direct OpenAI Responses API access. The OpenAI connector uses a pinned maintained Rust SDK rather than hand-written SSE. Backend ids are open-ended strings, role selection is independent, calls pin their resolved backend, and cache identity includes connector plus model. SDK and subprocess types never cross into `core`, `insight`, or `advisor`. Codex runs ephemerally in an empty directory with a read-only sandbox, user configuration/rules ignored, known tool features disabled, and tool events rejected; these are defenses, not a claim that the model-visible tool inventory is empty.
- **RAG:** `rusqlite` + sqlite-vec, embeddings via `fastembed-rs`. User documents and past session timelines live in one local SQLite file. Retrieval is in-process with the pipeline: zero IPC hops on the hot path.
- **MCP client:** the official Rust SDK (`rmcp`) stays behind Sotto-owned traits. V1 is resources-first and application-controlled: explicitly enabled text resources are fetched with strict time/size/count limits and normalized into provenance-bearing evidence. The model receives evidence, never MCP credentials or an MCP tool surface. The first supported transport is bounded Streamable HTTP: remote connections require HTTPS outside loopback and credentials live in Keychain. Local stdio remains disabled until T046 proves bounded framing, cancellation, and process-tree cleanup; any later enablement also requires exact-command consent.
- **Key storage:** `keyring` crate → macOS Keychain / Windows Credential Manager.
- **Auto-update:** no electron-updater equivalent exists — build a minimal updater: signed release manifest over HTTPS, download, verify signature, swap .app bundle, relaunch. Keep it boring and auditable.

## Architecture principles

- **Timeline as spine.** Every stage produces into or consumes from the session timeline. New capability = new event kind or new consumer, not new plumbing.
- **Conveyor belt, not request/response.** Event-driven pipeline over tokio broadcast channels: `AudioFrame → VadSegment → PartialTranscript`, with optional proposal stages consuming the same append-only spine. Every stage is independent; notes run after durable finalization, while live proposals never block the record.
- **Notes before interruption.** Optimize first for factual, cited post-call notes. Realtime proposals are a later capability and cannot delay shipping or degrade the meeting record.
- **Quiet by default.** Proposals are off by default and "no proposal" is the common result. They may observe either speaker, arrive at a natural pause, and must be dismissible without mutating the record.
- **Latency budget:** once proposals are enabled, pause detected → first proposal tokens rendering inline within ~1s. Notes have no realtime latency budget.
- **MCP is evidence, not authority.** Sotto chooses and retrieves only session-enabled resources. Meeting-derived search text is never disclosed to an MCP server without an explicit session grant. Resource content is untrusted, prompt-injection-resistant context; all external claims cite known evidence ids. Side-effecting MCP calls are impossible in v1.
- **Transcript first, never audio.** Initial reasoning payloads contain timestamped, stream-labelled transcript with local prosody annotations, e.g. `[meeting audio, hesitant, 2.5s pause] "sure, sounds fine"`. Audio-native realtime APIs are out of scope (cost, provider lock-in, privacy). Screen evidence is a typed second pass for a specific timestamp or event, not ambient prompt decoration.
- **Headless core.** Capture, pipeline, timeline, and intelligence layers compile and run without GPUI (crate boundary). This gives us: a CLI test mode (WAV in → timeline events out), CI without a display server, and a permanently open door to a different UI layer if GPUI ever becomes untenable. The UI depends on the core; the core never depends on the UI.

## Repo structure (cargo workspace)

```
/crates/core           # timeline model, event bus, pipeline orchestration, domain types
/crates/capture        # capture trait + macOS impl (links Swift bridge) + future Windows impl
/crates/capture/bridge-macos  # Swift package: ScreenCaptureKit (audio + frames), exposed via FFI
/crates/asr            # whisper.cpp integration, ring buffer, sliding-window partials
/crates/vad            # Silero/ONNX
/crates/prosody        # annotation extraction
/crates/screen         # bounded change frames, screen.snapshot events, on-demand inspection/OCR
/crates/providers      # reasoning backend identity + Codex/OpenAI connectors
/crates/advisor        # optional realtime proposal path: quiet watcher, grounding, anchored output
/crates/insight        # offline AI path: cited meeting notes, topical clustering, derived views
/crates/rag            # rusqlite + sqlite-vec + fastembed; timeline persistence + ingestion
/crates/mcp            # MCP client wrapper (isolates rmcp)
/crates/app            # GPUI application: transcript + notes workspace, settings, tray, updater
/crates/cli            # headless harness: WAV in → timeline events out, bench + fixtures
/docs                  # ADRs, living version of this file
```

## Phased plan — build in this order

### Phase 0 — Two de-risk spikes (gating decisions, do before everything else)

**Spike A — Capture (highest technical risk):** a throwaway, **signed and notarized** macOS binary that lets the user pick a target application or window via `SCContentSharingPicker`, then captures mic + that target's audio as two streams (plus a low-rate screen frame every 10s) via the Swift/ScreenCaptureKit bridge for 60+ minutes without drift or dropout, and handles the Screen & System Audio Recording permission flow including detecting revoked permission and guiding re-grant. Success criterion: dual-stream WAVs + frame PNGs on disk, correct and in sync, containing only the chosen target. Must also answer whether audio can be scoped to the target application, and what happens when the chosen window closes mid-session. Set up code signing + notarization in CI **now** — capture bugs on unsigned builds waste days.

**Historical Spike B — GPUI canvas:** this proved GPUI could draw the earlier whiteboard concept. The product later cut over to a top-down transcript-and-notes workspace under ADR-0015; canvas performance is no longer a shipping gate. The original spike exercised:
1. A zoomable/pannable canvas that appends fake utterance blocks continuously at 60fps for 30+ minutes (append-only layout, no reflow of existing content), with a proposal card streaming token-by-token anchored to a block.
2. The same content presented through a compact always-on-top, **non-activating** overlay window that (a) stays visible over full-screen Zoom/Meet, (b) never steals keyboard focus, (c) supports click-through toggling.
Its historical result remains evidence about GPUI, not a requirement to preserve the canvas product concept.

### Phase 1 — Pipeline core (headless) + timeline
- `core`: timeline model (event kinds, append-only semantics, supersede references), broadcast-channel bus, SQLite persistence
- `capture` + `vad` + `asr` + `prosody` + `screen`: ring buffers, sliding-window partials, annotations, and bounded change-frame snapshots — all emitting timeline events; no eager OCR
- `cli` harness: WAV (+ frame fixtures) in → timeline events out; latency measurements; CI integration tests
- Minimal GPUI dev window rendering the raw live timeline (first real GPUI code beyond the spike)

### Phase 2 — The map: the first usable tier (works with no API key)
- Plain chronological transcript: wrapped utterance rows per speaker, stable rolling partial replacement, and timestamped evidence navigation
- Session start/stop with the target picker, and post-call review of a persisted timeline
- Timeline → RAG ingestion (past meetings become retrievable meeting memory)
- **This tier ships without any model configured.** Everything above is deterministic and local. Onboarding must not require a key.
- **Gate:** use the transcript on real conversations. The fused timeline must be accurate and readable before advising work begins — if it is not good enough to read, it is not good enough to reason over.
  - Any two-party conversation exercises the whole record tier: two audio streams, speaker attribution, prosody, and retained change frames. A 1:1 with a colleague works — mic is you, the captured app is the meeting audio.

### Phase 3a — AI meeting notes
- `providers`: an explicit experimental Codex CLI subscription path plus optional OpenAI Responses API via a pinned Rust SDK, normalized streaming/cancellation, explicit output capabilities, backend-aware cache identity, and Keychain storage for the API path. `No reasoning` remains first-class; Codex remains off by default and visibly discloses T030's failed isolation result when enabled.
- `insight`: cited structured meeting notes first, then optional topical clustering. Both begin from transcript-only context and may issue one typed, timestamp-addressed screen inspection; inspection evidence is derived and never mutates the timeline.
- The main review workspace presents one transcript-and-notes view over the selected session. Reopening an unchanged meeting uses its cached derived artifact and performs no provider call.

### Phase 3b — MCP context
- `mcp`: explicitly configured servers expose selected text resources through a bounded, read-only, Sotto-controlled context plane.
- Every excerpt carries an opaque evidence id, server id, source URI/title, retrieval time, digest, and truncation status. External evidence citations remain distinguishable from meeting `EventId` citations.
- No MCP source is enabled by default. Local process launch, remote network access, and disclosure of meeting-derived queries are separate visible grants. Missing sources degrade notes honestly; they never erase transcript-only value.

### Phase 3c — Optional proposals
- `advisor`: a quiet watcher decides whether a proposal may help; only then does Sotto retrieve approved RAG/MCP context and call the proposer.
- Proposal kinds are meeting-general: clarifying question, decision check, next step, follow-up, and relevant context. Proposals cite their meeting anchors and any external evidence, never execute actions, and are off by default.
- Cited proposals render inline in the meeting view after the proposal engine is accepted.

### Phase 4 — Product shell
- Proposal history, open-question/action tracking, and tray/menubar presence
- Consent features: visible recording indicator, per-call on/off, configurable consent notice
- Whisper model download on first run (do not bundle weights in installer) with integrity checks and progress UI
- Minimal auto-updater (signed manifest → verify → swap → relaunch)

### Phase 5 — Openness
- Backend-capability-gated, per-user opt-in image attachment for explicit `inspect_screen` requests
- Additional reasoning backends and separately authorized MCP actions only after dedicated contracts and evidence
- Windows: capture-trait implementation (WASAPI + Windows.Graphics.Capture); GPUI Windows support re-evaluated at that time

## Engineering conventions

- Rust stable (latest, as GPUI requires), clippy-clean, `#![deny(warnings)]` in CI; no `unwrap()`/`expect()` outside tests and startup.
- GPUI version pinned exactly; upgrades are deliberate PRs with an ADR noting breaking-change fallout.
- Every pipeline stage gets unit tests; the headless core gets integration tests against WAV fixtures; latency assertions in CI where feasible.
- Timeline schema changes are ADR-worthy: consumers depend on append-only semantics and event-kind stability.
- ADRs in /docs for any deviation from decisions in this file.
- Memory discipline: Whisper model loads lazily, unloads when idle. We share the machine with Zoom — profile regularly; the small-footprint claim is a feature. Screen frames are sampled, referenced, and pruned — never accumulate raw video.
- The core must always build and run headless (`cli` crate is the proof and stays green in CI).
- **Files no gate parses need their own check, named in the task.** `cargo test`, clippy, `fmt` and
  `git diff --check` say nothing about `scripts/Info.plist`, and on 2026-08-16 four green gates
  shipped a malformed one that stopped the app launching. Any edit there runs
  `plutil -lint scripts/Info.plist`. The general rule: when you edit something the Rust gates cannot
  read, say which command does read it, and run that one too.
- **A wall-clock budget is a poor proxy for "did not block."** Two tests failed intermittently on
  2026-08-16 asserting that a non-blocking call returned within 50 ms while the worker it must not
  join slept 200 ms. Under a loaded `--workspace` run the non-blocking path itself exceeded the
  budget. Prefer a deadline loop over a spin count, and where a timing assertion is genuinely the
  clearest expression of the property, make the separation an order of magnitude, not a factor of
  four.

## Explicit non-goals (v1)

- No transcript-only product pretending to be an AI copilot. Structured, cited meeting notes are the minimum reasoning capability; proposals are an additional capability, not a substitute for trustworthy notes.
- **No ambient or always-on capture.** No background listening, no "record my whole day", no auto-start on detecting a meeting. Every session is explicitly started and explicitly scoped by the user. This is a deliberate decision, not a missing feature: ambient capture would record people who never consented (a legal exposure in two-party-consent jurisdictions), break the two-stream speaker model that gives us diarization for free, and trade a defensible wedge for a crowded undifferentiated one. Revisit only via an ADR with evidence.
- No general-purpose ambient personal-recall product. Sotto operates only on deliberately started, scoped meetings.
- No reasoning in `core`. Notes, clustering, and proposal generation all live above the `providers` boundary. If `core` ever needs an API key to do its job, the no-key tier has been lost and the architecture has drifted.
- No model output masquerading as record. Topical clusters and summaries are derived views stored alongside the timeline, never edits to it — a different model, or none, must always be able to reproduce the original.
- No cloud backend, no accounts, no telemetry beyond opt-in crash reports
- No meeting bots that join calls
- No stealth/undetectability features — ever (see Core differentiators #4)
- No audio-native LLM streaming
- No emotion inference on people — no speech-emotion-recognition labels, no facial-expression
  analysis, no per-person affect scores in any record or view. Deterministic prosody facts (pause,
  rate, interruption, pitch, energy) are the permitted layer: verifiable against the recording,
  never a verdict about a person's internal state. The EU AI Act prohibits workplace emotion
  inference, the science on facial inference is unreliable, and an unverifiable emotion label on a
  named colleague breaks the cited-record trust model. Revisit only via an ADR.
- No model-visible MCP tool surface and no side-effecting MCP actions in v1. Read-only annotations supplied by an MCP server are not treated as proof of safety.
- No Anthropic, Google, OpenRouter, Ollama, or other reasoning connector in the v1 product UI. The backend contract stays open so evidence can justify adding one later.
- No whiteboard/canvas editing surface; the meeting artifact is a transcript with cited derived notes
- No mobile app (if a companion app happens later, it talks to exported data, not the core)
- No Windows in v1 (capture stays behind a platform trait; GPUI Windows maturity re-checked then)
- No web version
