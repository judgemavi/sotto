//! Deterministic, text-first prosody extraction.
//!
//! Stream timestamps are normalized with [`Config::stream_offsets`] before comparison,
//! allowing measured drift correction to land without changing event data or algorithms.

#![deny(warnings)]

use std::{collections::VecDeque, time::Duration};

use sotto_core::{Annotation, ProsodyDelta, Source, Utterance, VadSegment};

#[derive(Clone, Copy, Debug)]
pub enum Event<'a> {
    Vad(&'a VadSegment),
    Utterance(&'a Utterance),
}

#[derive(Clone, Debug)]
pub struct Config {
    pub pause_threshold: Duration,
    pub recent_window: Duration,
    pub baseline_alpha: f32,
    pub rate_change_threshold: f32,
    pub echo_overlap_ratio: f32,
    /// Signed seconds added to mic and system timestamps, respectively.
    pub stream_offsets: [f32; 2],
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pause_threshold: Duration::from_millis(700),
            recent_window: Duration::from_secs(60),
            baseline_alpha: 0.1,
            rate_change_threshold: 0.3,
            echo_overlap_ratio: 0.8,
            stream_offsets: [0.0, 0.0],
        }
    }
}

#[derive(Clone, Debug)]
struct Observation {
    source: Source,
    start: f32,
    end: f32,
    text: String,
}

#[derive(Clone, Debug)]
pub struct Annotator {
    config: Config,
    utterances: VecDeque<Observation>,
    baselines: [Option<f32>; 2],
    call_speech_seconds: [f32; 2],
    last_delta: Option<ProsodyDelta>,
}

impl Default for Annotator {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl Annotator {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            utterances: VecDeque::new(),
            baselines: [None, None],
            call_speech_seconds: [0.0, 0.0],
            last_delta: None,
        }
    }

    pub fn observe(&mut self, event: Event<'_>) -> Vec<Annotation> {
        match event {
            Event::Vad(segment) => {
                self.prune(self.corrected(segment.source, segment.start));
                Vec::new()
            }
            Event::Utterance(utterance) => self.observe_utterance(utterance),
        }
    }

    #[must_use]
    pub fn last_delta(&self) -> Option<&ProsodyDelta> {
        self.last_delta.as_ref()
    }

    #[must_use]
    pub fn recent_talk_time_ratio(&self, source: Source) -> f32 {
        talk_ratio(self.utterances.iter(), source)
    }

    /// Share of speaking time for `source` over the whole observed call.
    #[must_use]
    pub fn talk_time_ratio(&self, source: Source) -> f32 {
        let total: f32 = self.call_speech_seconds.iter().sum();
        if total > 0.0 {
            self.call_speech_seconds[source_index(source)] / total
        } else {
            0.0
        }
    }

    fn observe_utterance(&mut self, utterance: &Utterance) -> Vec<Annotation> {
        let start = self.corrected(utterance.source, utterance.start);
        let end = self.corrected(utterance.source, utterance.end).max(start);
        self.prune(start);
        let words = word_count(&utterance.text);
        let minutes = ((end - start) / 60.0).max(1.0 / 600.0);
        let rate = words as f32 / minutes;
        let index = source_index(utterance.source);
        let old_baseline = self.baselines[index];
        let mut annotations = Vec::new();

        if let Some(previous) = self.utterances.back()
            && start >= previous.end
            && start - previous.end >= self.config.pause_threshold.as_secs_f32()
        {
            let rounded_millis =
                Duration::from_secs_f32(start - previous.end + 0.000_5).as_millis();
            annotations.push(Annotation::Pause(Duration::from_millis(
                u64::try_from(rounded_millis).unwrap_or(u64::MAX),
            )));
        }
        if let Some(other) =
            self.utterances.iter().rev().find(|item| {
                item.source != utterance.source && item.start < start && item.end > start
            })
            && !is_probable_echo(
                other,
                start,
                end,
                &utterance.text,
                self.config.echo_overlap_ratio,
            )
        {
            annotations.push(Annotation::Interruption {
                by: utterance.source,
            });
        }
        if is_hesitant(&utterance.text) {
            annotations.push(Annotation::Hesitant);
        }
        if old_baseline.is_some_and(|baseline| {
            baseline > 0.0
                && ((rate - baseline) / baseline).abs() >= self.config.rate_change_threshold
        }) {
            annotations.push(Annotation::SpeechRate(rate));
        }

        self.baselines[index] = Some(old_baseline.map_or(rate, |old| {
            old.mul_add(
                1.0 - self.config.baseline_alpha,
                rate * self.config.baseline_alpha,
            )
        }));
        self.utterances.push_back(Observation {
            source: utterance.source,
            start,
            end,
            text: normalize(&utterance.text),
        });
        self.call_speech_seconds[index] += end - start;
        let ratio = self.talk_time_ratio(utterance.source);
        self.last_delta = Some(ProsodyDelta {
            source: utterance.source,
            speech_rate: (words > 0).then_some(rate),
            talk_time_ratio: ratio,
            annotations: annotations.clone(),
        });
        annotations
    }

    fn corrected(&self, source: Source, timestamp: Duration) -> f32 {
        (timestamp.as_secs_f32() + self.config.stream_offsets[source_index(source)]).max(0.0)
    }

    fn prune(&mut self, now: f32) {
        let cutoff = now - self.config.recent_window.as_secs_f32();
        while self
            .utterances
            .front()
            .is_some_and(|item| item.end < cutoff)
        {
            self.utterances.pop_front();
        }
    }
}

impl sotto_core::TranscriptAnnotator for Annotator {
    fn observe_vad(&mut self, segment: &sotto_core::VadSegment) {
        self.observe(Event::Vad(segment));
    }

    fn annotate(&mut self, utterance: &mut sotto_core::Utterance) -> Option<ProsodyDelta> {
        let annotations = self.observe(Event::Utterance(utterance));
        utterance.annotations.extend(annotations);
        self.last_delta().cloned()
    }
}

#[must_use]
pub fn select(annotations: &[Annotation], budget: usize) -> Vec<Annotation> {
    let mut indexed: Vec<_> = annotations.iter().cloned().enumerate().collect();
    indexed.sort_by_key(|(index, annotation)| (salience(annotation), *index));
    let mut remaining = budget;
    let mut chosen = Vec::new();
    for (_, annotation) in indexed {
        let cost = token_cost(&annotation);
        if cost <= remaining {
            remaining -= cost;
            chosen.push(annotation);
        }
    }
    chosen
}

fn source_index(source: Source) -> usize {
    if source == Source::Mic { 0 } else { 1 }
}
fn word_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count()
}
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_alphanumeric() || ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}
fn is_hesitant(text: &str) -> bool {
    let normalized = format!(" {} ", normalize(text));
    const FILLERS: [&str; 6] = [" um ", " uh ", " erm ", " hmm ", " kind of ", " sort of "];
    FILLERS
        .iter()
        .filter(|filler| normalized.contains(**filler))
        .count()
        >= 2
        || text.contains("--")
        || text.contains("...")
}
fn is_probable_echo(other: &Observation, start: f32, end: f32, text: &str, threshold: f32) -> bool {
    let overlap = (other.end.min(end) - other.start.max(start)).max(0.0);
    let shorter = (other.end - other.start).min(end - start).max(f32::EPSILON);
    overlap / shorter >= threshold && other.text == normalize(text) && !other.text.is_empty()
}
fn talk_ratio<'a>(items: impl Iterator<Item = &'a Observation>, source: Source) -> f32 {
    let (own, total) = items.fold((0.0, 0.0), |(own, total), item| {
        let duration = (item.end - item.start).max(0.0);
        (
            own + if item.source == source { duration } else { 0.0 },
            total + duration,
        )
    });
    if total > 0.0 { own / total } else { 0.0 }
}
fn salience(annotation: &Annotation) -> u8 {
    match annotation {
        Annotation::Interruption { .. } => 0,
        Annotation::Hesitant => 1,
        Annotation::Pause(_) => 2,
        Annotation::Emphatic => 3,
        Annotation::SpeechRate(_) => 4,
        Annotation::TalkTimeRatio(_) => 5,
    }
}
fn token_cost(annotation: &Annotation) -> usize {
    match annotation {
        Annotation::Hesitant | Annotation::Emphatic => 1,
        Annotation::Pause(_) | Annotation::SpeechRate(_) | Annotation::TalkTimeRatio(_) => 3,
        Annotation::Interruption { .. } => 4,
    }
}
