//! CSS Positioned Layout Level 4 §4.1: sticky positioning.
//!
//! Layout never learns where the page is scrolled to. The fragment tree is
//! built once in document space and the shell translates it when it paints
//! (`FragmentTree::viewport_to_document_point`), so a displacement that depends
//! on the scroll offset cannot be baked into a fragment without re-laying out
//! the whole document for every scroll tick. Sticky positioning is exactly that
//! kind of displacement, so the split is: layout resolves everything that does
//! not move with the scroll - the insets and the two rectangles the
//! specification constrains the box against - and the displacement itself is a
//! pure function of that data and the scroll offset, which the consumer (the
//! only place the offset exists) applies when it paints.
//!
//! The box itself is never moved in the fragment tree, so a sticky box keeps
//! its normal layout position, its paint order and its stacking behaviour
//! (§4.1: sticky positioning does not create a stacking context).

use crate::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};

/// The `top`/`right`/`bottom`/`left` insets of a sticky box (§9.4.3).
///
/// Each inset constrains one edge, so `None` - an `auto` inset - is not the
/// same as zero: an axis with no inset on it is not constrained at all, and an
/// axis with one inset is constrained on that side only. `top: 0` therefore
/// never pulls a box up towards the scrollport and `bottom: 0` never pushes one
/// down, and `top: 0` alone leaves a box's horizontal position alone entirely.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StickyInsets {
    pub top: Option<f32>,
    pub right: Option<f32>,
    pub bottom: Option<f32>,
    pub left: Option<f32>,
}

impl StickyInsets {
    /// The view rectangle is the scrollport inset by the insets that are set.
    fn start(&self, horizontal: bool) -> Option<f32> {
        if horizontal { self.left } else { self.top }
    }

    fn end(&self, horizontal: bool) -> Option<f32> {
        if horizontal { self.right } else { self.bottom }
    }

    /// `auto` insets inset the view rectangle by nothing.
    fn amount(&self, horizontal: bool, start: bool) -> f32 {
        let value = if start {
            self.start(horizontal)
        } else {
            self.end(horizontal)
        };
        value.unwrap_or(0.0)
    }
}

/// Everything a sticky displacement depends on except the scroll offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StickyConstraint {
    /// The box's margin box in document space, before any displacement.
    pub margin_rect: PhysicalRect,
    /// The box's containing block in document space. §4.1 constrains the box to
    /// its containing block, which is what makes a sticky element inside a short
    /// parent stop sticking where the parent ends instead of travelling with the
    /// page.
    pub containing_block: PhysicalRect,
    /// The insets that form the sticky view rectangle out of the scrollport.
    pub insets: StickyInsets,
    /// The size of the scrollport that constrains the box. The page scrollport
    /// starts at the document origin, so its document rectangle is the scroll
    /// offset plus this size.
    pub scrollport: PhysicalSize,
}

/// §4.1: the sticky view rectangle is the scrollport inset by the box's own
/// insets, positioned in document space by where the scrollport is scrolled to.
#[must_use]
pub fn sticky_view_rect(
    constraint: &StickyConstraint,
    scroll_offset: PhysicalPoint,
) -> PhysicalRect {
    let left = constraint.insets.amount(true, true);
    let right = constraint.insets.amount(true, false);
    let top = constraint.insets.amount(false, true);
    let bottom = constraint.insets.amount(false, false);
    PhysicalRect::new(
        scroll_offset.x + left,
        scroll_offset.y + top,
        (constraint.scrollport.width - left - right).max(0.0),
        (constraint.scrollport.height - top - bottom).max(0.0),
    )
}

/// §4.1: how far the box is displaced from [`StickyConstraint::margin_rect`]
/// when the page is scrolled to `scroll_offset`, in document space. Add it to
/// the box's fragment rectangle to get the position to paint it at.
#[must_use]
pub fn sticky_offset(constraint: &StickyConstraint, scroll_offset: PhysicalPoint) -> PhysicalPoint {
    let view = sticky_view_rect(constraint, scroll_offset);
    let box_rect = constraint.margin_rect;
    PhysicalPoint {
        x: axis_offset(
            &constraint.insets,
            true,
            box_rect.origin.x,
            box_rect.right(),
            view.origin.x,
            view.right(),
            constraint.containing_block.origin.x,
            constraint.containing_block.right(),
        ),
        y: axis_offset(
            &constraint.insets,
            false,
            box_rect.origin.y,
            box_rect.bottom(),
            view.origin.y,
            view.bottom(),
            constraint.containing_block.origin.y,
            constraint.containing_block.bottom(),
        ),
    }
}

/// One axis of §4.1.
///
/// A box is only displaced while its containing block is at least partly inside
/// the sticky view rectangle. That is what keeps a sticky element in a parent
/// that has been scrolled past, or one that is further down the page than the
/// scrollport reaches, from being dragged into view instead of scrolling away
/// with its parent. It costs nothing at the edges of that range: the box is off
/// the scrollport both before the containing block enters it and after it
/// leaves, so the displacement never becomes visible in one step.
///
/// Each inset then constrains its own edge, and the containing block bounds how
/// far the box may travel on that axis. A box that cannot satisfy both edges of
/// its containing block is left where layout put it.
#[allow(clippy::too_many_arguments)]
fn axis_offset(
    insets: &StickyInsets,
    horizontal: bool,
    box_start: f32,
    box_end: f32,
    view_start: f32,
    view_end: f32,
    block_start: f32,
    block_end: f32,
) -> f32 {
    if block_end <= view_start || block_start >= view_end {
        return 0.0;
    }
    let inward = match (insets.start(horizontal), insets.end(horizontal)) {
        (Some(_), Some(_)) => {
            if box_start < view_start {
                view_start - box_start
            } else if box_end > view_end {
                view_end - box_end
            } else {
                0.0
            }
        }
        (Some(_), None) => (view_start - box_start).max(0.0),
        (None, Some(_)) => (view_end - box_end).min(0.0),
        (None, None) => 0.0,
    };
    let lowest = block_start - box_start;
    let highest = block_end - box_end;
    if lowest > highest {
        0.0
    } else {
        inward.clamp(lowest, highest)
    }
}
