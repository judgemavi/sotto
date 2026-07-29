//! Offline reasoning over persisted timelines.

#![deny(warnings)]

pub mod summarizer;

pub use summarizer::{
    Attendee, Claim, Commitment, ContextMode, Cost, Objection, Pricing, Recap, Summarizer,
    SummaryError, SummaryReport, TalkTime,
};
