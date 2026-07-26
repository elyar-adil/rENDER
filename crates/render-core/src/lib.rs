//! Standards-driven primitives for the rENDER web runtime.
//!
//! The Rust implementation is replacing the Python engine incrementally. New
//! behavior is defined by web standards and conformance tests; the Python code
//! is a migration reference, not a behavioral authority.

pub mod css;
pub mod document;
pub mod dom;
pub mod event_loop;
pub mod html;
pub mod image;
pub mod interaction;
pub mod invalidation;
pub mod js;
pub mod layout;
pub mod navigation;
pub mod page;
pub mod paint;
pub mod spec;
