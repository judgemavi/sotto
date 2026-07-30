use std::{
    env,
    error::Error,
    fs::File,
    io::BufWriter,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use capture::macos::{FrameReceiver, MacCapture, RawFrame};
use sotto_core::{CaptureBackend, Source};
use tokio::sync::broadcast;

const DEFAULT_SECONDS: u64 = 3_900;
/// Must match `capture`'s internal output rate.
const OUTPUT_RATE: u32 = 16_000;

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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let target = runtime
        .block_on(MacCapture::pick_target())
        .ok_or("target selection was cancelled")?;
    println!("selected capture target: {:?}", target.description());
    capture.start_with_target(&target, audio_tx)?;
    let frames = capture
        .take_frame_receiver()
        .ok_or("capture did not install its frame receiver")?;
    let mut mic = wav_writer(&output.join("mic.wav"))?;
    let mut system = wav_writer(&output.join("system.wav"))?;
    let started = Instant::now();
    let mut mic_drift = DriftTracker::default();
    let mut system_drift = DriftTracker::default();
    let mut mic_rate = RateTracker::default();
    let mut system_rate = RateTracker::default();

    // PNG encoding is far too slow to sit on the audio path: a 3456x2234 frame is ~7.7M
    // pixels of BGRA->RGBA shuffling plus deflate, and doing that inline stalls the
    // broadcast receiver long enough to drop audio. The bridge already isolates video
    // backpressure from audio; the harness has to do the same or it measures itself.
    let writing = Arc::new(AtomicBool::new(true));
    let writer_flag = Arc::clone(&writing);
    let frame_writer = thread::spawn(move || -> Result<u64, String> {
        let mut index = 0_u64;
        while writer_flag.load(Ordering::Acquire) {
            drain_frames(&frames, &output, &mut index).map_err(|e| e.to_string())?;
            thread::sleep(Duration::from_millis(50));
        }
        drain_frames(&frames, &output, &mut index).map_err(|e| e.to_string())?;
        Ok(index)
    });

    let mut lagged_packets = 0_u64;
    while started.elapsed() < Duration::from_secs(seconds) {
        loop {
            let frame = match audio_rx.try_recv() {
                Ok(frame) => frame,
                // Lagged means the harness dropped audio it never wrote. Silently
                // breaking here would understate the delivered rate and look like drift.
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    lagged_packets = lagged_packets.saturating_add(skipped);
                    continue;
                }
                Err(_) => break,
            };
            let writer = match frame.source {
                Source::Mic => {
                    mic_drift.observe(frame.stream_offset);
                    mic_rate.observe(frame.samples.len());
                    &mut mic
                }
                Source::System => {
                    system_drift.observe(frame.stream_offset);
                    system_rate.observe(frame.samples.len());
                    &mut system
                }
            };
            for sample in frame.samples.iter().copied() {
                writer.write_sample(sample)?;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    writing.store(false, Ordering::Release);
    let frame_index = frame_writer
        .join()
        .map_err(|_| "frame writer thread panicked")??;
    capture.stop();
    mic.finalize()?;
    system.finalize()?;
    println!(
        "captured {frame_index} frames; {} frames dropped",
        capture.dropped_frame_count()
    );
    println!("audio packets lost to harness lag: {lagged_packets}");
    println!(
        "mic timestamp drift: {:.3} samples/s (host-derived; see note)",
        mic_drift.samples_per_second()
    );
    println!(
        "system timestamp drift: {:.3} samples/s",
        system_drift.samples_per_second()
    );
    println!(
        "mic delivered rate:    {:.3} Hz ({:+.1} ppm vs {OUTPUT_RATE})",
        mic_rate.hz(),
        mic_rate.ppm()
    );
    println!(
        "system delivered rate: {:.3} Hz ({:+.1} ppm vs {OUTPUT_RATE})",
        system_rate.hz(),
        system_rate.ppm()
    );
    let relative_ppm = system_rate.ppm() - mic_rate.ppm();
    let slip_per_hour = relative_ppm * 3_600.0 / 1_000_000.0;
    println!(
        "RELATIVE stream drift: {relative_ppm:+.1} ppm -> {slip_per_hour:+.3} s of slip per hour"
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
        (device - host) * f64::from(OUTPUT_RATE) / host
    }
}

/// Measures how many samples a stream actually delivers per second of wall clock.
///
/// This is the measurement that matters for the product. [`DriftTracker`] compares a
/// stream's own timestamps against the host clock, which is only meaningful when those
/// timestamps come from an independent clock: the mic path derives `stream_offset` from
/// the host clock itself, so its "drift" is the host clock compared to itself and reads
/// near zero no matter how badly the device is actually running. Counting delivered
/// samples works identically for both paths, and the difference between the two streams
/// is exactly what makes cross-stream timestamps comparable — or not.
#[derive(Default)]
struct RateTracker {
    samples: u64,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl RateTracker {
    fn observe(&mut self, samples: usize) {
        let now = Instant::now();
        if self.first.is_none() {
            // Exclude the first packet's own duration: we time from its arrival onward.
            self.first = Some(now);
            self.last = Some(now);
            return;
        }
        self.samples = self
            .samples
            .saturating_add(u64::try_from(samples).unwrap_or(u64::MAX));
        self.last = Some(now);
    }

    fn hz(&self) -> f64 {
        let (Some(first), Some(last)) = (self.first, self.last) else {
            return 0.0;
        };
        let elapsed = last.duration_since(first).as_secs_f64();
        if elapsed <= 0.0 {
            return 0.0;
        }
        let samples = u32::try_from(self.samples).map_or_else(|_| f64::from(u32::MAX), f64::from);
        samples / elapsed
    }

    fn ppm(&self) -> f64 {
        let expected = f64::from(OUTPUT_RATE);
        (self.hz() - expected) / expected * 1_000_000.0
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
