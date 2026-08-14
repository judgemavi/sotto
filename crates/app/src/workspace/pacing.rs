//! Presentation-only pacing for committed transcript rows.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Duration,
};

use gpui::{Context, ListOffset, Timer, px};
use sotto_core::EventId;

use super::{MeetingWorkspace, transcript, transcript::TranscriptRow};

/// One word every 50 ms keeps the presentation inside T052's 40–60 ms band.
pub(crate) const WORD_CADENCE_MS: u64 = 50;
/// At the selected cadence, thirty queued words represent at most 1.5 seconds.
const MAX_QUEUED_WORDS: usize = 30;

#[derive(Clone, Debug)]
struct QueuedWord {
    row: TranscriptRow,
    word: String,
}

/// Releases already-committed words smoothly without changing ASR finality.
#[derive(Clone, Debug, Default)]
pub(crate) struct TranscriptPacer {
    rows: Vec<TranscriptRow>,
    row_index: HashMap<EventId, usize>,
    accepted: HashSet<EventId>,
    queue: VecDeque<QueuedWord>,
}

impl TranscriptPacer {
    /// Replaces presentation state when opening a persisted meeting.
    pub(crate) fn replace(&mut self, rows: Vec<TranscriptRow>) {
        self.rows = rows;
        self.queue.clear();
        self.reindex();
        self.accepted = self.row_index.keys().copied().collect();
    }

    /// Clears presentation state at the start of a different live session.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Accepts newly committed rows in timeline order.
    ///
    /// When a burst would exceed the lag budget, the oldest queued words are
    /// released immediately until the remaining queue is bounded.
    pub(crate) fn ingest(&mut self, committed: &[TranscriptRow]) {
        for row in committed {
            if !self.accepted.insert(row.event_id) {
                continue;
            }
            for word in row.text.split_whitespace() {
                self.queue.push_back(QueuedWord {
                    row: row.clone(),
                    word: word.to_owned(),
                });
            }
        }
        while self.queue.len() > MAX_QUEUED_WORDS {
            self.release_one();
        }
    }

    /// Releases one committed word. Returns whether presentation changed.
    pub(crate) fn tick(&mut self) -> bool {
        if self.queue.is_empty() {
            return false;
        }
        self.release_one();
        true
    }

    /// Releases everything already committed, used when capture stops.
    pub(crate) fn drain(&mut self) {
        while !self.queue.is_empty() {
            self.release_one();
        }
    }

    pub(crate) fn rows(&self) -> &[TranscriptRow] {
        &self.rows
    }

    #[cfg(test)]
    #[cfg(test)]
    fn has_pending(&self) -> bool {
        !self.queue.is_empty()
    }

    pub(crate) fn citation_index(&self, event_id: EventId) -> Option<usize> {
        self.row_index.get(&event_id).copied()
    }

    fn release_one(&mut self) {
        let Some(queued) = self.queue.pop_front() else {
            return;
        };
        if let Some(index) = self.row_index.get(&queued.row.event_id).copied() {
            let text = &mut self.rows[index].text;
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&queued.word);
            return;
        }

        let mut row = queued.row;
        row.text = queued.word;
        self.row_index.insert(row.event_id, self.rows.len());
        self.rows.push(row);
    }

    fn reindex(&mut self) {
        self.row_index = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.event_id, index))
            .collect();
    }
}

impl MeetingWorkspace {
    /// Starts the presentation clock. Timeline ingestion remains append-only;
    /// this loop controls only when committed words become visible.
    pub(super) fn poll_transcript_pacing(&mut self, cx: &mut Context<Self>) {
        let workspace = cx.entity();
        cx.spawn(async move |_, cx| {
            loop {
                Timer::after(Duration::from_millis(WORD_CADENCE_MS)).await;
                if workspace
                    .update(cx, |workspace, cx| {
                        if !workspace.transcript_live {
                            return;
                        }
                        let Some(session_id) = workspace.transcript_session else {
                            return;
                        };
                        let events = workspace
                            .timeline
                            .read(cx)
                            .events()
                            .iter()
                            .filter(|event| event.session_id() == session_id)
                            .cloned()
                            .collect::<Vec<_>>();
                        let projection = transcript::project_transcript(&events);
                        workspace.transcript_pacer.ingest(&projection.committed);
                        let changed = workspace.transcript_pacer.tick();
                        transcript::sync_list_state(
                            &workspace.transcript_list,
                            workspace.transcript_pacer.rows().len(),
                        );
                        if changed && workspace.follow_transcript {
                            workspace.transcript_list.scroll_to(ListOffset {
                                item_ix: workspace.transcript_pacer.rows().len(),
                                offset_in_item: px(0.0),
                            });
                        }
                        if changed {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sotto_core::{EventId, Source};

    use super::{MAX_QUEUED_WORDS, TranscriptPacer};
    use crate::workspace::transcript::TranscriptRow;

    fn row(id: u64, start: u64, text: impl Into<String>) -> TranscriptRow {
        TranscriptRow {
            event_id: EventId::new(id),
            source: Source::Mic,
            start: Duration::from_secs(start),
            text: text.into(),
            prosody: vec![],
            unfinalized: false,
        }
    }

    #[test]
    fn committed_presentation_is_monotonic_across_tail_changes() {
        let hypotheses = [
            row(1, 0, "we should"),
            row(2, 1, "ship Friday"),
            row(3, 2, "after review"),
        ];
        let mut pacer = TranscriptPacer::default();
        let mut previous = String::new();

        for prefix_len in 1..=hypotheses.len() {
            pacer.ingest(&hypotheses[..prefix_len]);
            while pacer.tick() {
                let rendered = pacer
                    .rows()
                    .iter()
                    .map(|value| value.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(rendered.starts_with(&previous));
                previous = rendered;
            }
        }

        assert_eq!(previous, "we should ship Friday after review");
    }

    #[test]
    fn queue_is_bounded_and_stop_drains_persisted_finals_exactly() {
        let text = (0..(MAX_QUEUED_WORDS + 12))
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let final_row = row(7, 0, text);
        let mut pacer = TranscriptPacer::default();

        pacer.ingest(std::slice::from_ref(&final_row));
        assert!(pacer.queue.len() <= MAX_QUEUED_WORDS);
        assert!(pacer.has_pending());

        pacer.drain();
        assert!(!pacer.has_pending());
        assert_eq!(pacer.rows(), std::slice::from_ref(&final_row));
    }

    #[test]
    fn citation_index_tracks_the_exact_committed_event() {
        let mut pacer = TranscriptPacer::default();
        pacer.ingest(&[row(11, 0, "first"), row(19, 1, "second")]);
        pacer.drain();

        assert_eq!(pacer.citation_index(EventId::new(11)), Some(0));
        assert_eq!(pacer.citation_index(EventId::new(19)), Some(1));
        assert_eq!(pacer.citation_index(EventId::new(20)), None);
    }
}
