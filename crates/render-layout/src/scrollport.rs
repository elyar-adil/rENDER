//! CSS Overflow 3 §3.1: what a box does with the content that exceeds it.
//!
//! `overflow: hidden` and `overflow: auto` look alike to layout and are not the
//! same thing. Both clip: content outside the padding box is not painted. Only
//! `auto` and `scroll` also make the box a *scrollport* - a box the consumer can
//! scroll to reach the clipped content - and they differ only in whether the
//! scrollbar is always present. A clip has no scroll range at all, so the two
//! are kept apart in the type rather than in a flag nobody checks.
//!
//! The scroll offset is deliberately absent. It belongs to whoever owns input
//! and paint scheduling (the shell), the same split sticky positioning makes:
//! layout produces the rectangle and the range, and the consumer supplies the
//! offset and translates the content by it.

use crate::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};

/// What a box does with the content that exceeds its padding box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipMode {
    /// The content is painted only inside the padding box, and there is no
    /// scrollport: nothing can reach the clipped content.
    Clip,
    /// The content is painted only inside the padding box, and the consumer can
    /// scroll it within [`ScrollportGeometry::scrollable`].
    Scrollport,
}

/// The geometry of a box that clips its overflow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollportGeometry {
    pub mode: ClipMode,
    /// The padding box in document space. Content is painted only inside it and
    /// the scroll offset moves the content within it.
    pub clip: PhysicalRect,
    /// The content extent reachable by scrolling, measured from the clip's
    /// origin. Never smaller than the clip itself, so a box with nothing to
    /// scroll has a zero range.
    ///
    /// Only the end direction is reachable: content above or to the left of the
    /// padding box is clipped away and cannot be scrolled to, which is why this
    /// is an extent from the origin rather than a rectangle.
    pub scrollable: PhysicalSize,
}

impl ScrollportGeometry {
    /// The largest offset this box accepts, in document space. A clip accepts
    /// none: it reports what its content reaches, but nothing can be scrolled to
    /// reach it.
    #[must_use]
    pub fn max_scroll_offset(&self) -> PhysicalPoint {
        if self.mode == ClipMode::Clip {
            return PhysicalPoint::default();
        }
        PhysicalPoint {
            x: (self.scrollable.width - self.clip.size.width).max(0.0),
            y: (self.scrollable.height - self.clip.size.height).max(0.0),
        }
    }

    /// Clamp a scroll offset to this box's range. A clip has no range, so it
    /// only ever answers the origin.
    #[must_use]
    pub fn clamp_scroll_offset(&self, offset: PhysicalPoint) -> PhysicalPoint {
        let maximum = self.max_scroll_offset();
        PhysicalPoint {
            x: finite_non_negative(offset.x).min(maximum.x),
            y: finite_non_negative(offset.y).min(maximum.y),
        }
    }
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}
