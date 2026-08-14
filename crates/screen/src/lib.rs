//! Low-rate screen sampling, change detection, bounded frame storage, and explicit inspection.
//!
//! Capture owns the ScreenCaptureKit session. This crate intentionally accepts owned BGRA
//! frames so capture callbacks can hand work to a background consumer without importing a
//! second capture implementation or permission surface.

#![deny(warnings)]

use std::{
    collections::VecDeque,
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};
use sotto_core::{CaptureTarget, EventPayload, FrameRef, ScreenSnapshot};
use thiserror::Error;

#[cfg(target_os = "macos")]
mod vision;
#[cfg(target_os = "macos")]
pub use vision::VisionOcr;

pub mod recording;
pub use recording::{
    DecodedRecordingFrame, PlatformFrameDecoder, RecordingBackedScreenInspector,
    RecordingFrameDecoder, RecordingFrameProvenance, RecordingFrameUnavailable,
    RecordingFrameUnavailableReason, extract_recording_frame,
};

pub mod inspection;
pub use inspection::{
    AuthorizedImageMediaType, AuthorizedImageProvenance, AuthorizedReasoningImage,
    ImageInspectionPolicy, InspectScreenRequest, MAX_AUTHORIZED_IMAGE_BYTES,
    RecordingScreenProvenance, RetainedScreenInspector, ScreenEvidence, ScreenInspection,
    ScreenInspectionSource, ScreenPrecision, ScreenProvenance, ScreenSelector,
    ScreenUnavailableReason,
};

const HASH_EDGE: usize = 16;

#[derive(Debug, Error)]
pub enum ScreenError {
    #[error("invalid BGRA frame: {0}")]
    InvalidFrame(String),
    #[error("frame cache I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("PNG encoding: {0}")]
    Png(#[from] png::EncodingError),
    #[error("PNG decoding: {0}")]
    PngDecode(String),
    #[error("OCR: {0}")]
    Ocr(String),
    #[error("recording frame decode: {0}")]
    RecordingDecode(String),
    #[error("recording frame extraction is unsupported on this platform")]
    UnsupportedPlatform,
}

/// An owned BGRA frame suitable for processing away from the capture callback.
#[derive(Clone, Debug)]
pub struct Frame {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub captured_at: Duration,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FrameMetadata {
    pub active_app: Option<String>,
    pub window_title: Option<String>,
}

/// Pluggable OCR boundary. Apple Vision implementations run this method off capture threads.
pub trait OcrEngine: Send + Sync {
    fn recognize(&self, frame: &Frame) -> Result<String, ScreenError>;
}

#[derive(Clone, Debug)]
pub struct SamplerConfig {
    /// Minimum interval between frames considered for hashing and retention.
    pub min_interval: Duration,
    /// Mean absolute luma-hash difference that denotes a meaningful visual change.
    /// The default of 12/255 ignores cursor movement but detects typical slide changes.
    pub change_threshold: u8,
    /// Hard per-session cache bound. The default is 128 MiB.
    pub max_cache_bytes: u64,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            min_interval: Duration::from_secs(5),
            change_threshold: 12,
            max_cache_bytes: 128 * 1024 * 1024,
        }
    }
}

struct PendingSnapshot {
    frame_ref: FrameRef,
    metadata: FrameMetadata,
    visible_from: Duration,
    hash: [u8; HASH_EDGE * HASH_EDGE],
}

struct CacheEntry {
    path: PathBuf,
    bytes: u64,
}

/// Stateful producer that delays a snapshot until its visibility interval is known.
pub struct ScreenSampler<O> {
    config: SamplerConfig,
    cache_dir: PathBuf,
    target: CaptureTarget,
    _ocr: O,
    pending: Option<PendingSnapshot>,
    last_considered: Option<Duration>,
    cache: VecDeque<CacheEntry>,
    cache_bytes: u64,
}

impl<O: OcrEngine> ScreenSampler<O> {
    pub fn new(
        config: SamplerConfig,
        cache_dir: impl Into<PathBuf>,
        target: CaptureTarget,
        ocr: O,
    ) -> Result<Self, ScreenError> {
        let cache_dir = cache_dir.into();
        fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            config,
            cache_dir,
            target,
            _ocr: ocr,
            pending: None,
            last_considered: None,
            cache: VecDeque::new(),
            cache_bytes: 0,
        })
    }

    /// Considers a frame and returns the previous snapshot when a meaningful change closes it.
    pub fn push(
        &mut self,
        frame: Frame,
        mut metadata: FrameMetadata,
    ) -> Result<Option<EventPayload>, ScreenError> {
        validate(&frame)?;
        if self
            .last_considered
            .is_some_and(|last| frame.captured_at.saturating_sub(last) < self.config.min_interval)
        {
            return Ok(None);
        }
        self.last_considered = Some(frame.captured_at);
        let hash = luma_hash(&frame);
        if self.pending.as_ref().is_some_and(|pending| {
            hash_difference(&pending.hash, &hash) < self.config.change_threshold
        }) {
            return Ok(None);
        }

        // Capture scope is structural; recording it on every snapshot makes that scope
        // inspectable without inferring it from whichever application is frontmost.
        if metadata.active_app.is_none() {
            metadata.active_app = Some(self.target.display_name.clone());
        }
        if metadata.window_title.is_none() {
            metadata.window_title = self.target.window_title.clone();
        }
        let frame_ref = self.store_frame(&frame)?;
        let completed = self
            .pending
            .take()
            .map(|pending| payload(pending, Some(frame.captured_at)));
        self.pending = Some(PendingSnapshot {
            frame_ref,
            metadata,
            visible_from: frame.captured_at,
            hash,
        });
        Ok(completed)
    }

    /// Flushes the final visible interval when a session ends.
    pub fn finish(&mut self, visible_to: Duration) -> Option<EventPayload> {
        self.pending
            .take()
            .map(|pending| payload(pending, Some(visible_to)))
    }

    #[must_use]
    pub const fn cached_bytes(&self) -> u64 {
        self.cache_bytes
    }

    /// Deletes all sampled frames owned by this sampler, for session deletion.
    pub fn drop_session_frames(&mut self) -> Result<(), ScreenError> {
        while let Some(entry) = self.cache.pop_front() {
            remove_if_present(&entry.path)?;
        }
        self.cache_bytes = 0;
        Ok(())
    }

    fn store_frame(&mut self, frame: &Frame) -> Result<FrameRef, ScreenError> {
        let rgba = rgba_pixels(frame);
        let digest = Sha256::digest(&rgba);
        let name = format!("{digest:x}.png");
        let path = self.cache_dir.join(&name);
        if !path.exists() {
            let file = fs::File::create(&path)?;
            let mut encoder = png::Encoder::new(BufWriter::new(file), frame.width, frame.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(&rgba)?;
            writer.finish()?;
            let bytes = fs::metadata(&path)?.len();
            self.cache.push_back(CacheEntry {
                path: path.clone(),
                bytes,
            });
            self.cache_bytes = self.cache_bytes.saturating_add(bytes);
            self.prune()?;
        }
        Ok(FrameRef::new(path.to_string_lossy()))
    }

    fn prune(&mut self) -> Result<(), ScreenError> {
        while self.cache_bytes > self.config.max_cache_bytes {
            let Some(entry) = self.cache.pop_front() else {
                break;
            };
            remove_if_present(&entry.path)?;
            self.cache_bytes = self.cache_bytes.saturating_sub(entry.bytes);
        }
        Ok(())
    }
}

fn payload(pending: PendingSnapshot, visible_to: Option<Duration>) -> EventPayload {
    EventPayload::ScreenSnapshot(ScreenSnapshot {
        frame_ref: pending.frame_ref,
        // Kept empty for schema compatibility. OCR is derived only through `inspect_screen`.
        ocr_text: String::new(),
        active_app: pending.metadata.active_app,
        window_title: pending.metadata.window_title,
        visible_from: pending.visible_from,
        visible_to,
    })
}

fn validate(frame: &Frame) -> Result<(), ScreenError> {
    let row_bytes = frame
        .width
        .checked_mul(4)
        .ok_or_else(|| ScreenError::InvalidFrame("width overflow".to_owned()))?;
    if frame.width == 0 || frame.height == 0 || frame.stride < row_bytes {
        return Err(ScreenError::InvalidFrame(
            "zero dimensions or stride shorter than a BGRA row".to_owned(),
        ));
    }
    let required = u64::from(frame.stride) * u64::from(frame.height);
    if u64::try_from(frame.bgra.len()).unwrap_or(u64::MAX) < required {
        return Err(ScreenError::InvalidFrame(
            "buffer is shorter than stride × height".to_owned(),
        ));
    }
    Ok(())
}

fn luma_hash(frame: &Frame) -> [u8; HASH_EDGE * HASH_EDGE] {
    let mut hash = [0; HASH_EDGE * HASH_EDGE];
    let width = frame.width as usize;
    let height = frame.height as usize;
    let stride = frame.stride as usize;
    for y in 0..HASH_EDGE {
        let source_y = (y * height / HASH_EDGE).min(height.saturating_sub(1));
        for x in 0..HASH_EDGE {
            let source_x = (x * width / HASH_EDGE).min(width.saturating_sub(1));
            let offset = source_y * stride + source_x * 4;
            let blue = u16::from(frame.bgra[offset]);
            let green = u16::from(frame.bgra[offset + 1]);
            let red = u16::from(frame.bgra[offset + 2]);
            hash[y * HASH_EDGE + x] = ((red * 77 + green * 150 + blue * 29) >> 8) as u8;
        }
    }
    hash
}

fn hash_difference(left: &[u8], right: &[u8]) -> u8 {
    let total: u32 = left
        .iter()
        .zip(right)
        .map(|(a, b)| u32::from(a.abs_diff(*b)))
        .sum();
    u8::try_from(total / u32::try_from(left.len()).unwrap_or(1)).unwrap_or(u8::MAX)
}

fn rgba_pixels(frame: &Frame) -> Vec<u8> {
    let row_bytes = frame.width as usize * 4;
    let mut rgba = Vec::with_capacity(row_bytes * frame.height as usize);
    for row in frame
        .bgra
        .chunks_exact(frame.stride as usize)
        .take(frame.height as usize)
    {
        for pixel in row[..row_bytes].chunks_exact(4) {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    rgba
}

fn remove_if_present(path: &Path) -> Result<(), std::io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
