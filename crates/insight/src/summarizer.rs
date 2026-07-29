use std::{collections::HashSet, sync::Arc, time::Duration};

use futures_util::StreamExt;
use rag::Store;
use serde::{Deserialize, Serialize};
use sotto_core::{
    CancellationToken, CompletionMessage, CompletionProvider, CompletionRequest, EventId,
    EventPayload, MessageRole, ProviderError, SessionId, TimelineEvent, Usage,
};
use thiserror::Error;

const MAP_PROMPT: &str = include_str!("../../../prompts/summary/v1-map.md");
const REDUCE_PROMPT: &str = include_str!("../../../prompts/summary/v1-reduce.md");
const WINDOW: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ContextMode {
    #[default]
    Metadata,
    MetadataAndOcr,
    MetadataAndImages,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Pricing {
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
    pub cache_read_per_million_usd: f64,
    pub cache_write_per_million_usd: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Cost {
    pub usd: f64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Claim {
    pub text: String,
    pub citations: Vec<EventId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Attendee {
    pub name: String,
    pub role: String,
    pub citations: Vec<EventId>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TalkTime {
    pub rep_percent: u8,
    pub customer_percent: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Objection {
    pub text: String,
    pub resolved: bool,
    pub resolution: Option<String>,
    pub citations: Vec<EventId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Commitment {
    pub text: String,
    pub owner: String,
    pub citations: Vec<EventId>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Recap {
    pub attendees: Vec<Attendee>,
    pub talk_time: TalkTime,
    pub topics: Vec<Claim>,
    pub customer_questions: Vec<Claim>,
    pub objections: Vec<Objection>,
    pub commitments: Vec<Commitment>,
    pub next_steps: Vec<Commitment>,
    pub competitor_mentions: Vec<Claim>,
}

impl Recap {
    fn citation_groups(&self) -> impl Iterator<Item = &[EventId]> {
        self.attendees
            .iter()
            .map(|item| item.citations.as_slice())
            .chain(self.topics.iter().map(|item| item.citations.as_slice()))
            .chain(
                self.customer_questions
                    .iter()
                    .map(|item| item.citations.as_slice()),
            )
            .chain(self.objections.iter().map(|item| item.citations.as_slice()))
            .chain(
                self.commitments
                    .iter()
                    .map(|item| item.citations.as_slice()),
            )
            .chain(self.next_steps.iter().map(|item| item.citations.as_slice()))
            .chain(
                self.competitor_mentions
                    .iter()
                    .map(|item| item.citations.as_slice()),
            )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SummaryReport {
    pub recap: Recap,
    pub usage: Usage,
    /// Estimated provider charge, or `None` when model pricing was not configured.
    pub cost: Option<Cost>,
    pub model: String,
    pub calls: usize,
}

#[derive(Debug, Error)]
pub enum SummaryError {
    #[error(transparent)]
    Persistence(#[from] sotto_core::RagError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("provider returned invalid recap JSON: {0}")]
    InvalidOutput(#[from] serde_json::Error),
    #[error("recap claim has no timeline citation")]
    MissingCitation,
    #[error("recap cites unknown timeline event {0:?}")]
    UnknownCitation(EventId),
    #[error("image context is unsupported by the provider-neutral text completion contract")]
    ImageContextUnsupported,
    #[error("persisted session contains no final utterances")]
    EmptyTimeline,
}

pub struct Summarizer<'a> {
    store: &'a Store,
    provider: Arc<dyn CompletionProvider>,
    pricing: Option<Pricing>,
    context_mode: ContextMode,
}

impl<'a> Summarizer<'a> {
    #[must_use]
    pub fn new(store: &'a Store, provider: Arc<dyn CompletionProvider>) -> Self {
        Self {
            store,
            provider,
            pricing: None,
            context_mode: ContextMode::default(),
        }
    }

    #[must_use]
    pub const fn with_pricing(mut self, pricing: Pricing) -> Self {
        self.pricing = Some(pricing);
        self
    }

    #[must_use]
    pub const fn with_context(mut self, context_mode: ContextMode) -> Self {
        self.context_mode = context_mode;
        self
    }

    pub async fn summarize(&self, session_id: SessionId) -> Result<SummaryReport, SummaryError> {
        if self.context_mode == ContextMode::MetadataAndImages {
            return Err(SummaryError::ImageContextUnsupported);
        }
        let session = self.store.load_session_record(session_id)?;
        let events = self.store.load_session(session_id)?;
        if !events
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::UtteranceFinal(_)))
        {
            return Err(SummaryError::EmptyTimeline);
        }
        let mut usage = Usage::default();
        let windows = windows(&events);
        let mut partials = Vec::with_capacity(windows.len());
        for window in &windows {
            let target = session.capture_target();
            let input = format!(
                "Capture target: app={} window={}\n\n{}",
                target.display_name,
                target.window_title.as_deref().unwrap_or("unknown"),
                render_window(window, self.context_mode)
            );
            let (recap, call_usage) = self.complete(MAP_PROMPT, input).await?;
            add_usage(&mut usage, call_usage);
            partials.push(recap);
        }
        let calls;
        let recap = if partials.len() == 1 {
            calls = 1;
            partials.pop().ok_or(SummaryError::EmptyTimeline)?
        } else {
            let input = serde_json::to_string(&partials)?;
            let (recap, call_usage) = self.complete(REDUCE_PROMPT, input).await?;
            add_usage(&mut usage, call_usage);
            calls = partials.len() + 1;
            recap
        };
        validate_citations(&recap, &events)?;
        Ok(SummaryReport {
            recap,
            usage,
            cost: self.pricing.map(|pricing| cost(usage, pricing)),
            model: self.provider.model_id().to_owned(),
            calls,
        })
    }

    async fn complete(&self, system: &str, input: String) -> Result<(Recap, Usage), SummaryError> {
        let request = CompletionRequest {
            model: self.provider.model_id().to_owned(),
            system: Some(system.to_owned()),
            messages: vec![CompletionMessage {
                role: MessageRole::User,
                content: input,
                cache_boundary: false,
            }],
            max_tokens: Some(4_096),
            temperature: Some(0.0),
            stop: Vec::new(),
        };
        let mut stream = self
            .provider
            .stream(request, CancellationToken::new())
            .await?;
        let mut output = String::new();
        let mut usage = Usage::default();
        while let Some(delta) = stream.next().await {
            let delta = delta?;
            output.push_str(&delta.text);
            if let Some(final_usage) = delta.usage {
                usage = final_usage;
            }
        }
        Ok((serde_json::from_str(strip_fence(&output))?, usage))
    }
}

fn windows(events: &[TimelineEvent]) -> Vec<Vec<&TimelineEvent>> {
    let max = events
        .iter()
        .filter_map(|event| match event.payload() {
            EventPayload::UtteranceFinal(utterance) => Some(utterance.start),
            _ => None,
        })
        .max()
        .unwrap_or_default();
    let count = usize::try_from(max.as_secs() / WINDOW.as_secs())
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    (0..count)
        .filter_map(|index| {
            let start = WINDOW.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX));
            let end = start.saturating_add(WINDOW);
            let window: Vec<_> = events
                .iter()
                .filter(|event| match event.payload() {
                    EventPayload::UtteranceFinal(utterance) => {
                        utterance.start >= start && utterance.start < end
                    }
                    EventPayload::ScreenSnapshot(snapshot) => {
                        snapshot.visible_from < end
                            && snapshot.visible_to.is_none_or(|until| until >= start)
                    }
                    _ => false,
                })
                .collect();
            window
                .iter()
                .any(|event| matches!(event.payload(), EventPayload::UtteranceFinal(_)))
                .then_some(window)
        })
        .collect()
}

fn render_window(events: &[&TimelineEvent], context: ContextMode) -> String {
    let mut lines = Vec::new();
    for event in events {
        match event.payload() {
            EventPayload::UtteranceFinal(utterance) => lines.push(format!(
                "[event:{} at:{:.1}s] {}",
                event.id().get(),
                utterance.start.as_secs_f64(),
                utterance.render_inline()
            )),
            EventPayload::ScreenSnapshot(snapshot) => {
                let mut line = format!(
                    "[event:{} screen at:{:.1}s app={} window={}]",
                    event.id().get(),
                    snapshot.visible_from.as_secs_f64(),
                    snapshot.active_app.as_deref().unwrap_or("unknown"),
                    snapshot.window_title.as_deref().unwrap_or("unknown")
                );
                if context == ContextMode::MetadataAndOcr && !snapshot.ocr_text.trim().is_empty() {
                    line.push_str(" OCR: ");
                    line.push_str(snapshot.ocr_text.trim());
                }
                lines.push(line);
            }
            _ => {}
        }
    }
    lines.join("\n")
}

fn validate_citations(recap: &Recap, events: &[TimelineEvent]) -> Result<(), SummaryError> {
    let ids: HashSet<_> = events.iter().map(TimelineEvent::id).collect();
    for citations in recap.citation_groups() {
        if citations.is_empty() {
            return Err(SummaryError::MissingCitation);
        }
        for citation in citations {
            if !ids.contains(citation) {
                return Err(SummaryError::UnknownCitation(*citation));
            }
        }
    }
    Ok(())
}

fn add_usage(total: &mut Usage, increment: Usage) {
    total.input_tokens = total.input_tokens.saturating_add(increment.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(increment.output_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(increment.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(increment.cache_write_tokens);
}

fn cost(usage: Usage, pricing: Pricing) -> Cost {
    let usd = (f64::from(usage.input_tokens) * pricing.input_per_million_usd
        + f64::from(usage.output_tokens) * pricing.output_per_million_usd
        + f64::from(usage.cache_read_tokens) * pricing.cache_read_per_million_usd
        + f64::from(usage.cache_write_tokens) * pricing.cache_write_per_million_usd)
        / 1_000_000.0;
    Cost { usd }
}

fn strip_fence(output: &str) -> &str {
    let trimmed = output.trim();
    trimmed
        .strip_prefix("```json")
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed)
}
