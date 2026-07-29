use std::{
    env,
    error::Error,
    fs::File,
    io::BufWriter,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use capture::macos::{FrameReceiver, MacCapture, RawFrame};
use sotto_core::{CaptureBackend, Source};
use tokio::sync::broadcast;

const DEFAULT_SECONDS: u64 = 3_900;

fn main() -> Result<(), Box<dyn Error>> {
    let seconds = env::args()
        .nth(1)
        .map_or(Ok(DEFAULT_SECONDS), |value| value.parse::<u64>())?;
    let output = env::current_dir()?.join("capture-soak-output");
    std::fs::create_dir_all(&output)?;

    let (audio_tx, mut audio_rx) = broadcast::channel(256);
    let mut capture = MacCapture::new();
    if capture.permission_status() != sotto_core::PermissionStatus::Authorized {
        let _ = MacCapture::request_permission();
        return Err(
            "Screen & System Audio Recording permission is required; re-run after granting it"
                .into(),
        );
    }
    capture.start(audio_tx)?;
    let frames = capture
        .take_frame_receiver()
        .ok_or("capture did not install its frame receiver")?;
    let mut mic = wav_writer(&output.join("mic.wav"))?;
    let mut system = wav_writer(&output.join("system.wav"))?;
    let started = Instant::now();
    let mut frame_index = 0_u64;
    let mut mic_drift = DriftTracker::default();
    let mut system_drift = DriftTracker::default();

    while started.elapsed() < Duration::from_secs(seconds) {
        while let Ok(frame) = audio_rx.try_recv() {
            let writer = match frame.source {
                Source::Mic => {
                    mic_drift.observe(frame.stream_offset);
                    &mut mic
                }
                Source::System => {
                    system_drift.observe(frame.stream_offset);
                    &mut system
                }
            };
            for sample in frame.samples.iter().copied() {
                writer.write_sample(sample)?;
            }
        }
        drain_frames(&frames, &output, &mut frame_index)?;
        thread::sleep(Duration::from_millis(5));
    }
    capture.stop();
    mic.finalize()?;
    system.finalize()?;
    println!(
        "captured {frame_index} frames; {} frames dropped",
        capture.dropped_frame_count()
    );
    println!("mic drift: {:.3} samples/s", mic_drift.samples_per_second());
    println!(
        "system drift: {:.3} samples/s",
        system_drift.samples_per_second()
    );
    println!(
        "sign before the real run: MACOS_SIGNING_IDENTITY=- scripts/sign.sh target/release/examples/soak"
    );
    Ok(())
}

#[derive(Default)]
struct DriftTracker {
    first_stream: Option<Duration>,
    first_host: Option<Instant>,
    last_stream: Duration,
    last_host: Option<Instant>,
}

impl DriftTracker {
    fn observe(&mut self, stream: Duration) {
        let now = Instant::now();
        self.first_stream.get_or_insert(stream);
        self.first_host.get_or_insert(now);
        self.last_stream = stream;
        self.last_host = Some(now);
    }

    fn samples_per_second(&self) -> f64 {
        let (Some(first_stream), Some(first_host), Some(last_host)) =
            (self.first_stream, self.first_host, self.last_host)
        else {
            return 0.0;
        };
        let host = last_host.duration_since(first_host).as_secs_f64();
        if host == 0.0 {
            return 0.0;
        }
        let device = self.last_stream.saturating_sub(first_stream).as_secs_f64();
        (device - host) * 16_000.0 / host
    }
}

fn wav_writer(path: &Path) -> Result<hound::WavWriter<BufWriter<File>>, hound::Error> {
    hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
}

fn drain_frames(
    receiver: &FrameReceiver,
    output: &Path,
    index: &mut u64,
) -> Result<(), Box<dyn Error>> {
    while let Some(frame) = receiver.try_recv() {
        write_png(output, *index, &frame)?;
        *index = index.saturating_add(1);
        receiver.recycle(frame);
    }
    Ok(())
}

fn write_png(output: &Path, index: u64, frame: &RawFrame) -> Result<(), Box<dyn Error>> {
    let path = output.join(format!(
        "frame-{index:06}-stream-{}-host-{}.png",
        frame.stream_time_ns, frame.host_time_ns
    ));
    let file = BufWriter::new(File::create(path)?);
    let mut encoder = png::Encoder::new(file, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let width_bytes = usize::try_from(frame.width)?.saturating_mul(4);
    let stride = usize::try_from(frame.stride)?;
    let height = usize::try_from(frame.height)?;
    let mut rgba = Vec::with_capacity(width_bytes.saturating_mul(height));
    for row in frame.bytes.chunks(stride).take(height) {
        for pixel in row[..width_bytes.min(row.len())].chunks_exact(4) {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    writer.write_image_data(&rgba)?;
    Ok(())
}
