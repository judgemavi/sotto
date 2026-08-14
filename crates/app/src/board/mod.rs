//! Stable, transcript-first board projection over the shared timeline seam.

mod metrics;
mod navigation;
mod projection;
mod state;
mod thumbnails;
mod view;

pub use navigation::BoardNavigation;
pub use projection::{
    BoardEventKey, BoardItem, BoardItemId, BoardItemKind, BoardProjection, BoardRect,
    BoardViewport, SnapshotCard, UtteranceCard,
};
pub use state::BoardState;
pub use view::BoardCanvas;
