use std::{fs, path::Path, process::Command};

const RATE: u32 = 16_000;
const SECONDS: u32 = 30;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !cfg!(target_os = "macos") {
        return Err("fixture generation requires macOS `say` and `afconvert`".into());
    }

    let root = Path::new("fixtures");
    fs::create_dir_all(root.join("call-01-frames"))?;
    fs::create_dir_all(root.join("timelines"))?;
    let temporary = tempfile::tempdir()?;

    write_dialogue(
        root.join("call-01-mic.wav"),
        "Samantha",
        &[
            (1, "Thanks for joining. How are you handling pricing today?"),
            (13, "Let me walk through the enterprise plan."),
            (24, "The migration and support are included."),
        ],
        temporary.path(),
    )?;
    write_dialogue(
        root.join("call-01-sys.wav"),
        "Daniel",
        &[
            (6, "We need predictable pricing for the enterprise rollout."),
            (18, "The price is higher than Acme, our current vendor."),
        ],
        temporary.path(),
    )?;
    write_dialogue(
        root.join("objection-mic.wav"),
        "Samantha",
        &[
            (1, "What concerns do you have about moving forward?"),
            (
                18,
                "Our enterprise plan includes migration and priority support.",
            ),
        ],
        temporary.path(),
    )?;
    write_dialogue(
        root.join("objection-system.wav"),
        "Daniel",
        &[(8, "The price is higher than Acme, our current vendor.")],
        temporary.path(),
    )?;
    write_dialogue(
        root.join("crosstalk-mic.wav"),
        "Samantha",
        &[
            (
                2,
                "Let me explain how the rollout works across your entire organization.",
            ),
            (
                19,
                "We can phase the migration and train each regional team.",
            ),
        ],
        temporary.path(),
    )?;
    write_dialogue(
        root.join("crosstalk-system.wav"),
        "Daniel",
        &[(
            8,
            "I need to stop you there because our timeline is much shorter than that.",
        )],
        temporary.path(),
    )?;
    write_dialogue(
        root.join("long-silence.wav"),
        "Samantha",
        &[(1, "Let me check that."), (25, "Yes, support is included.")],
        temporary.path(),
    )?;
    write_silence(root.join("silence.wav"))?;
    write_music(root.join("music.wav"))?;

    let status = Command::new("swift")
        .arg("crates/cli/examples/render_fixture_frames.swift")
        .arg(root.join("call-01-frames"))
        .status()?;
    if !status.success() {
        return Err("Swift frame renderer failed".into());
    }
    Ok(())
}

fn write_dialogue(
    path: impl AsRef<Path>,
    voice: &str,
    turns: &[(u32, &str)],
    temporary: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut track = vec![0_i16; (RATE * SECONDS) as usize];
    for (index, (start, text)) in turns.iter().enumerate() {
        let aiff = temporary.join(format!("{voice}-{index}.aiff"));
        let wav = temporary.join(format!("{voice}-{index}.wav"));
        run(Command::new("say")
            .args(["-v", voice, "-r", "185", "-o"])
            .arg(&aiff)
            .arg(text))?;
        run(Command::new("afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
            .arg(&aiff)
            .arg(&wav))?;
        let mut reader = hound::WavReader::open(wav)?;
        let offset = (*start * RATE) as usize;
        for (destination, sample) in track[offset..].iter_mut().zip(reader.samples::<i16>()) {
            *destination = sample?;
        }
    }
    write_samples(path, &track)
}

fn write_silence(path: impl AsRef<Path>) -> Result<(), Box<dyn std::error::Error>> {
    write_samples(path, &vec![0; (RATE * SECONDS) as usize])
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the bounded sine amplitude is deliberately quantized to PCM i16"
)]
fn write_music(path: impl AsRef<Path>) -> Result<(), Box<dyn std::error::Error>> {
    let samples = (0..RATE * SECONDS)
        .map(|index| {
            let phase = index as f32 / RATE as f32 * 440.0 * std::f32::consts::TAU;
            (phase.sin() * 5_898.0) as i16
        })
        .collect::<Vec<_>>();
    write_samples(path, &samples)
}

fn write_samples(
    path: impl AsRef<Path>,
    samples: &[i16],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(())
}

fn run(command: &mut Command) -> Result<(), Box<dyn std::error::Error>> {
    let display = format!("{command:?}");
    if command.status()?.success() {
        Ok(())
    } else {
        Err(format!("command failed: {display}").into())
    }
}
