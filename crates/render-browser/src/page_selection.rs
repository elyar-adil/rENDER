//! Per-tab page text selection.
//!
//! The pointer gesture is built on `render-core`'s [`PointerSelection`], so
//! caret placement, word boundaries, direction and mutation repair are shared
//! with every other embedding. This module only remembers which DOM revision
//! the boundaries belong to; the shell paints the resulting rectangles as an
//! overlay on the retained page raster.

use render_core::dom::{Dom, DomRevision};
use render_core::interaction::hit_test::{
    HitTestError, InteractionScene, PointerDownMode, PointerOutcome, PointerSelection,
    SelectionGeometry,
};
use render_core::interaction::{Selection, SelectionClearReason, SelectionRepair};
use render_core::layout::PhysicalPoint;
use render_net::Url;

/// One tab's page selection and the DOM revision whose ranges it names.
#[derive(Clone, Debug, Default)]
pub(super) struct PageSelection {
    pointer: PointerSelection,
    revision: Option<DomRevision>,
}

impl PageSelection {
    #[must_use]
    pub(super) const fn selection(&self) -> &Selection {
        self.pointer.selection()
    }

    #[must_use]
    pub(super) fn is_empty(&self) -> bool {
        self.pointer.selection().is_empty()
    }

    /// A collapsed caret has a range but paints no highlight and does not
    /// require the non-composited scroll path.
    #[must_use]
    pub(super) fn has_visible_range(&self) -> bool {
        self.pointer
            .selection()
            .range()
            .is_some_and(|range| !range.collapsed())
    }

    #[must_use]
    pub(super) const fn is_dragged(&self) -> bool {
        self.pointer.is_dragged()
    }

    /// Start a gesture at the caret nearest `point`.
    pub(super) fn begin(
        &mut self,
        scene: &InteractionScene<'_>,
        point: PhysicalPoint,
        mode: PointerDownMode,
    ) -> Result<PointerOutcome, HitTestError> {
        let outcome = self.pointer.pointer_down_with_mode(scene, point, mode)?;
        self.revision = Some(scene.dom().revision());
        Ok(outcome)
    }

    /// Extend the active gesture to `point`.
    pub(super) fn drag(
        &mut self,
        scene: &InteractionScene<'_>,
        point: PhysicalPoint,
    ) -> Result<PointerOutcome, HitTestError> {
        let outcome = self.pointer.pointer_drag(scene, point)?;
        if outcome.selection_changed {
            self.revision = Some(scene.dom().revision());
        }
        Ok(outcome)
    }

    /// Finish the gesture. Only a real drag moves the focus; a click or a
    /// double-click word selection keeps the range established on press.
    pub(super) fn finish(
        &mut self,
        scene: &InteractionScene<'_>,
        point: PhysicalPoint,
        document_base_url: &Url,
    ) -> Result<PointerOutcome, HitTestError> {
        let outcome = self.pointer.pointer_up(scene, point, document_base_url)?;
        if self.pointer.selection().is_empty() {
            self.revision = None;
        } else {
            self.revision = Some(scene.dom().revision());
        }
        Ok(outcome)
    }

    pub(super) fn clear(&mut self) {
        self.pointer.selection_mut().clear();
        self.revision = None;
    }

    /// Highlight rectangles for the current DOM revision. A selection whose
    /// revision no longer matches the scene is not painted; the caller repairs
    /// it before asking for geometry.
    pub(super) fn geometry(
        &self,
        scene: &InteractionScene<'_>,
    ) -> Result<SelectionGeometry, HitTestError> {
        if self.revision != Some(scene.dom().revision()) {
            return Ok(SelectionGeometry {
                dom_revision: scene.dom().revision(),
                rects: Vec::new(),
            });
        }
        scene.selection_geometry(self.pointer.selection())
    }

    /// Reconcile the boundaries with mutations applied since the gesture.
    /// Character-data edits clamp their offsets, and an ambiguous child-list
    /// change or a detached container clears the selection, exactly as
    /// [`Selection::apply_mutations`] specifies.
    pub(super) fn repair(&mut self, dom: &Dom) -> SelectionRepair {
        let Some(revision) = self.revision else {
            return SelectionRepair::Unchanged;
        };
        let current = dom.revision();
        if revision == current {
            return SelectionRepair::Unchanged;
        }
        // The journal cannot reconstruct a discarded change. Clearing is the
        // only safe answer; guessing could leave offsets in the middle of a
        // different node's text.
        let Ok(batch) = dom.mutations_since(revision) else {
            self.clear();
            return SelectionRepair::Cleared(SelectionClearReason::InvalidBoundary);
        };
        let repair = self.pointer.selection_mut().apply_mutations(dom, &batch);
        self.revision = if self.pointer.selection().is_empty() {
            None
        } else {
            Some(current)
        };
        repair
    }
}
