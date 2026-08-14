# ADR-0010: Provider-neutral reasoning request shape

- Status: Accepted
- Date: 2026-08-11
- Decision owners: Sotto maintainers

## Context

The original `CompletionRequest` carries text only. T028 established that an initial reasoning
turn receives timestamped transcript/prosody and capture-target metadata, then may request one
sampled screen inspection. Direct OpenAI can constrain output with JSON Schema and accept image
input, but SDK types, local frame paths, and implicit image consent must not cross Sotto's shared
boundary. T030 failed the Codex no-tools isolation gate, so Codex cannot inherit either capability.

Adding fields to `CompletionRequest` would break existing text-only consumers and mocks. Making
the optional `serde_json` dependency mandatory in `core` solely for schema transport would also
weaken the headless core boundary. An initial public consent enum and image constructor in `core`
were rejected during independent review: any safe caller could mint or deserialize that assertion
without passing through screen policy.

## Decision

`core` retains the existing `CompletionRequest` unchanged and adds `ReasoningRequest`, a wrapper
containing only:

- the original text completion;
- one `ReasoningOutput`: `Text`, `JsonObject`, or `JsonSchema`.

`JsonSchemaConstraint` owns a validated schema name, optional description, and JSON text. The
connector parses and validates the JSON before transport. Strict schema dispatch is mandatory;
schema mode never degrades to prompt wording.

`CompletionProvider::stream_reasoning` is source-compatible: existing implementations keep their
`stream` method. Its default implementation delegates ordinary text/legacy JSON-object work and
rejects JSON Schema before transport. A connector must override the method before advertising
constrained schema output.

Image authority lives in `screen`, not `core`. `AuthorizedReasoningImage` owns bounded bytes,
sniffed PNG/JPEG media type, and path-free sampled-frame provenance. It has no public constructor,
no Clone or Serde implementation, and no local-path getter; dispatch takes it out of the inspection
at most once. `RetainedScreenInspector` mints it only after
explicit `ImageInspectionPolicy::Allow`, exact selector/event/frame/interval agreement, canonical
cache-root containment, a bounded 4 MiB read, and byte sniffing. It resolves the path again after
the read so a substituted path does not retain authority.

`providers::ReasoningProvider` accepts the core request and an optional concrete
`AuthorizedReasoningImage`; its default rejects image input before delegation. This creates a
deliberate `providers -> screen` edge while preserving the required absence of `core -> screen`
and `core -> providers` edges. Existing insight constructors keep a text-only adapter; an additive
builder enables authorized image transport without breaking CLI call sites.

The T028 orchestration bridge creates an image only when all of these are true at the final common
boundary immediately before dispatch:

1. the model explicitly requested image evidence;
2. the retained inspector resolves the selected snapshot under explicit local opt-in;
3. request selector, snapshot event, frame reference, visibility interval, and capture time match;
4. the canonical file remains inside the cache root before and after a capped read;
5. byte sniffing recognizes PNG or JPEG within the 4 MiB limit.

Missing, pruned, out-of-range, OCR-only, denied-consent, mismatched, oversized, unreadable, or
unsupported images produce no attachment. The local path is used only inside `screen`; neither
insight nor providers receives it.

### Direct OpenAI

The OpenAI connector maps the Sotto contract through pinned `async-openai` Responses types:

- JSON-object mode becomes `text.format.type = json_object`;
- schema mode becomes `text.format.type = json_schema` with `strict = true`;
- image bytes become one `input_image` data URL next to an `input_text` provenance part;
- tools remain explicitly empty and tool choice remains `none`.

Request-shape tests capture the SDK-generated HTTP body and assert the schema, data URL,
provenance, absence of local paths, and empty tool surface. The connector now advertises
`JsonSchemaOutput` and `ImageInput` in addition to its existing capabilities.
Its connector revision is r3 because the authorization and cache contract changed materially.

### Codex

Codex advertises neither JSON Schema nor image input. Core rejects schema and the providers-owned
advanced default rejects image input without spawning the CLI. T030's failed no-tools verdict
prevents a CLI mapping even if a future
Codex command exposes output-schema or image flags; isolation must first be resolved in a new,
reviewed gate.

## Consequences

- Existing text-only request construction and provider implementations remain source-compatible.
- `core` gains no dependency on `providers`, `screen`, or a provider SDK, keeps JSON parsing
  optional, and carries no consent assertion another caller can forge.
- Initial reasoning remains transcript-only. Image cost and disclosure occur only after the typed
  second pass and explicit opt-in.
- The opaque screen value and SDK request hold image bytes only for dispatch and drop them after
  the request; raw video and local paths do not enter reasoning transport.
- Future connectors can implement the same output/image method and advertise capabilities without
  changing reasoning consumers.

## References

- [OpenAI structured model outputs](https://developers.openai.com/api/docs/guides/structured-outputs)
- [OpenAI images and vision](https://developers.openai.com/api/docs/guides/images-vision)
- [ADR-0008](0008-openai-first-reasoning.md)
- [ADR-0009](0009-transcript-first-screen-context.md)
- [T030 experiment](../experiments/codex-isolation-protocol.md)
