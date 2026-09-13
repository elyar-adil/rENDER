//! DOM-facing content interaction helpers used by the native shell.
//!
//! Keeping hit testing, control value access, and form association here makes
//! the window event loop responsible only for translating platform events.

use std::collections::BTreeMap;

use render_browser::chrome::Point;
use render_core::interaction::{
    ButtonBehavior, DefaultActionKind, FormMethod, activation_plan, plan_form_submission,
};
use render_core::js::ElementRect;
use render_core::layout::{PhysicalPoint, PhysicalRect};
use render_core::paint::{DisplayCommand, PaintCoordinateSpace};
use render_net::Url;

#[derive(Clone, Copy)]
pub(crate) struct ContentHitRegion {
    pub(crate) bounds: PhysicalRect,
    pub(crate) source: Option<render_core::dom::NodeId>,
    pub(crate) coordinate_space: PaintCoordinateSpace,
    pub(crate) hit_testable: bool,
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn hit_test_content_regions(
    regions: impl DoubleEndedIterator<Item = ContentHitRegion>,
    window_point: Point,
    chrome_height: u32,
    scroll_offset: PhysicalPoint,
) -> Option<render_core::dom::NodeId> {
    let viewport_point = PhysicalPoint {
        x: window_point.x,
        y: window_point.y - chrome_height as f32,
    };
    if viewport_point.x < 0.0
        || viewport_point.y < 0.0
        || !viewport_point.x.is_finite()
        || !viewport_point.y.is_finite()
    {
        return None;
    }
    regions.rev().find_map(|region| {
        if !region.hit_testable {
            return None;
        }
        let source = region.source?;
        let point = match region.coordinate_space {
            PaintCoordinateSpace::Document => PhysicalPoint {
                x: viewport_point.x + scroll_offset.x,
                y: viewport_point.y + scroll_offset.y,
            },
            PaintCoordinateSpace::Viewport => viewport_point,
        };
        (point.x >= region.bounds.origin.x
            && point.x < region.bounds.right()
            && point.y >= region.bounds.origin.y
            && point.y < region.bounds.bottom())
        .then_some(source)
    })
}

pub(crate) fn is_content_hit_command(command: &DisplayCommand) -> bool {
    !matches!(
        command,
        DisplayCommand::PushClip(_)
            | DisplayCommand::PopClip
            | DisplayCommand::PushTransform(_)
            | DisplayCommand::PopTransform
            | DisplayCommand::PushStackingContext(_)
            | DisplayCommand::PopStackingContext
    )
}

pub(crate) fn get_content_navigation_target(
    dom: &render_core::dom::Dom,
    hit_node: render_core::dom::NodeId,
    document_url: &Url,
) -> Option<Url> {
    let mut candidate = Some(hit_node);
    while let Some(node) = candidate {
        match activation_plan(dom, node).map(|plan| plan.default_action) {
            Some(DefaultActionKind::FollowHyperlink { href }) => {
                return document_url.join(&href).ok();
            }
            Some(DefaultActionKind::InvokeButton(ButtonBehavior::Submit))
                if dom.attribute(node, "disabled").ok().flatten().is_none() =>
            {
                let submission = plan_form_submission(dom, node, document_url).ok()?;
                return (submission.method == FormMethod::Get).then_some(submission.target);
            }
            _ => {}
        }
        candidate = dom.parent(node);
    }
    None
}

pub(crate) fn submit_form_for_node(
    dom: &render_core::dom::Dom,
    node: render_core::dom::NodeId,
) -> Option<render_core::dom::NodeId> {
    let mut candidate = Some(node);
    while let Some(current) = candidate {
        if activation_plan(dom, current).is_some_and(|plan| {
            matches!(
                plan.default_action,
                DefaultActionKind::InvokeButton(ButtonBehavior::Submit)
            )
        }) {
            let mut parent = dom.parent(current);
            while let Some(ancestor) = parent {
                if is_form(dom, ancestor) {
                    return Some(ancestor);
                }
                parent = dom.parent(ancestor);
            }
            let form_id = dom.attribute(current, "form").ok().flatten()?;
            return find_form_by_id(dom, form_id);
        }
        candidate = dom.parent(current);
    }
    None
}

pub(crate) fn associated_form_for_node(
    dom: &render_core::dom::Dom,
    node: render_core::dom::NodeId,
) -> Option<render_core::dom::NodeId> {
    let mut candidate = dom.parent(node);
    while let Some(current) = candidate {
        if is_form(dom, current) {
            return Some(current);
        }
        candidate = dom.parent(current);
    }
    find_form_by_id(dom, dom.attribute(node, "form").ok().flatten()?)
}

fn is_form(dom: &render_core::dom::Dom, node: render_core::dom::NodeId) -> bool {
    matches!(dom.node(node).map(render_core::dom::Node::kind), Some(render_core::dom::NodeKind::Element(element)) if element.local_name == "form")
}

fn find_form_by_id(dom: &render_core::dom::Dom, id: &str) -> Option<render_core::dom::NodeId> {
    let mut pending = vec![dom.document()];
    while let Some(current) = pending.pop() {
        if is_form(dom, current) && dom.attribute(current, "id").ok().flatten() == Some(id) {
            return Some(current);
        }
        pending.extend(
            dom.children(current)
                .unwrap_or_default()
                .iter()
                .rev()
                .copied(),
        );
    }
    None
}

pub(crate) fn is_content_editable(
    dom: &render_core::dom::Dom,
    node: render_core::dom::NodeId,
) -> bool {
    dom.attribute(node, "contenteditable")
        .ok()
        .flatten()
        .is_some_and(|value| {
            value.is_empty()
                || value.eq_ignore_ascii_case("true")
                || value.eq_ignore_ascii_case("plaintext-only")
        })
}

pub(crate) fn content_text_input_value(
    dom: &render_core::dom::Dom,
    node: render_core::dom::NodeId,
) -> Option<String> {
    let render_core::dom::NodeKind::Element(element) = dom.node(node)?.kind() else {
        return None;
    };
    match element.local_name.as_str() {
        "input" => {
            let input_type = dom
                .attribute(node, "type")
                .ok()
                .flatten()
                .filter(|value| !value.is_empty())
                .unwrap_or("text");
            matches!(input_type.to_ascii_lowercase().as_str(), "text" | "search").then(|| {
                dom.attribute(node, "value")
                    .ok()
                    .flatten()
                    .unwrap_or("")
                    .to_owned()
            })
        }
        "textarea" => Some(descendant_text(dom, node)),
        _ if is_content_editable(dom, node) => Some(descendant_text(dom, node)),
        _ => None,
    }
}

pub(crate) fn content_wrapper_control(
    dom: &render_core::dom::Dom,
    geometry: &BTreeMap<u64, ElementRect>,
    node: render_core::dom::NodeId,
) -> Option<render_core::dom::NodeId> {
    let bounds = geometry.get(&node.as_u64())?;
    let wrapper_area = bounds.width * bounds.height;
    if wrapper_area <= 0.0 {
        return None;
    }
    let mut pending = dom.children(node).unwrap_or_default().to_vec();
    let mut best = None;
    while let Some(current) = pending.pop() {
        pending.extend(dom.children(current).unwrap_or_default().iter().copied());
        if content_text_input_value(dom, current).is_none() {
            continue;
        }
        let Some(rect) = geometry.get(&current.as_u64()) else {
            continue;
        };
        let area = rect.width * rect.height;
        if best.is_none_or(|(smallest, _)| area < smallest) {
            best = Some((area, current));
        }
    }
    let (area, control) = best?;
    (area >= wrapper_area * 0.25).then_some(control)
}

fn descendant_text(dom: &render_core::dom::Dom, root: render_core::dom::NodeId) -> String {
    let mut output = String::new();
    let mut pending = dom
        .children(root)
        .unwrap_or_default()
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        if let Some(render_core::dom::NodeKind::Text(text)) =
            dom.node(node).map(render_core::dom::Node::kind)
        {
            output.push_str(text);
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev().copied());
    }
    output
}

pub(crate) fn set_content_text_value(
    dom: &mut render_core::dom::Dom,
    node: render_core::dom::NodeId,
    value: &str,
) -> Result<(), render_core::dom::DomError> {
    let kind = dom.node(node).map(render_core::dom::Node::kind);
    let Some(render_core::dom::NodeKind::Element(element)) = kind else {
        return Ok(());
    };
    if element.local_name == "input" {
        return dom.set_attribute(node, "value", value);
    }
    for child in dom.children(node).unwrap_or_default().to_vec() {
        dom.remove_child(node, child)?;
    }
    if !value.is_empty() {
        dom.append_text(node, value)?;
    }
    Ok(())
}

pub(crate) fn is_clickable_wrapper(
    dom: &render_core::dom::Dom,
    node: render_core::dom::NodeId,
) -> bool {
    let Some(render_core::dom::NodeKind::Element(element)) =
        dom.node(node).map(render_core::dom::Node::kind)
    else {
        return false;
    };
    if dom.attribute(node, "role").ok().flatten() == Some("button")
        || dom.attribute(node, "tabindex").ok().flatten().is_some()
    {
        return true;
    }
    dom.attribute(node, "class")
        .ok()
        .flatten()
        .is_some_and(|class| {
            class.split_ascii_whitespace().any(|token| {
                token.eq_ignore_ascii_case("btn")
                    || token.eq_ignore_ascii_case("button")
                    || token.ends_with("-btn")
                    || token.ends_with("-button")
            })
        })
        || matches!(element.local_name.as_str(), "summary" | "label")
}
