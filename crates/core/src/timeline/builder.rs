use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Duration,
};

use thiserror::Error;

use super::{
    EventClass, EventId, EventKind, EventPayload, ProposalDisposition, ProposalDispositionKind,
    ProposalEvent, ProposalPhase, ProposalRunAudit, Session, SessionId, TimelineEvent,
    UserAnnotation,
};
use crate::{MarkKind, Proposal, ProposalKind, ProposalTrigger, types::ProposalProvenance};

/// Rejected append-only timeline operation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TimelineError {
    #[error("event belongs to session {actual:?}, expected {expected:?}")]
    ForeignSession {
        expected: SessionId,
        actual: SessionId,
    },
    #[error("superseded event {0:?} is not present in this timeline")]
    UnknownSupersededEvent(EventId),
    #[error("event {superseded:?} must precede replacement {replacement:?}")]
    SupersededEventNotEarlier {
        superseded: EventId,
        replacement: EventId,
    },
    #[error("event ids are not strictly increasing: {previous:?} then {current:?}")]
    NonMonotonicId { previous: EventId, current: EventId },
    #[error("proposal reference {0:?} is not a prior captured meeting fact")]
    InvalidProposalReference(EventId),
    #[error("proposal payload must use a proposal-specific timeline method")]
    UncheckedProposalPayload,
    #[error("user annotation payload must use a checked annotation timeline method")]
    UncheckedUserAnnotationPayload,
    #[error("user annotation text must not be empty")]
    EmptyUserAnnotation,
    #[error("user annotation anchor {0:?} is not a prior timeline event")]
    InvalidUserAnnotationAnchor(EventId),
    #[error("user annotation replacement must supersede an active user annotation")]
    InvalidUserAnnotationTarget,
    #[error("proposal final must supersede an active proposal partial")]
    InvalidProposalFinalTarget,
    #[error("proposal partial and final must retain the same kind, anchors, and evidence")]
    ProposalProvenanceChanged,
    #[error("proposal disposition must refer to a prior final proposal")]
    InvalidProposalDispositionTarget,
    #[error("proposal run outcome is incompatible with the referenced proposal phase")]
    InvalidProposalRunAuditPhase,
    #[error("proposal run audit must refer to an active proposal event without supersession")]
    InvalidProposalRunAuditTarget,
    #[error("proposal event already has a terminal run audit")]
    DuplicateProposalRunAudit,
    #[error("an audited proposal event cannot be superseded")]
    AuditedProposalCannotSupersede,
}

/// Constructs valid events for one session and retains the append-only log.
#[derive(Debug)]
pub struct TimelineBuilder {
    session: Session,
    known_events: HashMap<EventId, KnownEvent>,
    active_ids: HashSet<EventId>,
    audited_proposals: HashSet<EventId>,
    /// Events not yet handed to persistence by `checkpoint`.
    events: Vec<TimelineEvent>,
}

#[derive(Clone, Debug)]
enum KnownEvent {
    CapturedMeetingFact,
    Proposal(KnownProposal),
    UserAnnotation(EventId),
    Other,
}

#[derive(Clone, Debug)]
struct KnownProposal {
    phase: ProposalPhase,
    kind: ProposalKind,
    anchors: Vec<EventId>,
    provenance: Option<ProposalProvenance>,
}

impl KnownProposal {
    fn from_event(value: &ProposalEvent) -> Option<Self> {
        if let Some(trigger) = value.trigger() {
            return Some(Self {
                phase: ProposalPhase::Trigger,
                kind: trigger.kind(),
                anchors: trigger.anchors().to_vec(),
                provenance: None,
            });
        }
        let proposal = value.proposal()?;
        Some(Self {
            phase: value.phase(),
            kind: proposal.kind(),
            anchors: proposal.anchors().to_vec(),
            provenance: Some(proposal.provenance()),
        })
    }

    fn has_same_provenance(&self, replacement: &ProposalEvent) -> bool {
        let Some(proposal) = replacement.proposal() else {
            return false;
        };
        self.kind == proposal.kind()
            && self.anchors == proposal.anchors()
            && self.provenance.as_ref() == Some(&proposal.provenance())
    }
}

impl TimelineBuilder {
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self {
            session,
            known_events: HashMap::new(),
            active_ids: HashSet::new(),
            audited_proposals: HashSet::new(),
            events: Vec::new(),
        }
    }

    #[must_use]
    pub const fn session(&self) -> &Session {
        &self.session
    }

    pub fn append(&mut self, ts: Duration, payload: EventPayload) -> TimelineEvent {
        assert!(
            !matches!(
                payload,
                EventPayload::Proposal(_)
                    | EventPayload::ProposalDisposition(_)
                    | EventPayload::ProposalRunAudit(_)
                    | EventPayload::UserAnnotation(_)
            ),
            "proposal and user annotation payloads must use checked TimelineBuilder methods"
        );
        self.append_unchecked(ts, payload)
    }

    /// Appends the user's own words, anchored to a prior event in this session.
    pub fn append_user_annotation(
        &mut self,
        ts: Duration,
        anchor: EventId,
        text: impl Into<String>,
        mark: MarkKind,
    ) -> Result<TimelineEvent, TimelineError> {
        self.validate_user_annotation_anchor(anchor)?;
        let annotation = checked_user_annotation(anchor, text, mark)?;
        Ok(self.append_unchecked(ts, EventPayload::UserAnnotation(annotation)))
    }

    /// Edits an annotation append-only by replacing its active event envelope.
    ///
    /// Identified by **id**, not by envelope. The builder already retains every id it allocated
    /// and which of them are still active, so it needs nothing from the caller that it does not
    /// already hold — and a caller that had to produce the envelope could not: [`Self::checkpoint`]
    /// drains the pending payload buffer every 64 events, so the live pipeline lost the envelope
    /// of any note older than that and silently dropped the edit.
    pub fn supersede_user_annotation(
        &mut self,
        ts: Duration,
        text: impl Into<String>,
        mark: MarkKind,
        target: EventId,
    ) -> Result<TimelineEvent, TimelineError> {
        let Some(KnownEvent::UserAnnotation(anchor)) = self.known_events.get(&target) else {
            return Err(TimelineError::InvalidUserAnnotationTarget);
        };
        if !self.active_ids.contains(&target) {
            return Err(TimelineError::InvalidUserAnnotationTarget);
        }
        let annotation = checked_user_annotation(*anchor, text, mark)?;
        self.supersede_active(ts, EventPayload::UserAnnotation(annotation), target)
    }

    fn append_unchecked(&mut self, ts: Duration, payload: EventPayload) -> TimelineEvent {
        let event = TimelineEvent::new(
            self.session.next_event_id(),
            self.session.id(),
            ts,
            None,
            payload,
        );
        self.remember(&event);
        self.active_ids.insert(event.id());
        self.events.push(event.clone());
        event
    }

    pub fn append_proposal_trigger(
        &mut self,
        ts: Duration,
        trigger: ProposalTrigger,
    ) -> Result<TimelineEvent, TimelineError> {
        self.validate_captured_references(trigger.anchors())?;
        Ok(self.append_unchecked(
            ts,
            EventPayload::Proposal(ProposalEvent::from_trigger(trigger)),
        ))
    }

    pub fn append_proposal_partial(
        &mut self,
        ts: Duration,
        proposal: Proposal,
    ) -> Result<TimelineEvent, TimelineError> {
        self.validate_proposal_references(&proposal)?;
        Ok(self.append_unchecked(ts, EventPayload::Proposal(ProposalEvent::partial(proposal))))
    }

    pub fn finalize_proposal(
        &mut self,
        ts: Duration,
        proposal: Proposal,
        target: &TimelineEvent,
    ) -> Result<TimelineEvent, TimelineError> {
        self.validate_proposal_references(&proposal)?;
        let replacement = ProposalEvent::final_value(proposal);
        self.validate_proposal_replacement(&replacement, target, ProposalPhase::Partial)?;
        self.supersede_unchecked(ts, EventPayload::Proposal(replacement), target)
    }

    pub fn append_proposal_disposition(
        &mut self,
        ts: Duration,
        proposal: &TimelineEvent,
        kind: ProposalDispositionKind,
    ) -> Result<TimelineEvent, TimelineError> {
        if proposal.session_id() != self.session.id() {
            return Err(TimelineError::ForeignSession {
                expected: self.session.id(),
                actual: proposal.session_id(),
            });
        }
        if !matches!(
            self.known_events.get(&proposal.id()),
            Some(KnownEvent::Proposal(value))
                if value.phase == ProposalPhase::Final
                    && self.active_ids.contains(&proposal.id())
        ) {
            return Err(TimelineError::InvalidProposalDispositionTarget);
        }
        Ok(self.append_unchecked(
            ts,
            EventPayload::ProposalDisposition(ProposalDisposition::new(proposal.id(), kind)),
        ))
    }

    pub fn append_proposal_run_audit(
        &mut self,
        ts: Duration,
        audit: ProposalRunAudit,
    ) -> Result<TimelineEvent, TimelineError> {
        let Some(KnownEvent::Proposal(proposal)) = self.known_events.get(&audit.proposal()) else {
            return Err(TimelineError::InvalidProposalRunAuditTarget);
        };
        if !self.active_ids.contains(&audit.proposal()) || proposal.anchors != audit.anchors() {
            return Err(TimelineError::ProposalProvenanceChanged);
        }
        validate_audit_phase(proposal.phase, audit.outcome())?;
        if !self.audited_proposals.insert(audit.proposal()) {
            return Err(TimelineError::DuplicateProposalRunAudit);
        }
        Ok(self.append_unchecked(ts, EventPayload::ProposalRunAudit(audit)))
    }

    pub fn supersede_proposal_partial(
        &mut self,
        ts: Duration,
        proposal: Proposal,
        target: &TimelineEvent,
    ) -> Result<TimelineEvent, TimelineError> {
        self.validate_proposal_references(&proposal)?;
        let replacement = ProposalEvent::partial(proposal);
        self.validate_proposal_replacement(&replacement, target, ProposalPhase::Partial)?;
        self.supersede_unchecked(ts, EventPayload::Proposal(replacement), target)
    }

    pub fn supersede(
        &mut self,
        ts: Duration,
        payload: EventPayload,
        target: &TimelineEvent,
    ) -> Result<TimelineEvent, TimelineError> {
        if matches!(
            payload,
            EventPayload::Proposal(_)
                | EventPayload::ProposalDisposition(_)
                | EventPayload::ProposalRunAudit(_)
                | EventPayload::UserAnnotation(_)
        ) || matches!(
            target.kind(),
            EventKind::ProposalTrigger
                | EventKind::ProposalPartial
                | EventKind::ProposalFinal
                | EventKind::ProposalDisposition
                | EventKind::ProposalRunAudit
        ) {
            return Err(
                if matches!(payload, EventPayload::UserAnnotation(_))
                    || matches!(target.kind(), EventKind::UserAnnotation)
                {
                    TimelineError::UncheckedUserAnnotationPayload
                } else {
                    TimelineError::UncheckedProposalPayload
                },
            );
        }
        self.supersede_unchecked(ts, payload, target)
    }

    fn supersede_unchecked(
        &mut self,
        ts: Duration,
        payload: EventPayload,
        target: &TimelineEvent,
    ) -> Result<TimelineEvent, TimelineError> {
        if target.session_id() != self.session.id() {
            return Err(TimelineError::ForeignSession {
                expected: self.session.id(),
                actual: target.session_id(),
            });
        }
        self.supersede_active(ts, payload, target.id())
    }

    /// Supersedes an id this builder allocated and still holds active.
    ///
    /// The envelope adds nothing here: an id absent from `known_events` is rejected either way, and
    /// a foreign session's ids cannot appear in it. Callers holding an envelope go through
    /// [`Self::supersede_unchecked`], which names the foreign-session case explicitly.
    fn supersede_active(
        &mut self,
        ts: Duration,
        payload: EventPayload,
        target: EventId,
    ) -> Result<TimelineEvent, TimelineError> {
        let replacement = self.session.peek_next_event_id();
        if target >= replacement {
            return Err(TimelineError::SupersededEventNotEarlier {
                superseded: target,
                replacement,
            });
        }
        if !self.known_events.contains_key(&target) || !self.active_ids.contains(&target) {
            return Err(TimelineError::UnknownSupersededEvent(target));
        }

        let event = TimelineEvent::new(
            self.session.next_event_id(),
            self.session.id(),
            ts,
            Some(target),
            payload,
        );
        self.active_ids.remove(&target);
        if matches!(
            self.known_events.get(&target),
            Some(KnownEvent::Proposal(_))
        ) {
            self.known_events.insert(target, KnownEvent::Other);
        }
        self.remember(&event);
        self.active_ids.insert(event.id());
        self.events.push(event.clone());
        Ok(event)
    }

    fn remember(&mut self, event: &TimelineEvent) {
        let known = match event.payload() {
            EventPayload::Proposal(value) => {
                KnownProposal::from_event(value).map_or(KnownEvent::Other, KnownEvent::Proposal)
            }
            EventPayload::UserAnnotation(value) => KnownEvent::UserAnnotation(value.anchor),
            _ if event.kind().class() == EventClass::CapturedMeetingFact => {
                KnownEvent::CapturedMeetingFact
            }
            _ => KnownEvent::Other,
        };
        self.known_events.insert(event.id(), known);
    }

    fn validate_captured_references(&self, references: &[EventId]) -> Result<(), TimelineError> {
        for reference in references {
            if !matches!(
                self.known_events.get(reference),
                Some(KnownEvent::CapturedMeetingFact)
            ) {
                return Err(TimelineError::InvalidProposalReference(*reference));
            }
        }
        Ok(())
    }

    fn validate_user_annotation_anchor(&self, anchor: EventId) -> Result<(), TimelineError> {
        self.known_events
            .contains_key(&anchor)
            .then_some(())
            .ok_or(TimelineError::InvalidUserAnnotationAnchor(anchor))
    }

    fn validate_proposal_references(&self, proposal: &Proposal) -> Result<(), TimelineError> {
        self.validate_captured_references(proposal.anchors())?;
        self.validate_captured_references(proposal.meeting_evidence())
    }

    fn validate_proposal_replacement(
        &self,
        replacement: &ProposalEvent,
        target: &TimelineEvent,
        required_phase: ProposalPhase,
    ) -> Result<(), TimelineError> {
        if target.session_id() != self.session.id() {
            return Err(TimelineError::ForeignSession {
                expected: self.session.id(),
                actual: target.session_id(),
            });
        }
        let Some(KnownEvent::Proposal(previous)) = self.known_events.get(&target.id()) else {
            return Err(TimelineError::InvalidProposalFinalTarget);
        };
        if previous.phase != required_phase || !self.active_ids.contains(&target.id()) {
            return Err(TimelineError::InvalidProposalFinalTarget);
        }
        if self.audited_proposals.contains(&target.id()) {
            return Err(TimelineError::AuditedProposalCannotSupersede);
        }
        if !previous.has_same_provenance(replacement) {
            return Err(TimelineError::ProposalProvenanceChanged);
        }
        Ok(())
    }

    #[must_use]
    pub fn events(&self) -> &[TimelineEvent] {
        &self.events
    }

    /// Drains events ready for durable persistence while retaining compact validation
    /// state for every allocated id.
    ///
    /// T011 must persist the returned batch before dropping it. A previously drained
    /// event may still be superseded if it remains active; callers only need retain the
    /// small target envelope, not its place in this pending payload buffer.
    pub fn checkpoint(&mut self) -> Vec<TimelineEvent> {
        std::mem::take(&mut self.events)
    }

    #[must_use]
    pub fn into_events(self) -> Vec<TimelineEvent> {
        self.events
    }
}

/// Constructs a validated annotation payload for callers that allocate the event envelope.
///
/// Live capture normally uses [`TimelineBuilder::append_user_annotation`]. Durable post-session
/// writers may need to allocate the event id inside their own transaction; this keeps both paths
/// on the same text normalization and payload-construction contract.
pub fn checked_user_annotation(
    anchor: EventId,
    text: impl Into<String>,
    mark: MarkKind,
) -> Result<UserAnnotation, TimelineError> {
    let text = text.into();
    if text.trim().is_empty() {
        return Err(TimelineError::EmptyUserAnnotation);
    }
    Ok(UserAnnotation {
        anchor,
        text: text.trim().to_owned(),
        mark,
    })
}

fn validate_audit_phase(
    phase: ProposalPhase,
    outcome: super::ProposalRunOutcome,
) -> Result<(), TimelineError> {
    let valid = matches!(
        (phase, outcome),
        (ProposalPhase::Final, super::ProposalRunOutcome::Completed)
            | (
                ProposalPhase::Trigger | ProposalPhase::Partial,
                super::ProposalRunOutcome::Cancelled | super::ProposalRunOutcome::Failed
            )
    );
    if valid {
        Ok(())
    } else {
        Err(TimelineError::InvalidProposalRunAuditPhase)
    }
}

/// Deterministic projection containing only the latest active event versions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayState {
    active: BTreeMap<EventId, TimelineEvent>,
}

/// Best-effort replay result for persisted timelines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LenientReplay {
    state: ReplayState,
    issues: Vec<TimelineError>,
}

impl LenientReplay {
    #[must_use]
    pub const fn state(&self) -> &ReplayState {
        &self.state
    }

    #[must_use]
    pub fn issues(&self) -> &[TimelineError] {
        &self.issues
    }
}

impl ReplayState {
    #[must_use]
    pub fn active(&self) -> &BTreeMap<EventId, TimelineEvent> {
        &self.active
    }
}

pub fn replay(events: &[TimelineEvent]) -> Result<ReplayState, TimelineError> {
    let mut state = ReplayState::default();
    let mut captured_facts = HashSet::new();
    let mut audited_proposals = HashSet::new();
    let mut previous = None;
    let session_id = events.first().map(TimelineEvent::session_id);

    for event in events {
        apply_event(
            &mut state,
            &mut captured_facts,
            &mut audited_proposals,
            &mut previous,
            session_id,
            event,
        )?;
    }
    Ok(state)
}

/// Replays a persisted timeline without allowing one malformed event to hide the call.
///
/// Invalid events are skipped and returned in `issues`; all valid events still render.
#[must_use]
pub fn replay_lenient(events: &[TimelineEvent]) -> LenientReplay {
    let mut result = LenientReplay::default();
    let mut captured_facts = HashSet::new();
    let mut audited_proposals = HashSet::new();
    let mut previous = None;
    let session_id = events.first().map(TimelineEvent::session_id);

    for event in events {
        if let Err(error) = apply_event(
            &mut result.state,
            &mut captured_facts,
            &mut audited_proposals,
            &mut previous,
            session_id,
            event,
        ) {
            result.issues.push(error);
        }
    }
    result
}

fn apply_event(
    state: &mut ReplayState,
    captured_facts: &mut HashSet<EventId>,
    audited_proposals: &mut HashSet<EventId>,
    previous: &mut Option<EventId>,
    session_id: Option<SessionId>,
    event: &TimelineEvent,
) -> Result<(), TimelineError> {
    if let Some(expected) = session_id
        && event.session_id() != expected
    {
        return Err(TimelineError::ForeignSession {
            expected,
            actual: event.session_id(),
        });
    }
    if let Some(previous_id) = *previous
        && event.id() <= previous_id
    {
        return Err(TimelineError::NonMonotonicId {
            previous: previous_id,
            current: event.id(),
        });
    }
    validate_replayed_proposal(state, captured_facts, audited_proposals, event)?;
    if let Some(target) = event.supersedes() {
        if target >= event.id() {
            return Err(TimelineError::SupersededEventNotEarlier {
                superseded: target,
                replacement: event.id(),
            });
        }
        if !state.active.contains_key(&target) {
            return Err(TimelineError::UnknownSupersededEvent(target));
        }
        if matches!(
            state.active.get(&target).map(TimelineEvent::kind),
            Some(
                EventKind::ProposalTrigger
                    | EventKind::ProposalPartial
                    | EventKind::ProposalFinal
                    | EventKind::ProposalDisposition
                    | EventKind::ProposalRunAudit
            )
        ) && !matches!(
            event.payload(),
            EventPayload::Proposal(value)
                if matches!(value.phase(), ProposalPhase::Partial | ProposalPhase::Final)
        ) {
            return Err(TimelineError::InvalidProposalFinalTarget);
        }
        state.active.remove(&target);
    }
    state.active.insert(event.id(), event.clone());
    if event.kind().class() == EventClass::CapturedMeetingFact {
        captured_facts.insert(event.id());
    }
    if let EventPayload::ProposalRunAudit(audit) = event.payload() {
        audited_proposals.insert(audit.proposal());
    }
    *previous = Some(event.id());
    Ok(())
}

fn validate_replayed_proposal(
    state: &ReplayState,
    captured_facts: &HashSet<EventId>,
    audited_proposals: &HashSet<EventId>,
    event: &TimelineEvent,
) -> Result<(), TimelineError> {
    match event.payload() {
        EventPayload::Proposal(value) => {
            for reference in value.anchors().iter().chain(
                value
                    .proposal()
                    .into_iter()
                    .flat_map(crate::Proposal::meeting_evidence),
            ) {
                if !captured_facts.contains(reference) {
                    return Err(TimelineError::InvalidProposalReference(*reference));
                }
            }
            match value.phase() {
                ProposalPhase::Trigger => {
                    if event.supersedes().is_some() {
                        return Err(TimelineError::InvalidProposalFinalTarget);
                    }
                }
                ProposalPhase::Partial => {
                    if event.supersedes().is_some() {
                        validate_replayed_proposal_replacement(
                            state,
                            audited_proposals,
                            event,
                            value,
                        )?;
                    }
                }
                ProposalPhase::Final => {
                    validate_replayed_proposal_replacement(state, audited_proposals, event, value)?;
                }
            }
        }
        EventPayload::ProposalDisposition(value)
            if event.supersedes().is_some()
                || !matches!(
                    state.active.get(&value.proposal()).map(TimelineEvent::payload),
                    Some(EventPayload::Proposal(target)) if target.phase() == ProposalPhase::Final
                ) =>
        {
            return Err(TimelineError::InvalidProposalDispositionTarget);
        }
        EventPayload::ProposalRunAudit(value)
            if event.supersedes().is_some()
                || audited_proposals.contains(&value.proposal())
                || !matches!(
                    state.active.get(&value.proposal()).map(TimelineEvent::payload),
                    Some(EventPayload::Proposal(target))
                        if target.anchors() == value.anchors()
                            && validate_audit_phase(target.phase(), value.outcome()).is_ok()
                ) =>
        {
            if audited_proposals.contains(&value.proposal()) {
                return Err(TimelineError::DuplicateProposalRunAudit);
            }
            let Some(EventPayload::Proposal(target)) = state
                .active
                .get(&value.proposal())
                .map(TimelineEvent::payload)
            else {
                return Err(TimelineError::InvalidProposalRunAuditTarget);
            };
            if target.anchors() != value.anchors() {
                return Err(TimelineError::ProposalProvenanceChanged);
            }
            validate_audit_phase(target.phase(), value.outcome())?;
            return Err(TimelineError::InvalidProposalRunAuditTarget);
        }
        _ => {}
    }
    Ok(())
}

fn validate_replayed_proposal_replacement(
    state: &ReplayState,
    audited_proposals: &HashSet<EventId>,
    event: &TimelineEvent,
    replacement: &ProposalEvent,
) -> Result<(), TimelineError> {
    let Some(target_id) = event.supersedes() else {
        return Err(TimelineError::InvalidProposalFinalTarget);
    };
    if audited_proposals.contains(&target_id) {
        return Err(TimelineError::AuditedProposalCannotSupersede);
    }
    let Some(EventPayload::Proposal(target)) =
        state.active.get(&target_id).map(TimelineEvent::payload)
    else {
        return Err(TimelineError::InvalidProposalFinalTarget);
    };
    if target.phase() != ProposalPhase::Partial {
        return Err(TimelineError::InvalidProposalFinalTarget);
    }
    if !target.has_same_provenance(replacement) {
        return Err(TimelineError::ProposalProvenanceChanged);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{
        Annotation, CaptureTarget, EventClass, ExternalEvidenceRef, MarkKind, Proposal,
        ProposalDispositionKind, ProposalKind, ProposalRunAudit, ProposalRunOutcome,
        ProposalTrigger, Source, TargetKind, Usage, Utterance,
    };

    fn session(id: u128) -> Session {
        Session::new(
            SessionId::new(id),
            CaptureTarget {
                bundle_id: Some("com.example.calls".to_owned()),
                display_name: "Calls".to_owned(),
                window_title: None,
                kind: TargetKind::Application,
                audio_scoped: true,
            },
            1_753_776_000_000,
        )
    }

    use super::{
        EventPayload, Session, SessionId, TimelineBuilder, TimelineError, replay, replay_lenient,
    };

    fn utterance(text: &str) -> Utterance {
        Utterance {
            source: Source::System,
            start: Duration::ZERO,
            end: Duration::from_secs(1),
            text: text.to_owned(),
            avg_logprob: -0.1,
            annotations: vec![Annotation::Hesitant],
        }
    }

    fn proposal(anchor: super::EventId, text: &str) -> Result<Proposal, crate::ProposalError> {
        Proposal::new(
            ProposalKind::NextStep,
            vec![anchor],
            text,
            vec![anchor],
            vec![ExternalEvidenceRef::new("mcp-evidence-v1-plan")?],
        )
    }

    #[test]
    fn supersede_chain_replays_deterministically() -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(session(7));
        let partial = builder.append(
            Duration::from_millis(100),
            EventPayload::UtterancePartial(utterance("we")),
        );
        let longer = builder.supersede(
            Duration::from_millis(200),
            EventPayload::UtterancePartial(utterance("we need")),
            &partial,
        )?;
        let final_event = builder.supersede(
            Duration::from_millis(300),
            EventPayload::UtteranceFinal(utterance("we need security review")),
            &longer,
        )?;

        let first = replay(builder.events())?;
        let second = replay(builder.events())?;
        assert_eq!(
            first, second,
            "replaying the same log must be deterministic"
        );
        assert_eq!(
            first.active().keys().copied().collect::<Vec<_>>(),
            vec![final_event.id()],
            "only the latest correction should remain active"
        );
        assert_eq!(
            builder.events().len(),
            3,
            "superseded events must remain in the log"
        );
        Ok(())
    }

    #[test]
    fn rejects_foreign_session_supersede() {
        let mut first = TimelineBuilder::new(session(1));
        let foreign = first.append(Duration::ZERO, EventPayload::UtteranceFinal(utterance("x")));
        let mut second = TimelineBuilder::new(session(2));

        let result = second.supersede(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("y")),
            &foreign,
        );
        assert!(
            matches!(result, Err(TimelineError::ForeignSession { .. })),
            "foreign-session supersession must be rejected"
        );
    }

    #[test]
    fn rejects_later_event_supersede() {
        let session_id = SessionId::new(3);
        let mut builder = TimelineBuilder::new(session(session_id.get()));
        let future = super::TimelineEvent::new(
            super::EventId::new(9),
            session_id,
            Duration::from_secs(9),
            None,
            EventPayload::UtteranceFinal(utterance("future")),
        );

        let result = builder.supersede(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("now")),
            &future,
        );
        assert!(
            matches!(result, Err(TimelineError::SupersededEventNotEarlier { .. })),
            "later-event supersession must be rejected"
        );
    }

    #[test]
    fn checkpoint_bounds_payload_buffer_without_losing_supersession() -> Result<(), TimelineError> {
        let mut builder = TimelineBuilder::new(session(4));
        let partial = builder.append(
            Duration::ZERO,
            EventPayload::UtterancePartial(utterance("partial")),
        );
        let persisted = builder.checkpoint();
        assert_eq!(
            persisted.len(),
            1,
            "checkpoint must return pending payloads"
        );
        assert!(
            builder.events().is_empty(),
            "checkpoint must bound pending payload memory"
        );

        let replacement = builder.supersede(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("final")),
            &partial,
        )?;
        assert_eq!(
            replacement.supersedes(),
            Some(partial.id()),
            "an active checkpointed event must remain supersedable"
        );
        Ok(())
    }

    #[test]
    fn lenient_replay_reports_and_skips_bad_rows() {
        let session_id = SessionId::new(5);
        let valid = super::TimelineEvent::new(
            super::EventId::new(1),
            session_id,
            Duration::ZERO,
            None,
            EventPayload::UtteranceFinal(utterance("valid")),
        );
        let malformed = super::TimelineEvent::new(
            super::EventId::new(2),
            session_id,
            Duration::from_secs(1),
            Some(super::EventId::new(99)),
            EventPayload::UtteranceFinal(utterance("bad")),
        );
        let recovered = replay_lenient(&[valid.clone(), malformed]);

        assert_eq!(recovered.issues().len(), 1, "bad rows must be reported");
        assert_eq!(
            recovered.state().active().get(&valid.id()),
            Some(&valid),
            "valid rows must remain renderable"
        );
    }

    #[test]
    fn checked_user_annotations_require_a_prior_anchor_and_nonempty_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(session(51));
        assert_eq!(
            builder.append_user_annotation(
                Duration::ZERO,
                super::EventId::new(99),
                "note",
                MarkKind::Note,
            ),
            Err(TimelineError::InvalidUserAnnotationAnchor(
                super::EventId::new(99)
            ))
        );

        let anchor = builder.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("anchor")),
        );
        assert_eq!(
            builder.append_user_annotation(
                Duration::from_secs(1),
                anchor.id(),
                "   ",
                MarkKind::Note,
            ),
            Err(TimelineError::EmptyUserAnnotation)
        );
        let note = builder.append_user_annotation(
            Duration::from_secs(2),
            anchor.id(),
            "  Remember the owner  ",
            MarkKind::Important,
        )?;
        let EventPayload::UserAnnotation(annotation) = note.payload() else {
            return Err("checked constructor did not emit a user annotation".into());
        };
        assert_eq!(annotation.anchor, anchor.id());
        assert_eq!(annotation.text, "Remember the owner");
        assert_eq!(note.kind().class(), EventClass::UserInteraction);
        Ok(())
    }

    #[test]
    fn editing_user_annotation_appends_a_superseding_event()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(session(52));
        let anchor = builder.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("anchor")),
        );
        let first = builder.append_user_annotation(
            Duration::from_secs(1),
            anchor.id(),
            "First wording",
            MarkKind::Note,
        )?;
        // Drained first, deliberately: an edit must survive the checkpoint that discards the
        // envelope of the note being edited.
        let drained = builder.checkpoint();
        let replacement = builder.supersede_user_annotation(
            Duration::from_secs(2),
            "Final wording",
            MarkKind::FollowUp,
            first.id(),
        )?;

        assert_eq!(replacement.supersedes(), Some(first.id()));
        let mut logged = drained;
        logged.extend_from_slice(builder.events());
        let active = replay(&logged)?;
        assert!(!active.active().contains_key(&first.id()));
        let EventPayload::UserAnnotation(annotation) = replacement.payload() else {
            return Err("replacement did not remain a user annotation".into());
        };
        assert_eq!(annotation.anchor, anchor.id());
        assert_eq!(annotation.text, "Final wording");
        assert_eq!(annotation.mark, MarkKind::FollowUp);
        Ok(())
    }

    #[test]
    fn checked_proposal_lifecycle_preserves_anchor_and_classification()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(session(6));
        let anchor = builder.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("plan")),
        );
        let trigger = builder.append_proposal_trigger(
            Duration::from_secs(1),
            ProposalTrigger::new(ProposalKind::NextStep, vec![anchor.id()], 0.8)?,
        )?;
        let partial = builder
            .append_proposal_partial(Duration::from_secs(2), proposal(anchor.id(), "Write")?)?;
        let final_event = builder.finalize_proposal(
            Duration::from_secs(3),
            proposal(anchor.id(), "Write the plan")?,
            &partial,
        )?;
        let disposition = builder.append_proposal_disposition(
            Duration::from_secs(4),
            &final_event,
            ProposalDispositionKind::Accepted,
        )?;
        let audit = builder.append_proposal_run_audit(
            Duration::from_secs(5),
            ProposalRunAudit::new(
                final_event.id(),
                vec![anchor.id()],
                "openai:gpt-5:fixture",
                Some(Usage {
                    input_tokens: 10,
                    output_tokens: 4,
                    cache_read_tokens: 2,
                    cache_write_tokens: 0,
                }),
                ProposalRunOutcome::Completed,
            )?,
        )?;

        assert_eq!(
            trigger.kind().class(),
            EventClass::SystemOutput,
            "a proposal trigger must never classify as a meeting fact"
        );
        assert_eq!(
            final_event.supersedes(),
            Some(partial.id()),
            "final must replace partial"
        );
        assert_eq!(
            disposition.kind().class(),
            EventClass::UserInteraction,
            "acceptance records UI state, not a meeting fact"
        );
        assert_eq!(
            audit.kind().class(),
            EventClass::SystemOutput,
            "run audit is provider-neutral system output"
        );
        replay(builder.events())?;
        Ok(())
    }

    #[test]
    fn rejects_unknown_or_system_output_proposal_anchors() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut builder = TimelineBuilder::new(session(8));
        let unknown = builder
            .append_proposal_partial(Duration::ZERO, proposal(super::EventId::new(99), "Ask")?);
        assert!(
            matches!(unknown, Err(TimelineError::InvalidProposalReference(_))),
            "unknown anchors must be rejected"
        );

        let fact = builder.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("fact")),
        );
        let trigger = builder.append_proposal_trigger(
            Duration::from_secs(1),
            ProposalTrigger::new(ProposalKind::NextStep, vec![fact.id()], 0.8)?,
        )?;
        let system_anchor =
            builder.append_proposal_partial(Duration::from_secs(2), proposal(trigger.id(), "Ask")?);
        assert!(
            matches!(
                system_anchor,
                Err(TimelineError::InvalidProposalReference(_))
            ),
            "system output cannot masquerade as meeting evidence"
        );
        Ok(())
    }

    #[test]
    fn rejects_relocated_final_and_cross_kind_supersession()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(session(9));
        let first = builder.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("one")),
        );
        let second = builder.append(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("two")),
        );
        let partial = builder
            .append_proposal_partial(Duration::from_secs(2), proposal(first.id(), "Draft")?)?;
        let moved = builder.finalize_proposal(
            Duration::from_secs(3),
            proposal(second.id(), "Final")?,
            &partial,
        );
        assert_eq!(
            moved,
            Err(TimelineError::ProposalProvenanceChanged),
            "finalization must not relocate its anchor"
        );
        let cross_kind = builder.supersede(
            Duration::from_secs(4),
            EventPayload::UtteranceFinal(utterance("overwrite")),
            &partial,
        );
        assert_eq!(
            cross_kind,
            Err(TimelineError::UncheckedProposalPayload),
            "captured facts cannot supersede proposal output"
        );
        Ok(())
    }

    #[test]
    fn partial_stream_to_final_replays_with_stable_provenance()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(session(11));
        let anchor = builder.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("plan")),
        );
        let first =
            builder.append_proposal_partial(Duration::from_secs(1), proposal(anchor.id(), "W")?)?;
        let longer = builder.supersede_proposal_partial(
            Duration::from_secs(2),
            proposal(anchor.id(), "Write")?,
            &first,
        )?;
        let final_event = builder.finalize_proposal(
            Duration::from_secs(3),
            proposal(anchor.id(), "Write the plan")?,
            &longer,
        )?;
        let replayed = replay(builder.events())?;
        let active = replayed
            .active()
            .get(&final_event.id())
            .ok_or("final proposal should be active")?;
        let EventPayload::Proposal(value) = active.payload() else {
            return Err("active event should be a proposal".into());
        };
        let value = value.proposal().ok_or("final should contain proposal")?;

        assert_eq!(
            value.kind(),
            ProposalKind::NextStep,
            "kind must remain stable"
        );
        assert_eq!(value.anchors(), &[anchor.id()], "anchor must remain stable");
        assert_eq!(
            value.meeting_evidence(),
            &[anchor.id()],
            "meeting evidence must remain stable"
        );
        assert_eq!(
            value.external_evidence()[0].as_str(),
            "mcp-evidence-v1-plan",
            "external evidence must remain stable"
        );
        assert_eq!(
            replayed.active().len(),
            2,
            "only the fact and final proposal should remain active"
        );
        Ok(())
    }

    fn audit(
        target: super::EventId,
        anchor: super::EventId,
        outcome: ProposalRunOutcome,
    ) -> Result<ProposalRunAudit, crate::ProposalRunAuditError> {
        ProposalRunAudit::new(
            target,
            vec![anchor],
            "openai:gpt-5:audit-matrix",
            None,
            outcome,
        )
    }

    #[test]
    fn run_audit_phase_matrix_and_single_terminal_are_enforced()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(session(12));
        let anchor = builder.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("fact")),
        );
        let trigger = builder.append_proposal_trigger(
            Duration::from_secs(1),
            ProposalTrigger::new(ProposalKind::NextStep, vec![anchor.id()], 0.8)?,
        )?;
        assert_eq!(
            builder.append_proposal_run_audit(
                Duration::from_secs(2),
                audit(trigger.id(), anchor.id(), ProposalRunOutcome::Completed)?,
            ),
            Err(TimelineError::InvalidProposalRunAuditPhase),
            "completed cannot terminate a trigger"
        );
        builder.append_proposal_run_audit(
            Duration::from_secs(3),
            audit(trigger.id(), anchor.id(), ProposalRunOutcome::Cancelled)?,
        )?;
        assert_eq!(
            builder.append_proposal_run_audit(
                Duration::from_secs(4),
                audit(trigger.id(), anchor.id(), ProposalRunOutcome::Failed)?,
            ),
            Err(TimelineError::DuplicateProposalRunAudit),
            "one proposal event cannot have contradictory terminal audits"
        );

        let partial = builder
            .append_proposal_partial(Duration::from_secs(5), proposal(anchor.id(), "Draft")?)?;
        assert_eq!(
            builder.append_proposal_run_audit(
                Duration::from_secs(6),
                audit(partial.id(), anchor.id(), ProposalRunOutcome::Completed)?,
            ),
            Err(TimelineError::InvalidProposalRunAuditPhase),
            "completed cannot terminate a partial"
        );
        builder.append_proposal_run_audit(
            Duration::from_secs(7),
            audit(partial.id(), anchor.id(), ProposalRunOutcome::Failed)?,
        )?;
        assert_eq!(
            builder.supersede_proposal_partial(
                Duration::from_secs(8),
                proposal(anchor.id(), "Longer")?,
                &partial,
            ),
            Err(TimelineError::AuditedProposalCannotSupersede),
            "an audited partial is terminal and cannot stream further"
        );

        let fresh_partial = builder
            .append_proposal_partial(Duration::from_secs(9), proposal(anchor.id(), "Fresh")?)?;
        let final_event = builder.finalize_proposal(
            Duration::from_secs(10),
            proposal(anchor.id(), "Fresh final")?,
            &fresh_partial,
        )?;
        for outcome in [ProposalRunOutcome::Cancelled, ProposalRunOutcome::Failed] {
            assert_eq!(
                builder.append_proposal_run_audit(
                    Duration::from_secs(11),
                    audit(final_event.id(), anchor.id(), outcome)?,
                ),
                Err(TimelineError::InvalidProposalRunAuditPhase),
                "cancelled or failed cannot terminate a final"
            );
        }
        builder.append_proposal_run_audit(
            Duration::from_secs(12),
            audit(final_event.id(), anchor.id(), ProposalRunOutcome::Completed)?,
        )?;
        replay(builder.events())?;
        Ok(())
    }

    #[cfg(feature = "serde")]
    #[test]
    fn replay_rejects_invalid_duplicate_and_superseded_terminal_audits()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(session(13));
        let anchor = builder.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("fact")),
        );
        let partial = builder
            .append_proposal_partial(Duration::from_secs(1), proposal(anchor.id(), "Draft")?)?;
        let invalid_completed = super::TimelineEvent::new(
            super::EventId::new(3),
            partial.session_id(),
            Duration::from_secs(2),
            None,
            EventPayload::ProposalRunAudit(audit(
                partial.id(),
                anchor.id(),
                ProposalRunOutcome::Completed,
            )?),
        );
        let serialized =
            serde_json::to_string(&vec![anchor.clone(), partial.clone(), invalid_completed])?;
        let decoded: Vec<super::TimelineEvent> = serde_json::from_str(&serialized)?;
        assert_eq!(
            replay(&decoded),
            Err(TimelineError::InvalidProposalRunAuditPhase),
            "serialized completed-to-partial audit must fail"
        );

        let failed = super::TimelineEvent::new(
            super::EventId::new(3),
            partial.session_id(),
            Duration::from_secs(2),
            None,
            EventPayload::ProposalRunAudit(audit(
                partial.id(),
                anchor.id(),
                ProposalRunOutcome::Failed,
            )?),
        );
        let duplicate = super::TimelineEvent::new(
            super::EventId::new(4),
            partial.session_id(),
            Duration::from_secs(3),
            None,
            EventPayload::ProposalRunAudit(audit(
                partial.id(),
                anchor.id(),
                ProposalRunOutcome::Cancelled,
            )?),
        );
        assert_eq!(
            replay(&[anchor.clone(), partial.clone(), failed.clone(), duplicate,]),
            Err(TimelineError::DuplicateProposalRunAudit),
            "serialized contradictory duplicate audits must fail"
        );

        let replacement = super::TimelineEvent::new(
            super::EventId::new(4),
            partial.session_id(),
            Duration::from_secs(3),
            Some(partial.id()),
            EventPayload::Proposal(super::ProposalEvent::partial(proposal(
                anchor.id(),
                "Longer",
            )?)),
        );
        assert_eq!(
            replay(&[anchor, partial, failed, replacement]),
            Err(TimelineError::AuditedProposalCannotSupersede),
            "serialized audited partial supersession must fail"
        );
        Ok(())
    }

    #[cfg(feature = "serde")]
    #[test]
    fn replay_rejects_serialized_unknown_anchor_and_cross_kind_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(session(10));
        let fact = builder.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("fact")),
        );
        let partial = builder
            .append_proposal_partial(Duration::from_secs(1), proposal(fact.id(), "Draft")?)?;

        let mut invalid_anchor = serde_json::to_value(&partial)?;
        invalid_anchor["payload"]["value"]["content"]["anchors"] = serde_json::json!([999]);
        invalid_anchor["payload"]["value"]["content"]["meeting_evidence"] =
            serde_json::json!([999]);
        let invalid_anchor: super::TimelineEvent = serde_json::from_value(invalid_anchor)?;
        assert!(
            matches!(
                replay(&[fact.clone(), invalid_anchor]),
                Err(TimelineError::InvalidProposalReference(_))
            ),
            "serialized unknown anchors must fail replay"
        );

        let replacement = super::TimelineEvent::new(
            super::EventId::new(partial.id().get() + 1),
            partial.session_id(),
            Duration::from_secs(2),
            Some(partial.id()),
            EventPayload::UtteranceFinal(utterance("not a proposal")),
        );
        let serialized = serde_json::to_string(&vec![fact, partial, replacement])?;
        let decoded: Vec<super::TimelineEvent> = serde_json::from_str(&serialized)?;
        assert_eq!(
            replay(&decoded),
            Err(TimelineError::InvalidProposalFinalTarget),
            "serialized cross-kind replacement must fail replay"
        );
        Ok(())
    }
}
