use std::{collections::HashSet, hash::Hasher, sync::Arc};

use futures_util::StreamExt;
use rag::Store;
use serde::{Deserialize, Serialize};
use sotto_core::{
    CancellationToken, CompletionMessage, CompletionProvider, CompletionRequest, EventId,
    EventPayload, MessageRole, ProviderError, SessionId, TimelineEvent, Usage,
};
use thiserror::Error;

const PROMPT: &str = include_str!("../../../../prompts/clustering/v1.md");
const VIEW_KIND: &str = "topical_clusters.v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TopicRegion {
    pub label: String,
    pub event_ids: Vec<EventId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TopicLink {
    pub from: EventId,
    pub to: EventId,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenThreadKind {
    Question,
    Objection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenThread {
    pub kind: OpenThreadKind,
    pub event_id: EventId,
    pub summary: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DerivedView {
    pub regions: Vec<TopicRegion>,
    pub links: Vec<TopicLink>,
    pub open_threads: Vec<OpenThread>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClusterReport {
    pub view: DerivedView,
    pub usage: Usage,
    pub model: String,
    pub cached: bool,
}

#[derive(Debug, Error)]
pub enum ClusterError {
    #[error(transparent)]
    Persistence(#[from] sotto_core::RagError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("provider returned invalid clustering JSON: {0}")]
    InvalidOutput(#[from] serde_json::Error),
    #[error("derived view references unknown timeline event {0:?}")]
    UnknownEvent(EventId),
    #[error("derived view contains an empty topic region")]
    EmptyRegion,
    #[error("persisted session contains no final utterances")]
    EmptyTimeline,
}

pub struct Clusterer<'a> {
    store: &'a Store,
    provider: Arc<dyn CompletionProvider>,
}

impl<'a> Clusterer<'a> {
    #[must_use]
    pub fn new(store: &'a Store, provider: Arc<dyn CompletionProvider>) -> Self {
        Self { store, provider }
    }

    pub async fn cluster(&self, session_id: SessionId) -> Result<ClusterReport, ClusterError> {
        let events = self.store.load_session(session_id)?;
        let input = render_timeline(&events)?;
        // Prompt wording is part of the model input. Include it so ordinary in-place
        // prompt iteration cannot silently reuse an artifact produced by older instructions.
        let content_hash = clustering_content_hash(PROMPT, &input);
        let model = self.provider.model_id().to_owned();
        if let Some((artifact, usage)) =
            self.store
                .load_derived_view(session_id, VIEW_KIND, &model, &content_hash)?
        {
            let view = serde_json::from_str(&artifact)?;
            let usage = serde_json::from_str(&usage)?;
            validate(&view, &events)?;
            return Ok(ClusterReport {
                view,
                usage,
                model,
                cached: true,
            });
        }

        let request = CompletionRequest {
            model: model.clone(),
            system: Some(PROMPT.to_owned()),
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
            if let Some(value) = delta.usage {
                usage = value;
            }
        }
        let view: DerivedView = serde_json::from_str(strip_fence(&output))?;
        validate(&view, &events)?;
        self.store.save_derived_view(
            session_id,
            VIEW_KIND,
            &model,
            &content_hash,
            &serde_json::to_string(&view)?,
            &serde_json::to_string(&usage)?,
        )?;
        Ok(ClusterReport {
            view,
            usage,
            model,
            cached: false,
        })
    }
}

fn render_timeline(events: &[TimelineEvent]) -> Result<String, ClusterError> {
    let lines: Vec<_> = events
        .iter()
        .filter_map(|event| match event.payload() {
            EventPayload::UtteranceFinal(utterance) => Some(format!(
                "[event:{} at:{:.1}s] {}",
                event.id().get(),
                event.ts().as_secs_f64(),
                utterance.render_inline()
            )),
            EventPayload::ScreenSnapshot(snapshot) => Some(format!(
                "[event:{} screen at:{:.1}s app={} window={} OCR: {}]",
                event.id().get(),
                event.ts().as_secs_f64(),
                snapshot.active_app.as_deref().unwrap_or("unknown"),
                snapshot.window_title.as_deref().unwrap_or("unknown"),
                snapshot.ocr_text.trim()
            )),
            _ => None,
        })
        .collect();
    if !events
        .iter()
        .any(|event| matches!(event.payload(), EventPayload::UtteranceFinal(_)))
    {
        return Err(ClusterError::EmptyTimeline);
    }
    Ok(lines.join("\n"))
}

fn validate(view: &DerivedView, events: &[TimelineEvent]) -> Result<(), ClusterError> {
    let ids: HashSet<_> = events.iter().map(TimelineEvent::id).collect();
    for region in &view.regions {
        if region.event_ids.is_empty() {
            return Err(ClusterError::EmptyRegion);
        }
        for id in &region.event_ids {
            ensure_known(*id, &ids)?;
        }
    }
    for link in &view.links {
        ensure_known(link.from, &ids)?;
        ensure_known(link.to, &ids)?;
    }
    for thread in &view.open_threads {
        ensure_known(thread.event_id, &ids)?;
    }
    Ok(())
}

fn ensure_known(id: EventId, ids: &HashSet<EventId>) -> Result<(), ClusterError> {
    if ids.contains(&id) {
        Ok(())
    } else {
        Err(ClusterError::UnknownEvent(id))
    }
}

fn stable_hash(bytes: &[u8]) -> String {
    struct Fnv(u64);
    impl Hasher for Fnv {
        fn finish(&self) -> u64 {
            self.0
        }
        fn write(&mut self, bytes: &[u8]) {
            for byte in bytes {
                self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
        }
    }
    let mut hash = Fnv(0xcbf29ce484222325);
    hash.write(bytes);
    format!("{:016x}", hash.finish())
}

fn clustering_content_hash(prompt: &str, timeline: &str) -> String {
    // Length-prefix the fields rather than relying on a separator that either input could
    // contain. VIEW_KIND and model remain independent dimensions in the database key.
    let material = format!("{}:{prompt}{}:{timeline}", prompt.len(), timeline.len());
    stable_hash(material.as_bytes())
}

fn strip_fence(output: &str) -> &str {
    let trimmed = output.trim();
    trimmed
        .strip_prefix("```json")
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::clustering_content_hash;

    #[test]
    fn editing_prompt_text_invalidates_content_hash() {
        let timeline = "[event:1 at:0.0s] customer: pricing?";
        assert_ne!(
            clustering_content_hash("cluster conservatively", timeline),
            clustering_content_hash("cluster very conservatively", timeline)
        );
    }
}
