# Project: Sotto — Local-First Real-Time Sales Call Copilot

*Name: from "sotto voce" — under the voice. The quiet prompt beneath the conversation.*

## What we are building

A desktop app for sales teams that listens to live calls (any meeting app — Zoom, Meet, Teams, dialers), transcribes audio and captures screen context locally, lays the conversation out as a live visual timeline, and places talking points, objection responses, and battlecards *into* that timeline as the call unfolds — grounded in the company's own materials and CRM context.

**Sessions are explicitly started and explicitly scoped.** The user turns Sotto on and picks what to capture — an application or a window — through the system picker. Nothing outside that scope is ever captured. Sotto is not ambient, does not run in the background waiting for a call, and has no always-on mode.

## Two layers: the map, and the reasoning over it

Sotto is built as a **deterministic local layer** with an **optional reasoning layer** on top. This is not an implementation detail — it is the shape of the product.

**The local layer needs no API key and no network.** It captures the chosen target's audio and screen, transcribes on-device, and assembles a *map* of the session: a chronological, append-only timeline of who said what, when, with what prosody, against what was on screen. This layer is deterministic, private, and complete on its own. A user with no credentials configured still gets a reviewable record of their call.

**The reasoning layer makes the map smart.** The user brings a model — Ollama for fully local, or any cloud provider with their own key — and it earns its place by doing what local tooling cannot: organising the map by *topic* rather than only by time, generating recaps, and, live, offering suggestions grounded in the company's own material. MCP extends this with the customer's own connectors and knowledge bases.

The distinction that matters, because it decides what belongs where:

- **Chronological structure is deterministic.** Who spoke, when, for how long, over which screen. No semantics required — local tools produce it exactly, every time.
- **Topical structure requires meaning.** "These six exchanges are the pricing discussion." "This objection echoes one from forty minutes ago." That is the model's job, and it is a *derived view* over the append-only log, never a mutation of it.

Add a model and the board gains a second organising axis — the map becomes closer to a mind map than a transcript. Remove it and the board is still correct, just chronological. Nothing breaks; capability degrades.

Two consequences to hold onto:

1. **The map layer is domain-neutral; the advice layer is not.** Organising a conversation generalises fine. Knowing which moment deserves an interruption does not — restraint requires knowing what matters, which is what keeps the advisor specifically a sales tool. General substrate, specific advice.
2. **`core` must never depend on `providers`.** The crate graph is what enforces the tier boundary. If reasoning code lands in `core`, the no-key tier stops being real.

Core differentiators (do not compromise these):

1. **Local-first.** Audio never leaves the device. Transcription runs on-device (whisper.cpp). Only redacted transcript text is sent to LLM providers.
2. **BYOK (bring your own keys).** Users supply their own API keys (Anthropic, OpenAI, Google, OpenRouter, Ollama for fully local). We never proxy inference through our servers. Keys live in the OS keychain.
3. **Bring your own agents.** MCP client support so companies plug in their own CRM connectors / knowledge bases / agents.
4. **Transparent by design.** NO stealth features. We are an enablement tool (Gong/Balto category), not a concealment tool (Cluely category). Never implement screen-share invisibility, capture-evasion, or anything designed to hide the app from other call participants. Consent handling is a first-class feature.
   **Consent is structural, not a policy.** Capture scope is enforced by the OS content filter, not by us filtering afterwards: if the user picked one window, nothing else is ever in the buffer, so there is nothing to redact, prune, or be trusted about. Prefer the system picker — it is the affordance users already know from screen sharing, and macOS draws its own indicator around the captured window. An allowlist chosen by the user beats any blocklist we maintain.
5. **Lightweight by construction.** Sotto is a single native Rust binary. Small footprint is part of the product's identity ("local-first" should *feel* local-first) — resist dependencies and architecture choices that bloat it.
6. **The board, not a toast.** Suggestions appear anchored in the conversation that triggered them, on a spatial canvas — not as disembodied pop-ups. Context is what makes a suggestion trustworthy.
7. **Useful before it is smart.** The app works with no API key configured: capture, transcribe, and map. Intelligence is an upgrade the user opts into by bringing a model, not a gate on getting any value at all. This is also the honest onboarding path — a rep can run it on one call before anyone asks them for a credential.

## The session timeline (core abstraction)

The canonical data structure of the entire product is the **session timeline**: an append-only, timestamped, heterogeneous event log per call. Everything is a producer into it or a consumer of it.

Event kinds (extend deliberately; all share `{id, session_id, ts, kind, payload}`):
- `utterance.partial` / `utterance.final` — per speaker stream (rep = mic, customer = captured target's audio), text + prosody annotations
- `vad` — speech start/stop per stream
- `prosody` — pauses, interruptions, speech rate, talk-time ratio deltas
- `screen.snapshot` — low-rate frame reference + OCR-extracted text + active-app metadata ("Zoom fullscreen", "slide changed")
- `trigger` — watcher-model classification (competitor mention, pricing question, objection, discovery-gap)
- `suggestion.partial` / `suggestion.final` — advising-layer output, **anchored to the event(s) that triggered it**
- `annotation.user` — rep's own marks/notes on the board

The **session record** carries what the timeline is *of*: start and end wall-clock, and the capture target the user chose (bundle id, window title). A timeline without a recorded scope is not reproducible and cannot be explained to the person in it.

Rules:
- **Append-only.** Corrections (partial → final) are new events referencing the superseded id, never mutations. Layout and consumers rely on this.
- **Both partials and finals are first-class** from day one. The map tier only needs finals; the live copilot needs partials. The schema never assumes batch.
- **Persistence:** timelines land in SQLite (same file as RAG). Past timelines are ingested into RAG — every recorded call makes future advising smarter about that account ("what did they object to last time?" is answerable for free).
- Consumers: (a) live board UI, (b) advising layer, (c) post-call summarizer, (d) topical clustering, (e) RAG ingester. All read the same spine.
- **Derived views never mutate the log.** Topical clusters, themes and cross-references produced by a model are projections *over* the timeline, stored alongside it and recomputable from it. A model's opinion is not a fact about what happened, and the record of what happened has to survive being reinterpreted — including by a different model, or none.

## UI model: the whiteboard

**Starting a session is a deliberate, two-step act:** turn Sotto on, pick the target. The picker is the system's, not ours. While a session runs, the indicator shows *what* is being captured — the target's name, and audio versus screen distinctly — not merely that something is. Stopping is always one obvious action away, and the app never resumes a session on its own.

The conversation renders as a **spatial, zoomable, live-appending canvas**:

- Utterance blocks flow along the time axis, colored per speaker; prosody is visible spatially (a long pause literally reads as a gap; interruptions overlap).
- Suggestions bud off the utterance/region that triggered them — the rep sees *why* each card exists.
- Screen snapshots pin as thumbnails to the stretch of conversation they were visible for.
- Topic regions may cluster (pricing discussion accumulates as a zone); unresolved objections remain visually *open* — a spatial to-do the rep can glance at.
- After the call, the board **is** the meeting artifact: reviewing = panning a map of the conversation, not scrubbing a transcript.

**Two lenses, one model:**
- **Board lens** — the full canvas. Shines on a second monitor / large screen and for post-call review.
- **Overlay lens** — a compact always-on-top, non-activating panel for single-screen calls. It is a *viewport onto the board's newest edge* — same data, same objects, zoomed in. Never a separate UI with separate state.

**Visual calm is a hard requirement.** "Quiet by default" applies to pixels: append-only layout that never reflows what the user already saw, no jumping, new objects arrive gently at the frontier. The rep glances; the rep never *watches*. Layout is incremental and stable by construction (the append-only timeline makes this tractable — exploit it).

## Stack (decided — pure Rust)

One language, one binary. No Electron, no webview, no sidecar/IPC boundary.

- **UI: GPUI** (Zed's GPU-accelerated UI framework) + `gpui-component` for standard widgets (settings screens, lists, inputs). The whiteboard canvas is the reason GPUI is the right tool: a zoomable, smoothly-scrolling, constantly-appending scene with hundreds of live objects is custom GPU-accelerated rendering — GPUI's exact strength.
  - GPUI is pre-1.0 with breaking changes between versions: **pin the exact version** in Cargo.toml, upgrade deliberately with an ADR per upgrade, never `*`.
  - Prefer the official crates.io release; fall back to a pinned git revision of the Zed repo if a needed fix isn't released. Avoid unofficial forks unless unavoidable (record as ADR).
  - When docs run out, the reference is the Zed source code — reading it is the expected workflow, not a workaround.
- **Async runtime:** tokio for the pipeline and network. Bridge carefully to GPUI's own executor at the UI boundary (single, well-defined seam: timeline events → UI entities).
- **Capture scope:** every session begins with the user choosing a target — an application or a window — via `SCContentSharingPicker`. That choice builds the `SCContentFilter` for both video and audio, so scope is enforced by the OS rather than by us discarding data afterwards. The chosen target (bundle id, window title) is recorded on the session and is useful context downstream: knowing the session is Zoom versus Keynote is free signal for the board and the advisor.
  - **Open question for Spike A:** ScreenCaptureKit scopes *video* per-window/per-application natively. Whether *audio* can be scoped to the target application on our minimum macOS version must be verified, not assumed. If audio remains system-wide, say so plainly in the UI and the ADR — the scope guarantee is then video-only, and Slack pings and Spotify are in the recording.
- **Audio capture** (two separate streams: mic = rep, target-app audio = customer):
  - macOS: ScreenCaptureKit via a small Swift bridge (static lib, FFI). Mic via `cpal` or the same bridge.
  - Windows (later): WASAPI loopback via `cpal`, Windows.Graphics.Capture via `windows` crate, behind the same capture trait.
  - Two streams remain two speakers: the mic is the rep, the captured target is the other party. This is what gives us speaker attribution without a diarization model, and it is a reason to keep sessions scoped rather than ambient.
- **Screen capture:** same ScreenCaptureKit session and the same content filter, low-rate (0.1–0.2 fps or on significant change); local OCR via Apple Vision; emits `screen.snapshot` timeline events. Screen context is core to the timeline from Phase 1 — not a later add-on. Sending *images* to LLMs remains opt-in per user (cost); OCR text flows by default. Because capture is scoped to a chosen window, there is no exclusion list to maintain — the password manager was never in frame.
- **VAD:** Silero via ONNX Runtime (`ort` crate), per-frame on both streams.
- **ASR:** whisper.cpp via `whisper-rs`, Metal acceleration. Sliding-window partial transcripts: ring buffer, re-transcribe last ~10s every ~500ms, emit partials + finals.
  - **Weights are downloaded on first run, not bundled.** Default to `base.en`; offer `small.en` and `medium.en` for users who will trade latency for accuracy. Confirm the default against the CLI latency bench rather than assuming — the observed misrecognition of "Acme" as "acne" on fixture audio is the kind of thing a larger model fixes and a latency budget may not afford.
  - **Be honest that first launch needs the network.** Everything else about this product is offline, so the one download is worth stating plainly in the UI rather than discovering. Verify integrity, show progress, and make it resumable. After that first fetch the map tier is fully local and needs no key and no connection.
- **Prosody extraction:** pause lengths, interruptions, speech rate, talk-time ratio — emitted as annotations alongside transcript text.
- **LLM provider layer:** hand-rolled abstraction over Anthropic / OpenAI / Google / OpenRouter / Ollama. `reqwest` + SSE streaming; speculative calls as tokio tasks with abort handles; prompt caching for static context. No heavy framework — this is ~500 lines we control.
- **RAG:** `rusqlite` + sqlite-vec, embeddings via `fastembed-rs`. Battlecards, product docs, account notes, and past session timelines — all in one local SQLite file. Retrieval is in-process with the pipeline: zero IPC hops on the hot path.
- **MCP client:** official Rust SDK (`rmcp`). It is younger than the TypeScript SDK — budget extra time for OAuth flows some servers need; isolate MCP behind our own trait so SDK churn doesn't leak.
- **Key storage:** `keyring` crate → macOS Keychain / Windows Credential Manager.
- **Auto-update:** no electron-updater equivalent exists — build a minimal updater: signed release manifest over HTTPS, download, verify signature, swap .app bundle, relaunch. Keep it boring and auditable.

## Architecture principles

- **Timeline as spine.** Every stage produces into or consumes from the session timeline. New capability = new event kind or new consumer, not new plumbing.
- **Conveyor belt, not request/response.** Event-driven pipeline over tokio broadcast channels: `AudioFrame → VadSegment → PartialTranscript → Trigger → Suggestion`, all as timeline events. Every stage is an independent task consuming upstream partials and emitting its own partials. No stage waits for the previous to "finish."
- **Speculative execution.** Start suggestion LLM calls on partial transcripts while the customer is still talking; cancel (abort handle) and re-fire if the final transcript changes meaning. Aggressiveness is a user setting (their tokens, their tradeoff).
- **Two-tier models.** A cheap/fast watcher model classifies every partial ("suggestion warranted? trigger type?"); the larger model is called only on trigger, with battlecard context pre-retrieved in parallel.
- **Quiet by default.** A copilot that fires constantly gets closed. The watcher should say "no suggestion" most of the time. Bias suggestions to fire during the *customer's* speaking turns (rep reads while listening); stay quiet during the rep's own turns. Visually: calm, stable, append-at-the-frontier rendering.
- **Latency budget:** pause detected → first suggestion tokens rendering on the board within ~1s. Everything is designed backward from this.
- **Text as the LLM payload, not audio.** Prosody is recovered via local annotations inline in the transcript, e.g. `[customer, hesitant, 2.5s pause] "sure, sounds fine"`. Audio-native realtime APIs are out of scope (cost, provider lock-in, privacy).
- **Headless core.** Capture, pipeline, timeline, and intelligence layers compile and run without GPUI (crate boundary). This gives us: a CLI test mode (WAV in → timeline events out), CI without a display server, and a permanently open door to a different UI layer if GPUI ever becomes untenable. The UI depends on the core; the core never depends on the UI.

## Repo structure (cargo workspace)

```
/crates/core           # timeline model, event bus, pipeline orchestration, domain types
/crates/capture        # capture trait + macOS impl (links Swift bridge) + future Windows impl
/crates/capture/bridge-macos  # Swift package: ScreenCaptureKit (audio + frames), exposed via FFI
/crates/asr            # whisper.cpp integration, ring buffer, sliding-window partials
/crates/vad            # Silero/ONNX
/crates/prosody        # annotation extraction
/crates/screen         # frame sampling, Apple Vision OCR, screen.snapshot events
/crates/providers      # LLM provider abstraction (streaming, cancellation, caching)
/crates/advisor        # realtime LLM path: watcher/suggester loop, triggers, speculative execution
/crates/insight        # offline LLM path: topical clustering, post-call summaries, derived views
/crates/rag            # rusqlite + sqlite-vec + fastembed; timeline persistence + ingestion
/crates/mcp            # MCP client wrapper (isolates rmcp)
/crates/app            # GPUI application: board canvas, overlay lens, settings, tray, updater
/crates/cli            # headless harness: WAV in → timeline events out, bench + fixtures
/docs                  # ADRs, living version of this file
```

## Phased plan — build in this order

### Phase 0 — Two de-risk spikes (gating decisions, do before everything else)

**Spike A — Capture (highest technical risk):** a throwaway, **signed and notarized** macOS binary that lets the user pick a target application or window via `SCContentSharingPicker`, then captures mic + that target's audio as two streams (plus a low-rate screen frame every 10s) via the Swift/ScreenCaptureKit bridge for 60+ minutes without drift or dropout, and handles the Screen & System Audio Recording permission flow including detecting revoked permission and guiding re-grant. Success criterion: dual-stream WAVs + frame PNGs on disk, correct and in sync, containing only the chosen target. Must also answer whether audio can be scoped to the target application, and what happens when the chosen window closes mid-session. Set up code signing + notarization in CI **now** — capture bugs on unsigned builds waste days.

**Spike B — GPUI canvas (gates the UI decision):** a GPUI app proving the whiteboard is buildable:
1. A zoomable/pannable canvas that appends fake utterance blocks continuously at 60fps for 30+ minutes (append-only layout, no reflow of existing content), with a suggestion card streaming token-by-token anchored to a block.
2. The same content presented through a compact always-on-top, **non-activating** overlay window that (a) stays visible over full-screen Zoom/Meet, (b) never steals keyboard focus, (c) supports click-through toggling.
Timebox: 7 days.
- **Pass →** GPUI is confirmed; proceed.
- **Fail (canvas perf or NSPanel behaviors GPUI doesn't expose) →** record an ADR and fall back to a thin Electron shell (canvas via WebGL/2D) over the same headless core. The core is identical either way — this spike only decides who draws pixels. Do not exceed the timebox.

### Phase 1 — Pipeline core (headless) + timeline
- `core`: timeline model (event kinds, append-only semantics, supersede references), broadcast-channel bus, SQLite persistence
- `capture` + `vad` + `asr` + `prosody` + `screen`: ring buffers, sliding-window partials, annotations, OCR snapshots — all emitting timeline events
- `cli` harness: WAV (+ frame fixtures) in → timeline events out; latency measurements; CI integration tests
- Minimal GPUI dev window rendering the raw live timeline (first real GPUI code beyond the spike)

### Phase 2 — The map: the first usable tier (works with no API key)
- Board canvas v1: utterance blocks per speaker, prosody-as-space, screen thumbnails pinned to their interval
- Session start/stop with the target picker, and post-call review of a persisted timeline
- Timeline → RAG ingestion (past calls become retrievable account memory)
- **This tier ships without any model configured.** Everything above is deterministic and local. Onboarding must not require a key.
- **Gate:** use the board on real conversations. The fused timeline must be accurate and *readable* before advising work begins — if it isn't good enough to read, it isn't good enough to reason over. This is a product bar, not only a dogfood checkpoint.
  - **It does not have to be a sales call.** Capture is scoped to any window or application, so any two-party conversation exercises the whole map tier: two audio streams, speaker attribution, prosody, screen frames, OCR, board layout. A 1:1 with a colleague works — mic is you, the captured app is them. Only the *advisor* needs real sales calls, because trigger types and battlecard grounding are the sales-specific parts. Do not let the map tier's validation wait on access to a sales pipeline.

### Phase 3 — The reasoning layer (offline first)
- `providers`: streaming + cancellation + keychain storage; GPUI settings screens for keys/model selection (use gpui-component), including the no-key and Ollama paths
- `insight`: post-call summaries and **topical clustering** — the second organising axis that turns a chronological timeline into a mind-map-like view. No latency budget, so this is where screen-context and prompt-shape questions get settled cheaply before the realtime path inherits them.
- Derived views render on the board as an optional overlay the user can turn off, falling back to pure chronology

### Phase 3b — The realtime loop
- `advisor`: two-tier watcher/suggester loop with speculative calls; trigger types v1: competitor mention, pricing question, objection, discovery-gap
- `rag`: battlecard/doc ingestion, sqlite-vec retrieval (docs + past timelines), prompt assembly with cached static prefix
- Suggestions land on the board anchored to their triggering events; overlay lens shows the board's newest edge

### Phase 4 — Product shell
- Production overlay lens (from Spike B learnings), suggestion history, open-objection tracking, tray/menubar presence
- Consent features: visible recording indicator, per-call on/off, configurable consent notice
- Whisper model download on first run (do not bundle weights in installer) with integrity checks and progress UI
- Minimal auto-updater (signed manifest → verify → swap → relaunch)

### Phase 5 — Openness
- `mcp`: user-configured servers feed context into the suggestion prompt
- Opt-in image context to LLMs (screen frames, not just OCR text)
- Windows: capture-trait implementation (WASAPI + Windows.Graphics.Capture); GPUI Windows support re-evaluated at that time

## Engineering conventions

- Rust stable (latest, as GPUI requires), clippy-clean, `#![deny(warnings)]` in CI; no `unwrap()`/`expect()` outside tests and startup.
- GPUI version pinned exactly; upgrades are deliberate PRs with an ADR noting breaking-change fallout.
- Every pipeline stage gets unit tests; the headless core gets integration tests against WAV fixtures; latency assertions in CI where feasible.
- Timeline schema changes are ADR-worthy: consumers depend on append-only semantics and event-kind stability.
- ADRs in /docs for any deviation from decisions in this file.
- Memory discipline: Whisper model loads lazily, unloads when idle. We share the machine with Zoom — profile regularly; the small-footprint claim is a feature. Screen frames are sampled, referenced, and pruned — never accumulate raw video.
- The core must always build and run headless (`cli` crate is the proof and stays green in CI).

## Explicit non-goals (v1)

- No *marketing* of a standalone note-taker. The map tier is a real product tier — it ships, it works without a key, and onboarding runs through it — but Sotto is positioned and sold as a sales copilot. We are not entering the Granola/Otter/Fathom category; we are making the substrate useful so the copilot has somewhere to stand.
- **No ambient or always-on capture.** No background listening, no "record my whole day", no auto-start on detecting a meeting. Every session is explicitly started and explicitly scoped by the user. This is a deliberate decision, not a missing feature: ambient capture would record people who never consented (a legal exposure in two-party-consent jurisdictions), break the two-stream speaker model that gives us diarization for free, and trade a defensible wedge for a crowded undifferentiated one. Revisit only via an ADR with evidence.
- No general-purpose personal-recall product — the map layer is domain-neutral by construction and that is deliberate, but the advice layer stays specifically a sales tool. Domain specificity is what makes "quiet by default" computable: restraint requires knowing what matters.
- No reasoning in `core`. Summarization, clustering and suggestion all live above the `providers` boundary. If `core` ever needs an API key to do its job, the no-key tier has been lost and the architecture has drifted.
- No model output masquerading as record. Topical clusters and summaries are derived views stored alongside the timeline, never edits to it — a different model, or none, must always be able to reproduce the original.
- No cloud backend, no accounts, no telemetry beyond opt-in crash reports
- No meeting bots that join calls
- No stealth/undetectability features — ever (see Core differentiators #4)
- No audio-native LLM streaming
- No freeform infinite-whiteboard editing (Miro-style) — the board is conversation-generated with light user annotation, not a drawing tool
- No mobile app (if a companion app happens later, it talks to exported data, not the core)
- No Windows in v1 (capture stays behind a platform trait; GPUI Windows maturity re-checked then)
- No web version