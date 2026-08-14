use std::{collections::HashSet, hash::Hasher, sync::Arc};

use providers::{BackendFingerprint, ReasoningProvider, text_reasoning_provider};
use rag::Store;
use screen::ScreenInspectionSource;
use serde::{Deserialize, Serialize};
use sotto_core::{
    CompletionProvider, EventId, EventPayload, ProviderError, SessionId, TimelineEvent, Usage,
};
use thiserror::Error;

use crate::context::{ReasoningContextError, complete_with_optional_inspection, render_transcript};

const PROMPT: &str = include_str!("../../../../prompts/clustering/v1.md");
const VIEW_KIND: &str = "topical_clusters.v2";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TopicRegion {
    pub label: String,
    pub event_ids: Vec<EventId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TopicLink {
    pub from: EventId,
    pub to: EventId,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenThreadKind {
    Question,
    Decision,
    ActionItem,
    Risk,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenThread {
    pub kind: OpenThreadKind,
    pub event_id: EventId,
    pub summary: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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
    #[error("derived-view caching requires a resolved reasoning backend fingerprint")]
    MissingBackendFingerprint,
    #[error(transparent)]
    Context(#[from] ReasoningContextError),
}

pub struct Clusterer<'a> {
    store: &'a Store,
    provider: Arc<dyn ReasoningProvider>,
    backend_fingerprint: Option<BackendFingerprint>,
    screen_inspector: Option<Arc<dyn ScreenInspectionSource>>,
}

impl<'a> Clusterer<'a> {
    #[must_use]
    pub fn new(store: &'a Store, provider: Arc<dyn CompletionProvider>) -> Self {
        Self {
            store,
            provider: text_reasoning_provider(provider),
            backend_fingerprint: None,
            screen_inspector: None,
        }
    }

    /// Replaces the text-compatible adapter with an image-capable reasoning transport.
    #[must_use]
    pub fn with_reasoning_provider(mut self, provider: Arc<dyn ReasoningProvider>) -> Self {
        self.provider = provider;
        self
    }

    #[must_use]
    pub fn with_backend_fingerprint(mut self, fingerprint: BackendFingerprint) -> Self {
        self.backend_fingerprint = Some(fingerprint);
        self
    }

    #[must_use]
    pub fn with_screen_inspector(mut self, inspector: Arc<dyn ScreenInspectionSource>) -> Self {
        self.screen_inspector = Some(inspector);
        self
    }

    pub async fn cluster(&self, session_id: SessionId) -> Result<ClusterReport, ClusterError> {
        let backend_fingerprint = self
            .backend_fingerprint
            .as_ref()
            .ok_or(ClusterError::MissingBackendFingerprint)?;
        let session = self.store.load_session_record(session_id)?;
        let events = self.store.load_session(session_id)?;
        let input = render_timeline(session.capture_target(), &events)?;
        // Prompt wording is part of the model input. Include it so ordinary in-place
        // prompt iteration cannot silently reuse an artifact produced by older instructions.
        let content_hash = clustering_content_hash(PROMPT, &input);
        let model = self.provider.model_id().to_owned();
        if let Some((artifact, usage)) = self.store.load_derived_view(
            session_id,
            VIEW_KIND,
            backend_fingerprint.as_str(),
            &content_hash,
        )? {
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

        let result = complete_with_optional_inspection::<DerivedView>(
            self.provider.as_ref(),
            PROMPT,
            input,
            &events,
            self.screen_inspector.as_deref(),
        )
        .await?;
        let view = result.value;
        let usage = result.usage;
        validate(&view, &events)?;
        self.store.save_derived_view(
            session_id,
            VIEW_KIND,
            backend_fingerprint.as_str(),
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

fn render_timeline(
    target: &sotto_core::CaptureTarget,
    events: &[TimelineEvent],
) -> Result<String, ClusterError> {
    if !events
        .iter()
        .any(|event| matches!(event.payload(), EventPayload::UtteranceFinal(_)))
    {
        return Err(ClusterError::EmptyTimeline);
    }
    Ok(render_transcript(target, events))
}

fn validate(view: &DerivedView, events: &[TimelineEvent]) -> Result<(), ClusterError> {
    // The initial prompt renders final utterances only. A session-known VAD, partial,
    // prosody, snapshot, or system-output id is not evidence the model received and
    // therefore cannot be cited. Screen-inspection evidence remains derived and needs
    // its own explicit provenance contract before an inspection id can enter this set.
    let ids: HashSet<_> = events
        .iter()
        .filter_map(|event| {
            matches!(event.payload(), EventPayload::UtteranceFinal(_)).then_some(event.id())
        })
        .collect();
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

#[cfg(test)]
mod tests {
    use super::clustering_content_hash;

    #[test]
    fn editing_prompt_text_invalidates_content_hash() {
        let timeline = "[event:1 at:0.0s] meeting audio: which launch date?";
        assert_ne!(
            clustering_content_hash("cluster conservatively", timeline),
            clustering_content_hash("cluster very conservatively", timeline)
        );
    }
}
