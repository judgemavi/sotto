use super::{EventId, SessionId};

/// Identity and monotonic id allocator for one call.
#[derive(Debug)]
pub struct Session {
    id: SessionId,
    next_id: u64,
}

impl Session {
    #[must_use]
    pub const fn new(id: SessionId) -> Self {
        Self { id, next_id: 1 }
    }

    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
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
