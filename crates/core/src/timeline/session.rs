use super::{EventId, SessionId};

/// The kind of OS-scoped target selected for a capture session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum TargetKind {
    Application,
    Window,
}

/// The application or window explicitly selected through the system picker.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct CaptureTarget {
    pub bundle_id: Option<String>,
    pub display_name: String,
    pub window_title: Option<String>,
    pub kind: TargetKind,
    /// Whether captured audio was scoped to this target rather than system-wide.
    pub audio_scoped: bool,
}

/// Identity and monotonic id allocator for one call.
#[derive(Debug)]
pub struct Session {
    id: SessionId,
    capture_target: CaptureTarget,
    started_at_unix_ms: u64,
    ended_at_unix_ms: Option<u64>,
    next_id: u64,
}

impl Session {
    #[must_use]
    pub const fn new(
        id: SessionId,
        capture_target: CaptureTarget,
        started_at_unix_ms: u64,
    ) -> Self {
        Self {
            id,
            capture_target,
            started_at_unix_ms,
            ended_at_unix_ms: None,
            next_id: 1,
        }
    }

    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    #[must_use]
    pub const fn capture_target(&self) -> &CaptureTarget {
        &self.capture_target
    }

    #[must_use]
    pub const fn started_at_unix_ms(&self) -> u64 {
        self.started_at_unix_ms
    }

    #[must_use]
    pub const fn ended_at_unix_ms(&self) -> Option<u64> {
        self.ended_at_unix_ms
    }

    pub const fn end(&mut self, ended_at_unix_ms: u64) {
        self.ended_at_unix_ms = Some(ended_at_unix_ms);
    }

    /// Allocates the next monotonic id for this session.
    #[must_use]
    pub const fn next_event_id(&mut self) -> EventId {
        let id = EventId::new(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    pub(super) const fn peek_next_event_id(&self) -> EventId {
        EventId::new(self.next_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{CaptureTarget, Session, SessionId, TargetKind};

    fn target() -> CaptureTarget {
        CaptureTarget {
            bundle_id: Some("us.zoom.xos".to_owned()),
            display_name: "Zoom".to_owned(),
            window_title: Some("Customer call".to_owned()),
            kind: TargetKind::Window,
            audio_scoped: false,
        }
    }

    #[test]
    fn session_retains_capture_scope() {
        let target = target();
        let session = Session::new(SessionId::new(1), target.clone(), 1_753_776_000_000);

        assert_eq!(
            session.capture_target(),
            &target,
            "the session record must retain its selected capture target"
        );
    }

    #[test]
    fn session_lifecycle_uses_caller_supplied_wall_clock() {
        let mut session = Session::new(SessionId::new(2), target(), 1_753_776_000_000);

        assert_eq!(
            session.started_at_unix_ms(),
            1_753_776_000_000,
            "the session must retain its caller-supplied start time"
        );
        assert_eq!(
            session.ended_at_unix_ms(),
            None,
            "a newly started session must remain open"
        );

        session.end(1_753_779_600_000);

        assert_eq!(
            session.ended_at_unix_ms(),
            Some(1_753_779_600_000),
            "ending a session must retain the caller-supplied end time"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn capture_target_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let target = target();
        let json = serde_json::to_string(&target)?;
        let decoded = serde_json::from_str::<CaptureTarget>(&json)?;

        assert_eq!(
            decoded, target,
            "persisted capture scope must survive serialization"
        );
        assert!(
            json.contains("\"kind\":\"window\""),
            "target kinds must use the SQLite-compatible snake-case representation"
        );
        Ok(())
    }
}
