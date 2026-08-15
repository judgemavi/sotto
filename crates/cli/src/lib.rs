//! Headless, file-backed pipeline harness used by the CLI and integration tests.

#![deny(warnings)]

pub mod file_capture;
pub mod pipeline;
pub mod reasoning;

pub use file_capture::{FileCapture, FileCaptureMode, FrameFile, TimestampedFrames};
pub use pipeline::{LatencyReport, PipelineOptions, PipelineRun, run_files, run_files_async};
