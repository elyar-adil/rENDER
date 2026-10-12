//! CSS Positioned Layout 4 §4.1: where a sticky box is painted at a scroll offset.
//!
//! Layout resolves each sticky box's constraint in document space and never
//! moves the box. The displacement depends on the scroll offset, which only the
//! caller that paints or hit-tests knows, so both resolve it here with one rule:
//! a sticky box and its containing block sit where their ancestors have already
//! placed them, and the constraint is evaluated there.

use std::collections::BTreeMap;

use crate::layout::{
    FragmentId, FragmentTree, PhysicalPoint, PhysicalRect, StickyConstraint, sticky_offset,
};

/// The displacement of one sticky box.
///
/// `placed_by` is the displacement the box's ancestors already received, which
/// moves both its margin box and its containing block. `viewport_origin` is the
/// page's scroll offset. A constraint whose scrollport is the viewport uses that
/// offset; one whose scrollport belongs to a nested scroll container uses zero,
/// because no user scroll offset is recorded for a nested container yet.
#[must_use]
pub(crate) fn sticky_displacement(
    constraint: &StickyConstraint,
    placed_by: PhysicalPoint,
    viewport_origin: PhysicalPoint,
) -> PhysicalPoint {
    let placed = StickyConstraint {
        margin_rect: translate(constraint.margin_rect, placed_by),
        containing_block: translate(constraint.containing_block, placed_by),
        ..*constraint
    };
    let scroll = match constraint.scroll_container {
        None => viewport_origin,
        Some(_) => PhysicalPoint::default(),
    };
    sticky_offset(&placed, scroll)
}

/// The cumulative displacement of every fragment that a sticky box moves,
/// keyed by fragment. A fragment absent from the map is painted where layout
/// put it.
///
/// A box inside a sticky box takes the sticky box's displacement, and a sticky
/// box inside another is constrained after that displacement. This is the same
/// nesting the rasterizer applies, so hit testing and painting agree.
#[must_use]
pub(crate) fn sticky_displacements(
    fragments: &FragmentTree,
    viewport_origin: PhysicalPoint,
) -> BTreeMap<FragmentId, PhysicalPoint> {
    let mut displacements = BTreeMap::new();
    if fragments.sticky_fragments().next().is_none() {
        return displacements;
    }
    let mut stack = vec![(fragments.root(), PhysicalPoint::default())];
    while let Some((id, inherited)) = stack.pop() {
        let Some(fragment) = fragments.get(id) else {
            continue;
        };
        let total = match fragments.sticky_constraint(id) {
            Some(constraint) => {
                let own = sticky_displacement(constraint, inherited, viewport_origin);
                PhysicalPoint {
                    x: inherited.x + own.x,
                    y: inherited.y + own.y,
                }
            }
            None => inherited,
        };
        if is_displaced(total) {
            displacements.insert(id, total);
        }
        stack.extend(fragment.children.iter().map(|child| (*child, total)));
    }
    displacements
}

/// Whether a displacement moves a box at all. A displacement that was never set
/// is exactly zero and a sum of exact zeros stays zero, so the comparison is
/// against zero rather than an epsilon.
#[allow(
    clippy::float_cmp,
    reason = "an unset displacement is exactly zero and a sum of zeros stays zero"
)]
fn is_displaced(point: PhysicalPoint) -> bool {
    point.x != 0.0 || point.y != 0.0
}

fn translate(rect: PhysicalRect, by: PhysicalPoint) -> PhysicalRect {
    PhysicalRect::new(
        rect.origin.x + by.x,
        rect.origin.y + by.y,
        rect.size.width,
        rect.size.height,
    )
}

#[cfg(test)]
mod tests {
    use crate::document::{Document, DocumentRenderOptions, DocumentRenderOutput};
    use crate::dom::{Dom, NodeId};
    use crate::interaction::hit_test::{InteractionLimits, InteractionScene};
    use crate::layout::{PhysicalPoint, SimpleTextMeasurer};
    use crate::paint::{
        Color, CpuRasterizer, DisplayCommand, NoGlyphMasks, NoRasterCancellation, PaintScene,
        RasterDiagnosticCode, RasterRequest, Surface,
    };

    const WHITE: Color = Color::rgb(255, 255, 255);
    const RED: Color = Color::rgb(255, 0, 0);
    const BLUE: Color = Color::rgb(0, 0, 255);

    /// A sticky header in a body taller than the probes, so the body is the
    /// containing block and the header stays pinned at every probed offset.
    const HEADER_PAGE: &str = "<!doctype html><style>body{margin:0} \
        #bar{position:sticky;top:0;width:400px;height:40px;background-color:#ff0000} \
        .row{width:400px;height:300px} .tail{height:2000px}</style>\
        <div id=bar></div><div class=row></div><div class=row></div>\
        <div class=row></div><div class=tail></div>";

    /// The header's containing block ends at document y 400, so the header is
    /// released once the viewport passes it and is pushed up as it reaches the end.
    const CONTAINED_PAGE: &str = "<!doctype html><style>body{margin:0} #wrap{height:400px} \
        #bar{position:sticky;top:0;width:400px;height:40px;background-color:#ff0000} \
        .tail{height:2000px}</style>\
        <div id=wrap><div id=bar>bar</div></div><div class=tail></div>";

    /// A sticky box inside a 100px scroll container that sits at document y 1000.
    /// Its own containing block is the container's content box, so the page
    /// scroll must not move it.
    const NESTED_SCROLLER_PAGE: &str = "<!doctype html><style>body{margin:0} \
        #spacer{height:1000px} #scroller{height:100px;overflow:hidden} \
        .gap{height:1500px} \
        #stuck{position:sticky;top:0;height:40px;background-color:#ff0000} \
        .tail{height:2000px}</style>\
        <div id=spacer></div>\
        <div id=scroller><div class=gap></div><div id=stuck>stuck</div></div>\
        <div class=tail></div>";

    /// A sticky box inside another sticky box. The inner box is constrained
    /// after the outer box has moved, so it lands 20px below the viewport top
    /// rather than at the outer box's bottom edge.
    const NESTED_STICKY_PAGE: &str = "<!doctype html><style>body{margin:0} \
        #outer{position:sticky;top:0;height:100px;background-color:#0000ff} \
        #inner{position:sticky;top:20px;height:40px;background-color:#ff0000} \
        .row{height:300px} .tail{height:2000px}</style>\
        <div id=outer><div id=inner></div></div>\
        <div class=row></div><div class=row></div><div class=row></div>\
        <div class=tail></div>";

    fn render(html: &str) -> (Document, DocumentRenderOutput) {
        let document = Document::parse(html);
        let output = document.render_reference(DocumentRenderOptions::default());
        (document, output)
    }

    /// Rasterises the page with its viewport at document `y`. The offset is not
    /// clamped, so each test names the scroll position it means.
    fn raster_at(output: &DocumentRenderOutput, y: f32) -> Surface {
        let scene = PaintScene::from_display_list(output.display.list.clone());
        CpuRasterizer
            .rasterize_request(
                RasterRequest::new(&scene, WHITE, &NoGlyphMasks)
                    .with_viewport_origin(PhysicalPoint { x: 0.0, y }),
                &NoRasterCancellation,
            )
            .expect("a reference raster request is never cancelled")
            .surface
    }

    fn pixel(surface: &Surface, x: u32, y: u32) -> Color {
        surface
            .pixel(x, y)
            .expect("the probe is inside the viewport")
    }

    fn by_id(dom: &Dom, id: &str) -> NodeId {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            if dom.attribute(node, "id").ok().flatten() == Some(id) {
                return node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        }
        panic!("the page has no element with id {id:?}");
    }

    #[test]
    fn a_sticky_header_is_painted_at_the_top_of_the_viewport() {
        let (_, output) = render(HEADER_PAGE);
        assert_eq!(pixel(&raster_at(&output, 500.0), 200, 2), RED);
        assert_eq!(pixel(&raster_at(&output, 500.0), 200, 39), RED);
        assert_eq!(pixel(&raster_at(&output, 500.0), 200, 41), WHITE);
    }

    #[test]
    fn a_pinned_header_is_painted_over_the_sections_that_follow_it() {
        let (_, output) = render(&HEADER_PAGE.replace(".row{", ".row{background-color:#00ff00;"));
        // The green sections come later in tree order and overlap the header at
        // viewport 0..40, so the header must be painted after them.
        assert_eq!(pixel(&raster_at(&output, 500.0), 200, 2), RED);
        assert_eq!(
            pixel(&raster_at(&output, 500.0), 200, 41),
            Color::rgb(0, 255, 0)
        );
    }

    #[test]
    fn a_sticky_header_is_pushed_up_by_the_end_of_its_containing_block() {
        let (_, output) = render(CONTAINED_PAGE);
        // Inside the containing block the header is pinned to the viewport top.
        assert_eq!(pixel(&raster_at(&output, 300.0), 200, 2), RED);
        // At document y 380 the header would leave the block, so it is held at
        // the block's end: 360..400 in document space, 20px of it visible.
        assert_eq!(pixel(&raster_at(&output, 380.0), 200, 2), RED);
        assert_eq!(pixel(&raster_at(&output, 380.0), 200, 19), RED);
        assert_eq!(pixel(&raster_at(&output, 380.0), 200, 21), WHITE);
        // Past the block the header has scrolled away with its container.
        assert_eq!(pixel(&raster_at(&output, 500.0), 200, 2), WHITE);
    }

    #[test]
    fn a_sticky_box_in_a_nested_scroller_is_not_moved_by_the_page_scroll() {
        let (_, output) = render(NESTED_SCROLLER_PAGE);
        let surface = raster_at(&output, 1000.0);
        // The scroller is at the top of the viewport, and the box sticks to its
        // bottom edge: document 1060..1100, which is viewport 60..100.
        assert_eq!(pixel(&surface, 10, 59), WHITE);
        assert_eq!(pixel(&surface, 10, 60), RED);
        assert_eq!(pixel(&surface, 10, 99), RED);
    }

    #[test]
    fn a_sticky_box_inside_another_sticky_box_is_constrained_after_it_moves() {
        let (_, output) = render(NESTED_STICKY_PAGE);
        let surface = raster_at(&output, 500.0);
        // The outer box is pinned at the top: viewport 0..100.
        assert_eq!(pixel(&surface, 200, 0), BLUE);
        assert_eq!(pixel(&surface, 200, 99), BLUE);
        // The inner box keeps its 20px inset from the viewport top, so it is at
        // viewport 20..60, painted over the outer box.
        assert_eq!(pixel(&surface, 200, 19), BLUE);
        assert_eq!(pixel(&surface, 200, 20), RED);
        assert_eq!(pixel(&surface, 200, 59), RED);
        assert_eq!(pixel(&surface, 200, 60), BLUE);
    }

    #[test]
    fn sticky_scopes_open_and_close_in_pairs() {
        let (_, output) = render(NESTED_STICKY_PAGE);
        let items = output.display.list.items();
        let opened = items
            .iter()
            .filter(|item| matches!(item.command, DisplayCommand::PushSticky(_)))
            .count();
        let closed = items
            .iter()
            .filter(|item| matches!(item.command, DisplayCommand::PopSticky))
            .count();
        assert_eq!((opened, closed), (2, 2));
        let unbalanced = output
            .raster
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == RasterDiagnosticCode::UnbalancedState)
            .count();
        assert_eq!(unbalanced, 0, "a sticky scope left the raster unbalanced");
    }

    #[test]
    fn a_pinned_header_is_hit_where_it_is_painted() {
        let (document, output) = render(HEADER_PAGE);
        let bar = by_id(document.dom(), "bar");
        let scene = InteractionScene::new(
            document.dom(),
            &output.layout.fragments,
            &SimpleTextMeasurer,
            PhysicalPoint { x: 0.0, y: 500.0 },
            InteractionLimits::default(),
        )
        .expect("the scene is built for the rendered revision");
        let hit = scene
            .hit_test(PhysicalPoint { x: 200.0, y: 2.0 })
            .expect("the hit is within the budget")
            .expect("the pinned header is under the pointer");
        assert_eq!(hit.node, bar);
        // The document point is the one the pointer resolved to, which is what
        // selection and activation consume.
        assert!(
            (hit.document_point.y - 502.0).abs() < 0.01,
            "document point {:?}",
            hit.document_point
        );
    }

    #[test]
    fn nested_sticky_boxes_are_hit_where_they_are_painted() {
        let (document, output) = render(NESTED_STICKY_PAGE);
        let inner = by_id(document.dom(), "inner");
        let outer = by_id(document.dom(), "outer");
        let scene = InteractionScene::new(
            document.dom(),
            &output.layout.fragments,
            &SimpleTextMeasurer,
            PhysicalPoint { x: 0.0, y: 500.0 },
            InteractionLimits::default(),
        )
        .expect("the scene is built for the rendered revision");
        let hit_at = |y: f32| {
            scene
                .hit_test(PhysicalPoint { x: 200.0, y })
                .expect("the hit is within the budget")
                .expect("a sticky box is under the pointer")
                .node
        };
        assert_eq!(hit_at(30.0), inner);
        assert_eq!(hit_at(70.0), outer);
    }
}
