//! CSS formatting structures and immutable layout output.
//!
//! Observable DOM effects remain ordered on the page coordinator. Rendering
//! consumes a specific [`crate::dom::DomRevision`] and produces immutable trees
//! whose independent formatting contexts can be scheduled in parallel.

mod fragment;
mod geometry;
mod grid;
mod solver;
mod tree;

pub use fragment::{
    BoxGeometry, Fragment, FragmentId, FragmentKind, FragmentTree, TextFragmentData,
};
pub use geometry::{
    Direction, EdgeSizes, LogicalPoint, LogicalRect, LogicalSize, PhysicalPoint, PhysicalRect,
    PhysicalSize, WritingMode,
};
pub use solver::{
    LayoutDiagnostic, LayoutDiagnosticCode, LayoutLimits, LayoutOptions, LayoutOutput,
    SimpleTextMeasurer, TextMeasure, TextMeasurer, TextStyle, layout_formatting_tree,
    layout_formatting_tree_with_images,
};
pub use tree::{
    FormattingContextKind, FormattingDiagnostic, FormattingDiagnosticCode, FormattingLimits,
    FormattingNode, FormattingNodeId, FormattingNodeKind, FormattingTree, FormattingWorkUnit,
    build_formatting_tree,
};
