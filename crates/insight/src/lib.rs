//! Offline reasoning over persisted timelines.

#![deny(warnings)]

pub mod clustering;
pub mod summarizer;

pub use clustering::{
    ClusterError, ClusterReport, Clusterer, DerivedView, OpenThread, OpenThreadKind, TopicLink,
    TopicRegion,
};

pub use summarizer::{
    Attendee, Claim, Commitment, ContextMode, Cost, Objection, Pricing, Recap, Summarizer,
    SummaryError, SummaryReport, TalkTime,
};
