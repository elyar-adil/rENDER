//! Deterministic reference layout for block and inline formatting contexts.

use crate::css::computed::ComputedStyle;
use crate::css::properties::AutoLengthPercentage;
use crate::css::properties::BorderStyle;
use crate::css::properties::BorderWidth;
use crate::css::properties::BoxSizing;
use crate::css::properties::Gap;
use crate::css::properties::LengthPercentage;
use crate::css::properties::LengthResolutionContext;
use crate::css::properties::MaxSize;
use crate::css::properties::Position;
use crate::css::properties::Size;
use crate::css::properties::TypedPropertyValue;
use crate::dom::NodeId;
use crate::layout::fragment::FragmentId;
use crate::layout::fragment::FragmentKind;
use crate::layout::geometry::PhysicalRect;
use crate::layout::solver::AutoEdge;
use crate::layout::solver::LayoutDiagnostic;
use crate::layout::solver::LayoutDiagnosticCode;
use crate::layout::solver::Solver;

pub(super) fn position(style: Option<&ComputedStyle>) -> Position {
    match style.and_then(|style| style.typed("position")) {
        Some(TypedPropertyValue::Position(position)) => *position,
        _ => Position::Static,
    }
}

#[allow(clippy::cast_precision_loss)]
pub(super) fn count_as_f32(count: usize) -> f32 {
    count as f32
}

impl Solver<'_> {
    pub(super) fn resolve_gap(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::Gap(Gap::LengthPercentage(value))) => {
                self.resolve_length(value, basis, source, property).max(0.0)
            }
            _ => 0.0,
        }
    }

    pub(super) fn fragment_outer_rect(&self, fragment: FragmentId) -> Option<PhysicalRect> {
        let fragment = self
            .fragments
            .get(usize::try_from(fragment.as_u32()).ok()?)?;
        match &fragment.kind {
            FragmentKind::Box(geometry) => Some(geometry.margin_rect()),
            FragmentKind::Text(_) => Some(fragment.rect),
        }
    }

    pub(super) fn translate_fragment_subtree(&mut self, root: FragmentId, dx: f32, dy: f32) {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let Some(fragment) = self.fragment_mut(id) else {
                continue;
            };
            fragment.rect.origin.x += dx;
            fragment.rect.origin.y += dy;
            if let FragmentKind::Box(geometry) = &mut fragment.kind {
                geometry.content_rect.origin.x += dx;
                geometry.content_rect.origin.y += dy;
            } else if let FragmentKind::Text(text) = &mut fragment.kind {
                text.baseline += dy;
            }
            stack.extend(fragment.children.iter().copied());
        }
    }

    pub(super) fn stretch_fragment_outer_height(
        &mut self,
        fragment: FragmentId,
        outer_height: f32,
    ) {
        self.resize_fragment_outer_height(fragment, outer_height);
    }

    pub(super) fn resize_fragment_outer_height(&mut self, fragment: FragmentId, outer_height: f32) {
        let Some(fragment) = self.fragment_mut(fragment) else {
            return;
        };
        let FragmentKind::Box(geometry) = &mut fragment.kind else {
            return;
        };
        geometry.content_rect.size.height = (outer_height
            - geometry.margin.vertical()
            - geometry.border.vertical()
            - geometry.padding.vertical())
        .max(0.0);
        fragment.rect = geometry.border_rect();
    }

    pub(super) fn resolve_auto_edge(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> AutoEdge {
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::Margin(AutoLengthPercentage::Auto)) => AutoEdge {
                value: 0.0,
                auto: true,
            },
            Some(TypedPropertyValue::Margin(AutoLengthPercentage::LengthPercentage(value))) => {
                AutoEdge {
                    value: self.resolve_length(value, basis, node, property),
                    auto: false,
                }
            }
            _ => AutoEdge::default(),
        }
    }

    pub(super) fn resolve_edge(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> f32 {
        let value = match style.and_then(|style| style.typed(property)) {
            Some(
                TypedPropertyValue::Margin(AutoLengthPercentage::LengthPercentage(value))
                | TypedPropertyValue::Inset(AutoLengthPercentage::LengthPercentage(value))
                | TypedPropertyValue::Padding(value),
            ) => Some(value),
            _ => None,
        };
        value.map_or(0.0, |value| {
            self.resolve_length(value, basis, node, property)
        })
    }

    pub(super) fn resolve_inset(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> Option<f32> {
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::Inset(AutoLengthPercentage::LengthPercentage(value))) => {
                Some(self.resolve_length(value, basis, node, property))
            }
            _ => None,
        }
    }

    pub(super) fn resolve_border(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> f32 {
        let style_property = match property {
            "border-top-width" => "border-top-style",
            "border-right-width" => "border-right-style",
            "border-bottom-width" => "border-bottom-style",
            "border-left-width" => "border-left-style",
            _ => unreachable!("only physical border-width longhands are resolved"),
        };
        if matches!(
            style.and_then(|style| style.typed(style_property)),
            Some(TypedPropertyValue::BorderStyle(
                BorderStyle::None | BorderStyle::Hidden
            )) | None
        ) {
            return 0.0;
        }
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::BorderWidth(BorderWidth::Thin)) => 1.0,
            Some(TypedPropertyValue::BorderWidth(BorderWidth::Medium)) => 3.0,
            Some(TypedPropertyValue::BorderWidth(BorderWidth::Thick)) => 5.0,
            Some(TypedPropertyValue::BorderWidth(BorderWidth::Length(value))) => {
                self.resolve_length(value, basis, node, property).max(0.0)
            }
            _ => 0.0,
        }
    }

    pub(super) fn resolve_size(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> Option<f32> {
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::Size(Size::LengthPercentage(value))) => {
                Some(self.resolve_length(value, basis, node, property).max(0.0))
            }
            Some(TypedPropertyValue::Size(
                Size::MinContent | Size::MaxContent | Size::FitContent(_) | Size::Stretch,
            )) => {
                self.diagnostics.push(LayoutDiagnostic {
                    node,
                    code: LayoutDiagnosticCode::IntrinsicSizingNotImplemented,
                    message: format!("intrinsic sizing for '{property}' is not implemented"),
                });
                None
            }
            _ => None,
        }
    }

    pub(super) fn apply_min_max_width(
        &mut self,
        style: Option<&ComputedStyle>,
        width: f32,
        basis: f32,
        node: Option<NodeId>,
        non_content: f32,
        box_sizing: BoxSizing,
    ) -> f32 {
        let to_content_width = |value: f32| match box_sizing {
            BoxSizing::ContentBox => value.max(0.0),
            BoxSizing::BorderBox => (value - non_content).max(0.0),
        };
        let min = match style.and_then(|style| style.typed("min-width")) {
            Some(TypedPropertyValue::Size(Size::LengthPercentage(value))) => {
                to_content_width(self.resolve_length(value, basis, node, "min-width"))
            }
            _ => 0.0,
        };
        let max = match style.and_then(|style| style.typed("max-width")) {
            Some(TypedPropertyValue::MaxSize(MaxSize::Size(Size::LengthPercentage(value)))) => {
                Some(to_content_width(self.resolve_length(
                    value,
                    basis,
                    node,
                    "max-width",
                )))
            }
            _ => None,
        };
        max.map_or(width.max(min), |max| width.max(min).min(max))
    }

    pub(super) fn apply_min_max_height(
        &mut self,
        style: Option<&ComputedStyle>,
        height: f32,
        basis: f32,
        node: Option<NodeId>,
        non_content: f32,
        box_sizing: BoxSizing,
    ) -> f32 {
        let to_content_height = |value: f32| match box_sizing {
            BoxSizing::ContentBox => value.max(0.0),
            BoxSizing::BorderBox => (value - non_content).max(0.0),
        };
        let min = match style.and_then(|style| style.typed("min-height")) {
            Some(TypedPropertyValue::Size(Size::LengthPercentage(value))) => {
                to_content_height(self.resolve_length(value, basis, node, "min-height"))
            }
            _ => 0.0,
        };
        let max = match style.and_then(|style| style.typed("max-height")) {
            Some(TypedPropertyValue::MaxSize(MaxSize::Size(Size::LengthPercentage(value)))) => {
                Some(to_content_height(self.resolve_length(
                    value,
                    basis,
                    node,
                    "max-height",
                )))
            }
            _ => None,
        };
        max.map_or(height.max(min), |max| height.min(max).max(min))
    }

    pub(super) fn resolve_length(
        &mut self,
        value: &LengthPercentage,
        basis: f32,
        node: Option<NodeId>,
        property: &str,
    ) -> f32 {
        let context = LengthResolutionContext {
            percentage_basis: Some(basis),
            font_size: self.options.root_font_size,
            root_font_size: self.options.root_font_size,
            line_height: self.options.default_line_height,
            root_line_height: self.options.default_line_height,
            viewport_width: self.options.viewport.width,
            viewport_height: self.options.viewport.height,
            ..LengthResolutionContext::default()
        };
        match value.resolve(&context) {
            Ok(value) => value,
            Err(error) => {
                self.diagnostics.push(LayoutDiagnostic {
                    node,
                    code: LayoutDiagnosticCode::UnresolvedUsedValue,
                    message: format!("could not resolve '{property}': {error}"),
                });
                0.0
            }
        }
    }
}
