//! Immutable output of formatting-context layout.

use crate::dom::{DomRevision, NodeId};

use super::geometry::{EdgeSizes, PhysicalRect, PhysicalSize};
use super::tree::FormattingNodeId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FragmentId(u32);

impl FragmentId {
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    pub(crate) fn from_index(index: usize) -> Self {
        Self(u32::try_from(index).expect("fragment arena exceeded u32 capacity"))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoxGeometry {
    pub margin: EdgeSizes,
    pub border: EdgeSizes,
    pub padding: EdgeSizes,
    pub content_rect: PhysicalRect,
}

impl BoxGeometry {
    #[must_use]
    pub fn padding_rect(&self) -> PhysicalRect {
        PhysicalRect::new(
            self.content_rect.origin.x - self.padding.left,
            self.content_rect.origin.y - self.padding.top,
            self.content_rect.size.width + self.padding.horizontal(),
            self.content_rect.size.height + self.padding.vertical(),
        )
    }

    #[must_use]
    pub fn border_rect(&self) -> PhysicalRect {
        let padding = self.padding_rect();
        PhysicalRect::new(
            padding.origin.x - self.border.left,
            padding.origin.y - self.border.top,
            padding.size.width + self.border.horizontal(),
            padding.size.height + self.border.vertical(),
        )
    }

    #[must_use]
    pub fn margin_rect(&self) -> PhysicalRect {
        let border = self.border_rect();
        PhysicalRect::new(
            border.origin.x - self.margin.left,
            border.origin.y - self.margin.top,
            border.size.width + self.margin.horizontal(),
            border.size.height + self.margin.vertical(),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextFragmentData {
    pub text: String,
    pub baseline: f32,
    pub font_size: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FragmentKind {
    Box(BoxGeometry),
    Text(TextFragmentData),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    pub id: FragmentId,
    pub formatting_node: FormattingNodeId,
    pub source: Option<NodeId>,
    pub rect: PhysicalRect,
    pub kind: FragmentKind,
    pub children: Vec<FragmentId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FragmentTree {
    pub dom_revision: DomRevision,
    /// CSS layout viewport. Its dimensions remain independent from document
    /// overflow so viewport units and fixed-position containing blocks do not
    /// change when a document becomes scrollable.
    pub viewport: PhysicalSize,
    /// Positive document-space extent reachable by the root viewport's
    /// scrollport. This is at least as large as `viewport`.
    pub scrollable_content_size: PhysicalSize,
    root: FragmentId,
    fragments: Vec<Fragment>,
}

impl FragmentTree {
    #[must_use]
    pub const fn root(&self) -> FragmentId {
        self.root
    }

    #[must_use]
    pub fn get(&self, id: FragmentId) -> Option<&Fragment> {
        usize::try_from(id.0)
            .ok()
            .and_then(|index| self.fragments.get(index))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Fragment> {
        self.fragments.iter()
    }

    /// Largest valid document-space origin for a viewport-sized scrollport.
    #[must_use]
    pub fn max_scroll_offset(&self) -> super::geometry::PhysicalPoint {
        super::geometry::PhysicalPoint {
            x: (self.scrollable_content_size.width - self.viewport.width).max(0.0),
            y: (self.scrollable_content_size.height - self.viewport.height).max(0.0),
        }
    }

    /// Clamp an externally supplied scroll offset to this fragment tree.
    #[must_use]
    pub fn clamp_scroll_offset(
        &self,
        offset: super::geometry::PhysicalPoint,
    ) -> super::geometry::PhysicalPoint {
        let maximum = self.max_scroll_offset();
        super::geometry::PhysicalPoint {
            x: finite_non_negative(offset.x).min(maximum.x),
            y: finite_non_negative(offset.y).min(maximum.y),
        }
    }

    /// Convert a pointer/selection coordinate from the painted viewport into
    /// the document coordinate space used by fragments and future hit tests.
    #[must_use]
    pub fn viewport_to_document_point(
        &self,
        point: super::geometry::PhysicalPoint,
        scroll_offset: super::geometry::PhysicalPoint,
    ) -> super::geometry::PhysicalPoint {
        let scroll_offset = self.clamp_scroll_offset(scroll_offset);
        super::geometry::PhysicalPoint {
            x: point.x + scroll_offset.x,
            y: point.y + scroll_offset.y,
        }
    }

    /// Convert a fragment/selection coordinate into the painted viewport.
    #[must_use]
    pub fn document_to_viewport_point(
        &self,
        point: super::geometry::PhysicalPoint,
        scroll_offset: super::geometry::PhysicalPoint,
    ) -> super::geometry::PhysicalPoint {
        let scroll_offset = self.clamp_scroll_offset(scroll_offset);
        super::geometry::PhysicalPoint {
            x: point.x - scroll_offset.x,
            y: point.y - scroll_offset.y,
        }
    }

    pub(crate) fn new(
        dom_revision: DomRevision,
        viewport: PhysicalSize,
        root: FragmentId,
        fragments: Vec<Fragment>,
    ) -> Self {
        let scrollable_content_size = scrollable_content_size(viewport, &fragments);
        Self {
            dom_revision,
            viewport,
            scrollable_content_size,
            root,
            fragments,
        }
    }
}

fn scrollable_content_size(viewport: PhysicalSize, fragments: &[Fragment]) -> PhysicalSize {
    let mut width = finite_non_negative(viewport.width);
    let mut height = finite_non_negative(viewport.height);
    for fragment in fragments {
        let rect = match &fragment.kind {
            FragmentKind::Box(geometry) => geometry.margin_rect(),
            FragmentKind::Text(_) => fragment.rect,
        };
        for right in [fragment.rect.right(), rect.right()] {
            if right.is_finite() {
                width = width.max(right.max(0.0));
            }
        }
        for bottom in [fragment.rect.bottom(), rect.bottom()] {
            if bottom.is_finite() {
                height = height.max(bottom.max(0.0));
            }
        }
    }
    PhysicalSize { width, height }
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}
