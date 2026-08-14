//! Repeatable benchmark for T063/T065 and the T086 accuracy sweep.
//!
//! This provisions through Sotto's production provisioner and drives `FinalWhisperTranscriber`
//! itself rather than reimplementing its decode loop, so what is measured is what ships. It accepts
//! either mono WAV or an explicitly selected channel of a retained Sotto MP4. Run under
//! `/usr/bin/time -l` to record peak resident memory.
//!
//! `--sweep` runs each named [`Variant`] over the same audio and prints one row per variant, which
//! is the only form in which these numbers mean anything: the reference transcripts available for
//! real captured audio carry their own errors, so an absolute WER is not a claim worth making. The
//! same reference applied to every variant makes the *differences* between them valid regardless.

use std::{env, ffi::OsString, fs, path::PathBuf, sync::Arc, time::Duration, time::Instant};

use asr::{
    Config, DecodingOptions, FinalWhisperTranscriber, ModelSize, decode_recording_channel,
    model::ModelProvisioner,
};
use sotto_core::{AudioFrame, CancellationToken, Source, Transcriber, TranscriptUpdate};

/// Samples per synthesized input frame, matching the pipeline's 100 ms delivery.
const FRAME_SAMPLES: usize = 1_600;
/// A committed segment at or below this many words is a fragment: too short to be a useful
/// transcript line, and short enough to produce a meaningless words-per-minute reading downstream.
const FRAGMENT_WORDS: usize = 2;
const USAGE: &str = "usage: model_benchmark <base.en|small.en|medium.en> <input.wav|recording.mp4> [--channel meeting|microphone] [--reference transcript.txt] [--variant <name>] [--sweep] [--transcript] [--draft out.txt]";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    asr::configure_native_logging();
    let arguments = parse_arguments(env::args_os().skip(1))?;
    let size = parse_size(&arguments.size)?;

    // A draft reads both channels itself, so the preflight only needs one to report signal levels.
    let preflight_channel = arguments
        .channel
        .or_else(|| arguments.draft.is_some().then_some(Source::System));
    let samples = read_input(&arguments.input, preflight_channel)?;
    let signal = signal_levels(&samples);
    println!("input\t{}", arguments.input.display());
    println!("channel\t{}", display_channel(arguments.channel));
    println!("peak_amplitude\t{:.9}", signal.peak);
    println!("rms_amplitude\t{:.9}", signal.rms);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let model_path =
        runtime.block_on(ModelProvisioner::for_current_user()?.resolve_or_download(
            size,
            &CancellationToken::new(),
            |_| {},
        ))?;

    let audio_seconds = samples.len() as f64 / 16_000.0;
    println!("model\t{}", display_size(size));
    println!("revision\t{}", asr::model::MODEL_REVISION);
    println!("model_path\t{}", model_path.display());
    println!("audio_seconds\t{audio_seconds:.6}");

    let reference = arguments
        .reference
        .as_ref()
        .map(fs::read_to_string)
        .transpose()?
        .as_deref()
        .map(|raw| reference_text(raw, arguments.channel.map(draft_speaker)));
    if let Some(path) = arguments.reference.as_ref() {
        println!("reference\t{}", path.display());
    }

    let source = arguments.channel.unwrap_or(Source::System);
    if let Some(path) = arguments.draft.as_ref() {
        // Both channels, whatever `--channel` selected for scoring: a conversation's reference is
        // useless with one side of it missing.
        let mut channels = Vec::new();
        for (speaker, channel) in [("them", Source::System), ("you", Source::Mic)] {
            let mut config = Config::new(&model_path);
            config.model_size = size;
            config.window = arguments.variant.window;
            config.decoding = arguments.variant.decoding;
            let mut transcriber = FinalWhisperTranscriber::new(config)?;
            let channel_samples = read_input(&arguments.input, Some(channel))?;
            channels.push((
                speaker,
                transcribe(&mut transcriber, channel, &channel_samples)?,
            ));
        }
        return write_draft(path, size, arguments.variant, channels);
    }
    let variants = if arguments.sweep {
        Variant::sweep()
    } else {
        vec![arguments.variant]
    };

    println!(
        "\n{:<18}{:>8}{:>8}{:>7}{:>7}{:>7}{:>9}{:>7}{:>8}",
        "variant", "segments", "frags", "sub", "del", "ins", "wer_pct", "xrt", "load_s"
    );
    for variant in variants {
        let mut config = Config::new(&model_path);
        config.model_size = size;
        config.window = variant.window;
        config.decoding = variant.decoding;

        let load_started = Instant::now();
        let mut transcriber = FinalWhisperTranscriber::new(config)?;
        let load_seconds = load_started.elapsed().as_secs_f64();

        let inference_started = Instant::now();
        let segments = transcribe(&mut transcriber, source, &samples)?;
        let inference_seconds = inference_started.elapsed().as_secs_f64();

        let transcript = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let fragments = segments
            .iter()
            .filter(|segment| segment.text.split_whitespace().count() <= FRAGMENT_WORDS)
            .count();
        let errors = reference
            .as_deref()
            .map(|reference| word_errors(reference, &transcript));
        println!(
            "{:<18}{:>8}{:>8}{:>7}{:>7}{:>7}{:>9}{:>7.1}{:>8.1}",
            variant.name,
            segments.len(),
            fragments,
            errors.map_or_else(|| "-".to_owned(), |errors| errors.substitutions.to_string()),
            errors.map_or_else(|| "-".to_owned(), |errors| errors.deletions.to_string()),
            errors.map_or_else(|| "-".to_owned(), |errors| errors.insertions.to_string()),
            errors.map_or_else(
                || "-".to_owned(),
                |errors| format!("{:.2}", errors.wer_percent())
            ),
            audio_seconds / inference_seconds,
            load_seconds,
        );
        if arguments.transcript {
            println!("--- {} ---", variant.name);
            for segment in &segments {
                println!("  {}", segment.text);
            }
        }
    }
    Ok(())
}

/// Writes a two-channel draft for a human to correct by ear, in the format `--reference` reads.
///
/// This exists because Sotto has no reference transcript for its own captured conversations and no
/// second transcription system to borrow one from. A machine draft that a listener corrects is a
/// legitimate reference provided its provenance is stated — and it is far less work than typing one
/// out, which is the difference between a reference existing and not.
///
/// It is not neutral, and the correcting listener is what makes it usable. A draft produced by one
/// configuration flatters that configuration on every word left uncorrected. Correct against the
/// audio, not against what reads plausibly.
fn write_draft(
    path: &PathBuf,
    model: ModelSize,
    variant: Variant,
    channels: Vec<(&str, Vec<Segment>)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut merged = channels
        .into_iter()
        .flat_map(|(speaker, segments)| {
            segments
                .into_iter()
                .map(move |segment| (segment.start, speaker, segment.text))
        })
        .collect::<Vec<_>>();
    merged.sort_by_key(|(start, _, _)| *start);

    let mut draft = String::new();
    draft.push_str("# Draft reference transcript — CORRECT THIS AGAINST THE AUDIO.\n");
    draft.push_str("#\n");
    draft.push_str(&format!(
        "# Generated by {} / variant {}. Every uncorrected line silently scores that\n",
        display_size(model),
        variant.name
    ));
    draft.push_str("# configuration as correct, so a skimmed draft measures nothing.\n");
    draft.push_str("#\n");
    draft
        .push_str("# Edit the text after the colon. Timecodes and speaker labels are navigation\n");
    draft.push_str(
        "# aids and are stripped before scoring, as are '#' lines. Delete a line whose\n",
    );
    draft.push_str("# words were never spoken; add one the models missed entirely.\n\n");
    for (start, speaker, text) in merged {
        let seconds = start.as_secs();
        draft.push_str(&format!(
            "[{:02}:{:02}] {speaker}: {text}\n",
            seconds / 60,
            seconds % 60,
        ));
    }
    fs::write(path, draft)?;
    println!("draft\t{}", path.display());
    Ok(())
}

/// Strips draft annotations so a corrected draft can be scored directly.
///
/// `speaker` keeps only that speaker's lines, which is required whenever a single channel is being
/// scored: a two-speaker reference measured against one channel counts the other speaker's every
/// word as a deletion, and the resulting WER describes the question rather than the transcriber.
/// Unlabelled lines always survive, so a plain prose reference still works.
fn reference_text(raw: &str, speaker: Option<&str>) -> String {
    raw.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let after_timecode = line.split_once(']').map_or(line, |(_, rest)| rest);
            let Some((label, text)) = after_timecode.split_once(':') else {
                return Some(line);
            };
            match speaker {
                Some(wanted) if label.trim() != wanted => None,
                _ => Some(text),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Draft speaker label for a channel, matching what [`write_draft`] emits.
const fn draft_speaker(source: Source) -> &'static str {
    match source {
        Source::System => "them",
        Source::Mic => "you",
    }
}

/// One committed utterance, kept with its media time so a draft can be navigated by ear.
struct Segment {
    start: Duration,
    text: String,
}

/// Feeds the whole input through the shipping transcriber and returns its committed segments.
fn transcribe(
    transcriber: &mut FinalWhisperTranscriber,
    source: Source,
    samples: &[f32],
) -> Result<Vec<Segment>, Box<dyn std::error::Error>> {
    let mut segments = Vec::new();
    let collect = |transcriber: &mut FinalWhisperTranscriber, segments: &mut Vec<Segment>| {
        for update in transcriber.poll() {
            if let TranscriptUpdate::Final(utterance) = update {
                segments.push(Segment {
                    start: utterance.start,
                    text: utterance.text,
                });
            }
        }
    };
    for (index, chunk) in samples.chunks(FRAME_SAMPLES).enumerate() {
        transcriber.push(&AudioFrame {
            source,
            samples: Arc::from(chunk),
            sample_rate: 16_000,
            seq: index as u64,
            capture_ts: Instant::now(),
            stream_offset: Duration::from_nanos(
                (index * FRAME_SAMPLES) as u64 * 1_000_000_000 / 16_000,
            ),
        });
        collect(transcriber, &mut segments);
    }
    transcriber.finish();
    collect(transcriber, &mut segments);
    if let Some(error) = transcriber.poll_errors().into_iter().next() {
        return Err(error.into());
    }
    Ok(segments)
}

/// One named point in the accuracy sweep.
#[derive(Clone, Copy, Debug)]
struct Variant {
    name: &'static str,
    window: Duration,
    decoding: DecodingOptions,
}

impl Variant {
    /// The behaviour that shipped before any of these techniques existed.
    const fn baseline() -> Self {
        Self {
            name: "baseline",
            window: Duration::from_secs(5),
            decoding: DecodingOptions {
                context: Duration::ZERO,
                boundary_search: Duration::ZERO,
                prompt_carryover: false,
                suppress_non_speech: false,
            },
        }
    }

    /// Each technique alone against the baseline, then all of them together.
    ///
    /// One-at-a-time is the point. A combined run that improves says nothing about which technique
    /// earned it, and a combined run that regresses hides which one caused it.
    fn sweep() -> Vec<Self> {
        let baseline = Self::baseline();
        let mut all = DecodingOptions {
            context: Duration::from_secs(10),
            boundary_search: Duration::from_millis(500),
            prompt_carryover: true,
            suppress_non_speech: true,
        };
        let mut variants = vec![
            baseline,
            Self {
                name: "context-2s",
                decoding: DecodingOptions {
                    context: Duration::from_secs(2),
                    ..baseline.decoding
                },
                ..baseline
            },
            Self {
                name: "context-10s",
                decoding: DecodingOptions {
                    context: all.context,
                    ..baseline.decoding
                },
                ..baseline
            },
            Self {
                name: "trough-cut",
                decoding: DecodingOptions {
                    boundary_search: all.boundary_search,
                    ..baseline.decoding
                },
                ..baseline
            },
            Self {
                name: "prompt",
                decoding: DecodingOptions {
                    prompt_carryover: true,
                    ..baseline.decoding
                },
                ..baseline
            },
            Self {
                name: "suppress-nst",
                decoding: DecodingOptions {
                    suppress_non_speech: true,
                    ..baseline.decoding
                },
                ..baseline
            },
            Self {
                name: "all",
                decoding: all,
                ..baseline
            },
        ];
        // The pre-T086 window, carried so the sweep can also answer whether the shorter window this
        // product now runs cost accuracy, rather than assuming it did.
        all.context = Duration::from_secs(5);
        variants.push(Self {
            name: "all-10s-window",
            window: Duration::from_secs(10),
            decoding: all,
        });
        variants
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SignalLevels {
    peak: f64,
    rms: f64,
}

fn signal_levels(samples: &[f32]) -> SignalLevels {
    if samples.is_empty() {
        return SignalLevels::default();
    }
    let mut peak = 0.0_f64;
    let mut squared_sum = 0.0_f64;
    for sample in samples {
        let amplitude = f64::from(*sample).abs();
        peak = peak.max(amplitude);
        squared_sum += amplitude * amplitude;
    }
    SignalLevels {
        peak,
        rms: (squared_sum / samples.len() as f64).sqrt(),
    }
}

struct Arguments {
    size: String,
    input: PathBuf,
    channel: Option<Source>,
    reference: Option<PathBuf>,
    variant: Variant,
    sweep: bool,
    transcript: bool,
    draft: Option<PathBuf>,
}

fn parse_arguments(
    mut values: impl Iterator<Item = OsString>,
) -> Result<Arguments, Box<dyn std::error::Error>> {
    let size = values
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or(USAGE)?;
    let input = values.next().map(PathBuf::from).ok_or(USAGE)?;
    let mut channel = None;
    let mut reference = None;
    let mut variant = Variant::baseline();
    let mut sweep = false;
    let mut transcript = false;
    let mut draft = None;
    while let Some(flag) = values.next().and_then(|value| value.into_string().ok()) {
        match flag.as_str() {
            "--sweep" => sweep = true,
            "--transcript" => transcript = true,
            "--draft" => {
                draft = Some(
                    values
                        .next()
                        .map(PathBuf::from)
                        .ok_or("--draft requires an output path")?,
                );
            }
            "--variant" => {
                let name = values
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or("--variant requires a name")?;
                variant = Variant::sweep()
                    .into_iter()
                    .find(|candidate| candidate.name == name)
                    .ok_or_else(|| {
                        format!(
                            "unknown variant: {name}\nknown: {}",
                            Variant::sweep()
                                .iter()
                                .map(|candidate| candidate.name)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?;
            }
            "--channel" => {
                let value = values
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or("--channel requires meeting or microphone")?;
                channel = Some(match value.as_str() {
                    "meeting" => Source::System,
                    "microphone" => Source::Mic,
                    _ => return Err("--channel requires meeting or microphone".into()),
                });
            }
            "--reference" => {
                reference = Some(
                    values
                        .next()
                        .map(PathBuf::from)
                        .ok_or("--reference requires a UTF-8 text file")?,
                );
            }
            _ => return Err(format!("unsupported argument: {flag}\n{USAGE}").into()),
        }
    }
    Ok(Arguments {
        size,
        input,
        channel,
        reference,
        variant,
        sweep,
        transcript,
        draft,
    })
}

fn parse_size(value: &str) -> Result<ModelSize, Box<dyn std::error::Error>> {
    match value {
        "base.en" => Ok(ModelSize::BaseEn),
        "small.en" => Ok(ModelSize::SmallEn),
        "medium.en" => Ok(ModelSize::MediumEn),
        _ => Err(format!("unsupported model size: {value}").into()),
    }
}

const fn display_size(size: ModelSize) -> &'static str {
    match size {
        ModelSize::BaseEn => "base.en",
        ModelSize::SmallEn => "small.en",
        ModelSize::MediumEn => "medium.en",
    }
}

fn display_channel(source: Option<Source>) -> &'static str {
    match source {
        Some(Source::System) => "meeting (left/channel 0)",
        Some(Source::Mic) => "microphone (right/channel 1)",
        None => "mono WAV",
    }
}

fn read_input(
    path: &PathBuf,
    channel: Option<Source>,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let is_mp4 = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"));
    if is_mp4 {
        let source = channel
            .ok_or("retained MP4 input requires --channel meeting or --channel microphone")?;
        return Ok(decode_recording_channel(path, source)?.samples);
    }
    if channel.is_some() {
        return Err("--channel is only valid for retained MP4 input".into());
    }
    read_wav(path)
}

fn read_wav(path: &PathBuf) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != 16_000 || spec.bits_per_sample != 16 {
        return Err(format!(
            "expected 16-kHz mono PCM16 WAV, got {} Hz, {} channels, {} bits",
            spec.sample_rate, spec.channels, spec.bits_per_sample
        )
        .into());
    }
    reader
        .samples::<i16>()
        .map(|sample| sample.map(|value| f32::from(value) / f32::from(i16::MAX)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WordErrors {
    reference_words: usize,
    substitutions: usize,
    deletions: usize,
    insertions: usize,
}

impl WordErrors {
    const fn total(self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }

    fn wer_percent(self) -> f64 {
        if self.reference_words == 0 {
            return if self.total() == 0 {
                0.0
            } else {
                f64::INFINITY
            };
        }
        100.0 * self.total() as f64 / self.reference_words as f64
    }
}

fn word_errors(reference: &str, hypothesis: &str) -> WordErrors {
    let reference = normalized_words(reference);
    let hypothesis = normalized_words(hypothesis);
    let mut previous = (0..=hypothesis.len())
        .map(|insertions| WordErrors {
            reference_words: reference.len(),
            insertions,
            ..WordErrors::default()
        })
        .collect::<Vec<_>>();
    for (reference_index, reference_word) in reference.iter().enumerate() {
        let mut current = vec![WordErrors::default(); hypothesis.len() + 1];
        current[0] = WordErrors {
            reference_words: reference.len(),
            deletions: reference_index + 1,
            ..WordErrors::default()
        };
        for (hypothesis_index, hypothesis_word) in hypothesis.iter().enumerate() {
            if reference_word == hypothesis_word {
                current[hypothesis_index + 1] = previous[hypothesis_index];
                continue;
            }
            let mut substitution = previous[hypothesis_index];
            substitution.substitutions += 1;
            let mut deletion = previous[hypothesis_index + 1];
            deletion.deletions += 1;
            let mut insertion = current[hypothesis_index];
            insertion.insertions += 1;
            current[hypothesis_index + 1] = [substitution, deletion, insertion]
                .into_iter()
                .min_by_key(|errors| {
                    (
                        errors.total(),
                        errors.substitutions,
                        errors.deletions,
                        errors.insertions,
                    )
                })
                .unwrap_or_default();
        }
        previous = current;
    }
    previous.last().copied().unwrap_or_default()
}

fn normalized_words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character.is_whitespace() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{signal_levels, word_errors};

    #[test]
    fn signal_levels_report_peak_and_root_mean_square() {
        let levels = signal_levels(&[0.5, -1.0, 0.5, 0.0]);

        assert_eq!(levels.peak, 1.0);
        assert_eq!(levels.rms, (0.375_f64).sqrt());
    }

    #[test]
    fn word_error_counts_substitutions_deletions_and_insertions() {
        let errors = word_errors("Alpha beta gamma delta", "alpha theta delta extra");

        assert_eq!(errors.reference_words, 4);
        assert_eq!(errors.substitutions, 1);
        assert_eq!(errors.deletions, 1);
        assert_eq!(errors.insertions, 1);
        assert_eq!(errors.wer_percent(), 75.0);
    }

    #[test]
    fn word_error_normalization_ignores_case_and_punctuation() {
        let errors = word_errors("Hello, WORLD!", "hello world");

        assert_eq!(errors.total(), 0);
        assert_eq!(errors.wer_percent(), 0.0);
    }
}
