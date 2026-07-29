//! macOS ScreenCaptureKit system-audio/frame capture plus a CPAL microphone.

use std::{
    ffi::{c_float, c_int, c_uchar, c_void},
    slice,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_queue::ArrayQueue;
use sotto_core::{AudioFrame, CaptureBackend, CaptureError, PermissionStatus, Source};
use tokio::sync::broadcast;

const OUTPUT_RATE: u32 = 16_000;
const FRAME_POOL_SIZE: usize = 3;
const MAX_FRAME_BYTES: usize = 40 * 1024 * 1024;
const AUDIO_POOL_SIZE: usize = 32;
const MAX_AUDIO_PACKET_SAMPLES: usize = 16_384;

type AudioCallback = unsafe extern "C" fn(*mut c_void, *const c_float, usize, u64, u64);
type ErrorCallback = unsafe extern "C" fn(*mut c_void, c_int);
type FrameCallback =
    unsafe extern "C" fn(*mut c_void, *const c_uchar, usize, u32, u32, u32, u32, u64, u64);

#[repr(C)]
struct BridgeConfig {
    audio: AudioCallback,
    error: ErrorCallback,
    frame: FrameCallback,
    context: *mut c_void,
}

unsafe extern "C" {
    fn sotto_capture_start(config: *const BridgeConfig) -> *mut c_void;
    fn sotto_capture_stop(handle: *mut c_void);
    fn sotto_capture_permission_status() -> c_int;
    fn sotto_capture_request_permission() -> bool;
    fn sotto_capture_open_permission_settings() -> bool;
}

/// An owned BGRA frame copied out of the callback-scoped CoreVideo buffer.
#[derive(Debug)]
pub struct RawFrame {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: u32,
    pub stream_time_ns: u64,
    pub host_time_ns: u64,
}

/// Single-consumer end of the bounded raw-frame handoff.
pub struct FrameReceiver {
    free: Arc<ArrayQueue<RawFrame>>,
    ready: Arc<ArrayQueue<RawFrame>>,
}

impl FrameReceiver {
    pub fn try_recv(&self) -> Option<RawFrame> {
        self.ready.pop()
    }

    pub fn recycle(&self, mut frame: RawFrame) {
        frame.bytes.clear();
        let _ = self.free.push(frame);
    }
}

struct CallbackState {
    error_sink: broadcast::Sender<CaptureError>,
    status_sink: broadcast::Sender<CaptureStatus>,
    status: Arc<AtomicU8>,
    running: Arc<AtomicBool>,
    free_audio: Arc<ArrayQueue<AudioPacket>>,
    ready_audio: Arc<ArrayQueue<AudioPacket>>,
    free_frames: Arc<ArrayQueue<RawFrame>>,
    ready_frames: Arc<ArrayQueue<RawFrame>>,
    dropped_frames: Arc<AtomicU64>,
}

/// Observable native capture lifecycle used by consent indicators and pipeline wiring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl CaptureStatus {
    const fn code(self) -> u8 {
        match self {
            Self::Stopped => 0,
            Self::Starting => 1,
            Self::Running => 2,
            Self::Stopping => 3,
            Self::Failed => 4,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Starting,
            2 => Self::Running,
            3 => Self::Stopping,
            4 => Self::Failed,
            _ => Self::Stopped,
        }
    }
}

struct AudioPacket {
    samples: Vec<f32>,
    source: Source,
    sample_rate: u32,
    channels: usize,
    sequence: u64,
    stream_offset: Duration,
    capture_ts: Instant,
}

struct AudioPacketMeta {
    source: Source,
    sample_rate: u32,
    channels: usize,
    sequence: u64,
    stream_offset: Duration,
    capture_ts: Instant,
}

/// ScreenCaptureKit/CPAL capture backend for macOS.
pub struct MacCapture {
    handle: *mut c_void,
    mic_stream: Option<cpal::Stream>,
    error_tx: broadcast::Sender<CaptureError>,
    status_tx: broadcast::Sender<CaptureStatus>,
    status: Arc<AtomicU8>,
    frame_receiver: Option<FrameReceiver>,
    dropped_frames: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    audio_worker: Option<JoinHandle<()>>,
}

// SAFETY: the native handle is only passed back to the bridge; callback state is synchronized.
unsafe impl Send for MacCapture {}

impl MacCapture {
    /// Creates an idle backend with bounded error and raw-frame subscriptions.
    #[must_use]
    pub fn new() -> Self {
        let (error_tx, _) = broadcast::channel(16);
        let (status_tx, _) = broadcast::channel(16);
        Self {
            handle: std::ptr::null_mut(),
            mic_stream: None,
            error_tx,
            status_tx,
            status: Arc::new(AtomicU8::new(CaptureStatus::Stopped.code())),
            frame_receiver: None,
            dropped_frames: Arc::new(AtomicU64::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            audio_worker: None,
        }
    }

    pub fn subscribe_errors(&self) -> broadcast::Receiver<CaptureError> {
        self.error_tx.subscribe()
    }

    /// Subscribes to transitions; `Running` is emitted only after `SCStream.startCapture` succeeds.
    pub fn subscribe_status(&self) -> broadcast::Receiver<CaptureStatus> {
        self.status_tx.subscribe()
    }

    #[must_use]
    pub fn status(&self) -> CaptureStatus {
        CaptureStatus::from_code(self.status.load(Ordering::Acquire))
    }

    pub fn take_frame_receiver(&mut self) -> Option<FrameReceiver> {
        self.frame_receiver.take()
    }

    #[must_use]
    pub fn dropped_frame_count(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// Prompts for Screen & System Audio Recording access.
    #[must_use]
    pub fn request_permission() -> bool {
        // SAFETY: no arguments or retained pointers cross this C ABI call.
        unsafe { sotto_capture_request_permission() }
    }

    /// Opens the Screen & System Audio Recording pane for re-grant after denial/revocation.
    #[must_use]
    pub fn open_permission_settings() -> bool {
        // SAFETY: no arguments or retained pointers cross this C ABI call.
        unsafe { sotto_capture_open_permission_settings() }
    }

    fn reset_local_start(&self) {
        self.running.store(false, Ordering::Release);
        self.status
            .store(CaptureStatus::Stopped.code(), Ordering::Release);
        let _ = self.status_tx.send(CaptureStatus::Stopped);
    }

    fn start_mic(
        &mut self,
        free_audio: Arc<ArrayQueue<AudioPacket>>,
        ready_audio: Arc<ArrayQueue<AudioPacket>>,
    ) -> Result<(), CaptureError> {
        let host = cpal::default_host();
        let device =
            host.default_input_device()
                .ok_or_else(|| CaptureError::DeviceUnavailable {
                    device: "default microphone".to_owned(),
                })?;
        let supported = device.default_input_config().map_err(|error| {
            CaptureError::StreamFailed(format!("microphone configuration: {error}"))
        })?;
        let sample_rate = supported.sample_rate();
        let channels = usize::from(supported.channels());
        let mut mic_origin = None;
        let sequence = Arc::new(AtomicU64::new(0));
        let sequence_callback = Arc::clone(&sequence);
        let errors = self.error_tx.clone();
        let error_callback = move |error| {
            let _ = errors.send(CaptureError::StreamFailed(format!(
                "microphone stream: {error}"
            )));
        };
        let config: cpal::StreamConfig = supported.into();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                config,
                move |data: &[f32], info| {
                    let captured = info.timestamp().capture;
                    let origin = *mic_origin.get_or_insert(captured);
                    queue_audio(
                        &free_audio,
                        &ready_audio,
                        data,
                        AudioPacketMeta {
                            source: Source::Mic,
                            sample_rate,
                            channels,
                            sequence: sequence_callback.fetch_add(1, Ordering::Relaxed),
                            stream_offset: captured.duration_since(origin),
                            capture_ts: Instant::now(),
                        },
                    )
                },
                error_callback,
                None,
            ),
            format => {
                return Err(CaptureError::Unsupported(format!(
                    "microphone sample format {format:?}"
                )));
            }
        }
        .map_err(|error| CaptureError::StreamFailed(format!("microphone build: {error}")))?;
        stream
            .play()
            .map_err(|error| CaptureError::StreamFailed(format!("microphone start: {error}")))?;
        self.mic_stream = Some(stream);
        Ok(())
    }
}

impl Default for MacCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for MacCapture {
    fn start(&mut self, sink: broadcast::Sender<AudioFrame>) -> Result<(), CaptureError> {
        if self.status() != CaptureStatus::Stopped {
            return Err(CaptureError::StreamFailed(
                "capture is already active".to_owned(),
            ));
        }
        let free_audio = Arc::new(ArrayQueue::new(AUDIO_POOL_SIZE));
        let ready_audio = Arc::new(ArrayQueue::new(AUDIO_POOL_SIZE));
        for _ in 0..AUDIO_POOL_SIZE {
            let _ = free_audio.push(AudioPacket {
                samples: Vec::with_capacity(MAX_AUDIO_PACKET_SAMPLES),
                source: Source::Mic,
                sample_rate: OUTPUT_RATE,
                channels: 1,
                sequence: 0,
                stream_offset: Duration::ZERO,
                capture_ts: Instant::now(),
            });
        }
        self.running.store(true, Ordering::Release);
        self.status
            .store(CaptureStatus::Starting.code(), Ordering::Release);
        let _ = self.status_tx.send(CaptureStatus::Starting);
        let worker_running = Arc::clone(&self.running);
        let worker_free = Arc::clone(&free_audio);
        let worker_ready = Arc::clone(&ready_audio);
        let worker = thread::Builder::new()
            .name("sotto-audio-delivery".to_owned())
            .spawn(move || audio_worker(worker_running, worker_free, worker_ready, sink))
            .map_err(|error| CaptureError::StreamFailed(format!("audio worker: {error}")));
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                self.reset_local_start();
                return Err(error);
            }
        };
        self.audio_worker = Some(worker);
        if let Err(error) = self.start_mic(Arc::clone(&free_audio), Arc::clone(&ready_audio)) {
            self.reset_local_start();
            let _ = self.audio_worker.take();
            return Err(error);
        }
        let free_frames = Arc::new(ArrayQueue::new(FRAME_POOL_SIZE));
        let ready_frames = Arc::new(ArrayQueue::new(FRAME_POOL_SIZE));
        for _ in 0..FRAME_POOL_SIZE {
            let _ = free_frames.push(RawFrame {
                bytes: Vec::with_capacity(MAX_FRAME_BYTES),
                width: 0,
                height: 0,
                stride: 0,
                pixel_format: 0,
                stream_time_ns: 0,
                host_time_ns: 0,
            });
        }
        let state = Box::new(CallbackState {
            error_sink: self.error_tx.clone(),
            status_sink: self.status_tx.clone(),
            status: Arc::clone(&self.status),
            running: Arc::clone(&self.running),
            free_audio,
            ready_audio,
            free_frames: Arc::clone(&free_frames),
            ready_frames: Arc::clone(&ready_frames),
            dropped_frames: Arc::clone(&self.dropped_frames),
        });
        let state_ptr = Box::into_raw(state);
        let config = BridgeConfig {
            audio: audio_callback,
            error: error_callback,
            frame: frame_callback,
            context: state_ptr.cast(),
        };
        // SAFETY: config is read synchronously and state remains boxed until stop.
        let handle = unsafe { sotto_capture_start(&raw const config) };
        if handle.is_null() {
            // SAFETY: the bridge rejected the config synchronously and cannot retain the context.
            unsafe { drop(Box::from_raw(state_ptr)) };
            self.mic_stream = None;
            self.reset_local_start();
            return Err(CaptureError::StreamFailed(
                "ScreenCaptureKit start rejected configuration".to_owned(),
            ));
        }
        self.frame_receiver = Some(FrameReceiver {
            free: free_frames,
            ready: ready_frames,
        });
        self.handle = handle;
        Ok(())
    }

    fn stop(&mut self) {
        if !self.handle.is_null() {
            self.status
                .store(CaptureStatus::Stopping.code(), Ordering::Release);
            let _ = self.status_tx.send(CaptureStatus::Stopping);
            // SAFETY: the handle came from sotto_capture_start and is consumed once.
            unsafe { sotto_capture_stop(self.handle) };
            self.handle = std::ptr::null_mut();
        }
        self.mic_stream = None;
        // The stopped callback owns context reclamation and terminates the audio worker.
        let _ = self.audio_worker.take();
    }

    fn permission_status(&self) -> PermissionStatus {
        // SAFETY: no arguments or retained pointers cross this C ABI call.
        match unsafe { sotto_capture_permission_status() } {
            1 => PermissionStatus::Authorized,
            _ => PermissionStatus::Denied,
        }
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn queue_audio(
    free: &ArrayQueue<AudioPacket>,
    ready: &ArrayQueue<AudioPacket>,
    input: &[f32],
    meta: AudioPacketMeta,
) {
    if meta.channels == 0 || meta.sample_rate == 0 || input.len() > MAX_AUDIO_PACKET_SAMPLES {
        return;
    }
    let Some(mut packet) = free.pop() else {
        return;
    };
    packet.samples.clear();
    packet.samples.extend_from_slice(input);
    packet.source = meta.source;
    packet.sample_rate = meta.sample_rate;
    packet.channels = meta.channels;
    packet.sequence = meta.sequence;
    packet.stream_offset = meta.stream_offset;
    packet.capture_ts = meta.capture_ts;
    if let Err(packet) = ready.push(packet) {
        let _ = free.push(packet);
    }
}

fn audio_worker(
    running: Arc<AtomicBool>,
    free: Arc<ArrayQueue<AudioPacket>>,
    ready: Arc<ArrayQueue<AudioPacket>>,
    sink: broadcast::Sender<AudioFrame>,
) {
    while running.load(Ordering::Acquire) || !ready.is_empty() {
        if let Some(mut packet) = ready.pop() {
            let mono: Vec<f32> = packet
                .samples
                .chunks_exact(packet.channels)
                .map(|frame| frame.iter().sum::<f32>() / packet.channels as f32)
                .collect();
            let output = resample_linear(&mono, packet.sample_rate, OUTPUT_RATE);
            let _ = sink.send(AudioFrame {
                source: packet.source,
                samples: output.into(),
                sample_rate: OUTPUT_RATE,
                seq: packet.sequence,
                capture_ts: packet.capture_ts,
                stream_offset: packet.stream_offset,
            });
            packet.samples.clear();
            let _ = free.push(packet);
        } else {
            thread::park_timeout(Duration::from_millis(1));
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded nonnegative interpolation positions are converted to sample indices and f32 fractions"
)]
fn resample_linear(input: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
    if input.is_empty() || input_rate == 0 {
        return Vec::new();
    }
    let output_len_u64 =
        (input.len() as u64).saturating_mul(u64::from(output_rate)) / u64::from(input_rate);
    let output_len = usize::try_from(output_len_u64).unwrap_or(usize::MAX);
    (0..output_len)
        .map(|index| {
            let position = index as f64 * f64::from(input_rate) / f64::from(output_rate);
            let lower = position.floor() as usize;
            let upper = lower.saturating_add(1).min(input.len() - 1);
            let fraction = (position - lower as f64) as f32;
            input[lower] + (input[upper] - input[lower]) * fraction
        })
        .collect()
}

unsafe extern "C" fn audio_callback(
    context: *mut c_void,
    samples: *const c_float,
    count: usize,
    sequence: u64,
    stream_ns: u64,
) {
    if context.is_null() || samples.is_null() {
        return;
    }
    // SAFETY: Swift guarantees both pointers remain valid for this callback and count floats exist.
    let (state, input) = unsafe {
        (
            &*(context.cast::<CallbackState>()),
            slice::from_raw_parts(samples, count),
        )
    };
    queue_audio(
        &state.free_audio,
        &state.ready_audio,
        input,
        AudioPacketMeta {
            source: Source::System,
            sample_rate: 48_000,
            channels: 1,
            sequence,
            stream_offset: Duration::from_nanos(stream_ns),
            capture_ts: Instant::now(),
        },
    );
}

unsafe extern "C" fn error_callback(context: *mut c_void, code: c_int) {
    if context.is_null() {
        return;
    }
    // SAFETY: state remains boxed from successful start through bridge stop.
    let state = unsafe { &*(context.cast::<CallbackState>()) };
    match code {
        1 => {
            state
                .status
                .store(CaptureStatus::Running.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::Running);
        }
        2 => {
            state.running.store(false, Ordering::Release);
            state
                .status
                .store(CaptureStatus::Stopped.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::Stopped);
            // SAFETY: Swift sends stopped exactly once, after SCStream has stopped all callbacks;
            // this callback owns the Box transferred by start and performs its sole reclamation.
            unsafe { drop(Box::from_raw(context.cast::<CallbackState>())) };
        }
        -3 => {
            state
                .status
                .store(CaptureStatus::Failed.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::Failed);
            let _ = state.error_sink.send(CaptureError::PermissionRevoked);
        }
        -4 => {
            state
                .status
                .store(CaptureStatus::Failed.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::Failed);
            let _ = state.error_sink.send(CaptureError::PermissionDenied {
                status: PermissionStatus::Denied,
            });
        }
        _ => {
            state
                .status
                .store(CaptureStatus::Failed.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::Failed);
            let _ = state.error_sink.send(CaptureError::StreamFailed(format!(
                "ScreenCaptureKit error {code}"
            )));
        }
    }
}

unsafe extern "C" fn frame_callback(
    context: *mut c_void,
    bytes: *const c_uchar,
    length: usize,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: u32,
    stream_time_ns: u64,
    host_time_ns: u64,
) {
    if context.is_null() || bytes.is_null() || length > MAX_FRAME_BYTES {
        return;
    }
    // SAFETY: pointers are callback-valid and Swift reports the accessible byte length.
    let (state, source) = unsafe {
        (
            &*(context.cast::<CallbackState>()),
            slice::from_raw_parts(bytes, length),
        )
    };
    let Some(mut frame) = state.free_frames.pop() else {
        state.dropped_frames.fetch_add(1, Ordering::Relaxed);
        return;
    };
    frame.bytes.clear();
    frame.bytes.extend_from_slice(source);
    frame.width = width;
    frame.height = height;
    frame.stride = stride;
    frame.pixel_format = pixel_format;
    frame.stream_time_ns = stream_time_ns;
    frame.host_time_ns = host_time_ns;
    if state.ready_frames.push(frame).is_err() {
        state.dropped_frames.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::{CaptureStatus, resample_linear};

    #[test]
    fn capture_status_codes_round_trip() {
        for status in [
            CaptureStatus::Stopped,
            CaptureStatus::Starting,
            CaptureStatus::Running,
            CaptureStatus::Stopping,
            CaptureStatus::Failed,
        ] {
            assert_eq!(
                CaptureStatus::from_code(status.code()),
                status,
                "every lifecycle state should survive atomic encoding"
            );
        }
    }

    #[test]
    fn resamples_48khz_to_16khz() {
        let input = vec![0.25; 480];
        let output = resample_linear(&input, 48_000, 16_000);
        assert_eq!(
            output.len(),
            160,
            "ten milliseconds should remain ten milliseconds"
        );
        assert!(
            output.iter().all(|sample| *sample == 0.25),
            "constant samples should remain constant"
        );
    }
}
