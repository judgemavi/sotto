# ADR-0006: Provisional screen context for reasoning

- Status: Accepted provisionally
- Date: 2026-07-29
- Decision owners: Sotto maintainers

## Context

T017 compared three prompt-context arms against the checked-in `call-01` timeline: capture-target and snapshot metadata; metadata plus locally extracted OCR; and metadata plus images. The fixture is about 30 seconds of synthetic speech with two synthetic slides, so it verifies plumbing but does not represent dense, noisy screen content from a real call.

Metadata identified the scoped application and window but added no recap fact. OCR produced relevant text, including the product overview and enterprise-pricing content, and corroborated the spoken migration-and-support resolution. It did not change the fixture's expected objections, competitor mention, or next steps. The image arm could not run: the frozen provider-neutral `CompletionRequest` accepts text only. This is absence of capability, not evidence that images have no value.

## Decision

Offline reasoning may include capture metadata and local OCR adjacent to the corresponding `screen.snapshot`, with timeline citations retained. OCR is supporting context and must not override spoken claims. Keep the local Vision OCR path while this assumption is evaluated.

Do not put images into advisor prompts or represent frame paths as if they were image content. A multimodal `CompletionRequest` is a frozen-core contract change and requires its own ADR, including the distinct privacy and cost implications of sending a captured screen off-device. Do not hard-wire screen context into the realtime advisor based on this fixture.

## Consequences

The summarizer and topical clustering can cheaply use scoped metadata and OCR while remaining text-only. The recommendation is provisional: current evidence shows useful characters and correct prompt assembly, not that OCR consistently earns its tokens on real calls.

Run the same three-way comparison on a real, explicitly scoped two-party session during the Phase 2 dogfood gate. Until then, metadata plus OCR is a working assumption and image value is unmeasured.

## Revisit if

- A real-session comparison shows OCR is redundant, noisy, or misleading relative to its token cost.
- A provider-neutral, opt-in multimodal request contract is proposed.
- The privacy promise or provider cost of screen context changes materially.
