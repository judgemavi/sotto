use std::{collections::VecDeque, path::Path, time::Duration};

use sotto_core::{
    Annotation, AsrError, AudioFrame, Source, Transcriber, TranscriptUpdate, Utterance,
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::{
    Config, SAMPLE_RATE, bound_hypotheses_to_audio, duration_samples, retain_speech,
    samples_duration,
};

/// Longest prompt carried between windows, in characters.
///
/// Whisper's known failure with prompt carry-over is a repetition loop, where the model copies the
/// prompt instead of decoding. A short prompt supplies the vocabulary without supplying a passage
/// worth repeating.
const PROMPT_CHARACTERS: usize = 200;
/// Width of the moving average used to locate the quietest point near a window edge.
const TROUGH_WINDOW_SAMPLES: usize = 160;
/// How much quieter than its surroundings a trough must be before the cut is moved to it.
const TROUGH_ACCEPTANCE_RATIO: f32 = 0.5;

/// Final-only Whisper path used by complete files and committed recording windows.
///
/// Audio is retained in an unbounded FIFO until inference consumes it. Falling behind therefore
/// increases latency without overwriting speech, unlike the retired realtime ring path.
pub struct FinalWhisperTranscriber {
    config: Config,
    mic: MediaWindowBuffer,
    system: MediaWindowBuffer,
    context: Option<WhisperContext>,
    output: VecDeque<TranscriptUpdate>,
    errors: VecDeque<AsrError>,
    finished: bool,
    /// Tracked per source: the two channels are different speakers, and seeding one with the
    /// other's vocabulary or text would invent continuity rather than observe it.
    mic_state: SourceState,
    system_state: SourceState,
}

/// What one source carries between windows.
#[derive(Default)]
struct SourceState {
    /// Tail of the previous committed text, offered to Whisper as lexical context.
    prompt: Option<String>,
    /// Normalized words already committed, used to remove text a re-decoded context repeats.
    emitted: VecDeque<String>,
}

impl SourceState {
    /// Words retained for overlap detection. Comfortably longer than the most text a context span
    /// can contain, so an overlap is never missed for want of history.
    const EMITTED_WORDS: usize = 64;

    fn remember(&mut self, words: impl IntoIterator<Item = String>) {
        self.emitted.extend(words);
        if let Some(excess) = self.emitted.len().checked_sub(Self::EMITTED_WORDS) {
            self.emitted.drain(..excess);
        }
    }

    /// Fraction of words that must match for a re-decode to count as the same passage.
    ///
    /// Not 1.0, and that is the whole point. A second decode of the same audio with different
    /// surrounding context is *not* required to be identical — the second pass is often the one
    /// that gets a word right. Demanding an exact match means one improved word at the join scores
    /// as no overlap at all, and the entire re-decoded passage is committed a second time.
    const OVERLAP_AGREEMENT: f64 = 0.7;

    /// Length of the longest prefix of `words` that repeats the already-emitted tail.
    ///
    /// Whisper re-segments the context freely, so the repeated text arrives split differently each
    /// window and cannot be matched by segment identity. Matching the word sequence is what
    /// survives that.
    fn overlap(&self, words: &[String]) -> usize {
        (1..=words.len().min(self.emitted.len()))
            .rev()
            .find(|length| {
                let tail = self.emitted.len() - length;
                let matched = self
                    .emitted
                    .range(tail..)
                    .zip(&words[..*length])
                    .filter(|(seen, word)| seen == word)
                    .count();
                // A short overlap judged by ratio alone is noise: two words of which one matches
                // scores 0.5 by accident far too often. Require the first word to anchor it.
                matched as f64 / *length as f64 >= Self::OVERLAP_AGREEMENT
                    && self.emitted.get(tail) == words.first()
            })
            .unwrap_or(0)
    }
}

/// Lowercased alphanumeric form used only to compare words across windows.
fn comparable(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

impl FinalWhisperTranscriber {
    pub fn new(config: Config) -> Result<Self, AsrError> {
        crate::configure_native_logging();
        crate::validate_model_and_window(&config)?;
        Ok(Self {
            config,
            mic: MediaWindowBuffer::default(),
            system: MediaWindowBuffer::default(),
            context: None,
            output: VecDeque::new(),
            errors: VecDeque::new(),
            finished: false,
            mic_state: SourceState::default(),
            system_state: SourceState::default(),
        })
    }

    pub fn poll_errors(&mut self) -> Vec<AsrError> {
        self.errors.drain(..).collect()
    }

    fn infer_ready(&mut self, include_tail: bool) {
        let shape = WindowShape {
            window: duration_samples(self.config.window),
            context: duration_samples(self.config.decoding.context),
            search: duration_samples(self.config.decoding.boundary_search),
        };
        loop {
            let next = self
                .system
                .take(shape, include_tail)
                .map(|window| (Source::System, window))
                .or_else(|| {
                    self.mic
                        .take(shape, include_tail)
                        .map(|window| (Source::Mic, window))
                });
            let Some((source, window)) = next else { break };
            match self.infer(source, &window) {
                Ok(updates) => self.output.extend(updates),
                Err(error) => self.errors.push_back(error),
            }
        }
    }

    const fn state(&mut self, source: Source) -> &mut SourceState {
        match source {
            Source::Mic => &mut self.mic_state,
            Source::System => &mut self.system_state,
        }
    }

    fn infer(
        &mut self,
        source: Source,
        window: &MediaWindow,
    ) -> Result<Vec<TranscriptUpdate>, AsrError> {
        if self.context.is_none() {
            tracing::info!(
                model = %self.config.model_path.display(),
                "loading whisper model for final recording transcription; inspect whisper.cpp backend log for Metal activation"
            );
            self.context = Some(load_context(&self.config.model_path)?);
        }
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| AsrError::ModelLoad("context absent after load".to_owned()))?;
        let mut state = context
            .create_state()
            .map_err(|error| AsrError::Inference(error.to_string()))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(4);
        params.set_language(Some("en"));
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if self.config.decoding.suppress_non_speech {
            // The only setter with an effect: whisper.cpp's temperature fallback, entropy/logprob
            // thresholds, no-speech threshold and blank suppression already equal its own built-in
            // defaults (`whisper_full_default_params`), so Sotto has never needed to set them.
            params.set_suppress_nst(true);
        }
        let prompt = self
            .config
            .decoding
            .prompt_carryover
            .then(|| self.state(source).prompt.clone())
            .flatten();
        // Borrowed by whisper for the duration of the call, so it must outlive `params`.
        if let Some(prompt) = prompt.as_deref() {
            params.set_initial_prompt(prompt);
        }
        state
            .full(params, &window.samples)
            .map_err(|error| AsrError::Inference(error.to_string()))?;
        let hypotheses = state
            .as_iter()
            .map(|segment| {
                let start = Duration::from_millis(
                    u64::try_from(segment.start_timestamp().max(0)).unwrap_or(0) * 10,
                );
                let end = Duration::from_millis(
                    u64::try_from(segment.end_timestamp().max(0)).unwrap_or(0) * 10,
                );
                Ok(crate::Hypothesis {
                    start,
                    end,
                    text: segment
                        .to_str_lossy()
                        .map_err(|error| AsrError::Inference(error.to_string()))?
                        .trim()
                        .to_owned(),
                    avg_logprob: 0.0,
                })
            })
            .collect::<Result<Vec<_>, AsrError>>()?;
        // Timestamps are relative to the encoded buffer, which begins at the retained context
        // rather than at the committed audio.
        let context = samples_duration(window.context_samples);
        let hypotheses = retain_speech(bound_hypotheses_to_audio(
            hypotheses,
            samples_duration(window.samples.len()),
        ));
        let mut candidates = hypotheses
            .into_iter()
            // Anything ending inside the context was fully committed by an earlier window. What
            // reaches past it is trimmed by word overlap below rather than by time, because
            // Whisper re-segments the context differently every window: a segment can begin well
            // inside the context and end well inside the commit, and judging such a segment by
            // its position would either lose its committed half or emit its context half a second
            // time.
            .filter(|hypothesis| hypothesis.end > context)
            .collect::<Vec<_>>();

        let words = candidates
            .iter()
            .flat_map(|hypothesis| hypothesis.text.split_whitespace().map(comparable))
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        let mut remaining = self.state(source).overlap(&words);
        for hypothesis in &mut candidates {
            if remaining == 0 {
                break;
            }
            let mut kept = hypothesis
                .text
                .split_whitespace()
                .skip_while(|word| {
                    let skip = remaining > 0 && !comparable(word).is_empty();
                    remaining = remaining.saturating_sub(usize::from(skip));
                    skip
                })
                .peekable();
            hypothesis.text = kept.by_ref().collect::<Vec<_>>().join(" ");
        }

        let updates = candidates
            .into_iter()
            .filter(|hypothesis| !hypothesis.text.is_empty())
            .map(|hypothesis| {
                TranscriptUpdate::Final(Utterance {
                    source,
                    start: window.start + hypothesis.start.saturating_sub(context),
                    end: window.start + hypothesis.end.saturating_sub(context),
                    text: hypothesis.text,
                    avg_logprob: hypothesis.avg_logprob,
                    annotations: Vec::<Annotation>::new(),
                })
            })
            .collect::<Vec<_>>();

        let committed = updates
            .iter()
            .filter_map(|update| match update {
                TranscriptUpdate::Final(utterance) => Some(utterance.text.as_str()),
                TranscriptUpdate::Partial(_) => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        let carry_prompt = self.config.decoding.prompt_carryover;
        let state = self.state(source);
        state.remember(
            committed
                .split_whitespace()
                .map(comparable)
                .filter(|word| !word.is_empty()),
        );
        if carry_prompt {
            // A window that produced nothing is a silence, and carrying a prompt across it would
            // seed the next passage with vocabulary from before an unknown gap. Clearing is the
            // conservative choice and also breaks any repetition loop that has started.
            state.prompt = prompt_tail(&committed);
        }
        Ok(updates)
    }
}

/// Keeps the last whole words of `text`, up to [`PROMPT_CHARACTERS`].
fn prompt_tail(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.len() <= PROMPT_CHARACTERS {
        return Some(text.to_owned());
    }
    // Cutting on a character boundary would hand whisper a partial word as vocabulary.
    let tail = text.get(text.len() - PROMPT_CHARACTERS..)?;
    Some(
        tail.find(char::is_whitespace)
            .map_or(tail, |boundary| &tail[boundary..])
            .trim()
            .to_owned(),
    )
}

impl Transcriber for FinalWhisperTranscriber {
    fn push(&mut self, frame: &AudioFrame) {
        if self.finished || frame.sample_rate != SAMPLE_RATE {
            return;
        }
        match frame.source {
            Source::Mic => self.mic.push(frame),
            Source::System => self.system.push(frame),
        }
    }

    fn poll(&mut self) -> Vec<TranscriptUpdate> {
        self.infer_ready(self.finished);
        self.output.drain(..).collect()
    }

    fn finish(&mut self) {
        self.finished = true;
    }
}

fn load_context(path: &Path) -> Result<WhisperContext, AsrError> {
    WhisperContext::new_with_params(
        path.to_string_lossy().as_ref(),
        WhisperContextParameters::default(),
    )
    .map_err(|error| AsrError::ModelLoad(error.to_string()))
}

#[derive(Default)]
struct MediaWindowBuffer {
    samples: VecDeque<f32>,
    /// Committed audio retained solely to precede the next window in the encoder.
    context: VecDeque<f32>,
    start_sample: Option<u64>,
    next_sample: Option<u64>,
}

/// Sample counts governing one window: what is committed, what precedes it, and how far the cut
/// may move back to find a quieter place to land.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowShape {
    window: usize,
    context: usize,
    search: usize,
}

struct MediaWindow {
    /// Media time of the first *committed* sample, not of the encoded buffer.
    start: Duration,
    /// Leading samples of `samples` that are context and must not be committed again.
    context_samples: usize,
    samples: Vec<f32>,
}

/// Index within `samples` of the quietest short-time window, searched over the trailing `search`
/// samples of a `count`-sample commit.
///
/// Returns `count` unchanged when there is nothing to search, so a caller that has disabled the
/// search or lacks the room for one keeps the exact clock-aligned cut.
fn quietest_cut(samples: &VecDeque<f32>, count: usize, search: usize) -> usize {
    // Never zero: a cut of zero would commit no audio, leave the read cursor where it was, and
    // spin `infer_ready` forever.
    let earliest = count.saturating_sub(search).max(1);
    if search == 0 || earliest >= count || count > samples.len() {
        return count;
    }
    // A trough in a short moving average of |x| is the closest cheap proxy for a pause between
    // words. It is not speech detection and does not need to be: the only requirement is that the
    // cut lands where the speaker was quietest, not that we know why.
    //
    // The average is over a fixed-width span and normalized by the samples actually in it, and it
    // reads past `count` into audio the next window will commit. Both matter: a truncated,
    // unnormalized sum is smallest exactly at the buffer's edges, which would drag every cut to a
    // boundary regardless of what the audio was doing there.
    let mut best = count;
    let mut best_energy = f32::MAX;
    let mut total_energy = 0.0_f32;
    for candidate in earliest..count {
        let from = candidate.saturating_sub(TROUGH_WINDOW_SAMPLES / 2);
        let to = (candidate + TROUGH_WINDOW_SAMPLES / 2).min(samples.len());
        let Some(span) = to.checked_sub(from).filter(|span| *span > 0) else {
            continue;
        };
        let energy = samples
            .range(from..to)
            .map(|sample| sample.abs())
            .sum::<f32>()
            / span as f32;
        total_energy += energy;
        if energy < best_energy {
            best_energy = energy;
            best = candidate;
        }
    }
    // Continuous speech has no pause to find. Moving the cut anyway would shorten the window for
    // nothing, so a trough is only accepted when it is markedly quieter than the span around it.
    let mean_energy = total_energy / (count - earliest) as f32;
    if best_energy > mean_energy * TROUGH_ACCEPTANCE_RATIO {
        return count;
    }
    best
}

impl MediaWindowBuffer {
    fn push(&mut self, frame: &AudioFrame) {
        let incoming_start = sample_index(frame.stream_offset);
        let expected = self.next_sample.unwrap_or(incoming_start);
        if self.start_sample.is_none() {
            self.start_sample = Some(expected);
        }
        if incoming_start > expected {
            self.samples.extend(std::iter::repeat_n(
                0.0,
                usize::try_from(incoming_start - expected).unwrap_or(usize::MAX),
            ));
        }
        let overlap =
            usize::try_from(expected.saturating_sub(incoming_start)).unwrap_or(usize::MAX);
        let retained = frame.samples.get(overlap..).unwrap_or_default();
        self.samples.extend(retained.iter().copied());
        self.next_sample = Some(
            expected
                .max(incoming_start)
                .saturating_add(u64::try_from(retained.len()).unwrap_or(u64::MAX)),
        );
    }

    fn take(&mut self, shape: WindowShape, include_tail: bool) -> Option<MediaWindow> {
        if self.samples.is_empty() || (self.samples.len() < shape.window && !include_tail) {
            return None;
        }
        let start_sample = self.start_sample?;
        let mut count = self.samples.len().min(shape.window);
        if count == shape.window {
            // The tail flush takes whatever remains, so moving its cut earlier would strand audio
            // that nothing will come back for.
            count = quietest_cut(&self.samples, count, shape.search);
        }
        let committed = self.samples.drain(..count).collect::<Vec<_>>();
        let context_samples = self.context.len();
        let mut samples = Vec::with_capacity(context_samples + committed.len());
        samples.extend(self.context.iter().copied());
        samples.extend_from_slice(&committed);

        self.context.extend(committed);
        if let Some(excess) = self.context.len().checked_sub(shape.context) {
            self.context.drain(..excess);
        }
        self.start_sample =
            Some(start_sample.saturating_add(u64::try_from(count).unwrap_or(u64::MAX)));
        Some(MediaWindow {
            start: duration_for_sample(start_sample),
            context_samples,
            samples,
        })
    }
}

fn sample_index(timestamp: Duration) -> u64 {
    timestamp
        .as_secs()
        .saturating_mul(u64::from(SAMPLE_RATE))
        .saturating_add(
            u64::from(timestamp.subsec_nanos()).saturating_mul(u64::from(SAMPLE_RATE))
                / 1_000_000_000,
        )
}

fn duration_for_sample(sample: u64) -> Duration {
    Duration::from_secs_f64(sample as f64 / f64::from(SAMPLE_RATE))
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use sotto_core::{AudioFrame, Source};

    use super::{MediaWindowBuffer, WindowShape, prompt_tail, quietest_cut};

    /// The pre-`DecodingOptions` shape: cut on the sample count, carry no context.
    const fn clock_aligned(window: usize) -> WindowShape {
        WindowShape {
            window,
            context: 0,
            search: 0,
        }
    }

    fn frame(source: Source, start_ms: u64, samples: &[f32]) -> AudioFrame {
        AudioFrame {
            source,
            samples: Arc::from(samples),
            sample_rate: 16_000,
            seq: 0,
            capture_ts: std::time::Instant::now(),
            stream_offset: Duration::from_millis(start_ms),
        }
    }

    #[test]
    fn backlog_is_fifo_and_media_timestamps_survive_windowing()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut buffer = MediaWindowBuffer::default();
        buffer.push(&frame(Source::System, 2_000, &[1.0, 2.0, 3.0, 4.0]));

        let first = buffer
            .take(clock_aligned(2), false)
            .ok_or("missing complete first window")?;
        let second = buffer
            .take(clock_aligned(2), false)
            .ok_or("missing complete second window")?;

        assert_eq!(first.start, Duration::from_secs(2));
        assert_eq!(first.samples, [1.0, 2.0]);
        assert_eq!(second.start, Duration::from_micros(2_000_125));
        assert_eq!(second.samples, [3.0, 4.0]);
        Ok(())
    }

    #[test]
    fn gaps_become_silence_and_overlap_is_not_transcribed_twice()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut buffer = MediaWindowBuffer::default();
        buffer.push(&frame(Source::Mic, 0, &[1.0, 2.0]));
        buffer.push(&frame(Source::Mic, 0, &[1.0, 2.0, 3.0]));
        buffer.push(&frame(Source::Mic, 1, &[4.0]));

        let window = buffer
            .take(clock_aligned(32), true)
            .ok_or("missing tail window")?;
        assert_eq!(window.samples[0..3], [1.0, 2.0, 3.0]);
        assert!(
            window.samples[3..16].iter().all(|sample| *sample == 0.0),
            "the missing media interval must be explicit silence"
        );
        assert_eq!(window.samples[16], 4.0);
        Ok(())
    }

    #[test]
    fn retained_context_precedes_the_next_window_without_recommitting_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut buffer = MediaWindowBuffer::default();
        buffer.push(&frame(Source::System, 0, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]));
        let shape = WindowShape {
            window: 2,
            context: 2,
            search: 0,
        };

        let first = buffer.take(shape, false).ok_or("missing first window")?;
        let second = buffer.take(shape, false).ok_or("missing second window")?;
        let third = buffer.take(shape, false).ok_or("missing third window")?;

        assert_eq!(
            first.context_samples, 0,
            "nothing precedes the first window"
        );
        assert_eq!(first.samples, [1.0, 2.0]);
        // The encoder sees the previous window again; `context_samples` is what tells the caller
        // those leading samples were already committed and must not be emitted twice.
        assert_eq!(second.context_samples, 2);
        assert_eq!(second.samples, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(third.context_samples, 2);
        assert_eq!(third.samples, [3.0, 4.0, 5.0, 6.0]);
        // Media time still advances by the committed audio alone, never by the context.
        assert_eq!(second.start, super::duration_for_sample(2));
        assert_eq!(third.start, super::duration_for_sample(4));
        Ok(())
    }

    /// Speech, then a 125 ms pause, then speech — the shape a between-words cut should find.
    fn speech_with_a_pause() -> std::collections::VecDeque<f32> {
        let mut samples = vec![1.0_f32; 4_000];
        samples.extend([0.0_f32; 2_000]);
        samples.extend([1.0_f32; 4_000]);
        samples.into_iter().collect()
    }

    #[test]
    fn the_cut_moves_into_the_pause_rather_than_landing_on_the_clock() {
        let samples = speech_with_a_pause();

        let clock_aligned = quietest_cut(&samples, 6_000, 0);
        let trough_aligned = quietest_cut(&samples, 6_000, 3_000);

        assert_eq!(clock_aligned, 6_000, "a zero search must not move the cut");
        assert!(
            (4_000..6_000).contains(&trough_aligned),
            "expected a cut inside the pause, got {trough_aligned}"
        );
    }

    #[test]
    fn continuous_speech_keeps_its_exact_cut() {
        let samples = [1.0_f32; 10_000].into_iter().collect();

        // With no pause to find, moving the cut would shorten the window and buy nothing.
        assert_eq!(quietest_cut(&samples, 6_000, 3_000), 6_000);
    }

    #[test]
    fn a_search_wider_than_the_buffer_never_yields_an_empty_commit() {
        let samples = [1.0_f32; 10].into_iter().collect();

        // A zero-length commit would leave the read cursor unmoved and spin `infer_ready` forever.
        assert!(quietest_cut(&samples, 10, 10_000) > 0);
        assert!(quietest_cut(&samples, 10, 50) > 0);
    }

    #[test]
    fn re_decoded_context_is_matched_by_word_sequence_not_by_segment() {
        let mut state = super::SourceState::default();
        state.remember(
            "the longest ramp up time to become hirable"
                .split_whitespace()
                .map(super::comparable),
        );

        // Whisper split the repeated audio differently this window and repunctuated it. Only the
        // word sequence is stable across that, which is why the overlap is measured on it.
        let words = "To become, hirable! And there are fewer"
            .split_whitespace()
            .map(super::comparable)
            .collect::<Vec<_>>();

        assert_eq!(state.overlap(&words), 3, "expected 'to become hirable'");
        assert_eq!(
            state.overlap(&["and".to_owned(), "there".to_owned()]),
            0,
            "text that does not continue the tail must never be trimmed"
        );
    }

    #[test]
    fn overlap_detection_survives_a_tail_longer_than_it_retains() {
        let mut state = super::SourceState::default();
        for index in 0..200 {
            state.remember([format!("word{index}")]);
        }

        assert!(
            state.emitted.len() <= super::SourceState::EMITTED_WORDS,
            "history must stay bounded across a long recording"
        );
        assert_eq!(state.overlap(&["word199".to_owned()]), 1);
    }

    #[test]
    fn the_carried_prompt_is_bounded_and_never_starts_mid_word() {
        let long = "alpha bravo charlie delta echo foxtrot golf hotel ".repeat(20);

        let tail = prompt_tail(&long).unwrap_or_default();

        assert!(tail.len() <= super::PROMPT_CHARACTERS, "unbounded prompt");
        assert!(
            long.trim_end().ends_with(&tail),
            "the prompt must be the most recent text, not an arbitrary slice"
        );
        assert!(
            long.split_whitespace()
                .any(|word| word == tail.split_whitespace().next().unwrap_or_default()),
            "the prompt must begin on a word boundary"
        );
        assert_eq!(prompt_tail("   "), None, "silence must clear the prompt");
    }
}
