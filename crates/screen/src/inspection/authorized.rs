//! Opaque, path-free image evidence minted only by the retained-screen policy boundary.

use std::{fs::File, io::Read, path::Path, time::Duration};

use sotto_core::{EventId, ScreenSnapshot};

use super::{
    ImageInspectionPolicy, InspectScreenRequest, ScreenEvidence, ScreenProvenance, ScreenSelector,
    ScreenUnavailableReason,
};

/// Maximum retained image payload permitted to leave the screen cache (4 MiB).
pub const MAX_AUTHORIZED_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// Media type established from retained bytes rather than a caller declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizedImageMediaType {
    Png,
    Jpeg,
}

impl AuthorizedImageMediaType {
    #[must_use]
    pub const fn as_mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// Path-free provenance retained with an authorized image payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedImageProvenance {
    requested: ScreenSelector,
    captured_at: Duration,
    visible_from: Duration,
    visible_to: Option<Duration>,
    snapshot_event_id: EventId,
}

impl AuthorizedImageProvenance {
    #[must_use]
    pub const fn requested(&self) -> &ScreenSelector {
        &self.requested
    }

    #[must_use]
    pub const fn captured_at(&self) -> Duration {
        self.captured_at
    }

    #[must_use]
    pub const fn visible_from(&self) -> Duration {
        self.visible_from
    }

    #[must_use]
    pub const fn visible_to(&self) -> Option<Duration> {
        self.visible_to
    }

    #[must_use]
    pub const fn snapshot_event_id(&self) -> EventId {
        self.snapshot_event_id
    }
}

/// Screen-policy-authorized image bytes. There is no public constructor and no Serde surface.
#[derive(Debug, Eq, PartialEq)]
pub struct AuthorizedReasoningImage {
    bytes: Vec<u8>,
    media_type: AuthorizedImageMediaType,
    provenance: AuthorizedImageProvenance,
}

impl AuthorizedReasoningImage {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn media_type(&self) -> AuthorizedImageMediaType {
        self.media_type
    }

    #[must_use]
    pub const fn provenance(&self) -> &AuthorizedImageProvenance {
        &self.provenance
    }
}

pub(super) fn authorize_retained_image(
    request: &InspectScreenRequest,
    provenance: &ScreenProvenance,
    snapshot_event_id: EventId,
    snapshot: &ScreenSnapshot,
    cache_root: &Path,
    policy: ImageInspectionPolicy,
) -> Result<AuthorizedReasoningImage, ScreenUnavailableReason> {
    if policy != ImageInspectionPolicy::Allow {
        return Err(ScreenUnavailableReason::ImageOptInRequired);
    }
    if request.evidence != ScreenEvidence::Image || request.selector != provenance.requested {
        return Err(ScreenUnavailableReason::ImageAuthorizationMismatch);
    }
    if snapshot_event_id != provenance.snapshot_event_id
        || snapshot.frame_ref != provenance.frame_ref
        || snapshot.visible_from != provenance.visible_from
        || snapshot.visible_from != provenance.captured_at
        || snapshot.visible_to != provenance.visible_to
        || snapshot
            .visible_to
            .is_some_and(|visible_to| visible_to <= snapshot.visible_from)
    {
        return Err(ScreenUnavailableReason::ImageAuthorizationMismatch);
    }

    let root = cache_root
        .canonicalize()
        .map_err(|_| ScreenUnavailableReason::FrameOutsideCache)?;
    let requested_path = Path::new(snapshot.frame_ref.as_str());
    let canonical_path = requested_path
        .canonicalize()
        .map_err(|_| ScreenUnavailableReason::FramePruned)?;
    if !canonical_path.starts_with(&root) || !canonical_path.is_file() {
        return Err(ScreenUnavailableReason::FrameOutsideCache);
    }

    let mut bytes = Vec::new();
    File::open(&canonical_path)
        .and_then(|file| {
            file.take((MAX_AUTHORIZED_IMAGE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| ScreenUnavailableReason::ImageUnreadable)?;
    if bytes.len() > MAX_AUTHORIZED_IMAGE_BYTES {
        return Err(ScreenUnavailableReason::ImageTooLarge);
    }

    // Re-resolve after reading so a path swapped during authorization does not retain authority.
    let current_path = requested_path
        .canonicalize()
        .map_err(|_| ScreenUnavailableReason::FramePruned)?;
    if current_path != canonical_path || !current_path.starts_with(&root) {
        return Err(ScreenUnavailableReason::FrameSubstituted);
    }
    let media_type = sniff_media_type(&bytes).ok_or(ScreenUnavailableReason::InvalidImageMedia)?;

    Ok(AuthorizedReasoningImage {
        bytes,
        media_type,
        provenance: AuthorizedImageProvenance {
            requested: provenance.requested.clone(),
            captured_at: provenance.captured_at,
            visible_from: provenance.visible_from,
            visible_to: provenance.visible_to,
            snapshot_event_id: provenance.snapshot_event_id,
        },
    })
}

fn sniff_media_type(bytes: &[u8]) -> Option<AuthorizedImageMediaType> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some(AuthorizedImageMediaType::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) && bytes.ends_with(&[0xff, 0xd9]) {
        Some(AuthorizedImageMediaType::Jpeg)
    } else {
        None
    }
}
