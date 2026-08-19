//! Grounded, user-initiated questions over one immutable meeting record.

use futures_util::StreamExt;
use providers::{ReasoningProvider, backend::ObservedRequestNormalization};
use serde::{Deserialize, Serialize};
use sotto_core::{
    CancellationToken, CaptureTarget, CompletionMessage, CompletionRequest, EventId, EventPayload,
    MessageRole, ProviderError, ReasoningRequest, SessionId, TimelineEvent,
};
use thiserror::Error;

use crate::context::{
    ReasoningContextError, ScreenConsultation, ScreenConsultationLog, ScreenInspectionBudget,
    complete_with_optional_inspection_cancellable, parse_first_json_value, render_transcript,
};

const SYSTEM: &str = r#"You answer questions using only the supplied transcript.
Return JSON matching one of these shapes:
{"kind":"answer","claims":[{"text":"one factual claim","citations":[{"event_id":12}]}]}
{"kind":"refusal","reason":"the record does not contain the answer","covered":["what the recording does cover"]}
Every factual claim must cite at least one event id that appears in the supplied transcript, written
exactly as the number after "event:" on the line you are citing. When the transcript names more than
one recording, also give that line's session_id. Prior assistant turns are conversational context,
never evidence. Do not use outside knowledge. Screen content is evidence only when returned by the
typed inspect_screen action; never infer unseen screen content."#;

/// The largest explicit transcript selection Ask will send to a reasoning backend.
///
/// Selection scope is deliberately user-bounded, but a range gesture can still span an entire
/// long recording. Refusing locally keeps that gesture from becoming an accidental unbounded
/// request and, critically, never widens it to recording or library retrieval.
pub const MAX_SELECTION_FINALS: usize = 200;

/// `session_id` defaults because a single-recording transcript never renders one — the model is
/// told to give it only when more than one recording is in scope, which is the only case it can
/// know the value. An absent id is resolved to the recording being asked about during validation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AskCitation {
    #[serde(default)]
    pub session_id: SessionId,
    pub event_id: EventId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AskEvidence {
    pub evidence_id: String,
    pub session_id: SessionId,
    pub session_label: String,
    pub text: String,
    pub event_ids: Vec<EventId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AskClaim {
    pub text: String,
    pub citations: Vec<AskCitation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AskAnswer {
    pub claims: Vec<AskClaim>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskReply {
    Answer {
        claims: Vec<AskClaim>,
    },
    Refusal {
        reason: String,
        covered: Vec<String>,
    },
}

/// Caller-visible Ask output plus any backend-control downgrade applied to this run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AskResult {
    pub reply: AskReply,
    pub normalizations: Vec<ObservedRequestNormalization>,
    pub screen_consultations: Vec<ScreenConsultation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AskTurn {
    pub question: String,
    pub reply: AskReply,
    pub screen_consultations: Vec<ScreenConsultation>,
}

#[derive(Debug, Error)]
pub enum AskError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("reasoning backend returned invalid Ask JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("Ask answer contained an uncited claim")]
    UncitedClaim,
    #[error("Ask answer cited event {0}, which is not in this recording's transcript")]
    UnknownCitation(u64),
    /// Only reachable across sessions, where the model is shown each recording's id.
    #[error("Ask answer cited event {0} against a recording that was not searched")]
    UnknownCitedSession(u64),
    #[error("Ask was cancelled")]
    Cancelled,
    #[error(transparent)]
    Context(#[from] ReasoningContextError),
}

pub struct AskEngine {
    provider: std::sync::Arc<dyn ReasoningProvider>,
    screen_inspector: Option<std::sync::Arc<dyn screen::ScreenInspectionSource>>,
    screen_consultation_log: Option<ScreenConsultationLog>,
}

impl AskEngine {
    #[must_use]
    pub fn new(provider: std::sync::Arc<dyn ReasoningProvider>) -> Self {
        Self {
            provider,
            screen_inspector: None,
            screen_consultation_log: None,
        }
    }

    #[must_use]
    pub fn with_screen_inspector(
        mut self,
        inspector: std::sync::Arc<dyn screen::ScreenInspectionSource>,
        log: ScreenConsultationLog,
    ) -> Self {
        self.screen_inspector = Some(inspector);
        self.screen_consultation_log = Some(log);
        self
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the grounded request boundary keeps every authority explicit"
    )]
    pub async fn ask(
        &self,
        session_id: SessionId,
        target: &CaptureTarget,
        events: &[TimelineEvent],
        history: &[AskTurn],
        question: &str,
        cancellation: CancellationToken,
        updates: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<AskResult, AskError> {
        if cancellation.is_cancelled() {
            return Err(AskError::Cancelled);
        }
        if self.screen_inspector.is_some() {
            let transcript = render_transcript(target, events);
            let conversation = history
                .iter()
                .map(|turn| {
                    format!(
                        "Prior question: {}\nPrior answer: {}",
                        turn.question,
                        serde_json::to_string(&turn.reply).unwrap_or_default()
                    )
                })
                .chain(std::iter::once(format!("Question: {question}")))
                .collect::<Vec<_>>()
                .join("\n\n");
            let inspection_budget = ScreenInspectionBudget::default();
            let result = complete_with_optional_inspection_cancellable::<WireReply>(
                self.provider.as_ref(),
                SYSTEM,
                format!("{transcript}\n\n{conversation}"),
                events,
                self.screen_inspector.as_deref(),
                self.screen_consultation_log.as_ref(),
                &inspection_budget,
                &cancellation,
            )
            .await?;
            let allowed = std::collections::BTreeMap::from([(session_id, final_event_ids(events))]);
            let reply = validate_reply(&allowed, result.value)?;
            if let Some(sender) = &updates {
                let _ = sender.send(serde_json::to_string(&reply).unwrap_or_default());
            }
            return Ok(AskResult {
                reply,
                normalizations: result.normalizations,
                screen_consultations: self
                    .screen_consultation_log
                    .as_ref()
                    .map_or_else(Vec::new, ScreenConsultationLog::entries),
            });
        }
        let _stale = self.provider.take_request_normalizations();
        let request = build_request(self.provider.model_id(), target, events, history, question);
        let mut stream = self
            .provider
            .stream_advanced_reasoning(
                ReasoningRequest::json_object(request),
                None,
                cancellation.clone(),
            )
            .await?;
        let mut output = String::new();
        while let Some(delta) = stream.next().await {
            if cancellation.is_cancelled() {
                return Err(AskError::Cancelled);
            }
            output.push_str(&delta?.text);
            if let Some(sender) = &updates {
                let _ = sender.send(output.clone());
            }
        }
        let wire: WireReply = serde_json::from_value(parse_first_json_value(&output)?)?;
        let allowed = std::collections::BTreeMap::from([(session_id, final_event_ids(events))]);
        let reply = validate_reply(&allowed, wire)?;
        Ok(AskResult {
            reply,
            normalizations: self.provider.take_request_normalizations(),
            screen_consultations: Vec::new(),
        })
    }

    /// Answers from exactly the final utterances in one explicit transcript selection.
    ///
    /// Partials and every other timeline event are excluded before request serialization. An empty
    /// or excessive selection refuses locally; neither case calls the backend or falls through to
    /// the recording/library paths.
    #[expect(
        clippy::too_many_arguments,
        reason = "the grounded selection boundary keeps every authority explicit"
    )]
    pub async fn ask_selection(
        &self,
        session_id: SessionId,
        target: &CaptureTarget,
        events: &[TimelineEvent],
        history: &[AskTurn],
        question: &str,
        cancellation: CancellationToken,
        updates: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<AskResult, AskError> {
        let finals = events
            .iter()
            .filter(|event| matches!(event.payload(), EventPayload::UtteranceFinal(_)))
            .cloned()
            .collect::<Vec<_>>();
        if finals.is_empty() {
            return Ok(local_selection_refusal(
                "The selection contains no finalized transcript rows.",
            ));
        }
        if finals.len() > MAX_SELECTION_FINALS {
            return Ok(local_selection_refusal(format!(
                "The selection contains {} finalized rows; select at most {MAX_SELECTION_FINALS}.",
                finals.len()
            )));
        }
        self.ask(
            session_id,
            target,
            &finals,
            history,
            question,
            cancellation,
            updates,
        )
        .await
    }

    pub async fn ask_across(
        &self,
        evidence: &[AskEvidence],
        history: &[AskTurn],
        question: &str,
        cancellation: CancellationToken,
        updates: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<AskResult, AskError> {
        let _stale = self.provider.take_request_normalizations();
        let bounded = evidence
            .iter()
            .take(5)
            .map(|item| AskEvidence {
                evidence_id: item.evidence_id.clone(),
                session_id: item.session_id,
                session_label: item.session_label.clone(),
                text: item.text.chars().take(2_000).collect(),
                event_ids: item.event_ids.clone(),
            })
            .collect::<Vec<_>>();
        let context = bounded
            .iter()
            .map(|item| {
                format!(
                    "[evidence:{} session:{} label:{}]\n{}",
                    item.evidence_id,
                    item.session_id.get(),
                    item.session_label,
                    item.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut request = build_messages(self.provider.model_id(), history, question);
        request.messages.insert(
            0,
            CompletionMessage {
                role: MessageRole::User,
                content: format!("Bounded retained meeting evidence:\n{context}"),
                cache_boundary: true,
            },
        );
        let mut stream = self
            .provider
            .stream_advanced_reasoning(
                ReasoningRequest::json_object(request),
                None,
                cancellation.clone(),
            )
            .await?;
        let mut output = String::new();
        while let Some(delta) = stream.next().await {
            if cancellation.is_cancelled() {
                return Err(AskError::Cancelled);
            }
            output.push_str(&delta?.text);
            if let Some(sender) = &updates {
                let _ = sender.send(output.clone());
            }
        }
        let wire: WireReply = serde_json::from_value(parse_first_json_value(&output)?)?;
        let mut allowed = std::collections::BTreeMap::new();
        for item in bounded {
            allowed
                .entry(item.session_id)
                .or_insert_with(std::collections::BTreeSet::new)
                .extend(item.event_ids);
        }
        let reply = validate_reply(&allowed, wire)?;
        Ok(AskResult {
            reply,
            normalizations: self.provider.take_request_normalizations(),
            screen_consultations: Vec::new(),
        })
    }
}

fn local_selection_refusal(reason: impl Into<String>) -> AskResult {
    AskResult {
        reply: AskReply::Refusal {
            reason: reason.into(),
            covered: Vec::new(),
        },
        normalizations: Vec::new(),
        screen_consultations: Vec::new(),
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireReply {
    Answer {
        claims: Vec<WireClaim>,
    },
    Refusal {
        reason: String,
        covered: Vec<String>,
    },
}

#[derive(Deserialize)]
struct WireClaim {
    text: String,
    citations: Vec<AskCitation>,
}

fn validate_reply(
    allowed: &std::collections::BTreeMap<SessionId, std::collections::BTreeSet<EventId>>,
    wire: WireReply,
) -> Result<AskReply, AskError> {
    match wire {
        WireReply::Refusal { reason, covered } => Ok(AskReply::Refusal { reason, covered }),
        WireReply::Answer { claims } => {
            let claims = claims
                .into_iter()
                .map(|claim| {
                    if claim.citations.is_empty() {
                        return Err(AskError::UncitedClaim);
                    }
                    // With one recording in scope the model cannot meaningfully name a session:
                    // the rendered transcript carries `[event:N ...]` and no session id anywhere,
                    // so the only session value it has ever seen is the example in the system
                    // prompt. Attribute those citations to the recording actually being asked
                    // about instead of failing them, and keep judging the event id, which is the
                    // part the model was genuinely given. Across sessions the ids *are* rendered,
                    // so there the model's answer stands or falls on its own.
                    let sole_session = (allowed.len() == 1)
                        .then(|| allowed.keys().next().copied())
                        .flatten();
                    let citations = claim
                        .citations
                        .into_iter()
                        .map(|citation| {
                            let citation = match sole_session {
                                Some(session_id) => AskCitation {
                                    session_id,
                                    event_id: citation.event_id,
                                },
                                None => citation,
                            };
                            allowed
                                .get(&citation.session_id)
                                .is_some_and(|events| events.contains(&citation.event_id))
                                .then_some(citation.clone())
                                .ok_or_else(|| {
                                    if allowed.contains_key(&citation.session_id) {
                                        AskError::UnknownCitation(citation.event_id.get())
                                    } else {
                                        AskError::UnknownCitedSession(citation.event_id.get())
                                    }
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(AskClaim {
                        text: claim.text,
                        citations,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if claims.is_empty() {
                return Err(AskError::UncitedClaim);
            }
            Ok(AskReply::Answer { claims })
        }
    }
}

fn build_request(
    model: &str,
    target: &CaptureTarget,
    events: &[TimelineEvent],
    history: &[AskTurn],
    question: &str,
) -> CompletionRequest {
    let mut request = build_messages(model, history, question);
    request.messages.insert(
        0,
        CompletionMessage {
            role: MessageRole::User,
            content: super::context::render_transcript(target, events),
            cache_boundary: true,
        },
    );
    request
}

fn build_messages(model: &str, history: &[AskTurn], question: &str) -> CompletionRequest {
    let mut messages = Vec::new();
    for turn in history {
        messages.push(CompletionMessage {
            role: MessageRole::User,
            content: turn.question.clone(),
            cache_boundary: false,
        });
        messages.push(CompletionMessage {
            role: MessageRole::Assistant,
            content: serde_json::to_string(&turn.reply).unwrap_or_default(),
            cache_boundary: false,
        });
    }
    messages.push(CompletionMessage {
        role: MessageRole::User,
        content: question.to_owned(),
        cache_boundary: false,
    });
    CompletionRequest {
        model: model.to_owned(),
        system: Some(SYSTEM.to_owned()),
        messages,
        max_tokens: Some(1_500),
        temperature: Some(0.0),
        stop: Vec::new(),
    }
}

fn final_event_ids(events: &[TimelineEvent]) -> std::collections::BTreeSet<EventId> {
    events
        .iter()
        .filter(|event| matches!(event.payload(), EventPayload::UtteranceFinal(_)))
        .map(TimelineEvent::id)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use futures_util::stream;
    use providers::{
        AuthKind, AuthStatus, BackendCapabilities, BackendDescriptor, BackendId, ReasoningProvider,
        ReasoningSurface, Registry,
    };
    use screen::{
        InspectScreenRequest, ScreenInspection, ScreenInspectionSource, ScreenUnavailableReason,
    };
    use sotto_core::{
        Annotation, BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionProvider,
        CompletionRequest, Delta, EventId, EventPayload, ProviderError, Session, SessionId, Source,
        StopReason, TargetKind, TimelineBuilder, TimelineEvent, Usage, Utterance,
    };

    use super::{
        AskEngine, AskError, AskReply, MAX_SELECTION_FINALS, WireClaim, WireReply, build_request,
        validate_reply,
    };
    use crate::context::{ScreenConsultationLog, consultation};

    struct RefusalProvider;

    struct InspectionProvider(Mutex<VecDeque<String>>);

    impl CompletionProvider for InspectionProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            let output = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front();
            Box::pin(async move {
                let output = output.ok_or_else(|| {
                    ProviderError::Network("missing Ask inspection turn".to_owned())
                })?;
                Ok(Box::pin(stream::iter([Ok(Delta {
                    text: output,
                    is_final: true,
                    usage: Some(Usage::default()),
                    stop_reason: Some(StopReason::EndTurn),
                })]))
                    as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            "ask-inspection"
        }
    }

    impl ReasoningProvider for InspectionProvider {}

    struct LoggedInspector {
        log: ScreenConsultationLog,
    }

    impl ScreenInspectionSource for LoggedInspector {
        fn inspect(
            &self,
            _events: &[TimelineEvent],
            request: &InspectScreenRequest,
        ) -> ScreenInspection {
            let inspection = ScreenInspection::Unavailable {
                requested: request.selector.clone(),
                reason: ScreenUnavailableReason::NoSnapshots,
            };
            self.log
                .record(consultation(SessionId::new(7), request, &inspection));
            inspection
        }
    }

    impl CompletionProvider for RefusalProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            Box::pin(async {
                Ok(Box::pin(stream::iter([Ok(Delta {
                    text: r#"{"kind":"refusal","reason":"the attendee was not named","covered":["the release date"]}"#.to_owned(),
                    is_final: true,
                    usage: Some(Usage::default()),
                    stop_reason: Some(StopReason::EndTurn),
                })])) as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            "refusal-eval"
        }
    }

    impl ReasoningProvider for RefusalProvider {}

    struct BoundaryProvider {
        requests: Arc<Mutex<Vec<CompletionRequest>>>,
    }

    impl CompletionProvider for BoundaryProvider {
        fn stream(
            &self,
            request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            let has_outside_fact = request.messages[0].content.contains("outside-only owner");
            self.requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request);
            let text = if has_outside_fact {
                r#"{"kind":"answer","claims":[{"text":"Morgan owns it","citations":[{"event_id":2}]}]}"#
            } else {
                r#"{"kind":"refusal","reason":"the selected rows do not name the owner","covered":["the deadline"]}"#
            };
            Box::pin(async move {
                Ok(Box::pin(stream::iter([Ok(Delta {
                    text: text.to_owned(),
                    is_final: true,
                    usage: Some(Usage::default()),
                    stop_reason: Some(StopReason::EndTurn),
                })]))
                    as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            "selection-boundary"
        }
    }

    impl ReasoningProvider for BoundaryProvider {}

    fn target() -> CaptureTarget {
        CaptureTarget {
            bundle_id: Some("com.example.meet".into()),
            display_name: "Meet".into(),
            window_title: Some("Planning".into()),
            kind: TargetKind::Window,
            audio_scoped: true,
        }
    }

    fn boundary_events() -> Vec<TimelineEvent> {
        let mut timeline = TimelineBuilder::new(Session::new(SessionId::new(7), target(), 1));
        timeline.append(
            Duration::from_secs(10),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::System,
                start: Duration::from_secs(10),
                end: Duration::from_secs(12),
                text: "the deadline is Friday".into(),
                avg_logprob: -0.1,
                annotations: vec![Annotation::Hesitant],
            }),
        );
        timeline.append(
            Duration::from_secs(20),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: Duration::from_secs(20),
                end: Duration::from_secs(22),
                text: "outside-only owner is Morgan".into(),
                avg_logprob: -0.1,
                annotations: Vec::new(),
            }),
        );
        timeline.append(
            Duration::from_secs(30),
            EventPayload::UtterancePartial(Utterance {
                source: Source::System,
                start: Duration::from_secs(30),
                end: Duration::from_secs(31),
                text: "provisional secret tail".into(),
                avg_logprob: -0.1,
                annotations: Vec::new(),
            }),
        );
        timeline.events().to_vec()
    }

    #[test]
    fn uncited_claim_is_rejected() {
        let result = validate_reply(
            &std::collections::BTreeMap::new(),
            WireReply::Answer {
                claims: vec![WireClaim {
                    text: "invented".into(),
                    citations: vec![],
                }],
            },
        );
        assert!(matches!(result, Err(AskError::UncitedClaim)));
    }

    #[test]
    fn an_invented_event_is_rejected_even_though_the_session_is_forgiven() {
        // One recording in scope, holding events 1 and 2. The model names a session it was never
        // shown — it has only ever seen the id in the system prompt's example — and an event that
        // does not exist. Coercing the session must not launder the event.
        let allowed = std::collections::BTreeMap::from([(
            SessionId::new(1_786_652_195_282_693_000),
            std::collections::BTreeSet::from([EventId::new(1), EventId::new(2)]),
        )]);
        let result = validate_reply(
            &allowed,
            WireReply::Answer {
                claims: vec![WireClaim {
                    text: "invented".into(),
                    citations: vec![super::AskCitation {
                        session_id: SessionId::new(7),
                        event_id: EventId::new(9),
                    }],
                }],
            },
        );
        assert!(matches!(result, Err(AskError::UnknownCitation(9))));
    }

    #[test]
    fn a_real_event_survives_a_session_id_the_model_was_never_given()
    -> Result<(), Box<dyn std::error::Error>> {
        // The transcript renders `[event:N ...]` and no session id anywhere, so the only session
        // value the model has seen is the prompt's example. With a single recording in scope the
        // citation is unambiguous, and failing it discarded correct answers over a field the model
        // could not have known.
        let session = SessionId::new(1_786_652_195_282_693_000);
        let allowed = std::collections::BTreeMap::from([(
            session,
            std::collections::BTreeSet::from([EventId::new(18)]),
        )]);
        let result = validate_reply(
            &allowed,
            WireReply::Answer {
                claims: vec![WireClaim {
                    text: "Scotiabank testing is blocked on FFT outages.".into(),
                    citations: vec![super::AskCitation {
                        session_id: SessionId::new(7),
                        event_id: EventId::new(18),
                    }],
                }],
            },
        );
        let Ok(AskReply::Answer { claims }) = result else {
            return Err("a real event must be accepted as an answer".into());
        };
        assert_eq!(
            claims[0].citations[0].session_id, session,
            "the citation is attributed to the recording actually being asked about"
        );
        Ok(())
    }

    #[test]
    fn serialized_initial_context_contains_no_screen_or_audio_payload()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = build_request("test", &target(), &[], &[], "What happened?");
        let context = &request.messages[0].content;
        assert!(context.contains("Timestamped final transcript"));
        assert!(!context.contains("screen_snapshot"));
        assert!(!context.contains("ocr_text"));
        assert!(!context.contains("frame_ref"));
        assert!(!context.contains("audio_bytes"));
        let serialized = serde_json::to_string(&request)?;
        assert!(!serialized.contains("annotation.user"));
        Ok(())
    }

    #[tokio::test]
    async fn selection_serializes_only_its_final_rows_and_never_the_provisional_tail()
    -> Result<(), Box<dyn std::error::Error>> {
        let events = boundary_events();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let engine = AskEngine::new(Arc::new(BoundaryProvider {
            requests: Arc::clone(&requests),
        }));
        let result = engine
            .ask_selection(
                SessionId::new(7),
                &target(),
                &[events[0].clone(), events[2].clone()],
                &[],
                "Who owns it?",
                CancellationToken::new(),
                None,
            )
            .await?;
        assert!(matches!(result.reply, AskReply::Refusal { .. }));

        let requests = requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(requests.len(), 1);
        let serialized = serde_json::to_string(&requests[0])?;
        assert!(serialized.contains("the deadline is Friday"));
        assert!(serialized.contains("meeting audio"));
        assert!(serialized.contains("hesitant"));
        assert!(!serialized.contains("outside-only owner"));
        assert!(!serialized.contains("provisional secret tail"));
        Ok(())
    }

    #[tokio::test]
    async fn retained_recording_ask_can_consult_screen_and_returns_its_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        let events = boundary_events();
        let log = ScreenConsultationLog::new(SessionId::new(7));
        let inspector = Arc::new(LoggedInspector { log: log.clone() });
        let provider = Arc::new(InspectionProvider(Mutex::new(VecDeque::from([
            r#"{"action":"inspect_screen","event_id":1,"evidence":"local_ocr","reason":"read the slide behind the cited words"}"#.to_owned(),
            r#"{"kind":"answer","claims":[{"text":"The deadline is Friday.","citations":[{"event_id":1}]}]}"#.to_owned(),
        ]))));
        let result = AskEngine::new(provider)
            .with_screen_inspector(inspector, log)
            .ask(
                SessionId::new(7),
                &target(),
                &events,
                &[],
                "What was on the slide?",
                CancellationToken::new(),
                None,
            )
            .await?;

        assert!(matches!(result.reply, AskReply::Answer { .. }));
        assert_eq!(result.screen_consultations.len(), 1);
        assert!(
            result.screen_consultations[0]
                .describe()
                .contains("unavailable (no_snapshots)")
        );
        Ok(())
    }

    #[tokio::test]
    async fn outside_the_selection_refuses_without_widening_but_recording_scope_answers()
    -> Result<(), Box<dyn std::error::Error>> {
        let events = boundary_events();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let engine = AskEngine::new(Arc::new(BoundaryProvider {
            requests: Arc::clone(&requests),
        }));

        let selected = engine
            .ask_selection(
                SessionId::new(7),
                &target(),
                &events[..1],
                &[],
                "Who owns it?",
                CancellationToken::new(),
                None,
            )
            .await?;
        assert!(matches!(selected.reply, AskReply::Refusal { .. }));

        let recording = engine
            .ask(
                SessionId::new(7),
                &target(),
                &events,
                &[],
                "Who owns it?",
                CancellationToken::new(),
                None,
            )
            .await?;
        let AskReply::Answer { claims } = recording.reply else {
            return Err("recording scope must answer from the outside row".into());
        };
        assert_eq!(claims[0].citations[0].event_id, EventId::new(2));
        assert_eq!(requests.lock().map_err(|_| "requests poisoned")?.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn excessive_selection_refuses_before_provider_dispatch()
    -> Result<(), Box<dyn std::error::Error>> {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let engine = AskEngine::new(Arc::new(BoundaryProvider {
            requests: Arc::clone(&requests),
        }));
        let seed = boundary_events()[0].clone();
        let events =
            std::iter::repeat_n(seed, MAX_SELECTION_FINALS.saturating_add(1)).collect::<Vec<_>>();
        let result = engine
            .ask_selection(
                SessionId::new(7),
                &target(),
                &events,
                &[],
                "What happened?",
                CancellationToken::new(),
                None,
            )
            .await?;
        let AskReply::Refusal { reason, .. } = result.reply else {
            return Err("an oversized selection must refuse locally".into());
        };
        assert!(reason.contains("select at most 200"));
        assert!(requests.lock().map_err(|_| "requests poisoned")?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn absent_answer_eval_returns_explicit_refusal() -> Result<(), Box<dyn std::error::Error>>
    {
        let engine = AskEngine::new(Arc::new(RefusalProvider));
        let result = engine
            .ask(
                SessionId::new(7),
                &target(),
                &[],
                &[],
                "Who owns billing?",
                CancellationToken::new(),
                None,
            )
            .await?;
        assert!(matches!(result.reply, AskReply::Refusal { .. }));
        assert!(result.normalizations.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn ask_dispatch_uses_backend_normalization_and_exposes_downgrades()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let descriptor = BackendDescriptor::new(
            BackendId::new("example.ask-no-sampling")?,
            "Ask no sampling",
            "refusal-eval",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::None,
            AuthStatus::Ready,
        )?;
        let id = descriptor.id().clone();
        registry.register_reasoning(descriptor, Arc::new(RefusalProvider))?;
        registry.select(ReasoningSurface::Ask, Some(&id))?;
        let resolved = registry
            .resolve(ReasoningSurface::Ask)?
            .ok_or("Ask backend must resolve")?;
        let engine = AskEngine::new(resolved.provider());
        let result = engine
            .ask(
                SessionId::new(7),
                &target(),
                &[],
                &[],
                "Who owns billing?",
                CancellationToken::new(),
                None,
            )
            .await?;
        assert!(matches!(result.reply, AskReply::Refusal { .. }));
        let observations = result.normalizations;
        assert_eq!(observations.len(), 3);
        assert_eq!(observations[0].dispatch_id, observations[1].dispatch_id);
        assert_eq!(observations[0].dispatch_id, observations[2].dispatch_id);
        assert_eq!(
            observations[0].normalization.control.to_string(),
            "max_tokens"
        );
        assert_eq!(
            observations[1].normalization.control.to_string(),
            "temperature"
        );
        // A backend that cannot guarantee a JSON object is a downgrade like any other: the parser
        // becomes the only guarantee, and the caller is told so rather than left to assume.
        assert_eq!(
            observations[2].normalization.control.to_string(),
            "json_object output"
        );
        assert!(
            resolved.normalization_observations().len() == 3,
            "the independent resolution diagnostic must retain the same downgrade evidence"
        );
        Ok(())
    }
}
