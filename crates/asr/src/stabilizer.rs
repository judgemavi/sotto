use sotto_core::{Source, TranscriptUpdate, Utterance};
use std::time::Duration;

#[derive(Clone, Debug)]
pub(crate) struct Hypothesis {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
    pub avg_logprob: f32,
}

pub(crate) struct Stabilizer {
    required: usize,
    unstable_tail: Duration,
    candidate: String,
    agreements: usize,
    committed_end: Duration,
    last_partial: String,
}

impl Stabilizer {
    pub(crate) fn new(required: usize, unstable_tail: Duration) -> Self {
        Self {
            required,
            unstable_tail,
            candidate: String::new(),
            agreements: 0,
            committed_end: Duration::ZERO,
            last_partial: String::new(),
        }
    }

    pub(crate) fn observe(
        &mut self,
        source: Source,
        base: Duration,
        segments: &[Hypothesis],
    ) -> Vec<TranscriptUpdate> {
        let window_end = segments.last().map_or(Duration::ZERO, |s| s.end);
        let cutoff = window_end.saturating_sub(self.unstable_tail);
        let stable = segments
            .iter()
            .filter(|s| s.end <= cutoff && base + s.end > self.committed_end)
            .collect::<Vec<_>>();
        let candidate = stable
            .iter()
            .map(|s| s.text.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !candidate.is_empty() && candidate == self.candidate {
            self.agreements += 1;
        } else {
            self.candidate = candidate.clone();
            self.agreements = usize::from(!candidate.is_empty());
        }
        let mut output = Vec::new();
        let mut committed = false;
        if self.agreements >= self.required
            && let (Some(first), Some(last)) = (stable.first(), stable.last())
        {
            output.push(TranscriptUpdate::Final(make(
                source,
                base + first.start,
                base + last.end,
                candidate,
                avg(&stable),
            )));
            self.committed_end = base + last.end;
            self.candidate.clear();
            self.agreements = 0;
            self.last_partial.clear();
            committed = true;
        }
        let partial_segments = segments
            .iter()
            .filter(|s| base + s.end > self.committed_end)
            .collect::<Vec<_>>();
        let partial = partial_segments
            .iter()
            .map(|s| s.text.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !committed
            && !partial.is_empty()
            && partial != self.last_partial
            && let (Some(first), Some(last)) = (partial_segments.first(), partial_segments.last())
        {
            output.push(TranscriptUpdate::Partial(make(
                source,
                base + first.start,
                base + last.end,
                partial.clone(),
                avg(&partial_segments),
            )));
            self.last_partial = partial;
        }
        output
    }
}

fn avg(segments: &[&Hypothesis]) -> f32 {
    if segments.is_empty() {
        0.0
    } else {
        segments.iter().map(|s| s.avg_logprob).sum::<f32>() / segments.len() as f32
    }
}
fn make(
    source: Source,
    start: Duration,
    end: Duration,
    text: String,
    avg_logprob: f32,
) -> Utterance {
    Utterance {
        source,
        start,
        end,
        text,
        avg_logprob,
        annotations: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Hypothesis, Stabilizer};
    use sotto_core::{Source, TranscriptUpdate};
    use std::time::Duration;

    fn segment(start: u64, end: u64, text: &str) -> Hypothesis {
        Hypothesis {
            start: Duration::from_secs(start),
            end: Duration::from_secs(end),
            text: text.to_owned(),
            avg_logprob: -0.1,
        }
    }

    #[test]
    fn identical_partials_are_suppressed_and_commits_do_not_retract() {
        let mut stabilizer = Stabilizer::new(2, Duration::from_secs(2));
        let first = vec![segment(0, 1, "pricing"), segment(1, 4, "may change")];
        let first_updates = stabilizer.observe(Source::System, Duration::ZERO, &first);
        assert_eq!(first_updates.len(), 1);
        let second = stabilizer.observe(Source::System, Duration::ZERO, &first);
        assert_eq!(
            second.len(),
            1,
            "stable prefix commits; duplicate partial is suppressed"
        );
        assert!(matches!(
            first_updates.as_slice(),
            [TranscriptUpdate::Partial(_)]
        ));
        assert!(matches!(second.as_slice(), [TranscriptUpdate::Final(_)]));
        assert_eq!(second[0].utterance().text, "pricing");
        let revised = vec![
            segment(0, 1, "pricing retracted"),
            segment(1, 5, "is fixed"),
        ];
        let emitted = stabilizer.observe(Source::System, Duration::ZERO, &revised);
        assert!(
            emitted
                .iter()
                .all(|update| update.utterance().start >= Duration::from_secs(1)),
            "committed interval must never be emitted again"
        );
    }
}
