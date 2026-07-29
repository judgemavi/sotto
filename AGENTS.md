# Project: Sotto — Local-First Real-Time Sales Call Copilot

*Name: from "sotto voce" — under the voice. The quiet prompt beneath the conversation.*

## What we are building

A desktop app for sales teams that listens to live calls (any meeting app — Zoom, Meet, Teams, dialers), transcribes locally, and surfaces real-time talking points, objection responses, and battlecards grounded in the company's own materials and CRM context.

Core differentiators (do not compromise these):

1. **Local-first.** Audio never leaves the device. Transcription runs on-device (whisper.cpp). Only redacted transcript text is sent to LLM providers.
2. **BYOK (bring your own keys).** Users supply their own API keys (Anthropic, OpenAI, Google, OpenRouter, Ollama for fully local). We never proxy inference through our servers. Keys live in the OS keychain.
3. **Bring your own agents.** MCP client support so companies plug in their own CRM connectors / knowledge bases / agents.
4. **Transparent by design.** NO stealth features. We are an enablement tool (Gong/Balto category), not a concealment tool (Cluely category). Never implement screen-share invisibility, capture-evasion, or anything designed to hide the app from other call participants. Consent handling is a first-class feature.
5. **Lightweight by construction.** Sotto is a single native Rust binary. Small footprint is part of the product's identity ("local-first" should *feel* local-first) — resist dependencies and architecture choices that bloat it.

## Stack (decided — pure Rust)

One language, one binary. No Electron, no webview, no sidecar/IPC boundary.

- **UI: GPUI** (Zed's GPU-accelerated UI framework) + `gpui-component` for standard widgets (settings screens, lists, inputs).
  - GPUI is pre-1.0 with breaking changes between versions: **pin the exact version** in Cargo.toml, upgrade deliberately with an ADR per upgrade, never `*`.
  - Prefer the official crates.io release; fall back to a pinned git revision of the Zed repo if a needed fix isn't released. Avoid unofficial forks unless unavoidable (record as ADR).
  - When docs run out, the reference is the Zed source code — reading it is the expected workflow, not a workaround.
- **Async runtime:** tokio for the pipeline and network. Bridge carefully to GPUI's own executor at the UI boundary (single, well-defined seam: pipeline events → UI entities).
- **Audio capture** (two separate streams: mic = rep, system audio = customer):
  - macOS: ScreenCaptureKit via a small Swift bridge (static lib, FFI). Mic via `cpal` or the same bridge.
  - Windows (later): WASAPI loopback via `cpal`, Windows.Graphics.Capture via `windows` crate, behind the same capture trait.
- **VAD:** Silero via ONNX Runtime (`ort` crate), per-frame on both streams.
- **ASR:** whisper.cpp via `whisper-rs`, Metal acceleration. Sliding-window partial transcripts: ring buffer, re-transcribe last ~10s every ~500ms, emit partials + finals.
- **Prosody extraction:** pause lengths, interruptions, speech rate, talk-time ratio — emitted as annotations alongside transcript text.
- **LLM provider layer:** hand-rolled abstraction over Anthropic / OpenAI / Google / OpenRouter / Ollama. `reqwest` + SSE streaming; speculative calls as tokio tasks with abort handles; prompt caching for static context. No heavy framework — this is ~500 lines we control.
- **RAG:** `rusqlite` + sqlite-vec, embeddings via `fastembed-rs`. Battlecards, product docs, account notes, transcripts — all in one local SQLite file. Retrieval is in-process with the pipeline: zero IPC hops on the hot path.
- **MCP client:** official Rust SDK (`rmcp`). It is younger than the TypeScript SDK — budget extra time for OAuth flows some servers need; isolate MCP behind our own trait so SDK churn doesn't leak.
- **Key storage:** `keyring` crate → macOS Keychain / Windows Credential Manager.
- **Auto-update:** no electron-updater equivalent exists — build a minimal updater: signed release manifest over HTTPS, download, verify signature, swap .app bundle, relaunch. Keep it boring and auditable.
- **Screen context (later phase):** low-rate frame capture (0.1–0.2 fps or on-change) via the same ScreenCaptureKit session; local OCR via Apple Vision to extract on-screen text into RAG context; sending images to LLMs is opt-in per user (cost).

## Architecture principles

- **Conveyor belt, not request/response.** Event-driven pipeline over tokio broadcast channels: `AudioFrame → VadSegment → PartialTranscript → Trigger → Suggestion`. Every stage is an independent task consuming upstream partials and emitting its own partials. No stage waits for the previous to "finish."
- **Speculative execution.** Start suggestion LLM calls on partial transcripts while the customer is still talking; cancel (abort handle) and re-fire if the final transcript changes meaning. Aggressiveness is a user setting (their tokens, their tradeoff).
- **Two-tier models.** A cheap/fast watcher model classifies every partial ("suggestion warranted? trigger type?"); the larger model is called only on trigger, with battlecard context pre-retrieved in parallel.
- **Quiet by default.** A copilot that fires constantly gets closed. The watcher should say "no suggestion" most of the time. Bias suggestions to fire during the *customer's* speaking turns (rep reads while listening); stay quiet during the rep's own turns.
- **Latency budget:** pause detected → first suggestion tokens rendering within ~1s. Everything is designed backward from this.
- **Text as the LLM payload, not audio.** Prosody is recovered via local annotations inline in the transcript, e.g. `[customer, hesitant, 2.5s pause] "sure, sounds fine"`. Audio-native realtime APIs are out of scope (cost, provider lock-in, privacy).
- **Headless core.** The pipeline + intelligence layers compile and run without GPUI (feature flag / separate crate boundary). This gives us: a CLI test mode (WAV in → events out), CI without a display server, and a permanently open door to a different UI layer if GPUI ever becomes untenable. The UI depends on the core; the core never depends on the UI.

## Repo structure (cargo workspace)

```
/crates/core           # pipeline orchestration, event bus, domain types
/crates/capture        # capture trait + macOS impl (links Swift bridge) + future Windows impl
/crates/capture/bridge-macos  # Swift package: ScreenCaptureKit, exposed via FFI
/crates/asr            # whisper.cpp integration, ring buffer, sliding-window partials
/crates/vad            # Silero/ONNX
/crates/prosody        # annotation extraction
/crates/providers      # LLM provider abstraction (streaming, cancellation, caching)
/crates/rag            # rusqlite + sqlite-vec + fastembed
/crates/mcp            # MCP client wrapper (isolates rmcp)
/crates/app            # GPUI application: overlay panel, settings, tray, updater
/crates/cli            # headless harness: WAV in → events out, bench + fixtures
/docs                  # ADRs, living version of this file
```

## Phased plan — build in this order

### Phase 0 — Two de-risk spikes (gating decisions, do before everything else)

**Spike A — Capture (highest technical risk):** a throwaway, **signed and notarized** macOS binary that captures mic + system audio as two streams via the Swift/ScreenCaptureKit bridge for 60+ minutes without drift or dropout, and handles the Screen & System Audio Recording permission flow including detecting revoked permission and guiding re-grant. Success criterion: dual-stream WAVs on disk, correct and in sync. Set up code signing + notarization in CI **now** — capture bugs on unsigned builds waste days.

**Spike B — GPUI overlay (gates the UI decision):** a GPUI app that renders a small always-on-top, **non-activating** panel that (a) stays visible over full-screen Zoom/Meet, (b) never steals keyboard focus from the meeting app, (c) streams fake suggestion text token-by-token smoothly, (d) supports click-through toggling. Timebox: 5 days.
- **Pass →** GPUI is confirmed for v1; proceed.
- **Fail (fighting NSPanel behaviors GPUI doesn't expose) →** record an ADR and fall back to a thin Electron shell over the same headless core. The core architecture is identical either way — this spike only decides who draws pixels. Do not spend more than the timebox trying to force it.

### Phase 1 — Pipeline core (headless)
- `core` + `capture` + `vad` + `asr` + `prosody`: ring buffers, sliding-window partials, annotations, broadcast-channel event bus
- `cli` harness: WAV fixtures in → transcript events out; latency measurements; CI integration tests
- Minimal GPUI dev window rendering the raw live transcript (first real GPUI code beyond the spike)

### Phase 2 — Intelligence loop
- `providers`: streaming + cancellation + keychain storage; GPUI settings screens for keys/model selection (use gpui-component)
- Two-tier watcher/suggester loop with speculative calls
- `rag`: battlecard/doc ingestion, sqlite-vec retrieval, prompt assembly with cached static prefix
- Trigger types v1: competitor mention, pricing question, objection, discovery-gap

### Phase 3 — Product shell
- Production overlay panel (from Spike B learnings): suggestion streaming UI, suggestion history, tray/menubar presence
- Consent features: visible recording indicator, per-call on/off, configurable consent notice
- Whisper model download on first run (do not bundle weights in installer) with integrity checks and progress UI
- Minimal auto-updater (signed manifest → verify → swap → relaunch)

### Phase 4 — Openness
- `mcp`: user-configured servers feed context into the suggestion prompt
- Screen context via OCR (opt-in)
- Windows: capture-trait implementation (WASAPI + Windows.Graphics.Capture); GPUI Windows support to be re-evaluated at that time

## Engineering conventions

- Rust stable (latest, as GPUI requires), clippy-clean, `#![deny(warnings)]` in CI; no `unwrap()`/`expect()` outside tests and startup.
- GPUI version pinned exactly; upgrades are deliberate PRs with an ADR noting breaking-change fallout.
- Every pipeline stage gets unit tests; the headless core gets integration tests against WAV fixtures; latency assertions in CI where feasible.
- ADRs in /docs for any deviation from decisions in this file.
- Memory discipline: Whisper model loads lazily, unloads when idle. We share the machine with Zoom — profile regularly; the small-footprint claim is a feature.
- The core must always build and run headless (`cli` crate is the proof and stays green in CI).

## Explicit non-goals (v1)

- No cloud backend, no accounts, no telemetry beyond opt-in crash reports
- No meeting bots that join calls
- No stealth/undetectability features — ever (see Core differentiators #4)
- No audio-native LLM streaming
- No mobile app (if a companion app happens later, it talks to exported data, not the core)
- No Windows in v1 (capture stays behind a platform trait; GPUI Windows maturity re-checked then)
- No web version
