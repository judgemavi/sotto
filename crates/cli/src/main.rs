#![deny(warnings)]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use cli::{PipelineOptions, run_files};
use sotto_core::SessionId;
use sotto_core::{EventKind, TimelineEvent, replay};
use std::{
    collections::HashSet,
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    name = "sotto-cli",
    about = "Headless Sotto harness (VAD/prosody/RAG live; ASR live with --model; OCR live on macOS)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run WAV streams and timestamped frames through the headless pipeline.
    Run {
        wav: PathBuf,
        #[arg(long)]
        system: Option<PathBuf>,
        #[arg(long)]
        frames: Option<PathBuf>,
        #[arg(long)]
        model: Option<PathBuf>,
        #[arg(long)]
        realtime: bool,
        #[arg(long, value_delimiter = ',')]
        kinds: Vec<String>,
    },
    /// Run on-device Whisper only (requires downloaded ggml model weights).
    Transcribe {
        wav: PathBuf,
        #[arg(long, env = "SOTTO_WHISPER_MODEL")]
        model: PathBuf,
    },
    /// Emit Silero VAD transitions only.
    Vad { wav: PathBuf },
    /// OCR timestamped PNG frames without display or capture hardware.
    Ocr { frames_dir: PathBuf },
    /// Report stage latency as a table and JSON.
    Bench {
        wav: PathBuf,
        #[arg(long)]
        system: Option<PathBuf>,
        #[arg(long, env = "SOTTO_WHISPER_MODEL")]
        model: Option<PathBuf>,
        #[arg(long)]
        realtime: bool,
    },
    /// Validate and project a recorded timeline through the replay consumer.
    Replay {
        jsonl: PathBuf,
        #[arg(long, value_delimiter = ',')]
        kinds: Vec<String>,
    },
    /// Add a UTF-8/Markdown document to the local RAG store.
    Ingest {
        path: PathBuf,
        #[arg(long, default_value = "sotto.sqlite3")]
        database: PathBuf,
    },
    /// Search the local hybrid RAG index.
    Search {
        query: String,
        #[arg(long, default_value = "sotto.sqlite3")]
        database: PathBuf,
        #[arg(short = 'k', long, default_value_t = 5)]
        limit: usize,
    },
    /// Generate a cited, structured recap from a persisted session.
    Summarize {
        session_id: u128,
        #[arg(long, default_value = "sotto.sqlite3")]
        database: PathBuf,
        #[arg(long, default_value = "ollama")]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long, default_value = "ocr")]
        screen_context: String,
    },
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::Run {
            wav,
            system,
            frames,
            model,
            realtime,
            kinds,
        } => {
            let asr_configured = model.is_some();
            let frames_configured = frames.is_some();
            let run = run_files(&PipelineOptions {
                mic: wav,
                system,
                frames,
                model,
                realtime,
            })?;
            warn_run_stages(&run.events, asr_configured, frames_configured);
            write_events(&run.events, &parse_kinds(&kinds)?)?;
        }
        Command::Transcribe { wav, model } => {
            let run = run_files(&PipelineOptions {
                mic: wav,
                system: None,
                frames: None,
                model: Some(model),
                realtime: false,
            })?;
            warn_if_missing(&run.events, "ASR", |kind| {
                matches!(
                    kind,
                    EventKind::UtterancePartial | EventKind::UtteranceFinal
                )
            });
            write_events(
                &run.events,
                &HashSet::from([EventKind::UtterancePartial, EventKind::UtteranceFinal]),
            )?;
        }
        Command::Vad { wav } => {
            let run = run_files(&PipelineOptions {
                mic: wav,
                system: None,
                frames: None,
                model: None,
                realtime: false,
            })?;
            warn_if_missing(&run.events, "VAD", |kind| kind == EventKind::Vad);
            write_events(&run.events, &HashSet::from([EventKind::Vad]))?;
        }
        Command::Ocr { frames_dir } => {
            let payloads = cli::pipeline::load_screen_payloads(&frames_dir)?;
            if payloads.is_empty() {
                eprintln!("warning: screen stage emitted no events (no frames found)");
            }
            for (_, payload) in payloads {
                println!("{}", serde_json::to_string(&payload)?);
            }
        }
        Command::Bench {
            wav,
            system,
            model,
            realtime,
        } => {
            let report = run_files(&PipelineOptions {
                mic: wav,
                system,
                frames: None,
                model,
                realtime,
            })?
            .latency;
            print_latency(&report)?;
        }
        Command::Replay { jsonl, kinds } => {
            let events = read_events(&jsonl)?;
            let state = replay(&events).context("timeline replay validation failed")?;
            let active: Vec<_> = state.active().values().cloned().collect();
            write_events(&active, &parse_kinds(&kinds)?)?;
        }
        Command::Ingest { path, database } => println!(
            "{}",
            if cli::pipeline::ingest_file(&database, &path)? {
                "ingested"
            } else {
                "unchanged"
            }
        ),
        Command::Search {
            query,
            database,
            limit,
        } => {
            let store = rag::Store::open(database)?;
            for chunk in store.search_filtered(&query, limit, &rag::SearchFilter::default())? {
                println!("{}", serde_json::to_string(&chunk)?);
            }
        }
        Command::Summarize {
            session_id,
            database,
            provider,
            model,
            screen_context,
        } => {
            let kind = parse_provider(&provider)?;
            let key = providers::load_key(kind)?;
            let provider = std::sync::Arc::new(providers::Provider::new(kind, model, key));
            let store = rag::Store::open(database)?;
            let summarizer = insight::Summarizer::new(&store, provider)
                .with_context(parse_screen_context(&screen_context)?);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let report = runtime.block_on(summarizer.summarize(SessionId::new(session_id)))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

fn parse_provider(value: &str) -> Result<providers::ProviderKind> {
    match value {
        "anthropic" => Ok(providers::ProviderKind::Anthropic),
        "openai" => Ok(providers::ProviderKind::OpenAi),
        "google" => Ok(providers::ProviderKind::Google),
        "openrouter" => Ok(providers::ProviderKind::OpenRouter),
        "ollama" => Ok(providers::ProviderKind::Ollama),
        _ => anyhow::bail!(
            "unknown provider {value}; expected anthropic, openai, google, openrouter, or ollama"
        ),
    }
}

fn parse_screen_context(value: &str) -> Result<insight::ContextMode> {
    match value {
        "metadata" => Ok(insight::ContextMode::Metadata),
        "ocr" => Ok(insight::ContextMode::MetadataAndOcr),
        "images" => Ok(insight::ContextMode::MetadataAndImages),
        _ => anyhow::bail!("unknown screen context {value}; expected metadata, ocr, or images"),
    }
}

fn warn_run_stages(events: &[TimelineEvent], asr_configured: bool, frames_configured: bool) {
    warn_if_missing(events, "VAD", |kind| kind == EventKind::Vad);
    if asr_configured {
        warn_if_missing(events, "ASR", |kind| {
            matches!(
                kind,
                EventKind::UtterancePartial | EventKind::UtteranceFinal
            )
        });
        warn_if_missing(events, "prosody", |kind| kind == EventKind::Prosody);
    } else {
        eprintln!("warning: ASR stage disabled (no model configured)");
        eprintln!("warning: prosody stage emitted no events (ASR produced no utterances)");
    }
    if frames_configured {
        warn_if_missing(events, "screen", |kind| kind == EventKind::ScreenSnapshot);
    } else {
        eprintln!("warning: screen stage disabled (no frames provided)");
    }
    eprintln!("warning: advisor stage disabled (no reasoning model configured)");
}

fn warn_if_missing(events: &[TimelineEvent], stage: &str, emitted: impl Fn(EventKind) -> bool) {
    if !events.iter().any(|event| emitted(event.kind())) {
        eprintln!("warning: {stage} stage emitted no events");
    }
}

fn write_events(events: &[TimelineEvent], kinds: &HashSet<EventKind>) -> Result<()> {
    let mut output = BufWriter::new(std::io::stdout().lock());
    for event in events {
        if kinds.is_empty() || kinds.contains(&event.kind()) {
            serde_json::to_writer(&mut output, event)?;
            output.write_all(b"\n")?;
        }
    }
    output.flush()?;
    Ok(())
}

fn read_events(path: &Path) -> Result<Vec<TimelineEvent>> {
    let input = BufReader::new(
        File::open(path).with_context(|| format!("open timeline {}", path.display()))?,
    );
    input
        .lines()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Ok(line) if line.trim().is_empty() => None,
            result => Some((index, result)),
        })
        .map(|(index, line)| {
            let line = line.with_context(|| format!("read line {}", index + 1))?;
            serde_json::from_str(&line).with_context(|| format!("parse line {}", index + 1))
        })
        .collect()
}

fn parse_kinds(values: &[String]) -> Result<HashSet<EventKind>> {
    values
        .iter()
        .map(|value| match value.as_str() {
            "utterance.partial" => Ok(EventKind::UtterancePartial),
            "utterance.final" => Ok(EventKind::UtteranceFinal),
            "vad" => Ok(EventKind::Vad),
            "prosody" => Ok(EventKind::Prosody),
            "screen.snapshot" => Ok(EventKind::ScreenSnapshot),
            "trigger" => Ok(EventKind::Trigger),
            "suggestion.partial" => Ok(EventKind::SuggestionPartial),
            "suggestion.final" => Ok(EventKind::SuggestionFinal),
            "annotation.user" => Ok(EventKind::UserAnnotation),
            "error" => Ok(EventKind::Error),
            _ => anyhow::bail!("unknown event kind: {value}"),
        })
        .collect()
}

fn print_latency(report: &cli::LatencyReport) -> Result<()> {
    println!("stage                              count      p50      p95      p99");
    print_row("frame -> VAD decision", &report.frame_to_vad);
    print_row("speech end -> first partial", &report.speech_end_to_partial);
    print_row("speech end -> final", &report.speech_end_to_final);
    if let Some(values) = &report.speech_end_to_suggestion {
        print_row("speech end -> suggestion", values);
    } else {
        println!("{:<34} {:>5}", "speech end -> suggestion", "n/a");
    }
    println!("{}", serde_json::to_string(report)?);
    Ok(())
}

fn print_row(name: &str, values: &cli::pipeline::Percentiles) {
    println!(
        "{name:<34} {:>5} {:>8.2} {:>8.2} {:>8.2}",
        values.count, values.p50_ms, values.p95_ms, values.p99_ms
    );
}
