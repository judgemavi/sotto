//! macOS ScreenCaptureKit system-audio/frame capture plus a CPAL microphone.

use std::{
    ffi::{CStr, CString, c_char, c_float, c_int, c_uchar, c_void},
    path::{Path, PathBuf},
    slice,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_queue::ArrayQueue;
use sotto_core::{
    AudioFrame, CaptureBackend, CaptureError, CaptureTarget, PermissionStatus, Source, TargetKind,
};
use tokio::sync::{broadcast, oneshot};

const OUTPUT_RATE: u32 = 16_000;
const FRAME_POOL_SIZE: usize = 3;
const MAX_FRAME_BYTES: usize = 40 * 1024 * 1024;
const AUDIO_POOL_SIZE: usize = 32;
const MAX_AUDIO_PACKET_SAMPLES: usize = 16_384;

type AudioCallback = unsafe extern "C" fn(*mut c_void, *const c_float, usize, u64, u64);
type ErrorCallback = unsafe extern "C" fn(*mut c_void, c_int, *const c_char);
type FrameCallback =
    unsafe extern "C" fn(*mut c_void, *const c_uchar, usize, u32, u32, u32, u32, u64, u64);
type TargetCallback = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    *const c_char,
    *const c_char,
    *const c_char,
    c_int,
    bool,
);

#[repr(C)]
struct BridgeConfig {
    audio: AudioCallback,
    error: ErrorCallback,
    frame: FrameCallback,
    context: *mut c_void,
    recording_path: *const c_char,
}

unsafe extern "C" {
    fn sotto_capture_pick_target(callback: TargetCallback, context: *mut c_void);
    fn sotto_capture_release_target(target: *mut c_void);
    fn sotto_capture_start_with_target(
        config: *const BridgeConfig,
        target: *mut c_void,
    ) -> *mut c_void;
    fn sotto_capture_start_microphone_only(config: *const BridgeConfig) -> *mut c_void;
    fn sotto_capture_stop(handle: *mut c_void);
    fn sotto_capture_append_microphone(
        handle: *mut c_void,
        samples: *const c_float,
        count: usize,
        sample_rate: u32,
        channels: u32,
        stream_time_ns: u64,
    );
    fn sotto_recording_probe(
        path: *const c_char,
        duration_ns: *mut u64,
        byte_size: *mut u64,
        first_video_ns: *mut u64,
        seek_video_ns: *mut u64,
    ) -> bool;
    fn sotto_recording_committed_duration(
        path: *const c_char,
        duration_ns: *mut u64,
        byte_size: *mut u64,
    ) -> bool;
    #[cfg(test)]
    fn sotto_recording_append_pts_probe(input: *const i64, output: *mut i64, count: usize) -> bool;
    fn sotto_capture_permission_status() -> c_int;
    fn sotto_capture_request_permission() -> bool;
    fn sotto_capture_open_permission_settings() -> bool;
    fn sotto_capture_pump_main_loop(seconds: f64);
}

/// A picker-produced ScreenCaptureKit filter and its user-visible description.
///
/// The opaque native filter cannot be constructed by Rust. Owning this value is
/// therefore the capability required to start a scoped capture.
#[derive(Debug)]
pub struct PickedTarget {
    handle: *mut c_void,
    description: CaptureTarget,
}

// SAFETY: Swift retains an immutable SCContentFilter behind the opaque handle.
unsafe impl Send for PickedTarget {}

impl PickedTarget {
    #[must_use]
    pub const fn description(&self) -> &CaptureTarget {
        &self.description
    }

    /// Consumes the picker capability and creates the only macOS value that can
    /// implement `CaptureBackend`.
    #[must_use]
    pub fn into_capture(self) -> PickedMacCapture {
        PickedMacCapture {
            capture: MacCapture::new(),
            target: self,
        }
    }
}

impl Drop for PickedTarget {
    fn drop(&mut self) {
        // SAFETY: this is the sole Rust owner of the retained picker handle.
        unsafe { sotto_capture_release_target(self.handle) };
    }
}

struct TargetCallbackState(Option<oneshot::Sender<Option<PickedTarget>>>);

/// What presenting the system content picker actually produced.
///
/// A bare `Option<PickedTarget>` cannot say why nothing was picked, and "the user cancelled the
/// picker" is the only one of those reasons that should stay silent — permission that was never
/// decided, or was refused outright, must reach the person who clicked Start.
#[derive(Debug)]
pub enum PickOutcome {
    /// The user chose a target; capture may proceed with it.
    Picked(PickedTarget),
    /// The picker was presented — permission was already granted — and the user dismissed it
    /// without choosing anything. The only outcome that is correctly silent.
    Cancelled,
    /// Screen & System Audio Recording has never been decided for this process identity. Calling
    /// this just triggered the system prompt; it is now on screen, pending an answer. The grant
    /// only takes effect for a newly launched process, so approving it still requires relaunching
    /// Sotto.
    NotDetermined,
    /// Screen & System Audio Recording was refused. macOS asks only once — the picker was never
    /// presented, and asking again will not either. Re-grant it from Settings and relaunch.
    Denied,
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
    session_clock: SessionClock,
}

/// One Rust-owned monotonic epoch shared by every capture callback in a session.
/// Native device/SCK timestamps remain source metadata and never become timeline time.
#[derive(Clone, Copy)]
struct SessionClock {
    origin: Instant,
}

impl SessionClock {
    fn start() -> Self {
        Self {
            origin: Instant::now(),
        }
    }

    fn now(self) -> (Instant, Duration) {
        let captured_at = Instant::now();
        (
            captured_at,
            captured_at.saturating_duration_since(self.origin),
        )
    }
}

/// Observable native capture lifecycle used by consent indicators and pipeline wiring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
    /// The picker-selected window/application/display ceased to exist.
    TargetEnded,
    /// The user deliberately chose Stop Sharing in the system capture UI.
    UserStopped,
}

impl CaptureStatus {
    const fn code(self) -> u8 {
        match self {
            Self::Stopped => 0,
            Self::Starting => 1,
            Self::Running => 2,
            Self::Stopping => 3,
            Self::Failed => 4,
            Self::TargetEnded => 5,
            Self::UserStopped => 6,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Starting,
            2 => Self::Running,
            3 => Self::Stopping,
            4 => Self::Failed,
            5 => Self::TargetEnded,
            6 => Self::UserStopped,
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
    recording_path: Option<CString>,
    native_handle: Arc<AtomicPtr<c_void>>,
}

/// A macOS capture backend carrying proof of a system-picker selection.
///
/// This type has no public constructor other than `PickedTarget::into_capture`.
pub struct PickedMacCapture {
    capture: MacCapture,
    target: PickedTarget,
}

/// Media metadata read from a finalized or fragmented recording on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingProbe {
    pub path: PathBuf,
    pub duration: Duration,
    pub byte_size: u64,
    /// Presentation timestamp of the first frame successfully decoded by AVAssetReader.
    pub first_video_timestamp: Option<Duration>,
    /// First presentation timestamp returned after requesting a range near the media end.
    /// AVAssetReader may return the preceding sync sample, so this can predate the range start.
    pub seek_video_timestamp: Option<Duration>,
}

/// Metadata available from the committed prefix of a recording that is still growing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRecordingProbe {
    pub path: PathBuf,
    pub duration: Duration,
    pub byte_size: u64,
}

// SAFETY: the native handle is only passed back to the bridge; callback state is synchronized.
unsafe impl Send for MacCapture {}

impl MacCapture {
    /// Creates an idle backend with bounded error and raw-frame subscriptions.
    #[must_use]
    fn new() -> Self {
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
            recording_path: None,
            native_handle: Arc::new(AtomicPtr::new(std::ptr::null_mut())),
        }
    }

    /// Creates the strictly smaller capture mode that uses CPAL only: no picker,
    /// ScreenCaptureKit filter, application audio, or screen frames.
    #[must_use]
    pub fn microphone_only() -> Self {
        Self::new()
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

    /// Presents the system content picker, or reports why it could not be presented.
    ///
    /// The picker is presented on the main queue, so this is only usable from a
    /// process that already runs an AppKit event loop — the GPUI app does.
    /// Anything else must use [`MacCapture::pick_target_blocking`]; awaiting this
    /// on a runtime that owns the main thread deadlocks, because the thread that
    /// would deliver the choice is the thread that is waiting for it.
    pub async fn pick_target() -> PickOutcome {
        if let Some(outcome) = Self::gate_permission() {
            return outcome;
        }
        let (sender, receiver) = oneshot::channel();
        let state = Box::into_raw(Box::new(TargetCallbackState(Some(sender))));
        // SAFETY: the callback reclaims `state`; the bridge invokes it exactly once.
        unsafe { sotto_capture_pick_target(target_callback, state.cast()) };
        match receiver.await.ok().flatten() {
            Some(target) => PickOutcome::Picked(target),
            None => PickOutcome::Cancelled,
        }
    }

    /// Presents the picker and drives the main run loop until the user chooses or
    /// cancels, for processes with no AppKit event loop of their own.
    ///
    /// Must be called on the main thread. Blocks until the picker resolves; there
    /// is no timeout, because waiting on a person is not a stall.
    #[must_use]
    pub fn pick_target_blocking() -> PickOutcome {
        if let Some(outcome) = Self::gate_permission() {
            return outcome;
        }
        let (sender, mut receiver) = oneshot::channel();
        let state = Box::into_raw(Box::new(TargetCallbackState(Some(sender))));
        // SAFETY: the callback reclaims `state`; the bridge invokes it exactly once.
        unsafe { sotto_capture_pick_target(target_callback, state.cast()) };
        loop {
            // SAFETY: called on the main thread; the bridge takes no pointers.
            unsafe { sotto_capture_pump_main_loop(0.05) };
            match receiver.try_recv() {
                Ok(Some(target)) => return PickOutcome::Picked(target),
                Ok(None) | Err(oneshot::error::TryRecvError::Closed) => {
                    return PickOutcome::Cancelled;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
    }

    /// Checks Screen & System Audio Recording permission ahead of presenting the picker.
    ///
    /// Returns `None` when the picker should proceed (permission is granted). Returns
    /// `Some(outcome)` when it must not: not-determined permission triggers the one-time system
    /// prompt as a side effect and reports that it is now pending; a stated denial reports itself
    /// without touching the OS again, since macOS will not prompt a second time.
    fn gate_permission() -> Option<PickOutcome> {
        match Self::permission_status() {
            PermissionStatus::Authorized => None,
            PermissionStatus::NotDetermined => {
                let _ = Self::request_permission();
                Some(PickOutcome::NotDetermined)
            }
            PermissionStatus::Denied | PermissionStatus::Restricted => {
                // The bridge's persisted "already asked" flag can outlive the TCC record it
                // describes. Turning the permission off in Settings, `tccutil reset`, and — for
                // an ad-hoc signed build — every rebuild, each return TCC to not-determined
                // while the flag stays set, so a stale flag would report a denial macOS would
                // in fact still prompt for. Asking again is a silent no-op against a real
                // denial, so issue the request regardless rather than trusting the flag to
                // suppress it, and re-read afterwards in case this process was already granted.
                let _ = Self::request_permission();
                match Self::permission_status() {
                    PermissionStatus::Authorized => None,
                    _ => Some(PickOutcome::Denied),
                }
            }
        }
    }

    /// Returns the current Screen & System Audio Recording permission state.
    #[must_use]
    pub fn permission_status() -> PermissionStatus {
        // SAFETY: no arguments or retained pointers cross this C ABI call.
        match unsafe { sotto_capture_permission_status() } {
            1 => PermissionStatus::Authorized,
            3 => PermissionStatus::NotDetermined,
            // 2, or any code this build does not yet know: treat as a stated denial rather than
            // silently proceeding as if permission were settled.
            _ => PermissionStatus::Denied,
        }
    }

    /// Prompts for Screen & System Audio Recording access. macOS shows the system prompt at most
    /// once per app identity; calling this after a denial is a silent no-op.
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
        session_clock: SessionClock,
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
        let sequence = Arc::new(AtomicU64::new(0));
        let sequence_callback = Arc::clone(&sequence);
        let errors = self.error_tx.clone();
        let native_handle = Arc::clone(&self.native_handle);
        let error_callback = move |error| {
            let _ = errors.send(CaptureError::StreamFailed(format!(
                "microphone stream: {error}"
            )));
        };
        let config: cpal::StreamConfig = supported.into();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                config,
                move |data: &[f32], _info| {
                    let (capture_ts, stream_offset) = session_clock.now();
                    queue_audio(
                        &free_audio,
                        &ready_audio,
                        data,
                        AudioPacketMeta {
                            source: Source::Mic,
                            sample_rate,
                            channels,
                            sequence: sequence_callback.fetch_add(1, Ordering::Relaxed),
                            stream_offset,
                            capture_ts,
                        },
                    );
                    let handle = native_handle.load(Ordering::Acquire);
                    if !handle.is_null() {
                        let channel_count = u32::try_from(channels).unwrap_or(u32::MAX);
                        let stream_ns = u64::try_from(stream_offset.as_nanos()).unwrap_or(u64::MAX);
                        // SAFETY: the bridge copies callback-scoped samples synchronously. The
                        // native handle remains retained until the mic stream is stopped.
                        unsafe {
                            sotto_capture_append_microphone(
                                handle,
                                data.as_ptr(),
                                data.len(),
                                sample_rate,
                                channel_count,
                                stream_ns,
                            );
                        }
                    }
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

    fn start_capture(
        &mut self,
        target: Option<&PickedTarget>,
        sink: broadcast::Sender<AudioFrame>,
    ) -> Result<(), CaptureError> {
        if self.status() != CaptureStatus::Stopped {
            return Err(CaptureError::StreamFailed(
                "capture is already active".to_owned(),
            ));
        }
        let session_clock = SessionClock::start();
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
        if let Err(error) = self.start_mic(
            Arc::clone(&free_audio),
            Arc::clone(&ready_audio),
            session_clock,
        ) {
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
            session_clock,
        });
        let state_ptr = Box::into_raw(state);
        let config = BridgeConfig {
            audio: audio_callback,
            error: error_callback,
            frame: frame_callback,
            context: state_ptr.cast(),
            recording_path: self
                .recording_path
                .as_ref()
                .map_or(std::ptr::null(), |path| path.as_ptr()),
        };
        // SAFETY: config is read synchronously and state remains boxed until stop.
        let handle = match target {
            Some(target) => {
                // SAFETY: config is read synchronously and the picker retains target.filter.
                unsafe { sotto_capture_start_with_target(&raw const config, target.handle) }
            }
            None => {
                // SAFETY: config is read synchronously; this entrypoint creates no SCK objects.
                unsafe { sotto_capture_start_microphone_only(&raw const config) }
            }
        };
        if handle.is_null() {
            // SAFETY: the bridge rejected the config synchronously and cannot retain the context.
            unsafe { drop(Box::from_raw(state_ptr)) };
            self.mic_stream = None;
            self.reset_local_start();
            return Err(CaptureError::StreamFailed(
                "ScreenCaptureKit start rejected configuration".to_owned(),
            ));
        }
        self.frame_receiver = target.map(|_| FrameReceiver {
            free: free_frames,
            ready: ready_frames,
        });
        self.handle = handle;
        self.native_handle.store(handle, Ordering::Release);
        Ok(())
    }

    fn stop_inner(&mut self) {
        self.mic_stream = None;
        self.native_handle
            .store(std::ptr::null_mut(), Ordering::Release);
        if !self.handle.is_null() {
            self.status
                .store(CaptureStatus::Stopping.code(), Ordering::Release);
            let _ = self.status_tx.send(CaptureStatus::Stopping);
            // SAFETY: the handle came from sotto_capture_start_with_target and is consumed once.
            unsafe { sotto_capture_stop(self.handle) };
            self.handle = std::ptr::null_mut();
        }
        // The stopped callback owns context reclamation and terminates the audio worker.
        let _ = self.audio_worker.take();
    }

    /// Selects the managed audio-only MP4 path before microphone capture starts.
    pub fn record_to(&mut self, path: &Path) -> Result<(), CaptureError> {
        if self.status() != CaptureStatus::Stopped {
            return Err(CaptureError::StreamFailed(
                "recording path cannot change after capture starts".to_owned(),
            ));
        }
        let encoded = path.to_string_lossy();
        self.recording_path = Some(CString::new(encoded.as_bytes()).map_err(|_| {
            CaptureError::Unsupported("recording path contains a NUL byte".to_owned())
        })?);
        Ok(())
    }
}

impl PickedMacCapture {
    /// Selects the managed MP4 path before capture starts.
    pub fn record_to(&mut self, path: &Path) -> Result<(), CaptureError> {
        self.capture.record_to(path)
    }

    pub fn subscribe_errors(&self) -> broadcast::Receiver<CaptureError> {
        self.capture.subscribe_errors()
    }

    pub fn subscribe_status(&self) -> broadcast::Receiver<CaptureStatus> {
        self.capture.subscribe_status()
    }

    #[must_use]
    pub fn status(&self) -> CaptureStatus {
        self.capture.status()
    }

    pub fn take_frame_receiver(&mut self) -> Option<FrameReceiver> {
        self.capture.take_frame_receiver()
    }

    #[must_use]
    pub fn dropped_frame_count(&self) -> u64 {
        self.capture.dropped_frame_count()
    }
}

/// Reads the actual media duration and byte size from a playable recording.
pub fn probe_recording(path: &Path) -> Result<RecordingProbe, CaptureError> {
    let encoded = path.to_string_lossy();
    let path_c = CString::new(encoded.as_bytes())
        .map_err(|_| CaptureError::Unsupported("recording path contains a NUL byte".to_owned()))?;
    let mut duration_ns = 0_u64;
    let mut byte_size = 0_u64;
    let mut first_video_ns = 0_u64;
    let mut seek_video_ns = 0_u64;
    // SAFETY: the path is NUL-terminated and output pointers are valid for this call.
    let readable = unsafe {
        sotto_recording_probe(
            path_c.as_ptr(),
            &raw mut duration_ns,
            &raw mut byte_size,
            &raw mut first_video_ns,
            &raw mut seek_video_ns,
        )
    };
    if !readable {
        return Err(CaptureError::StreamFailed(format!(
            "recording is not playable: {}",
            path.display()
        )));
    }
    Ok(RecordingProbe {
        path: path.to_path_buf(),
        duration: Duration::from_nanos(duration_ns),
        byte_size,
        first_video_timestamp: (first_video_ns != u64::MAX)
            .then(|| Duration::from_nanos(first_video_ns)),
        seek_video_timestamp: (seek_video_ns != u64::MAX)
            .then(|| Duration::from_nanos(seek_video_ns)),
    })
}

/// Reads the duration and current size of a growing recording without requiring an end seek.
pub fn probe_committed_recording(path: &Path) -> Result<CommittedRecordingProbe, CaptureError> {
    let encoded = path.to_string_lossy();
    let path_c = CString::new(encoded.as_bytes())
        .map_err(|_| CaptureError::Unsupported("recording path contains a NUL byte".to_owned()))?;
    let mut duration_ns = 0_u64;
    let mut byte_size = 0_u64;
    // SAFETY: the path is NUL-terminated and output pointers are valid for this call.
    let readable = unsafe {
        sotto_recording_committed_duration(
            path_c.as_ptr(),
            &raw mut duration_ns,
            &raw mut byte_size,
        )
    };
    if !readable {
        return Err(CaptureError::StreamFailed(format!(
            "recording has no readable committed duration: {}",
            path.display()
        )));
    }
    Ok(CommittedRecordingProbe {
        path: path.to_path_buf(),
        duration: Duration::from_nanos(duration_ns),
        byte_size,
    })
}

impl CaptureBackend for PickedMacCapture {
    fn start(&mut self, sink: broadcast::Sender<AudioFrame>) -> Result<(), CaptureError> {
        self.capture.start_capture(Some(&self.target), sink)
    }

    fn stop(&mut self) {
        self.capture.stop_inner();
    }

    fn permission_status(&self) -> PermissionStatus {
        MacCapture::permission_status()
    }
}

impl CaptureBackend for MacCapture {
    fn start(&mut self, sink: broadcast::Sender<AudioFrame>) -> Result<(), CaptureError> {
        self.start_capture(None, sink)
    }

    fn stop(&mut self) {
        self.stop_inner();
    }

    fn permission_status(&self) -> PermissionStatus {
        // CPAL owns the microphone permission prompt/error. This mode deliberately never queries
        // ScreenCaptureKit's Screen & System Audio Recording permission.
        PermissionStatus::Authorized
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

unsafe extern "C" fn target_callback(
    context: *mut c_void,
    handle: *mut c_void,
    bundle_id: *const c_char,
    display_name: *const c_char,
    window_title: *const c_char,
    kind: c_int,
    audio_scoped: bool,
) {
    if context.is_null() {
        if !handle.is_null() {
            // SAFETY: without callback state there is no Rust owner for this retained handle.
            unsafe { sotto_capture_release_target(handle) };
        }
        return;
    }
    // SAFETY: bridge calls exactly once with the Box pointer supplied to pick_target.
    let mut state = unsafe { Box::from_raw(context.cast::<TargetCallbackState>()) };
    let picked = if handle.is_null() {
        None
    } else {
        let kind = match kind {
            1 => TargetKind::Application,
            2 => TargetKind::Window,
            3 => TargetKind::Display,
            _ => {
                // SAFETY: an unknown kind cannot be represented truthfully, so release it.
                unsafe { sotto_capture_release_target(handle) };
                if let Some(sender) = state.0.take() {
                    let _ = sender.send(None);
                }
                return;
            }
        };
        Some(PickedTarget {
            handle,
            description: CaptureTarget {
                bundle_id: copy_callback_string(bundle_id),
                display_name: copy_callback_string(display_name)
                    .unwrap_or_else(|| "Selected target".to_owned()),
                window_title: copy_callback_string(window_title),
                kind,
                audio_scoped,
            },
        })
    };
    if let Some(sender) = state.0.take() {
        let _ = sender.send(picked);
    }
}

fn copy_callback_string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: Swift supplies a callback-scoped, NUL-terminated UTF-8 string.
    Some(
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned(),
    )
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
    let mut mic_resampler = Resampler::default();
    let mut system_resampler = Resampler::default();
    while running.load(Ordering::Acquire) || !ready.is_empty() {
        if let Some(mut packet) = ready.pop() {
            let mono: Vec<f32> = packet
                .samples
                .chunks_exact(packet.channels)
                .map(|frame| frame.iter().sum::<f32>() / packet.channels as f32)
                .collect();
            let resampler = match packet.source {
                Source::Mic => &mut mic_resampler,
                Source::System => &mut system_resampler,
            };
            let output = resampler.process(&mono, packet.sample_rate, OUTPUT_RATE);
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

/// Stateful linear resampler that preserves fractional phase across packet boundaries.
///
/// The previous implementation computed `input.len() * output_rate / input_rate` per
/// packet with integer division and no carry, so every packet whose length was not an
/// exact multiple of the ratio silently dropped the remainder. That is a systematic
/// sample deficit, not rounding noise: it measured as roughly -3,956 ppm on the mic
/// stream against -110 ppm on system audio, purely because the mic delivers smaller
/// packets and therefore loses proportionally more. Two streams losing samples at
/// different rates drift apart, which breaks the cross-stream timestamp comparison that
/// speaker attribution and interruption detection both depend on.
///
/// Keeping the read position and the trailing sample between calls makes the packet
/// boundary invisible: no samples are lost and interpolation stays continuous.
#[derive(Default)]
struct Resampler {
    /// Input sample index the next output position sits on, relative to the next
    /// buffer's start. `-1` interpolates against `previous`.
    index: i64,
    /// Sub-sample phase in `[0, 1)`, carried between packets so none are lost.
    fraction: f64,
    /// Final sample of the previous packet, acting as index -1 of the current one.
    previous: Option<f32>,
}

impl Resampler {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "phase is bounded to [0,1) and packet lengths to i64; audio samples are f32 by definition"
    )]
    fn process(&mut self, input: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
        if input.is_empty() || input_rate == 0 || output_rate == 0 {
            return Vec::new();
        }
        let step = f64::from(input_rate) / f64::from(output_rate);
        let length = input.len();
        let mut output = Vec::new();

        // `index` is the input sample just before the next output position; -1 means the
        // position falls between the previous packet's tail and this packet's first
        // sample, which is exactly the boundary the old implementation discarded.
        while self.index < length as i64 {
            let lower = self.sample_at(input, self.index);
            let Some(upper) = self.upper_sample(input, self.index) else {
                break;
            };
            output.push(lower + (upper - lower) * self.fraction as f32);

            self.fraction += step;
            let advance = self.fraction.floor();
            self.fraction -= advance;
            self.index = self.index.saturating_add(advance as i64);
        }

        self.previous = input.last().copied();
        self.index -= length as i64;
        output
    }

    fn sample_at(&self, input: &[f32], index: i64) -> f32 {
        usize::try_from(index).map_or_else(
            |_| {
                self.previous
                    .unwrap_or_else(|| input.first().copied().unwrap_or(0.0))
            },
            |index| input.get(index).copied().unwrap_or(0.0),
        )
    }

    /// The sample after `index`, or `None` when it has not been delivered yet — in which
    /// case the remaining phase is carried into the next packet rather than dropped.
    fn upper_sample(&self, input: &[f32], index: i64) -> Option<f32> {
        let next = index.saturating_add(1);
        match usize::try_from(next) {
            Ok(next) => input.get(next).copied(),
            Err(_) => Some(
                self.previous
                    .unwrap_or_else(|| input.first().copied().unwrap_or(0.0)),
            ),
        }
    }
}

unsafe extern "C" fn audio_callback(
    context: *mut c_void,
    samples: *const c_float,
    count: usize,
    sequence: u64,
    _native_stream_ns: u64,
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
    let (capture_ts, stream_offset) = state.session_clock.now();
    queue_audio(
        &state.free_audio,
        &state.ready_audio,
        input,
        AudioPacketMeta {
            source: Source::System,
            // The bridge validates the actual CMAudioFormatDescription and emits -7
            // instead of invoking this metadata-free callback unless it is exactly 48 kHz.
            sample_rate: 48_000,
            channels: 1,
            sequence,
            stream_offset,
            capture_ts,
        },
    );
}

unsafe extern "C" fn error_callback(context: *mut c_void, code: c_int, detail: *const c_char) {
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
            state.running.store(false, Ordering::Release);
            state
                .status
                .store(CaptureStatus::Failed.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::Failed);
            let _ = state.error_sink.send(CaptureError::PermissionRevoked);
        }
        -4 => {
            state.running.store(false, Ordering::Release);
            state
                .status
                .store(CaptureStatus::Failed.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::Failed);
            let _ = state.error_sink.send(CaptureError::PermissionDenied {
                status: PermissionStatus::Denied,
            });
        }
        -5 => {
            state.running.store(false, Ordering::Release);
            state
                .status
                .store(CaptureStatus::TargetEnded.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::TargetEnded);
        }
        -6 => {
            state.running.store(false, Ordering::Release);
            state
                .status
                .store(CaptureStatus::UserStopped.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::UserStopped);
        }
        -7 => {
            state.running.store(false, Ordering::Release);
            state
                .status
                .store(CaptureStatus::Failed.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::Failed);
            let _ = state.error_sink.send(CaptureError::StreamFailed(
                "ScreenCaptureKit supplied an unsupported system-audio buffer".to_owned(),
            ));
        }
        -8 => {
            state.running.store(false, Ordering::Release);
            state
                .status
                .store(CaptureStatus::Failed.code(), Ordering::Release);
            let detail = if detail.is_null() {
                "the native media writer reported an unknown failure".to_owned()
            } else {
                // SAFETY: the bridge guarantees callback-scoped, NUL-terminated UTF-8 detail.
                unsafe { CStr::from_ptr(detail) }
                    .to_string_lossy()
                    .into_owned()
            };
            let _ = state.error_sink.send(CaptureError::StreamFailed(format!(
                "Local recording stopped because {detail}. Capture stopped; the committed recording prefix was kept."
            )));
            let _ = state.status_sink.send(CaptureStatus::Failed);
        }
        _ => {
            state.running.store(false, Ordering::Release);
            state
                .status
                .store(CaptureStatus::Failed.code(), Ordering::Release);
            let _ = state.status_sink.send(CaptureStatus::Failed);
            let detail = copy_callback_string(detail);
            let message = detail.map_or_else(
                || format!("ScreenCaptureKit error {code}"),
                |detail| format!("ScreenCaptureKit error {code}: {detail}"),
            );
            let _ = state.error_sink.send(CaptureError::StreamFailed(message));
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
    _native_stream_time_ns: u64,
    _native_host_time_ns: u64,
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
    let (_, session_time) = state.session_clock.now();
    let session_time_ns = u64::try_from(session_time.as_nanos()).unwrap_or(u64::MAX);
    frame.stream_time_ns = session_time_ns;
    frame.host_time_ns = session_time_ns;
    if state.ready_frames.push(frame).is_err() {
        state.dropped_frames.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_status_codes_round_trip() {
        for status in [
            CaptureStatus::Stopped,
            CaptureStatus::Starting,
            CaptureStatus::Running,
            CaptureStatus::Stopping,
            CaptureStatus::Failed,
            CaptureStatus::TargetEnded,
            CaptureStatus::UserStopped,
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
        let output = Resampler::default().process(&input, 48_000, 16_000);
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

    #[test]
    fn ragged_packets_do_not_lose_samples() {
        // 48k -> 16k over packet lengths that are not multiples of the 3:1 ratio. The
        // previous per-packet integer division dropped the remainder every time, which
        // showed up as a systematic ~-3,956 ppm deficit on the mic stream.
        let mut resampler = Resampler::default();
        let mut produced = 0_usize;
        let mut consumed = 0_usize;
        for length in [100_usize, 101, 103, 97, 160, 1, 2, 512] {
            produced += resampler.process(&vec![0.5; length], 48_000, 16_000).len();
            consumed += length;
        }
        let expected = consumed / 3;
        assert!(
            produced.abs_diff(expected) <= 1,
            "expected about {expected} output samples from {consumed} input, got {produced}"
        );
    }

    #[test]
    fn phase_is_continuous_across_packet_boundaries() {
        // One ramp split into ragged packets must resample the same as the whole ramp.
        let ramp: Vec<f32> = (0..600).map(|index| index as f32).collect();
        let whole = Resampler::default().process(&ramp, 48_000, 16_000);

        let mut split = Vec::new();
        let mut resampler = Resampler::default();
        let mut offset = 0_usize;
        for length in [7_usize, 130, 44, 219, 200] {
            let end = (offset + length).min(ramp.len());
            split.extend(resampler.process(&ramp[offset..end], 48_000, 16_000));
            offset = end;
        }

        assert_eq!(
            whole.len(),
            split.len(),
            "splitting the input must not change how many samples come out"
        );
        assert!(
            whole.iter().zip(&split).all(|(a, b)| (a - b).abs() < 0.001),
            "packet boundaries must not perturb interpolation"
        );
    }

    #[test]
    fn repeated_and_out_of_order_pts_are_nudged_before_writer_append() {
        let input = [0_i64, 0, -1, 1_000_000_000, 500_000_000];
        let mut writer_pts = [0_i64; 5];

        // SAFETY: both arrays remain valid for the complete synchronous native probe call.
        let accepted = unsafe {
            sotto_recording_append_pts_probe(input.as_ptr(), writer_pts.as_mut_ptr(), input.len())
        };

        assert!(
            accepted,
            "the native append timestamp gate should accept valid PTS values"
        );
        assert_eq!(writer_pts, [0, 1, 2, 1_000_000_000, 1_000_000_001]);
        assert!(
            writer_pts.windows(2).all(|pair| pair[0] < pair[1]),
            "AVAssetWriter must only receive strictly increasing per-input PTS values"
        );
    }

    #[test]
    fn native_uptime_timestamps_cannot_enter_session_timeline_time() {
        let (error_sink, _) = broadcast::channel(1);
        let (status_sink, _) = broadcast::channel(1);
        let free_audio = Arc::new(ArrayQueue::new(1));
        let ready_audio = Arc::new(ArrayQueue::new(1));
        let free_frames = Arc::new(ArrayQueue::new(1));
        let ready_frames = Arc::new(ArrayQueue::new(1));
        let _ = free_audio.push(AudioPacket {
            samples: Vec::with_capacity(1),
            source: Source::Mic,
            sample_rate: OUTPUT_RATE,
            channels: 1,
            sequence: 0,
            stream_offset: Duration::ZERO,
            capture_ts: Instant::now(),
        });
        let _ = free_frames.push(RawFrame {
            bytes: Vec::with_capacity(4),
            width: 0,
            height: 0,
            stride: 0,
            pixel_format: 0,
            stream_time_ns: 0,
            host_time_ns: 0,
        });
        let session_clock = SessionClock {
            origin: Instant::now() - Duration::from_secs(2),
        };
        let state = Box::into_raw(Box::new(CallbackState {
            error_sink,
            status_sink,
            status: Arc::new(AtomicU8::new(CaptureStatus::Running.code())),
            running: Arc::new(AtomicBool::new(true)),
            free_audio,
            ready_audio: Arc::clone(&ready_audio),
            free_frames,
            ready_frames: Arc::clone(&ready_frames),
            dropped_frames: Arc::new(AtomicU64::new(0)),
            session_clock,
        }));
        let sample = [0.25_f32];
        let frame_bytes = [0_u8; 4];
        let uptime_ns = 223_547_000_000_000_u64;

        // SAFETY: callback state and both input arrays remain live for these synchronous calls.
        unsafe {
            audio_callback(state.cast(), sample.as_ptr(), sample.len(), 7, uptime_ns);
            frame_callback(
                state.cast(),
                frame_bytes.as_ptr(),
                frame_bytes.len(),
                1,
                1,
                4,
                0,
                uptime_ns,
                uptime_ns,
            );
        }

        let audio = ready_audio
            .pop()
            .ok_or("system audio callback did not queue a packet");
        let frame = ready_frames
            .pop()
            .ok_or("screen callback did not queue a frame");
        assert!(audio.is_ok(), "system audio must reach the capture seam");
        assert!(frame.is_ok(), "screen frame must reach the capture seam");
        if let (Ok(audio), Ok(frame)) = (audio, frame) {
            let elapsed = session_clock.origin.elapsed();
            assert_eq!(audio.source, Source::System);
            assert!(audio.stream_offset <= elapsed);
            assert!(Duration::from_nanos(frame.stream_time_ns) <= elapsed);
            assert!(Duration::from_nanos(frame.host_time_ns) <= elapsed);
            assert_ne!(audio.stream_offset, Duration::from_nanos(uptime_ns));
            assert_ne!(frame.stream_time_ns, uptime_ns);
            assert_ne!(frame.host_time_ns, uptime_ns);
        }
        // SAFETY: no terminal callback reclaimed the state; this test remains its sole owner.
        unsafe { drop(Box::from_raw(state)) };
    }

    #[test]
    fn unreadable_system_audio_fails_closed_with_typed_error() {
        let (error_sink, mut errors) = broadcast::channel(1);
        let (status_sink, mut statuses) = broadcast::channel(1);
        let running = Arc::new(AtomicBool::new(true));
        let status = Arc::new(AtomicU8::new(CaptureStatus::Running.code()));
        let state = Box::new(CallbackState {
            error_sink,
            status_sink,
            status: Arc::clone(&status),
            running: Arc::clone(&running),
            free_audio: Arc::new(ArrayQueue::new(1)),
            ready_audio: Arc::new(ArrayQueue::new(1)),
            free_frames: Arc::new(ArrayQueue::new(1)),
            ready_frames: Arc::new(ArrayQueue::new(1)),
            dropped_frames: Arc::new(AtomicU64::new(0)),
            session_clock: SessionClock::start(),
        });
        let state = Box::into_raw(state);

        // SAFETY: the boxed callback state remains live through this synchronous callback.
        unsafe { error_callback(state.cast(), -7, std::ptr::null()) };

        assert!(!running.load(Ordering::Acquire));
        assert_eq!(
            CaptureStatus::from_code(status.load(Ordering::Acquire)),
            CaptureStatus::Failed
        );
        assert_eq!(statuses.try_recv(), Ok(CaptureStatus::Failed));
        assert!(matches!(
            errors.try_recv(),
            Ok(CaptureError::StreamFailed(reason)) if reason.contains("system-audio buffer")
        ));
        // SAFETY: error -7 does not reclaim callback state; this test remains its sole owner.
        unsafe { drop(Box::from_raw(state)) };
    }

    #[test]
    fn recording_failure_preserves_native_cause_before_failed_status()
    -> Result<(), Box<dyn std::error::Error>> {
        let (error_sink, mut errors) = broadcast::channel(1);
        let (status_sink, mut statuses) = broadcast::channel(1);
        let running = Arc::new(AtomicBool::new(true));
        let status = Arc::new(AtomicU8::new(CaptureStatus::Running.code()));
        let state = Box::into_raw(Box::new(CallbackState {
            error_sink,
            status_sink,
            status: Arc::clone(&status),
            running: Arc::clone(&running),
            free_audio: Arc::new(ArrayQueue::new(1)),
            ready_audio: Arc::new(ArrayQueue::new(1)),
            free_frames: Arc::new(ArrayQueue::new(1)),
            ready_frames: Arc::new(ArrayQueue::new(1)),
            dropped_frames: Arc::new(AtomicU64::new(0)),
            session_clock: SessionClock::start(),
        }));

        let detail = CString::new(
            "system audio: input.append returned false; NSOSStatusErrorDomain code=-16341",
        )?;
        // SAFETY: the boxed callback state and detail remain live through this synchronous call.
        unsafe { error_callback(state.cast(), -8, detail.as_ptr()) };

        assert!(
            !running.load(Ordering::Acquire),
            "recording writer failure must stop delivery"
        );
        assert_eq!(
            CaptureStatus::from_code(status.load(Ordering::Acquire)),
            CaptureStatus::Failed,
            "recording writer failure must be terminal"
        );
        assert!(matches!(
            errors.try_recv(),
            Ok(CaptureError::StreamFailed(reason))
                if reason.contains("NSOSStatusErrorDomain code=-16341")
                    && reason.contains("prefix was kept")
        ));
        assert_eq!(
            statuses.try_recv(),
            Ok(CaptureStatus::Failed),
            "status must follow the actionable error publication"
        );
        // SAFETY: error -8 does not reclaim callback state; this test remains its sole owner.
        unsafe { drop(Box::from_raw(state)) };
        Ok(())
    }
}
