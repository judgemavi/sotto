//! The single Tokio-to-GPUI timeline ingress used by the meeting workspace.

mod seam;

#[cfg(test)]
pub(crate) use seam::test_ingress;
pub use seam::{TimelineIngress, TimelineState, attach_ingress};
