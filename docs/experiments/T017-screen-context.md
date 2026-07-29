# T017 screen-context experiment

Date: 2026-07-29

## Session and method

The checked-in `fixtures/timelines/call-01.jsonl` session was used for all arms. It has five final utterances, two scoped screen snapshots, OCR for a product-overview slide and an enterprise-pricing slide, and corresponding PNG frame references. Prompt assembly was compared in `insight` using the same map prompt and timeline window.

## Results

| Arm | Context added | Measurable result |
| --- | --- | --- |
| Metadata | Capture target plus snapshot app/title | Baseline prompt identifies only the fixture window; it contributes no sales facts beyond the transcript. |
| Metadata + OCR | `Sotto Product Overview`, `Local-first sales call copilot`, `Enterprise Pricing`, and `Migration and support included` at their visible intervals | OCR extracted non-empty, relevant text. It corroborates the Enterprise-plan topic and the spoken resolution that migration and support are included. It does not change the fixture's expected objections, competitor mention, or next steps. |
| Metadata + images | PNG frame references exist, but no image bytes can enter `CompletionRequest` | Not run: the provider-neutral API supports string message content only. `ContextMode::MetadataAndImages` returns `ImageContextUnsupported`, so a frame path cannot be mistaken for a multimodal experiment. |

No API key or locally configured multimodal model was available, and the current provider contract cannot express image parts. Therefore this is prompt/evidence verification, not a claim that three model-generated recaps were compared. That limitation settles the downstream decision for the current architecture: use metadata + OCR for offline summaries; omit images. Re-open image evaluation only after an explicit, opt-in multimodal provider contract exists.

## Verdict for T013

- Include the capture-target metadata once per prompt; its cost is negligible, but on this fixture it added no recap fact.
- Include locally extracted OCR adjacent to its `screen.snapshot` event for offline summaries. It extracted useful characters and corroborated one response.
- Do not send images in the advisor path. Image context is currently unrepresentable, unmeasured, higher-cost, and has a distinct privacy promise.
- Keep Vision for now because its checked-in output was non-empty and relevant, but treat OCR as supporting context and retain timeline citations.
