//! Incremental rendering invalidation derived from the DOM mutation journal.
//!
//! This module deliberately describes *what* must be reconsidered without
//! coupling the DOM to a particular style, layout, or paint implementation.
//! Roots are conservative invalidation seeds: a phase may widen them further
//! when selector dependencies, formatting-context boundaries, or compositing
//! relationships require it.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use crate::dom::{Dom, DomRevision, MutationBatch, MutationHistoryError, MutationImpact, NodeId};

macro_rules! define_phase_invalidation {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct $name {
            pub from_revision: DomRevision,
            pub to_revision: DomRevision,
            pub roots: BTreeSet<NodeId>,
            pub full_refresh: bool,
        }

        impl $name {
            #[must_use]
            pub fn is_required(&self) -> bool {
                self.full_refresh || !self.roots.is_empty()
            }
        }
    };
}

define_phase_invalidation!(
    StyleInvalidation,
    "Nodes that seed selector matching and computed-style invalidation."
);
define_phase_invalidation!(
    LayoutInvalidation,
    "Nodes that seed formatting-tree and fragment invalidation."
);
define_phase_invalidation!(
    PaintInvalidation,
    "Nodes that seed display-list and raster invalidation."
);

/// Why incremental journal processing had to fall back to a full refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullRefreshReason {
    /// The requested mutation prefix is no longer retained by the bounded
    /// journal, so no narrower plan can be proven correct.
    MutationHistoryDiscarded {
        requested: DomRevision,
        oldest_available: DomRevision,
    },
}

/// A revision-bounded plan for the three rendering phases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderingInvalidationPlan {
    pub from_revision: DomRevision,
    pub to_revision: DomRevision,
    pub style: StyleInvalidation,
    pub layout: LayoutInvalidation,
    pub paint: PaintInvalidation,
    pub full_refresh_reason: Option<FullRefreshReason>,
}

impl RenderingInvalidationPlan {
    /// Derive a plan from a successfully retained mutation batch.
    #[must_use]
    pub fn from_batch(batch: &MutationBatch) -> Self {
        let mut style_roots = BTreeSet::new();
        let mut layout_roots = BTreeSet::new();
        let mut paint_roots = BTreeSet::new();

        for record in &batch.records {
            let target = record.kind.target();
            let impact = record.kind.impact();
            insert_impacted_root(
                impact,
                target,
                &mut style_roots,
                &mut layout_roots,
                &mut paint_roots,
            );
        }

        Self::incremental(
            batch.from_revision,
            batch.to_revision,
            style_roots,
            layout_roots,
            paint_roots,
        )
    }

    /// Read the DOM journal from `from_revision` and derive a safe plan.
    ///
    /// Discarded journal history produces a full-refresh plan instead of
    /// silently omitting mutations. A cursor from the future remains an error,
    /// because it indicates that the consumer and document do not share a
    /// valid revision lineage.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidationError::RevisionInFuture`] when `from_revision`
    /// exceeds the DOM's current revision.
    pub fn from_dom(dom: &Dom, from_revision: DomRevision) -> Result<Self, InvalidationError> {
        match dom.mutations_since(from_revision) {
            Ok(batch) => Ok(Self::from_batch(&batch)),
            Err(MutationHistoryError::HistoryDiscarded {
                requested,
                oldest_available,
            }) => Ok(Self::full_refresh(
                requested,
                dom.revision(),
                dom.document(),
                FullRefreshReason::MutationHistoryDiscarded {
                    requested,
                    oldest_available,
                },
            )),
            Err(MutationHistoryError::RevisionInFuture { requested, current }) => {
                Err(InvalidationError::RevisionInFuture { requested, current })
            }
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.style.is_required() && !self.layout.is_required() && !self.paint.is_required()
    }

    fn incremental(
        from_revision: DomRevision,
        to_revision: DomRevision,
        style_roots: BTreeSet<NodeId>,
        layout_roots: BTreeSet<NodeId>,
        paint_roots: BTreeSet<NodeId>,
    ) -> Self {
        Self {
            from_revision,
            to_revision,
            style: StyleInvalidation {
                from_revision,
                to_revision,
                roots: style_roots,
                full_refresh: false,
            },
            layout: LayoutInvalidation {
                from_revision,
                to_revision,
                roots: layout_roots,
                full_refresh: false,
            },
            paint: PaintInvalidation {
                from_revision,
                to_revision,
                roots: paint_roots,
                full_refresh: false,
            },
            full_refresh_reason: None,
        }
    }

    fn full_refresh(
        from_revision: DomRevision,
        to_revision: DomRevision,
        document: NodeId,
        reason: FullRefreshReason,
    ) -> Self {
        let roots = BTreeSet::from([document]);
        Self {
            from_revision,
            to_revision,
            style: StyleInvalidation {
                from_revision,
                to_revision,
                roots: roots.clone(),
                full_refresh: true,
            },
            layout: LayoutInvalidation {
                from_revision,
                to_revision,
                roots: roots.clone(),
                full_refresh: true,
            },
            paint: PaintInvalidation {
                from_revision,
                to_revision,
                roots,
                full_refresh: true,
            },
            full_refresh_reason: Some(reason),
        }
    }
}

fn insert_impacted_root(
    impact: MutationImpact,
    target: NodeId,
    style_roots: &mut BTreeSet<NodeId>,
    layout_roots: &mut BTreeSet<NodeId>,
    paint_roots: &mut BTreeSet<NodeId>,
) {
    if impact.affects_style() {
        style_roots.insert(target);
    }
    if impact.affects_layout() {
        layout_roots.insert(target);
    }
    if impact.affects_paint() {
        paint_roots.insert(target);
    }
}

/// An independently advancing view of the shared DOM mutation journal.
///
/// Style/layout rendering, accessibility, automation, or diagnostics can each
/// own a cursor. Taking a plan advances only that cursor and never consumes
/// journal records needed by another consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidationCursor {
    revision: DomRevision,
}

impl InvalidationCursor {
    #[must_use]
    pub const fn new(revision: DomRevision) -> Self {
        Self { revision }
    }

    #[must_use]
    pub fn at_current(dom: &Dom) -> Self {
        Self::new(dom.revision())
    }

    #[must_use]
    pub const fn revision(self) -> DomRevision {
        self.revision
    }

    /// Build a plan without advancing this cursor.
    ///
    /// # Errors
    ///
    /// Returns an error if this cursor is newer than the DOM.
    pub fn peek(self, dom: &Dom) -> Result<RenderingInvalidationPlan, InvalidationError> {
        RenderingInvalidationPlan::from_dom(dom, self.revision)
    }

    /// Build a plan and advance this cursor to the plan's ending revision.
    ///
    /// A history-loss fallback also advances the cursor after scheduling a
    /// full refresh, allowing normal incremental processing to resume.
    ///
    /// # Errors
    ///
    /// Returns an error if this cursor is newer than the DOM. The cursor is not
    /// advanced in that case.
    pub fn take(&mut self, dom: &Dom) -> Result<RenderingInvalidationPlan, InvalidationError> {
        let plan = self.peek(dom)?;
        self.revision = plan.to_revision;
        Ok(plan)
    }
}

/// A cursor/document revision mismatch that cannot safely be recovered by
/// replaying or refreshing the current DOM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidationError {
    RevisionInFuture {
        requested: DomRevision,
        current: DomRevision,
    },
}

impl fmt::Display for InvalidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionInFuture { requested, current } => write!(
                formatter,
                "invalidation cursor revision {} is newer than DOM revision {}",
                requested.as_u64(),
                current.as_u64()
            ),
        }
    }
}

impl Error for InvalidationError {}

#[cfg(test)]
mod tests {
    use super::{FullRefreshReason, InvalidationCursor};
    use crate::dom::Dom;

    #[test]
    fn no_mutations_produces_an_empty_plan() {
        let dom = Dom::new();
        let mut cursor = InvalidationCursor::at_current(&dom);

        let plan = cursor.take(&dom).unwrap();

        assert!(plan.is_empty());
        assert_eq!(plan.from_revision, plan.to_revision);
        assert_eq!(cursor.revision(), dom.revision());
        assert!(plan.full_refresh_reason.is_none());
    }

    #[test]
    fn character_data_skips_style_but_invalidates_layout_and_paint() {
        let mut dom = Dom::new();
        let element = dom.create_element("p");
        let text = dom.create_text("before");
        dom.append_child(element, text).unwrap();
        let mut cursor = InvalidationCursor::at_current(&dom);

        dom.set_character_data(text, "after").unwrap();
        let plan = cursor.take(&dom).unwrap();

        assert!(!plan.style.is_required());
        assert_eq!(
            plan.layout.roots.iter().copied().collect::<Vec<_>>(),
            [text]
        );
        assert_eq!(plan.paint.roots.iter().copied().collect::<Vec<_>>(), [text]);
        assert_eq!(plan.to_revision, dom.revision());
    }

    #[test]
    fn attribute_mutation_invalidates_every_rendering_phase() {
        let mut dom = Dom::new();
        let element = dom.create_element("div");
        let mut cursor = InvalidationCursor::at_current(&dom);

        dom.set_attribute(element, "class", "notice").unwrap();
        let plan = cursor.take(&dom).unwrap();

        assert_eq!(
            plan.style.roots.iter().copied().collect::<Vec<_>>(),
            [element]
        );
        assert_eq!(plan.layout.roots, plan.style.roots);
        assert_eq!(plan.paint.roots, plan.style.roots);
    }

    #[test]
    fn append_and_remove_invalidate_the_child_list_target() {
        let mut dom = Dom::new();
        let parent = dom.create_element("main");
        let child = dom.create_element("article");
        let mut cursor = InvalidationCursor::at_current(&dom);

        dom.append_child(parent, child).unwrap();
        let append = cursor.take(&dom).unwrap();
        assert_eq!(
            append.style.roots.iter().copied().collect::<Vec<_>>(),
            [parent]
        );
        assert!(append.layout.is_required());
        assert!(append.paint.is_required());

        dom.remove_child(parent, child).unwrap();
        let remove = cursor.take(&dom).unwrap();
        assert_eq!(
            remove.style.roots.iter().copied().collect::<Vec<_>>(),
            [parent]
        );
        assert_eq!(remove.from_revision, append.to_revision);
    }

    #[test]
    fn independent_consumers_do_not_consume_each_others_records() {
        let mut dom = Dom::new();
        let element = dom.create_element("div");
        let mut renderer = InvalidationCursor::at_current(&dom);
        let mut observer = renderer;

        dom.set_attribute(element, "hidden", "").unwrap();
        let render_plan = renderer.take(&dom).unwrap();
        assert_eq!(observer.revision(), render_plan.from_revision);

        let observer_plan = observer.take(&dom).unwrap();
        assert_eq!(observer_plan, render_plan);
        assert_eq!(renderer.revision(), observer.revision());
    }

    #[test]
    fn discarded_history_fails_closed_with_a_full_refresh() {
        let mut dom = Dom::new();
        dom.set_mutation_journal_capacity(1);
        let element = dom.create_element("div");
        let mut cursor = InvalidationCursor::at_current(&dom);

        dom.set_attribute(element, "id", "first").unwrap();
        dom.set_attribute(element, "id", "second").unwrap();
        let plan = cursor.take(&dom).unwrap();

        assert!(matches!(
            plan.full_refresh_reason,
            Some(FullRefreshReason::MutationHistoryDiscarded { requested, .. })
                if requested == plan.from_revision
        ));
        for roots in [&plan.style.roots, &plan.layout.roots, &plan.paint.roots] {
            assert_eq!(roots.iter().copied().collect::<Vec<_>>(), [dom.document()]);
        }
        assert!(plan.style.full_refresh);
        assert!(plan.layout.full_refresh);
        assert!(plan.paint.full_refresh);
        assert_eq!(cursor.revision(), dom.revision());
    }
}
