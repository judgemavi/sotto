use std::{
    env,
    error::Error,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use capture::macos::{
    CaptureStatus, FrameReceiver, MacCapture, PickOutcome, RawFrame, probe_recording,
};
use sotto_core::{AudioFrame, CaptureBackend, Source};
use tokio::sync::broadcast;

const DEFAULT_SECONDS: u64 = 3_900;
/// Must match `capture`'s internal output rate.
const OUTPUT_RATE: u32 = 16_000;
const CRASH_ELAPSED_FILE: &str = "crash-elapsed-ns";
const CRASH_PREFIX_TOLERANCE: Duration = Duration::from_secs(6);
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigint(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Release);
}

fn install_sigint_handler() -> Result<(), std::io::Error> {
    INTERRUPTED.store(false, Ordering::Release);
    // SAFETY: `handle_sigint` only performs a lock-free atomic store, is valid for the
    // process lifetime, and has the C signal-handler ABI required by `signal`.
    let previous = unsafe {
        libc::signal(
            libc::SIGINT,
            handle_sigint as *const () as libc::sighandler_t,
        )
    };
    if previous == libc::SIG_ERR {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    install_sigint_handler()?;
    let seconds = env::args()
        .nth(1)
        .map_or(Ok(DEFAULT_SECONDS), |value| value.parse::<u64>())?;
    let output = env::current_dir()?.join("capture-soak-output");
    std::fs::create_dir_all(&output)?;
    let recording_path = output.join("session.mp4");
    if env::var_os("SOTTO_RECORDING_PROBE_ONLY").is_some() {
        let recording = probe_recording(&recording_path)?;
        let crash_elapsed = read_crash_elapsed(&output.join(CRASH_ELAPSED_FILE))?;
        let lost_tail = crash_elapsed.saturating_sub(recording.duration);
        if lost_tail > CRASH_PREFIX_TOLERANCE {
            return Err(format!(
                "retained prefix lost {lost_tail:?} from the {crash_elapsed:?} interrupted capture; tolerance is {CRASH_PREFIX_TOLERANCE:?}"
            )
            .into());
        }
        println!(
            "decoded retained prefix at {:?}, sought to {:?}; media duration {:?}; lost tail {:?}; {} bytes",
            recording.first_video_timestamp,
            recording.seek_video_timestamp,
            recording.duration,
            lost_tail,
            recording.byte_size
        );
        return Ok(());
    }
    let crash_after = env::var("SOTTO_CAPTURE_CRASH_AFTER_SECONDS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?;

    let (audio_tx, mut audio_rx) = broadcast::channel(256);
    if MacCapture::permission_status() != sotto_core::PermissionStatus::Authorized {
        let _ = MacCapture::request_permission();
        return Err(
            "Screen & System Audio Recording permission is required; re-run after granting it"
                .into(),
        );
    }
    // This example has no AppKit event loop, so it drives the run loop itself
    // rather than awaiting the async picker — see `pick_target_blocking`.
    println!("choose a capture target in the system picker…");
    let target = match MacCapture::pick_target_blocking() {
        PickOutcome::Picked(target) => target,
        PickOutcome::Cancelled => return Err("target selection was cancelled".into()),
        PickOutcome::NotDetermined => {
            return Err(
                "Screen & System Audio Recording was just requested; approve the prompt and re-run"
                    .into(),
            );
        }
        PickOutcome::Denied => {
            return Err(
                "Screen & System Audio Recording permission is required; re-run after granting it"
                    .into(),
            );
        }
    };
    println!("selected capture target: {:?}", target.description());
    let mut capture = target.into_capture();
    capture.record_to(&recording_path)?;
    // Subscribe before starting: these are broadcast channels, so a subscriber created
    // afterwards misses the Running transition and anything that raced it.
    let mut status_rx = capture.subscribe_status();
    let mut error_rx = capture.subscribe_errors();
    capture.start(audio_tx)?;
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
    let frame_output = output.clone();
    let frame_writer = thread::spawn(move || -> Result<u64, String> {
        let mut index = 0_u64;
        while writer_flag.load(Ordering::Acquire) {
            drain_frames(&frames, &frame_output, &mut index).map_err(|e| e.to_string())?;
            thread::sleep(Duration::from_millis(50));
        }
        drain_frames(&frames, &frame_output, &mut index).map_err(|e| e.to_string())?;
        Ok(index)
    });

    let mut lagged_packets = 0_u64;
    let mut terminal_status = None;
    let mut observed_errors = Vec::new();
    while started.elapsed() < Duration::from_secs(seconds) && !INTERRUPTED.load(Ordering::Acquire) {
        if crash_after.is_some_and(|limit| started.elapsed() >= Duration::from_secs(limit)) {
            write_crash_elapsed(&output.join(CRASH_ELAPSED_FILE), started.elapsed())?;
            eprintln!(
                "simulating abrupt process loss after {:.1}s; probe the retained prefix with SOTTO_RECORDING_PROBE_ONLY=1",
                started.elapsed().as_secs_f64()
            );
            // SAFETY: this opt-in evidence mode intentionally bypasses Rust and native destructors
            // to model power/process loss while AVAssetWriter has only committed fragments.
            unsafe { libc::_exit(86) };
        }
        while let Ok(error) = error_rx.try_recv() {
            observed_errors.push(error.to_string());
            println!(
                "[{:>6.1}s] capture error: {error}",
                started.elapsed().as_secs_f64()
            );
        }
        while let Ok(status) = status_rx.try_recv() {
            println!(
                "[{:>6.1}s] capture status: {status:?}",
                started.elapsed().as_secs_f64()
            );
            // The whole point of the target-disappeared path is that the session ends
            // when the thing being captured does. Running to the timer anyway would
            // hide whether that ever fired.
            if matches!(
                status,
                CaptureStatus::TargetEnded | CaptureStatus::UserStopped | CaptureStatus::Failed
            ) {
                terminal_status = Some(status);
                break;
            }
        }
        if terminal_status.is_some() {
            break;
        }
        drain_audio(
            &mut audio_rx,
            &mut mic,
            &mut system,
            &mut mic_drift,
            &mut system_drift,
            &mut mic_rate,
            &mut system_rate,
            &mut lagged_packets,
        )?;
        thread::sleep(Duration::from_millis(5));
    }

    // Stop the producer before the final drains. Swift shutdown is asynchronous, so wait
    // only for a bounded Stopped observation while continuing to drain audio; never turn a
    // bridge fault into a hanging evidence run.
    capture.stop();
    let shutdown_deadline = Instant::now() + Duration::from_secs(5);
    let mut stopped_observed = false;
    while Instant::now() < shutdown_deadline {
        drain_audio(
            &mut audio_rx,
            &mut mic,
            &mut system,
            &mut mic_drift,
            &mut system_drift,
            &mut mic_rate,
            &mut system_rate,
            &mut lagged_packets,
        )?;
        while let Ok(status) = status_rx.try_recv() {
            if status == CaptureStatus::Stopped {
                stopped_observed = true;
                break;
            }
        }
        while let Ok(error) = error_rx.try_recv() {
            observed_errors.push(error.to_string());
        }
        if stopped_observed {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    drain_audio(
        &mut audio_rx,
        &mut mic,
        &mut system,
        &mut mic_drift,
        &mut system_drift,
        &mut mic_rate,
        &mut system_rate,
        &mut lagged_packets,
    )?;
    if !stopped_observed {
        println!("capture stop was not observed within 5s; finalizing bounded evidence");
    }
    writing.store(false, Ordering::Release);
    let frame_index = frame_writer
        .join()
        .map_err(|_| "frame writer thread panicked")??;
    finalize_audio(mic, system)?;
    let interrupted = INTERRUPTED.load(Ordering::Acquire);
    match (interrupted, terminal_status) {
        (true, _) => println!(
            "capture interrupted after {:.1}s; outputs finalized",
            started.elapsed().as_secs_f64()
        ),
        (false, Some(status)) => println!(
            "capture ended early after {:.1}s: {status:?}",
            started.elapsed().as_secs_f64()
        ),
        (false, None) => println!("capture ran the full {seconds}s with no terminal status"),
    }
    println!(
        "captured {frame_index} frames; {} frames dropped",
        capture.dropped_frame_count()
    );
    println!("audio packets lost to harness lag: {lagged_packets}");
    let recording = probe_recording(&recording_path)?;
    if recording
        .first_video_timestamp
        .is_none_or(|timestamp| timestamp > Duration::from_secs(1))
    {
        return Err(format!(
            "first decoded recording frame was unexpectedly late at {:?}",
            recording.first_video_timestamp
        )
        .into());
    }
    if recording.duration.abs_diff(started.elapsed()) > Duration::from_secs(2) {
        return Err(format!(
            "recording duration {:?} did not match elapsed capture {:?}",
            recording.duration,
            started.elapsed()
        )
        .into());
    }
    if let Ok(limit) = env::var("SOTTO_RECORDING_MAX_BYTES") {
        if terminal_status != Some(CaptureStatus::Failed) {
            return Err(format!(
                "the {limit}-byte fault injection did not terminate capture as Failed"
            )
            .into());
        }
        if !observed_errors
            .iter()
            .any(|error| error.contains("SOTTO_RECORDING_MAX_BYTES cap"))
        {
            return Err(format!(
                "the {limit}-byte fault injection did not retain its truthful cause: {observed_errors:?}"
            )
            .into());
        }
    }
    println!(
        "decoded recording frame at {:?}, sought to {:?}; media duration {:?}; {} bytes",
        recording.first_video_timestamp,
        recording.seek_video_timestamp,
        recording.duration,
        recording.byte_size
    );
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
        "drift over a short run is dominated by startup transients; trust it only over 10+ minutes"
    );
    println!("bundle before the real run: scripts/dev-bundle.sh target/release/examples/soak");
    Ok(())
}

fn write_crash_elapsed(path: &Path, elapsed: Duration) -> Result<(), Box<dyn Error>> {
    let mut file = File::create(path)?;
    writeln!(file, "{}", elapsed.as_nanos())?;
    file.sync_all()?;
    Ok(())
}

fn read_crash_elapsed(path: &Path) -> Result<Duration, Box<dyn Error>> {
    let nanoseconds = std::fs::read_to_string(path)?.trim().parse::<u64>()?;
    Ok(Duration::from_nanos(nanoseconds))
}

fn finalize_audio(
    mic: hound::WavWriter<BufWriter<File>>,
    system: hound::WavWriter<BufWriter<File>>,
) -> Result<(), hound::Error> {
    let mic_result = mic.finalize();
    let system_result = system.finalize();
    mic_result?;
    system_result
}

#[expect(
    clippy::too_many_arguments,
    reason = "the evidence drain updates paired stream writers and measurements atomically"
)]
fn drain_audio(
    audio_rx: &mut broadcast::Receiver<AudioFrame>,
    mic: &mut hound::WavWriter<BufWriter<File>>,
    system: &mut hound::WavWriter<BufWriter<File>>,
    mic_drift: &mut DriftTracker,
    system_drift: &mut DriftTracker,
    mic_rate: &mut RateTracker,
    system_rate: &mut RateTracker,
    lagged_packets: &mut u64,
) -> Result<(), hound::Error> {
    loop {
        let frame = match audio_rx.try_recv() {
            Ok(frame) => frame,
            // Lagged means the harness dropped audio it never wrote. Silently
            // breaking here would understate the delivered rate and look like drift.
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                *lagged_packets = lagged_packets.saturating_add(skipped);
                continue;
            }
            Err(_) => break,
        };
        let writer = match frame.source {
            Source::Mic => {
                mic_drift.observe(frame.stream_offset);
                mic_rate.observe(frame.samples.len());
                &mut *mic
            }
            Source::System => {
                system_drift.observe(frame.stream_offset);
                system_rate.observe(frame.samples.len());
                &mut *system
            }
        };
        for sample in frame.samples.iter().copied() {
            writer.write_sample(sample)?;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigint_handler_requests_graceful_shutdown() {
        INTERRUPTED.store(false, Ordering::Release);
        handle_sigint(libc::SIGINT);
        assert!(INTERRUPTED.load(Ordering::Acquire));
    }

    #[test]
    fn interrupted_shutdown_finalizes_both_wav_headers() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mic_path = directory.path().join("mic.wav");
        let system_path = directory.path().join("system.wav");
        let mut mic = wav_writer(&mic_path)?;
        let mut system = wav_writer(&system_path)?;
        let (audio_tx, mut audio_rx) = broadcast::channel(2);
        for (source, sample) in [(Source::Mic, 0.25_f32), (Source::System, -0.25_f32)] {
            audio_tx.send(AudioFrame {
                source,
                samples: Arc::from([sample]),
                sample_rate: OUTPUT_RATE,
                seq: 0,
                capture_ts: Instant::now(),
                stream_offset: Duration::ZERO,
            })?;
        }
        let mut mic_drift = DriftTracker::default();
        let mut system_drift = DriftTracker::default();
        let mut mic_rate = RateTracker::default();
        let mut system_rate = RateTracker::default();
        let mut lagged_packets = 0;
        drain_audio(
            &mut audio_rx,
            &mut mic,
            &mut system,
            &mut mic_drift,
            &mut system_drift,
            &mut mic_rate,
            &mut system_rate,
            &mut lagged_packets,
        )?;

        finalize_audio(mic, system)?;

        assert_eq!(hound::WavReader::open(mic_path)?.len(), 1);
        assert_eq!(hound::WavReader::open(system_path)?.len(), 1);
        assert_eq!(lagged_packets, 0);
        Ok(())
    }
}
