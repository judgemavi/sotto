use super::{EventId, SessionId};

/// The kind of OS-scoped target selected for a capture session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum TargetKind {
    Application,
    Window,
    Display,
    /// A deliberately started session that captures only the local microphone.
    Microphone,
    /// A session whose media arrived by importing a file rather than by any OS-scoped capture
    /// (T071, ADR-0019). There is no content filter, no scoped-audio guarantee, and no screen
    /// frames to claim, so this is the honest "no capture target" state rather than a stretched
    /// reuse of one of the variants above.
    Imported,
}

/// The application, window, or display explicitly selected through the system picker.
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

impl CaptureTarget {
    /// Canonical recorded scope for a session that never presents the system picker.
    #[must_use]
    pub fn microphone_only() -> Self {
        Self {
            bundle_id: None,
            display_name: "Microphone only".to_owned(),
            window_title: None,
            kind: TargetKind::Microphone,
            audio_scoped: true,
        }
    }

    /// Whether this scope captures the microphone and no application audio or screen.
    #[must_use]
    pub const fn is_microphone_only(&self) -> bool {
        matches!(self.kind, TargetKind::Microphone)
    }

    /// Whether this scope names a session that arrived by import rather than capture.
    #[must_use]
    pub const fn is_imported(&self) -> bool {
        matches!(self.kind, TargetKind::Imported)
    }

    /// Rejects impossible combinations before they become durable session provenance.
    #[must_use]
    pub fn has_valid_scope(&self) -> bool {
        match self.kind {
            TargetKind::Microphone => {
                self.bundle_id.is_none()
                    && self.window_title.is_none()
                    && self.display_name == "Microphone only"
                    && self.audio_scoped
            }
            // No OS content filter exists for an import, so nothing it could scope is present:
            // no bundle id, no window title, and no audio-scope claim. `display_name` is the one
            // fact that is true — the imported file's own name — so it is the only field this
            // still requires to be non-empty, on the same terms as a captured target.
            TargetKind::Imported => {
                self.bundle_id.is_none()
                    && self.window_title.is_none()
                    && !self.audio_scoped
                    && !self.display_name.trim().is_empty()
            }
            TargetKind::Application | TargetKind::Window | TargetKind::Display => {
                !self.display_name.trim().is_empty()
            }
        }
    }
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
    fn microphone_only_is_an_explicit_valid_scope() {
        let target = CaptureTarget::microphone_only();

        assert!(target.is_microphone_only());
        assert!(target.has_valid_scope());
        assert_eq!(target.kind, TargetKind::Microphone);
        assert!(target.bundle_id.is_none());
        assert!(target.window_title.is_none());
    }

    #[test]
    fn an_imported_target_is_a_distinct_valid_scope_with_no_captured_claims() {
        let target = CaptureTarget {
            bundle_id: None,
            display_name: "lecture.mp4".to_owned(),
            window_title: None,
            kind: TargetKind::Imported,
            audio_scoped: false,
        };

        assert!(target.is_imported());
        assert!(!target.is_microphone_only());
        assert!(
            target.has_valid_scope(),
            "an honestly represented import must pass the same structural validation a \
             captured target does"
        );
    }

    #[test]
    fn an_imported_target_cannot_smuggle_in_a_capture_claim() {
        for dishonest in [
            CaptureTarget {
                bundle_id: Some("us.zoom.xos".to_owned()),
                display_name: "lecture.mp4".to_owned(),
                window_title: None,
                kind: TargetKind::Imported,
                audio_scoped: false,
            },
            CaptureTarget {
                bundle_id: None,
                display_name: "lecture.mp4".to_owned(),
                window_title: Some("Q3 Planning".to_owned()),
                kind: TargetKind::Imported,
                audio_scoped: false,
            },
            CaptureTarget {
                bundle_id: None,
                display_name: "lecture.mp4".to_owned(),
                window_title: None,
                kind: TargetKind::Imported,
                audio_scoped: true,
            },
        ] {
            assert!(
                !dishonest.has_valid_scope(),
                "an import must never carry a bundle id, window title, or scoped-audio claim it \
                 cannot support: {dishonest:?}"
            );
        }
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

    #[cfg(feature = "serde")]
    #[test]
    fn microphone_scope_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let target = CaptureTarget::microphone_only();
        let json = serde_json::to_string(&target)?;
        let decoded = serde_json::from_str::<CaptureTarget>(&json)?;

        assert_eq!(decoded, target);
        assert!(json.contains("\"kind\":\"microphone\""));
        Ok(())
    }

    #[cfg(feature = "serde")]
    #[test]
    fn imported_scope_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let target = CaptureTarget {
            bundle_id: None,
            display_name: "lecture.mp4".to_owned(),
            window_title: None,
            kind: TargetKind::Imported,
            audio_scoped: false,
        };
        let json = serde_json::to_string(&target)?;
        let decoded = serde_json::from_str::<CaptureTarget>(&json)?;

        assert_eq!(decoded, target);
        assert!(json.contains("\"kind\":\"imported\""));
        Ok(())
    }
}
