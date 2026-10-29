//! The loading state of image elements (HTML §4.8.4, "updating the image
//! data").
//!
//! The network and the decoder belong to the embedder, which reports what it
//! observed here. The state is document state, like focus, because script
//! reads it through `complete`/`naturalWidth`/`naturalHeight` and the canvas
//! `drawImage` usability check reads the same value.

use crate::{Dom, ImageLoadState, Namespace, Node, NodeId, NodeKind};

impl Dom {
    /// The loading state of `node`.
    ///
    /// A node with no recorded state has never started a load, which reads as
    /// [`ImageLoadState::Unavailable`]: `complete` is true and the natural size
    /// is zero.
    #[must_use]
    pub fn image_state(&self, node: NodeId) -> ImageLoadState {
        self.image_states.get(&node).copied().unwrap_or_default()
    }

    /// Records the loading state the embedder observed for an image element.
    ///
    /// [`ImageLoadState::Unavailable`] drops the entry rather than storing it.
    pub fn set_image_state(&mut self, node: NodeId, state: ImageLoadState) {
        if state == ImageLoadState::Unavailable {
            self.image_states.remove(&node);
        } else {
            self.image_states.insert(node, state);
        }
    }

    /// Resets the state of an HTML `<img>` whose `src` was set or removed.
    ///
    /// A new source makes the element pending immediately, so `complete`
    /// reads false at the next script statement instead of only once the next
    /// fetch is planned. Removing the source returns the element to
    /// [`ImageLoadState::Unavailable`].
    pub(crate) fn image_source_changed(&mut self, node: NodeId, has_source: bool) {
        if !self.is_html_image(node) {
            return;
        }
        self.set_image_state(
            node,
            if has_source {
                ImageLoadState::Pending
            } else {
                ImageLoadState::Unavailable
            },
        );
    }

    fn is_html_image(&self, node: NodeId) -> bool {
        matches!(
            self.node(node).map(Node::kind),
            Some(NodeKind::Element(element))
                if element.namespace == Namespace::Html && element.local_name == "img"
        )
    }
}
