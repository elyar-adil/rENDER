//! Standards-driven primitives for the rENDER web runtime.
//!
//! The Rust implementation is replacing the Python engine incrementally. New
//! behavior is defined by web standards and conformance tests; the Python code
//! is a migration reference, not a behavioral authority.

pub mod css {
    pub use render_css::length::{CssValueError, LengthContext, resolve_length_expr};
    pub use render_css::*;
}
pub mod document;
pub mod dom {
    pub use render_dom::*;
}
pub mod event_loop;
pub mod html {
    pub use render_html::*;
}
pub mod image;
pub mod interaction;
pub mod invalidation;
pub mod js;
pub use render_layout as layout;
pub mod media;
pub mod navigation;
pub mod page;
pub mod paint;
pub mod script;
pub mod spec;
pub mod video;

// TEMPORARY: `image` remains in render-core because it shares deep
// dependencies with painting, so layout consumes decoded-image intrinsic
// sizes through this seam instead of depending on render-core directly
// (that would be a circular package dependency).
impl render_layout::ImageResourceProvider for image::ImageResources {
    fn intrinsic_size_for_node(&self, node: render_dom::NodeId) -> Option<(u32, u32)> {
        self.get_for_node(node)
            .map(|loaded| loaded.image.intrinsic_size())
    }
}
