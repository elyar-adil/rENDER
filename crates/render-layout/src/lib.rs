//! Immutable layout output and the deterministic reference layout solver.
//!
//! Observable DOM effects remain ordered on the page coordinator. Rendering
//! consumes a specific [`render_dom::DomRevision`] and produces immutable trees
//! whose independent formatting contexts can be scheduled in parallel.

mod font;
mod fragment;
mod geometry;
mod grid;
mod linebreak;
mod scrollport;
mod solver;
mod sticky;
mod tree;

pub use font::{
    FamilyName, FontRequest, FontStyle, FontSynthesis, GenericFamily, NominalFace, caseless_match,
    computed_font_style, computed_font_synthesis, computed_font_weight, family_entries,
    family_entry, generic_family, is_wide_character, nominal_advance, nominal_face,
    unquote_family_name,
};
pub use fragment::{
    BoxGeometry, Fragment, FragmentId, FragmentKind, FragmentTree, StoredFontRequest,
    TextFragmentData,
};
pub use geometry::{
    Direction, EdgeSizes, LogicalPoint, LogicalRect, LogicalSize, PhysicalPoint, PhysicalRect,
    PhysicalSize, WritingMode,
};
pub use linebreak::{
    Break, LineBreakClass, LineBreakOptions, LineBreakStrictness, WordBreak, opportunities,
    widest_unbreakable_run,
};
pub use scrollport::{ClipMode, ScrollportGeometry};
pub use solver::{
    ImageResourceProvider, LayoutDiagnostic, LayoutDiagnosticCode, LayoutLimits, LayoutOptions,
    LayoutOutput, SimpleTextMeasurer, TextMeasure, TextMeasurer, TextSpacing, TextStyle,
    is_word_separator, layout_formatting_tree, layout_formatting_tree_with_images,
};
pub use sticky::{StickyConstraint, StickyInsets, sticky_offset, sticky_view_rect};
pub use tree::{
    FormattingContextKind, FormattingDiagnostic, FormattingDiagnosticCode, FormattingLimits,
    FormattingNode, FormattingNodeId, FormattingNodeKind, FormattingTree, FormattingWorkUnit,
    build_formatting_tree,
};
