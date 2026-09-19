//! Deterministic reference layout for block and inline formatting contexts.

use crate::css::properties::AlignItems;
use crate::css::properties::BoxSizing;
use crate::css::properties::JustifyContent;
use crate::css::properties::TextAlign;
use crate::css::properties::TypedPropertyValue;
use crate::dom::Node;
use crate::dom::NodeId;
use crate::dom::NodeKind;
use crate::layout::fragment::FragmentId;
use crate::layout::fragment::FragmentKind;
use crate::layout::fragment::TextFragmentData;
use crate::layout::geometry::PhysicalRect;
use crate::layout::geometry::PhysicalSize;
use crate::layout::solver::FloatArea;
use crate::layout::solver::InlineAtom;
use crate::layout::solver::LayoutDiagnostic;
use crate::layout::solver::LayoutDiagnosticCode;
use crate::layout::solver::Solver;
use crate::layout::solver::TextRun;
use crate::layout::solver::TextStyle;
use crate::layout::solver::block::inline_float_band;
use crate::layout::solver::resolve::count_as_f32;
use crate::layout::tree::FormattingNodeId;
use crate::layout::tree::FormattingNodeKind;

#[allow(
    clippy::cast_precision_loss,
    reason = "CSS layout geometry is f32 and decoded image dimensions are bounded by image limits"
)]
pub(super) fn image_dimension_to_f32(value: u32) -> f32 {
    value as f32
}

pub(super) fn parse_font_size(value: &str, basis: f32) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    // Absolute-size keywords share the mapping used by the computed-value
    // stage so both interpretations of `font-size` stay consistent.
    crate::css::properties::absolute_font_size_keyword(&value, basis)
        .or_else(|| parse_text_length(&value, basis))
}

pub(super) fn parse_line_height(value: &str, font_size: f32) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    if value == "normal" {
        return Some(font_size * 1.2);
    }
    value
        .parse::<f32>()
        .ok()
        .map(|factor| factor * font_size)
        .or_else(|| parse_text_length(&value, font_size))
}

pub(super) fn parse_text_length(value: &str, basis: f32) -> Option<f32> {
    if let Some(value) = value.strip_suffix("px") {
        value.trim().parse().ok()
    } else if let Some(value) = value
        .strip_suffix("rem")
        .or_else(|| value.strip_suffix("em"))
    {
        value.trim().parse::<f32>().ok().map(|value| value * basis)
    } else if let Some(value) = value.strip_suffix('%') {
        value
            .trim()
            .parse::<f32>()
            .ok()
            .map(|value| value * basis / 100.0)
    } else {
        None
    }
}

pub(super) fn justify_offsets(
    justify: JustifyContent,
    free: f32,
    item_count: usize,
    base_gap: f32,
) -> (f32, f32) {
    let slots = count_as_f32(item_count.saturating_sub(1));
    match justify {
        JustifyContent::FlexEnd | JustifyContent::End => (free, base_gap),
        JustifyContent::Center => (free / 2.0, base_gap),
        JustifyContent::SpaceBetween if item_count > 1 => (0.0, base_gap + free / slots),
        JustifyContent::SpaceAround if item_count > 0 => {
            let distributed = free / count_as_f32(item_count);
            (distributed / 2.0, base_gap + distributed)
        }
        JustifyContent::SpaceEvenly if item_count > 0 => {
            let distributed = free / count_as_f32(item_count.saturating_add(1));
            (distributed, base_gap + distributed)
        }
        JustifyContent::Normal
        | JustifyContent::FlexStart
        | JustifyContent::Start
        | JustifyContent::SpaceBetween
        | JustifyContent::SpaceAround
        | JustifyContent::SpaceEvenly => (0.0, base_gap),
    }
}

pub(super) const fn align_offset(align: AlignItems, line_cross: f32, item_cross: f32) -> f32 {
    match align {
        AlignItems::FlexEnd | AlignItems::End => line_cross - item_cross,
        AlignItems::Center => (line_cross - item_cross) / 2.0,
        AlignItems::Normal | AlignItems::Stretch | AlignItems::FlexStart | AlignItems::Start => 0.0,
    }
}

pub(super) fn inline_segment_end(atoms: &[InlineAtom], start: usize) -> usize {
    let mut end = start + 1;
    while let Some(next) = atoms.get(end) {
        let previous = atoms[end - 1];
        if next.forced_break
            || next.atomic.is_some()
            || previous.atomic.is_some()
            || next.character.is_whitespace()
            || (previous.wrap_allowed
                && next.wrap_allowed
                && is_soft_line_break(previous.character, next.character))
        {
            break;
        }
        end += 1;
    }
    end
}

pub(super) fn is_soft_line_break(previous: char, next: char) -> bool {
    if is_prohibited_line_end(previous) || is_prohibited_line_start(next) {
        return false;
    }
    is_wide_character(previous)
        || is_wide_character(next)
        || matches!(previous, '-' | '/' | '\u{2010}')
}

pub(super) const fn is_prohibited_line_end(character: char) -> bool {
    matches!(
        character,
        '(' | '['
            | '{'
            | '\u{00ab}'
            | '\u{2018}'
            | '\u{201c}'
            | '\u{3008}'
            | '\u{300a}'
            | '\u{300c}'
            | '\u{300e}'
            | '\u{3010}'
            | '\u{3014}'
            | '\u{3016}'
            | '\u{3018}'
            | '\u{301a}'
            | '\u{ff08}'
            | '\u{ff3b}'
            | '\u{ff5b}'
            | '\u{ff5f}'
    )
}

pub(super) const fn is_prohibited_line_start(character: char) -> bool {
    matches!(
        character,
        '!' | '%' | ')' | ','
            ..='.'
                | ':'
                | ';'
                | '?'
                | ']'
                | '}'
                | '\u{00bb}'
                | '\u{2019}'
                | '\u{201d}'
                | '\u{3001}'
                | '\u{3002}'
                | '\u{3009}'
                | '\u{300b}'
                | '\u{300d}'
                | '\u{300f}'
                | '\u{3011}'
                | '\u{3015}'
                | '\u{3017}'
                | '\u{3019}'
                | '\u{301b}'
                | '\u{ff01}'
                | '\u{ff09}'
                | '\u{ff0c}'
                | '\u{ff0e}'
                | '\u{ff1a}'
                | '\u{ff1b}'
                | '\u{ff1f}'
                | '\u{ff3d}'
                | '\u{ff5d}'
                | '\u{ff60}'
    )
}

pub(super) const fn is_wide_character(character: char) -> bool {
    matches!(
        character as u32,
        0x1100..=0x115f
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1faff
    )
}

impl Solver<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn layout_inline_content(
        &mut self,
        roots: &[FormattingNodeId],
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        text_align: TextAlign,
        depth: usize,
        floats: &[FloatArea],
    ) -> (Vec<FragmentId>, f32) {
        let atoms = self.collect_inline_content_atoms(roots, depth);
        if atoms.is_empty()
            || atoms.iter().all(|atom| {
                atom.atomic.is_none() && !atom.forced_break && atom.character.is_whitespace()
            })
        {
            return (Vec::new(), 0.0);
        }
        let ends_with_forced_break = atoms
            .iter()
            .rev()
            .find(|atom| atom.forced_break || !atom.character.is_whitespace())
            .is_some_and(|atom| atom.forced_break);

        let default_style = TextStyle {
            font_size: self.options.root_font_size,
            line_height: self.options.default_line_height,
        };
        let first_style = atoms.first().map_or(default_style, |atom| atom.style);
        let mut fragments = Vec::new();
        let mut line_y = containing.origin.y;
        let (mut line_left, mut line_right) = inline_float_band(
            floats,
            containing,
            &mut line_y,
            first_style.line_height,
            0.0,
        );
        let mut line_x = line_left;
        let mut current_line_height = first_style.line_height;
        let mut pending_space: Option<InlineAtom> = None;
        let mut current_run: Option<TextRun> = None;
        let mut cursor = 0;

        while cursor < atoms.len() {
            let atom = atoms[cursor];
            if atom.forced_break {
                self.flush_text_run(&mut current_run, &mut fragments);
                line_y += current_line_height;
                current_line_height = atom.style.line_height;
                (line_left, line_right) =
                    inline_float_band(floats, containing, &mut line_y, current_line_height, 0.0);
                line_x = line_left;
                pending_space = None;
                cursor += 1;
                continue;
            }
            if let Some(atomic) = atom.atomic {
                self.flush_text_run(&mut current_run, &mut fragments);
                if let Some(space) = pending_space.take()
                    && line_x > line_left
                {
                    let width = self.text_measurer.measure(" ", space.style).advance;
                    self.push_character(
                        &mut current_run,
                        &mut fragments,
                        space,
                        ' ',
                        line_x,
                        line_y,
                        width,
                        space.style,
                    );
                    self.flush_text_run(&mut current_run, &mut fragments);
                    line_x += width;
                }
                if let Some((fragment, outer)) = self.layout_atomic_inline(
                    atomic,
                    containing,
                    positioning_containing,
                    line_x,
                    line_y,
                    depth.saturating_add(1),
                ) {
                    if self.is_out_of_flow(atomic) {
                        // Out-of-flow boxes do not participate in the line
                        // box: they neither advance the inline cursor nor
                        // contribute to the line height, and text alignment
                        // must not shift them.
                        fragments.push(fragment);
                        cursor += 1;
                        continue;
                    }
                    if (line_x - line_left).abs() < f32::EPSILON
                        && line_x + outer.size.width > line_right
                    {
                        (line_left, line_right) = inline_float_band(
                            floats,
                            containing,
                            &mut line_y,
                            current_line_height,
                            outer.size.width,
                        );
                        line_x = line_left;
                        self.translate_fragment_subtree(
                            fragment,
                            line_x - outer.origin.x,
                            line_y - outer.origin.y,
                        );
                    }
                    if line_x > line_left && line_x + outer.size.width > line_right {
                        line_y += current_line_height;
                        current_line_height = atom.style.line_height;
                        (line_left, line_right) = inline_float_band(
                            floats,
                            containing,
                            &mut line_y,
                            current_line_height,
                            0.0,
                        );
                        line_x = line_left;
                        self.translate_fragment_subtree(
                            fragment,
                            line_x - outer.origin.x,
                            line_y - outer.origin.y,
                        );
                    }
                    line_x += outer.size.width;
                    current_line_height = current_line_height.max(outer.size.height);
                    fragments.push(fragment);
                }
                cursor += 1;
                continue;
            }
            if atom.character.is_whitespace() {
                pending_space = Some(atom);
                cursor += 1;
                continue;
            }

            let segment_end = inline_segment_end(&atoms, cursor);
            let segment = &atoms[cursor..segment_end];
            let segment_width = self.measure_inline_segment(segment);
            if (line_x - line_left).abs() < f32::EPSILON && segment_width > line_right - line_left {
                (line_left, line_right) = inline_float_band(
                    floats,
                    containing,
                    &mut line_y,
                    current_line_height,
                    segment_width,
                );
                line_x = line_left;
            }
            let space_width = pending_space
                .as_ref()
                .filter(|_| line_x > line_left)
                .map_or(0.0, |space| {
                    self.text_measurer.measure(" ", space.style).advance
                });
            if segment.first().is_some_and(|atom| atom.wrap_allowed)
                && line_x > line_left
                && line_x + space_width + segment_width > line_right
            {
                self.flush_text_run(&mut current_run, &mut fragments);
                line_y += current_line_height;
                current_line_height = segment[0].style.line_height;
                (line_left, line_right) =
                    inline_float_band(floats, containing, &mut line_y, current_line_height, 0.0);
                line_x = line_left;
                pending_space = None;
            }

            if let Some(space) = pending_space.take()
                && line_x > line_left
            {
                let width = self.text_measurer.measure(" ", space.style).advance;
                self.push_character(
                    &mut current_run,
                    &mut fragments,
                    space,
                    ' ',
                    line_x,
                    line_y,
                    width,
                    space.style,
                );
                line_x += width;
            }

            for atom in segment {
                let width = self.measure_inline_character(atom.character, atom.style);
                if atom.wrap_allowed && line_x + width > line_right && line_x > line_left {
                    self.flush_text_run(&mut current_run, &mut fragments);
                    line_y += current_line_height;
                    current_line_height = atom.style.line_height;
                    (line_left, line_right) = inline_float_band(
                        floats,
                        containing,
                        &mut line_y,
                        current_line_height,
                        0.0,
                    );
                    line_x = line_left;
                }
                current_line_height = current_line_height.max(atom.style.line_height);
                self.push_character(
                    &mut current_run,
                    &mut fragments,
                    *atom,
                    atom.character,
                    line_x,
                    line_y,
                    width,
                    atom.style,
                );
                line_x += width;
            }
            cursor = segment_end;
        }
        self.flush_text_run(&mut current_run, &mut fragments);
        let trailing_line_height = if ends_with_forced_break {
            0.0
        } else {
            current_line_height
        };
        let height = line_y - containing.origin.y + trailing_line_height;
        self.align_inline_fragments(&fragments, containing, text_align);
        (fragments, height)
    }

    pub(super) fn text_align(&self, style_source: Option<NodeId>) -> TextAlign {
        match style_source.and_then(|source| self.styles.get(&source)) {
            Some(style) => match style.typed("text-align") {
                Some(TypedPropertyValue::TextAlign(value)) => *value,
                _ => TextAlign::Start,
            },
            None => TextAlign::Start,
        }
    }

    pub(super) fn align_inline_fragments(
        &mut self,
        fragments: &[FragmentId],
        containing: PhysicalRect,
        text_align: TextAlign,
    ) {
        if matches!(
            text_align,
            TextAlign::Start | TextAlign::Left | TextAlign::Justify
        ) {
            return;
        }
        let mut lines = Vec::<(f32, f32, f32, Vec<FragmentId>)>::new();
        for fragment_id in fragments {
            let Some(fragment) = self.fragments.get(
                usize::try_from(fragment_id.as_u32())
                    .ok()
                    .unwrap_or(usize::MAX),
            ) else {
                continue;
            };
            if self.source_is_out_of_flow(fragment.source) {
                continue;
            }
            let rect = fragment.rect;
            let Some((_, min_x, max_x, line_fragments)) = lines
                .iter_mut()
                .find(|(line_y, _, _, _)| (*line_y - rect.origin.y).abs() < 0.5)
            else {
                lines.push((
                    rect.origin.y,
                    rect.origin.x,
                    rect.right(),
                    vec![*fragment_id],
                ));
                continue;
            };
            *min_x = min_x.min(rect.origin.x);
            *max_x = max_x.max(rect.right());
            line_fragments.push(*fragment_id);
        }
        for (_, min_x, max_x, line_fragments) in lines {
            let free = (containing.size.width - (max_x - min_x)).max(0.0);
            let offset = match text_align {
                TextAlign::End | TextAlign::Right => free,
                TextAlign::Center => free / 2.0,
                TextAlign::Start | TextAlign::Left | TextAlign::Justify => 0.0,
            };
            for fragment in line_fragments {
                self.translate_fragment_subtree(fragment, offset, 0.0);
            }
        }
    }

    pub(super) fn collect_inline_content_atoms(
        &mut self,
        roots: &[FormattingNodeId],
        depth: usize,
    ) -> Vec<InlineAtom> {
        let mut atoms = Vec::new();
        for root in roots {
            self.collect_inline_atoms(*root, &mut atoms, depth);
        }
        atoms
    }

    pub(super) fn measure_inline_segment(&self, segment: &[InlineAtom]) -> f32 {
        segment
            .iter()
            .map(|atom| self.measure_inline_character(atom.character, atom.style))
            .sum()
    }

    pub(super) fn measure_inline_character(&self, character: char, style: TextStyle) -> f32 {
        let mut encoded = [0_u8; 4];
        self.text_measurer
            .measure(character.encode_utf8(&mut encoded), style)
            .advance
    }

    pub(super) fn intrinsic_text_width(
        &self,
        text: &str,
        source: Option<NodeId>,
        style: TextStyle,
    ) -> f32 {
        let preserves_whitespace = source
            .and_then(|source| self.styles.get(&source))
            .and_then(|style| style.get("white-space"))
            .is_some_and(|value| {
                matches!(
                    value.css_text().to_ascii_lowercase().as_str(),
                    "pre" | "pre-wrap" | "break-spaces"
                )
            });
        if preserves_whitespace {
            return self.text_measurer.measure(text, style).advance;
        }
        let mut width = 0.0;
        let mut pending_space = false;
        let mut has_content = false;
        for character in text.chars() {
            if character.is_whitespace() {
                pending_space |= has_content;
                continue;
            }
            if pending_space {
                width += self.text_measurer.measure(" ", style).advance;
                pending_space = false;
            }
            width += self.measure_inline_character(character, style);
            has_content = true;
        }
        width
    }

    pub(super) fn text_style(&self, source: Option<NodeId>) -> TextStyle {
        let computed = source.and_then(|source| self.styles.get(&source));
        let font_size = computed
            .and_then(|style| style.get("font-size"))
            .and_then(|value| parse_font_size(value.css_text(), self.options.root_font_size))
            .unwrap_or(self.options.root_font_size)
            .clamp(1.0, 512.0);
        let line_height = computed
            .and_then(|style| style.get("line-height"))
            .and_then(|value| parse_line_height(value.css_text(), font_size))
            .unwrap_or(font_size * 1.2)
            .clamp(font_size, 1_024.0);
        TextStyle {
            font_size,
            line_height,
        }
    }

    pub(super) fn collect_inline_atoms(
        &mut self,
        node_id: FormattingNodeId,
        atoms: &mut Vec<InlineAtom>,
        depth: usize,
    ) {
        if depth > self.options.limits.max_depth {
            return;
        }
        let Some(node) = self.formatting.get(node_id).cloned() else {
            self.diagnostics.push(LayoutDiagnostic {
                node: None,
                code: LayoutDiagnosticCode::MissingFormattingNode,
                message: "inline layout referenced an unknown formatting node".to_owned(),
            });
            return;
        };
        if let FormattingNodeKind::Text(text) = node.kind {
            let text_style = self.text_style(node.style_source);
            let wrap_allowed = node
                .style_source
                .and_then(|source| self.styles.get(&source))
                .and_then(|style| style.get("white-space"))
                .is_none_or(|value| !value.css_text().eq_ignore_ascii_case("nowrap"));
            for character in text.chars() {
                if self.inline_characters >= self.options.limits.max_inline_characters {
                    self.diagnostics.push(LayoutDiagnostic {
                        node: node.source,
                        code: LayoutDiagnosticCode::InlineTextLimit,
                        message: "inline character limit exceeded".to_owned(),
                    });
                    return;
                }
                self.inline_characters += 1;
                atoms.push(InlineAtom {
                    formatting_node: node_id,
                    source: node.source,
                    character,
                    forced_break: false,
                    wrap_allowed,
                    atomic: None,
                    style: text_style,
                });
            }
            return;
        }
        if node.source.is_some_and(|source| {
            matches!(
                self.dom.node(source).map(Node::kind),
                Some(NodeKind::Element(data)) if data.local_name == "br"
            )
        }) {
            atoms.push(InlineAtom {
                formatting_node: node_id,
                source: node.source,
                character: '\n',
                forced_break: true,
                wrap_allowed: false,
                atomic: None,
                style: self.text_style(node.style_source),
            });
            return;
        }
        if matches!(node.kind, FormattingNodeKind::AtomicInline { .. }) {
            atoms.push(InlineAtom {
                formatting_node: node_id,
                source: node.source,
                character: '\0',
                forced_break: false,
                wrap_allowed: true,
                atomic: Some(node_id),
                style: self.text_style(node.style_source),
            });
            return;
        }
        for child in node.children {
            self.collect_inline_atoms(child, atoms, depth.saturating_add(1));
        }
    }

    pub(super) fn layout_atomic_inline(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        x: f32,
        y: f32,
        depth: usize,
    ) -> Option<(FragmentId, PhysicalRect)> {
        let content_width = self.atomic_inline_content_width(node_id, containing.size.width);
        let result = self.layout_block(
            node_id,
            PhysicalRect::new(
                x,
                containing.origin.y,
                containing.size.width,
                containing.size.height,
            ),
            positioning_containing,
            y,
            depth,
            Some(content_width),
        )?;
        let outer = self.fragment_outer_rect(result.fragment)?;
        Some((result.fragment, outer))
    }

    pub(super) fn atomic_inline_content_width(
        &mut self,
        node_id: FormattingNodeId,
        containing_width: f32,
    ) -> f32 {
        let node = self.formatting.get(node_id).cloned();
        let source = node.as_ref().and_then(|node| node.source);
        let style = node
            .as_ref()
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source))
            .cloned();
        let padding = self.resolve_edge(style.as_ref(), "padding-left", containing_width, source)
            + self.resolve_edge(style.as_ref(), "padding-right", containing_width, source);
        let border = self.resolve_border(
            style.as_ref(),
            "border-left-width",
            containing_width,
            source,
        ) + self.resolve_border(
            style.as_ref(),
            "border-right-width",
            containing_width,
            source,
        );
        let box_sizing = match style.as_ref().and_then(|style| style.typed("box-sizing")) {
            Some(TypedPropertyValue::BoxSizing(value)) => *value,
            _ => BoxSizing::ContentBox,
        };
        let css_width = self.resolve_size(style.as_ref(), "width", containing_width, source);
        let css_height = self.resolve_size(
            style.as_ref(),
            "height",
            self.options.viewport.height,
            source,
        );
        let replaced_width = self
            .replaced_size(source, css_width, css_height)
            .map(|size| size.width);
        let width = css_width.or(replaced_width).map_or_else(
            || {
                self.atomic_inline_intrinsic_width(node_id)
                    .min(containing_width)
            },
            |width| match (css_width.is_some(), box_sizing) {
                (true, BoxSizing::BorderBox) => (width - padding - border).max(0.0),
                _ => width,
            },
        );
        self.apply_min_max_width(
            style.as_ref(),
            width,
            containing_width,
            source,
            padding + border,
            box_sizing,
        )
    }

    pub(super) fn replaced_size(
        &self,
        source: Option<NodeId>,
        css_width: Option<f32>,
        css_height: Option<f32>,
    ) -> Option<PhysicalSize> {
        let source = source?;
        let Some(NodeKind::Element(element)) = self.dom.node(source).map(Node::kind) else {
            return None;
        };
        if !matches!(element.local_name.as_str(), "img" | "video") {
            return None;
        }
        let html_width = self.html_image_dimension(source, "width");
        let html_height = self.html_image_dimension(source, "height");
        let intrinsic = self
            .images
            .and_then(|images| images.get_for_node(source))
            .map(|loaded| {
                let (width, height) = loaded.image.intrinsic_size();
                (
                    image_dimension_to_f32(width),
                    image_dimension_to_f32(height),
                )
            });
        let ratio = intrinsic
            .filter(|(_, height)| *height > 0.0)
            .map(|(width, height)| width / height)
            .or_else(|| {
                html_width
                    .zip(html_height)
                    .filter(|(_, height)| *height > 0.0)
                    .map(|(width, height)| width / height)
            });
        let width = css_width
            .or(html_width)
            .or_else(|| {
                css_height
                    .or(html_height)
                    .zip(ratio)
                    .map(|(height, ratio)| height * ratio)
            })
            .or_else(|| intrinsic.map(|(width, _)| width))
            .unwrap_or(300.0);
        let height = css_height
            .or(html_height)
            .or_else(|| {
                ratio
                    .filter(|ratio| *ratio > 0.0)
                    .map(|ratio| width / ratio)
            })
            .or_else(|| intrinsic.map(|(_, height)| height))
            .unwrap_or(150.0);
        Some(PhysicalSize {
            width: width.max(0.0),
            height: height.max(0.0),
        })
    }

    pub(super) fn html_image_dimension(&self, source: NodeId, name: &str) -> Option<f32> {
        self.dom
            .attribute(source, name)
            .ok()
            .flatten()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .map(image_dimension_to_f32)
    }

    pub(super) fn atomic_inline_intrinsic_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let children = self
            .formatting
            .get(node_id)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        children
            .into_iter()
            .map(|child| self.max_content_width(child))
            .fold(0.0_f32, f32::max)
    }

    pub(super) fn max_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let Some(node) = self.formatting.get(node_id).cloned() else {
            return 0.0;
        };
        if let FormattingNodeKind::Text(text) = &node.kind {
            let style = self.text_style(node.style_source);
            return self.intrinsic_text_width(text, node.style_source, style);
        }
        if matches!(node.kind, FormattingNodeKind::AtomicInline { .. }) {
            return self.atomic_outer_max_content_width(node_id);
        }
        let inline_sequence = matches!(
            node.kind,
            FormattingNodeKind::AnonymousBlock | FormattingNodeKind::Inline
        );
        let widths = node
            .children
            .into_iter()
            .map(|child| self.max_content_width(child));
        if inline_sequence {
            widths.sum()
        } else {
            widths.fold(0.0_f32, f32::max)
        }
    }

    pub(super) fn atomic_outer_max_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let node = self.formatting.get(node_id).cloned();
        let source = node.as_ref().and_then(|node| node.source);
        let style = node
            .as_ref()
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source))
            .cloned();
        let basis = self.options.viewport.width;
        let margin = self.resolve_edge(style.as_ref(), "margin-left", basis, source)
            + self.resolve_edge(style.as_ref(), "margin-right", basis, source);
        let padding = self.resolve_edge(style.as_ref(), "padding-left", basis, source)
            + self.resolve_edge(style.as_ref(), "padding-right", basis, source);
        let border = self.resolve_border(style.as_ref(), "border-left-width", basis, source)
            + self.resolve_border(style.as_ref(), "border-right-width", basis, source);
        let non_content = padding + border;
        let box_sizing = match style.as_ref().and_then(|style| style.typed("box-sizing")) {
            Some(TypedPropertyValue::BoxSizing(value)) => *value,
            _ => BoxSizing::ContentBox,
        };
        let specified = self.resolve_size(style.as_ref(), "width", basis, source);
        let content_width = specified.map_or_else(
            || {
                node.map_or(0.0, |node| {
                    node.children
                        .into_iter()
                        .map(|child| self.max_content_width(child))
                        .fold(0.0_f32, f32::max)
                })
            },
            |width| match box_sizing {
                BoxSizing::ContentBox => width,
                BoxSizing::BorderBox => (width - non_content).max(0.0),
            },
        );
        self.apply_min_max_width(
            style.as_ref(),
            content_width,
            basis,
            source,
            non_content,
            box_sizing,
        ) + non_content
            + margin
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_character(
        &mut self,
        run: &mut Option<TextRun>,
        fragments: &mut Vec<FragmentId>,
        atom: InlineAtom,
        character: char,
        x: f32,
        y: f32,
        width: f32,
        style: TextStyle,
    ) {
        if run.as_ref().is_some_and(|run| {
            run.formatting_node != atom.formatting_node
                || (run.y - y).abs() > f32::EPSILON
                || run.style != style
        }) {
            self.flush_text_run(run, fragments);
        }
        let run = run.get_or_insert_with(|| TextRun {
            formatting_node: atom.formatting_node,
            source: atom.source,
            text: String::new(),
            x,
            y,
            width: 0.0,
            style,
        });
        run.text.push(character);
        run.width += width;
    }

    pub(super) fn flush_text_run(
        &mut self,
        run: &mut Option<TextRun>,
        fragments: &mut Vec<FragmentId>,
    ) {
        let Some(run) = run.take() else {
            return;
        };
        let metrics = self.text_measurer.measure(&run.text, run.style);
        let rect = PhysicalRect::new(run.x, run.y, run.width, run.style.line_height);
        if let Some(fragment) = self.allocate_fragment(
            run.formatting_node,
            run.source,
            rect,
            FragmentKind::Text(TextFragmentData {
                text: run.text,
                baseline: run.y + metrics.ascent,
                font_size: run.style.font_size,
            }),
        ) {
            fragments.push(fragment);
        }
    }
}
