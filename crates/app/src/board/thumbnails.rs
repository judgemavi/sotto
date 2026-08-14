//! Visible-only local thumbnail residency.

use std::{collections::HashSet, path::PathBuf};

/// Tracks visible local frame paths that the board currently permits GPUI to load.
///
/// This does not observe whether GPUI has decoded or cached an image successfully.
#[derive(Debug, Default)]
pub(super) struct ThumbnailResidency {
    resident: HashSet<PathBuf>,
}

impl ThumbnailResidency {
    pub(super) fn eligible_path_count(&self) -> usize {
        self.resident.len()
    }

    /// Replaces residency with the visible set and returns assets that must be evicted.
    pub(super) fn reconcile(&mut self, visible: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
        let next: HashSet<_> = visible.into_iter().collect();
        let mut evicted: Vec<_> = self.resident.difference(&next).cloned().collect();
        evicted.sort_unstable();
        self.resident = next;
        evicted
    }

    #[cfg(test)]
    fn contains(&self, path: &std::path::Path) -> bool {
        self.resident.contains(path)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::ThumbnailResidency;

    #[test]
    fn offscreen_frames_are_evicted_and_visible_frames_remain() {
        let first = PathBuf::from("first.png");
        let second = PathBuf::from("second.png");
        let third = PathBuf::from("third.png");
        let mut residency = ThumbnailResidency::default();

        assert!(
            residency
                .reconcile([first.clone(), second.clone()])
                .is_empty(),
            "the initial visible set should not evict anything"
        );
        let evicted = residency.reconcile([second, third]);

        assert_eq!(
            evicted,
            vec![first],
            "only the frame leaving view should be evicted"
        );
        assert!(
            residency.contains(Path::new("second.png")),
            "a still-visible frame must remain resident"
        );
        assert!(
            residency.contains(Path::new("third.png")),
            "a newly visible frame must become resident"
        );
        assert_eq!(
            residency.eligible_path_count(),
            2,
            "eligible path count must be available to the opt-in measurement harness"
        );
    }
}
