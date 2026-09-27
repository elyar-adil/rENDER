//! Deterministic reference layout for block and inline formatting contexts.

use crate::fragment::FragmentId;
use crate::fragment::FragmentKind;
use crate::fragment::TextFragmentData;
use crate::geometry::PhysicalRect;
use crate::geometry::PhysicalSize;
use crate::solver::FloatArea;
use crate::solver::InlineAtom;
use crate::solver::LayoutDiagnostic;
use crate::solver::LayoutDiagnosticCode;
use crate::solver::PreviousUnit;
use crate::solver::Solver;
use crate::solver::TextRun;
use crate::solver::TextSpacing;
use crate::solver::TextStyle;
use crate::solver::block::inline_float_band;
use crate::solver::is_word_separator;
use crate::solver::resolve::count_as_f32;
use crate::tree::FormattingNodeId;
use crate::tree::FormattingNodeKind;
use render_css::computed::ComputedStyle;
use render_css::properties::AlignItems;
use render_css::properties::BoxSizing;
use render_css::properties::Float;
use render_css::properties::JustifyContent;
use render_css::properties::Overflow;
use render_css::properties::TextAlign;
use render_css::properties::TypedPropertyValue;
use render_dom::Node;
use render_dom::NodeId;
use render_dom::NodeKind;

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
    render_css::properties::absolute_font_size_keyword(&value, basis)
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

/// The computed length of a CSS Text 3 §7 spacing value.
///
/// `letter-spacing` and `word-spacing` both compute to "an absolute length",
/// so what reaches layout is the specified length with `em` and `rem` still to
/// resolve against the element's own font size. A unitless zero is a valid
/// `<length>`; any other unitless number is not one, and the initial value
/// `normal` computes to zero.
fn text_spacing_length(value: &str, font_size: f32) -> f32 {
    let value = value.trim();
    if value.eq_ignore_ascii_case("normal") {
        return 0.0;
    }
    if value.parse::<f32>().is_ok_and(|number| number == 0.0) {
        return 0.0;
    }
    parse_text_length(&value.to_ascii_lowercase(), font_size)
        .filter(|length| length.is_finite())
        .unwrap_or(0.0)
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

/// The character CSS Overflow 3 §3.1 substitutes for clipped inline text.
const ELLIPSIS: char = '\u{2026}';

/// Everything text measurement needs about one inline box's typography: what
/// the measurer resolves glyph advances from, plus the CSS Text 3 §7 spacing
/// that changes the run's total advance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct InlineTextStyle {
    pub style: TextStyle,
    pub spacing: TextSpacing,
}

/// The block container a run of inline content belongs to, and whether its
/// first line counts as the first formatted line of the parent.
#[derive(Clone, Copy, Debug)]
pub(super) struct InlineTextContextSource {
    pub style_source: Option<NodeId>,
    pub is_first_child: bool,
}

/// The block container's own text properties that shape its inline content.
#[derive(Clone, Copy, Debug)]
pub(super) struct InlineTextContext {
    /// CSS Text 3 §8.1 `text-indent`.
    pub indent: FirstLineIndent,
    /// CSS Overflow 3 §3.1 `text-overflow: ellipsis`.
    pub ellipsis: bool,
}

/// CSS Text 3 §8.1 `text-indent`: a length or percentage plus the two
/// keywords that change which lines it applies to.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FirstLineIndent {
    /// Positive indents the start edge; negative hangs into the margin.
    pub length: f32,
    /// §8.1 `each-line`: also indent every line after a forced line break.
    pub each_line: bool,
    /// §8.1 `hanging`: invert which lines are affected, so the first line
    /// hangs out and the rest are indented.
    pub hanging: bool,
}

impl FirstLineIndent {
    /// The indent one line box receives, which is zero unless that line is
    /// the first formatted line of the block or `each-line`/`hanging` say so.
    fn for_line(self, line: usize, after_forced_break: bool) -> f32 {
        let first = line == 0 || (self.each_line && after_forced_break);
        match (self.hanging, first) {
            (false, true) | (true, false) => self.length,
            (false, false) | (true, true) => 0.0,
        }
    }
}

/// The available inline band for a line, narrowed by `text-indent`.
///
/// CSS Text 3 §8.1: "The indent is treated as a margin applied to the start
/// edge of the line box", so it moves where the line may begin; the end edge
/// stays the content edge, which is what makes the room left for the line
/// `content width - indent` without the band being shortened twice. `indent`
/// is zero on every line §8.1 does not affect, which makes this the single
/// place a line band is computed.
fn indented_line_band(
    floats: &[FloatArea],
    containing: PhysicalRect,
    line_y: &mut f32,
    line_height: f32,
    min_width: f32,
    indent: f32,
) -> (f32, f32) {
    let (left, right) = inline_float_band(floats, containing, line_y, line_height, min_width);
    (left + indent, right)
}

/// The typography one inline atom contributes to measurement.
fn inline_typography(atom: &InlineAtom) -> InlineTextStyle {
    InlineTextStyle {
        style: atom.style,
        spacing: atom.spacing,
    }
}

/// The CSS Text 3 §7 spacing inserted before a typographic character unit
/// whose own advance is `advance`.
///
/// §7.2 puts half of a unit's tracking on each side, so the gap between two
/// units is the average of their two values, and nothing is inserted at the
/// beginning or end of a line. That is why `previous` is part of the
/// measurement: the same character advances differently at the start of a
/// line than in the middle of one. A consecutive run of atomic inlines is a
/// single unit, so no gap goes inside one. §7.1 then adds the extra word
/// advance to a separator on top of the tracking it already receives as a
/// character unit in its own right.
fn spacing_before(
    character: char,
    style: InlineTextStyle,
    advance: f32,
    atomic: bool,
    previous: Option<&PreviousUnit>,
) -> f32 {
    let mut spacing = 0.0;
    if is_word_separator(character) && advance > 0.0 {
        // §7.1: a word separator that has no advance of its own opens no extra
        // space. None of the separators `is_word_separator` knows is
        // zero-advance in the reference measurer, so this agrees with
        // `TextSpacing::extra_advance`, which cannot see advances.
        spacing += style.spacing.word_spacing;
    }
    if let Some(previous) = previous
        && !(previous.atomic && atomic)
    {
        spacing += f32::midpoint(
            previous.typography.spacing.letter_spacing,
            style.spacing.letter_spacing,
        );
    }
    spacing
}

/// CSS Overflow 3 §3.1: `text-overflow: ellipsis` "only applies to blocks with
/// overflow other than visible", so the property needs a clipping box to mean
/// anything. The horizontal axis is the one a line box overflows in a
/// horizontal writing mode.
fn clips_inline_axis(style: Option<&ComputedStyle>) -> bool {
    matches!(
        style.and_then(|style| style.typed("overflow-x")),
        Some(TypedPropertyValue::Overflow(value)) if !matches!(value, Overflow::Visible)
    )
}

impl Solver<'_> {
    /// The block container's own text properties, resolved once per inline
    /// formatting context.
    pub(super) fn inline_text_context(
        &mut self,
        source: InlineTextContextSource,
        inline_size: f32,
    ) -> InlineTextContext {
        let style = source
            .style_source
            .and_then(|style_source| self.styles.get(&style_source))
            .cloned();
        // `text-overflow` has no entry in the property registry, so its
        // declaration arrives as raw token text. That is also why it is absent
        // from a descendant's computed style when it is not declared there:
        // §3.1 makes it a property of the block whose own content overflows,
        // so a child must not inherit it.
        let wants_ellipsis = style
            .as_ref()
            .and_then(|style| style.get("text-overflow"))
            .is_some_and(|value| value.css_text().trim().eq_ignore_ascii_case("ellipsis"));
        InlineTextContext {
            indent: self.text_indent(style.as_ref(), source, inline_size),
            ellipsis: wants_ellipsis && clips_inline_axis(style.as_ref()),
        }
    }

    /// CSS Text 3 §8.1 `text-indent`, read off the block container.
    ///
    /// §8.1 indents "only lines that are the first formatted line of an
    /// element", so an inline formatting context that is not its parent's
    /// first child is not indented at all. The computed value is a
    /// length-percentage plus the `hanging` and `each-line` keywords, and
    /// percentages refer to "the block container's own logical width", so the
    /// two components need different bases.
    fn text_indent(
        &mut self,
        style: Option<&ComputedStyle>,
        source: InlineTextContextSource,
        inline_size: f32,
    ) -> FirstLineIndent {
        if !source.is_first_child {
            return FirstLineIndent::default();
        }
        let Some(value) = style
            .and_then(|style| style.get("text-indent"))
            .map(|value| value.css_text().to_owned())
        else {
            return FirstLineIndent::default();
        };
        let mut components = value.split_whitespace();
        let length = components.next().unwrap_or_default().to_ascii_lowercase();
        let font_size = source
            .style_source
            .and_then(|style_source| self.styles.get(&style_source))
            .and_then(|style| style.get("font-size"))
            .and_then(|value| parse_font_size(value.css_text(), self.options.root_font_size))
            .unwrap_or(self.options.root_font_size);
        let resolved = if length.ends_with('%') {
            length
                .trim_end_matches('%')
                .trim()
                .parse::<f32>()
                .ok()
                .map(|percentage| percentage * inline_size / 100.0)
        } else {
            parse_text_length(&length, font_size)
        };
        let Some(resolved) = resolved else {
            self.diagnostics.push(LayoutDiagnostic {
                node: source.style_source,
                code: LayoutDiagnosticCode::UnresolvedUsedValue,
                message: format!("could not resolve 'text-indent': {value}"),
            });
            return FirstLineIndent::default();
        };
        let mut indent = FirstLineIndent {
            length: resolved,
            each_line: false,
            hanging: false,
        };
        for keyword in components {
            match keyword.to_ascii_lowercase().as_str() {
                "each-line" => indent.each_line = true,
                "hanging" => indent.hanging = true,
                other => self.diagnostics.push(LayoutDiagnostic {
                    node: source.style_source,
                    code: LayoutDiagnosticCode::UnresolvedUsedValue,
                    message: format!("could not resolve 'text-indent' keyword: {other}"),
                }),
            }
        }
        indent
    }
}

impl Solver<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn layout_inline_content(
        &mut self,
        roots: &[FormattingNodeId],
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        source: InlineTextContextSource,
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

        let text = self.inline_text_context(source, containing.size.width);
        let default_style = InlineTextStyle {
            style: TextStyle {
                font_size: self.options.root_font_size,
                line_height: self.options.default_line_height,
            },
            spacing: TextSpacing::default(),
        };
        let first_style = atoms.first().map_or(default_style, inline_typography);
        let mut fragments = Vec::new();
        let mut line_y = containing.origin.y;
        // CSS Text 3 §8.1: the indent is a margin on the start edge of the
        // first formatted line, so it moves where the line may start and the
        // room left for the line shrinks by the same amount. `line_indent` is
        // the indent of the line being built and is zero on every line §8.1
        // does not affect.
        let mut line_index = 0_usize;
        let mut after_forced_break = false;
        let mut line_indent = text.indent.for_line(line_index, after_forced_break);
        let (mut line_left, mut line_right) = indented_line_band(
            floats,
            containing,
            &mut line_y,
            first_style.style.line_height,
            0.0,
            line_indent,
        );
        let mut line_x = line_left;
        let mut current_line_height = first_style.style.line_height;
        let mut pending_space: Option<InlineAtom> = None;
        let mut current_run: Option<TextRun> = None;
        // The typographic unit before the one being placed, which CSS Text 3
        // §7.2 needs because it inserts tracking between units.
        let mut previous_unit: Option<PreviousUnit> = None;
        let mut cursor = 0;
        // Set when `text-overflow: ellipsis` has replaced the clipped text of
        // the line being built with one glyph.
        let mut ellipsized = false;

        while cursor < atoms.len() {
            let atom = atoms[cursor];
            if atom.forced_break {
                self.flush_text_run(&mut current_run, &mut fragments);
                line_y += current_line_height;
                line_index += 1;
                after_forced_break = true;
                current_line_height = atom.style.line_height;
                line_indent = text.indent.for_line(line_index, after_forced_break);
                (line_left, line_right) = indented_line_band(
                    floats,
                    containing,
                    &mut line_y,
                    current_line_height,
                    0.0,
                    line_indent,
                );
                line_x = line_left;
                pending_space = None;
                previous_unit = None;
                cursor += 1;
                continue;
            }
            if let Some(atomic) = atom.atomic {
                self.flush_text_run(&mut current_run, &mut fragments);
                if let Some(space) = pending_space.take()
                    && line_x > line_left
                {
                    let width = self.measure_inline_unit(
                        ' ',
                        inline_typography(&space),
                        false,
                        previous_unit.as_ref(),
                    );
                    self.push_character(
                        &mut current_run,
                        &mut fragments,
                        space,
                        ' ',
                        line_x,
                        line_y,
                        width,
                    );
                    self.flush_text_run(&mut current_run, &mut fragments);
                    line_x += width;
                    previous_unit = Some(PreviousUnit {
                        typography: inline_typography(&space),
                        atomic: false,
                    });
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
                        (line_left, line_right) = indented_line_band(
                            floats,
                            containing,
                            &mut line_y,
                            current_line_height,
                            outer.size.width,
                            line_indent,
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
                        line_index += 1;
                        after_forced_break = false;
                        current_line_height = atom.style.line_height;
                        line_indent = text.indent.for_line(line_index, after_forced_break);
                        (line_left, line_right) = indented_line_band(
                            floats,
                            containing,
                            &mut line_y,
                            current_line_height,
                            0.0,
                            line_indent,
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
                    // §7.2: a consecutive run of atomic inlines is a single
                    // typographic character unit, so the next unit is spaced
                    // from it like any other.
                    previous_unit = Some(PreviousUnit {
                        typography: inline_typography(&atom),
                        atomic: true,
                    });
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
            let segment_width = self.measure_inline_segment(segment, previous_unit.as_ref());
            if (line_x - line_left).abs() < f32::EPSILON && segment_width > line_right - line_left {
                (line_left, line_right) = indented_line_band(
                    floats,
                    containing,
                    &mut line_y,
                    current_line_height,
                    segment_width,
                    line_indent,
                );
                line_x = line_left;
            }
            let space_width = pending_space
                .as_ref()
                .filter(|_| line_x > line_left)
                .map_or(0.0, |space| {
                    self.measure_inline_unit(
                        ' ',
                        inline_typography(space),
                        false,
                        previous_unit.as_ref(),
                    )
                });
            if segment.first().is_some_and(|atom| atom.wrap_allowed)
                && line_x > line_left
                && line_x + space_width + segment_width > line_right
            {
                self.flush_text_run(&mut current_run, &mut fragments);
                line_y += current_line_height;
                line_index += 1;
                after_forced_break = false;
                current_line_height = segment[0].style.line_height;
                line_indent = text.indent.for_line(line_index, after_forced_break);
                (line_left, line_right) = indented_line_band(
                    floats,
                    containing,
                    &mut line_y,
                    current_line_height,
                    0.0,
                    line_indent,
                );
                line_x = line_left;
                pending_space = None;
                previous_unit = None;
            }

            if let Some(space) = pending_space.take()
                && line_x > line_left
            {
                let width = self.measure_inline_unit(
                    ' ',
                    inline_typography(&space),
                    false,
                    previous_unit.as_ref(),
                );
                self.push_character(
                    &mut current_run,
                    &mut fragments,
                    space,
                    ' ',
                    line_x,
                    line_y,
                    width,
                );
                line_x += width;
                previous_unit = Some(PreviousUnit {
                    typography: inline_typography(&space),
                    atomic: false,
                });
            }

            for atom in segment {
                // CSS Text 3 §7.2 inserts the gap between two units and nothing
                // at the start of a line, so the wrap decision comes first: it
                // decides what this unit's spacing even is. Measuring before the
                // decision would leave a wrapped character carrying the tracking
                // of the line it just left.
                let typography = inline_typography(atom);
                let bare = self.measure_inline_character(atom.character, atom.style);
                let extra = spacing_before(
                    atom.character,
                    typography,
                    bare,
                    false,
                    previous_unit.as_ref(),
                );
                // CSS Overflow 3 §3.1: room for the ellipsis is reserved out of
                // the text rather than added to it, so the character that would
                // collide with it is the one that gets dropped.
                let ellipsis = if text.ellipsis {
                    Some(self.measure_inline_unit(
                        ELLIPSIS,
                        typography,
                        false,
                        previous_unit.as_ref(),
                    ))
                } else {
                    None
                };
                let limit = ellipsis.map_or(line_right, |ellipsis| {
                    (line_right - ellipsis).max(line_left)
                });
                let overflows = line_x + bare + extra > limit;
                let wraps = atom.wrap_allowed && overflows && line_x > line_left;
                let width;
                if wraps {
                    self.flush_text_run(&mut current_run, &mut fragments);
                    line_y += current_line_height;
                    line_index += 1;
                    after_forced_break = false;
                    current_line_height = atom.style.line_height;
                    line_indent = text.indent.for_line(line_index, after_forced_break);
                    (line_left, line_right) = indented_line_band(
                        floats,
                        containing,
                        &mut line_y,
                        current_line_height,
                        0.0,
                        line_indent,
                    );
                    line_x = line_left;
                    width = bare.max(0.0);
                } else {
                    if let Some(ellipsis) = ellipsis
                        && overflows
                        && line_x > line_left
                    {
                        // The line cannot wrap, so it really does overflow, and
                        // the ellipsis takes the place of the clipped text. It
                        // reuses the measurement that reserved its room, so it
                        // ends exactly where the text it replaced would have.
                        self.flush_text_run(&mut current_run, &mut fragments);
                        self.push_character(
                            &mut current_run,
                            &mut fragments,
                            *atom,
                            ELLIPSIS,
                            line_x,
                            line_y,
                            ellipsis,
                        );
                        self.flush_text_run(&mut current_run, &mut fragments);
                        line_y += current_line_height;
                        line_index += 1;
                        after_forced_break = false;
                        current_line_height = atom.style.line_height;
                        line_indent = text.indent.for_line(line_index, after_forced_break);
                        (line_left, line_right) = indented_line_band(
                            floats,
                            containing,
                            &mut line_y,
                            current_line_height,
                            0.0,
                            line_indent,
                        );
                        line_x = line_left;
                        previous_unit = None;
                        ellipsized = true;
                        break;
                    }
                    width = (bare + extra).max(0.0);
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
                );
                line_x += width;
                previous_unit = Some(PreviousUnit {
                    typography,
                    atomic: false,
                });
            }
            if ellipsized {
                // Whatever is left on this line is clipped. A forced break ends
                // the line for real, so resume after it rather than dropping the
                // rest of the block: a `nowrap` block with a `<br>` keeps the
                // lines that follow the clipped one.
                pending_space = None;
                cursor = atoms[cursor..]
                    .iter()
                    .position(|atom| atom.forced_break)
                    .map_or(atoms.len(), |offset| cursor + offset + 1);
            } else {
                cursor = segment_end;
            }
        }
        self.flush_text_run(&mut current_run, &mut fragments);
        let trailing_line_height = if ends_with_forced_break {
            0.0
        } else {
            current_line_height
        };
        let height = line_y - containing.origin.y + trailing_line_height;
        self.align_inline_fragments(&fragments, containing, self.text_align(source.style_source));
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

    pub(super) fn measure_inline_segment(
        &self,
        segment: &[InlineAtom],
        previous: Option<&PreviousUnit>,
    ) -> f32 {
        let mut previous = previous.copied();
        let mut width = 0.0;
        for atom in segment {
            width += self.measure_inline_unit(
                atom.character,
                inline_typography(atom),
                atom.atomic.is_some(),
                previous.as_ref(),
            );
            previous = Some(PreviousUnit {
                typography: inline_typography(atom),
                atomic: atom.atomic.is_some(),
            });
        }
        width
    }

    pub(super) fn measure_inline_character(&self, character: char, style: TextStyle) -> f32 {
        let mut encoded = [0_u8; 4];
        self.text_measurer
            .measure(character.encode_utf8(&mut encoded), style)
            .advance
    }

    /// The advance of one typographic character unit, including the CSS Text
    /// 3 §7 spacing that precedes it.
    pub(super) fn measure_inline_unit(
        &self,
        character: char,
        style: InlineTextStyle,
        atomic: bool,
        previous: Option<&PreviousUnit>,
    ) -> f32 {
        let advance = self.measure_inline_character(character, style.style);
        let spacing = spacing_before(character, style, advance, atomic, previous);
        (advance + spacing).max(0.0)
    }

    pub(super) fn intrinsic_text_width(
        &self,
        text: &str,
        source: Option<NodeId>,
        style: InlineTextStyle,
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
            return self
                .text_measurer
                .measure_spaced(text, style.style, style.spacing)
                .advance;
        }
        let mut width = 0.0;
        let mut pending_space = false;
        let mut has_content = false;
        let mut previous: Option<PreviousUnit> = None;
        for character in text.chars() {
            if character.is_whitespace() {
                pending_space |= has_content;
                continue;
            }
            if pending_space {
                // One collapsed separator stands for the whole run of white
                // space, and it is a character unit like any other, so
                // tracking goes before it too.
                width += self.measure_inline_unit(' ', style, false, previous.as_ref());
                previous = Some(PreviousUnit {
                    typography: style,
                    atomic: false,
                });
                pending_space = false;
            }
            width += self.measure_inline_unit(character, style, false, previous.as_ref());
            previous = Some(PreviousUnit {
                typography: style,
                atomic: false,
            });
            has_content = true;
        }
        width
    }

    /// The typography text measurement needs for `source`: the font inputs the
    /// measurer resolves advances from, plus the CSS Text 3 §7 spacing that
    /// changes the total.
    pub(super) fn inline_text_style(&mut self, source: Option<NodeId>) -> InlineTextStyle {
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
        InlineTextStyle {
            style: TextStyle {
                font_size,
                line_height,
            },
            spacing: self.text_spacing(computed, source, font_size),
        }
    }

    /// CSS Text 3 §7.1/§7.2 `word-spacing` and `letter-spacing`.
    ///
    /// Both compute to an absolute length, so what reaches layout is still the
    /// specified length and an `em` has to be resolved against the element's
    /// own computed font size. A value this cannot read is the initial value -
    /// no additional spacing - which is the conservative reading, and the one
    /// that keeps a malformed declaration from moving text.
    fn text_spacing(
        &mut self,
        computed: Option<&ComputedStyle>,
        source: Option<NodeId>,
        font_size: f32,
    ) -> TextSpacing {
        let length = |property: &str| {
            computed
                .and_then(|style| style.get(property))
                .map_or(0.0, |value| {
                    text_spacing_length(value.css_text(), font_size)
                })
        };
        let spacing = TextSpacing {
            letter_spacing: length("letter-spacing"),
            word_spacing: length("word-spacing"),
        };
        for (property, value) in [
            ("letter-spacing", spacing.letter_spacing),
            ("word-spacing", spacing.word_spacing),
        ] {
            if !value.is_finite() {
                self.diagnostics.push(LayoutDiagnostic {
                    node: source,
                    code: LayoutDiagnosticCode::UnresolvedUsedValue,
                    message: format!("could not resolve '{property}': not a finite length"),
                });
            }
        }
        spacing
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
            let typography = self.inline_text_style(node.style_source);
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
                    style: typography.style,
                    spacing: typography.spacing,
                });
            }
            return;
        }
        let typography = self.inline_text_style(node.style_source);
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
                style: typography.style,
                spacing: typography.spacing,
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
                style: typography.style,
                spacing: typography.spacing,
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
            .and_then(|images| images.intrinsic_size_for_node(source))
            .map(|(width, height)| {
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
        let mut widest = 0.0_f32;
        let mut float_run = 0.0_f32;
        for child in children {
            let child_float = self.float_side(child);
            let child_width = self.max_content_width(child);
            if std::env::var_os("RENDER_DEBUG_INTRINSIC").is_some() {
                eprintln!("  child={child:?} float={child_float:?} max={child_width}");
            }
            if child_float == Float::None {
                // Whitespace between inline/floating children is represented
                // by empty anonymous blocks. It does not terminate a run of
                // adjacent floats; doing so makes every link in a real
                // navigation bar start on a new line.
                if child_width > f32::EPSILON {
                    float_run = 0.0;
                    widest = widest.max(child_width);
                }
            } else {
                // Adjacent floats share a line in a shrink-to-fit box. The
                // preferred width is their combined outer width, including
                // margins, rather than the width of only the widest float.
                float_run += self.atomic_outer_max_content_width(child);
                widest = widest.max(float_run);
            }
        }
        if std::env::var_os("RENDER_DEBUG_INTRINSIC").is_some() {
            eprintln!("intrinsic float node={node_id:?} width={widest}");
        }
        widest
    }

    pub(super) fn max_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let Some(node) = self.formatting.get(node_id).cloned() else {
            return 0.0;
        };
        if let FormattingNodeKind::Text(text) = &node.kind {
            let style = self.inline_text_style(node.style_source);
            return self.intrinsic_text_width(text, node.style_source, style);
        }
        if matches!(node.kind, FormattingNodeKind::AtomicInline { .. }) {
            return self.atomic_outer_max_content_width(node_id);
        }
        if let Some(width) = self.table_max_content_width(node_id) {
            return width;
        }
        if matches!(
            node.kind,
            FormattingNodeKind::AnonymousBlock | FormattingNodeKind::Inline
        ) {
            return self.inline_sequence_max_content_width(&node.children, 0);
        }
        node.children
            .into_iter()
            .map(|child| self.max_content_width(child))
            .fold(0.0_f32, f32::max)
    }

    /// CSS 2.1 §17.5.2.2: a table's max-content width is the sum of its column
    /// widths, not the widest of its rows, so it needs the table algorithm
    /// rather than the generic block measurement. Returns `None` for a box that
    /// is not a table.
    pub(super) fn table_max_content_width(&mut self, node_id: FormattingNodeId) -> Option<f32> {
        if !self.is_table(node_id) {
            return None;
        }
        let source = self
            .formatting
            .get(node_id)
            .and_then(|node| node.style_source)?;
        let style = self.styles.get(&source);
        Some(self.table_intrinsic_widths(node_id, style, self.options.viewport.width, false))
    }

    /// CSS 2.1 §10.3.5: the narrowest width a box can take without overflowing
    /// is the widest of its unbreakable runs. A soft wrap opportunity splits a
    /// text run at every word and a forced break ends a line outright, so
    /// neither raises the minimum above the widest single word.
    pub(super) fn min_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let Some(node) = self.formatting.get(node_id).cloned() else {
            return 0.0;
        };
        if let FormattingNodeKind::Text(text) = &node.kind {
            let style = self.inline_text_style(node.style_source);
            return text
                .split_whitespace()
                .map(|word| {
                    self.text_measurer
                        .measure_spaced(word, style.style, style.spacing)
                        .advance
                })
                .fold(0.0_f32, f32::max);
        }
        if matches!(node.kind, FormattingNodeKind::AtomicInline { .. }) {
            // A replaced box cannot be broken, so its minimum is its used
            // outer width.
            return self.atomic_outer_max_content_width(node_id);
        }
        if self.is_table(node_id) {
            // §17.5.2.2: a table's min-content width is the sum of its column
            // widths, not the widest of its rows.
            let style = self
                .formatting
                .get(node_id)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            return self.table_intrinsic_widths(node_id, style, self.options.viewport.width, true);
        }
        node.children
            .into_iter()
            .map(|child| self.min_content_width(child))
            .fold(0.0_f32, f32::max)
    }

    /// CSS 2.1 §10.3.5 / CSS Overflow 3 §3.1: the max-content width of an
    /// inline sequence is the width of its widest line, not the sum of all of
    /// its content, because a forced break (`<br>`) ends the line.
    pub(super) fn inline_sequence_max_content_width(
        &mut self,
        children: &[FormattingNodeId],
        depth: usize,
    ) -> f32 {
        if depth > self.options.limits.max_depth {
            return 0.0;
        }
        let mut widest: f32 = 0.0;
        let mut line: f32 = 0.0;
        for child in children.iter().copied() {
            if self.is_forced_break(child) {
                widest = widest.max(line);
                line = 0.0;
                continue;
            }
            line += self.max_content_width(child);
        }
        widest.max(line)
    }

    /// A `<br>` forces a line break, which every inline intrinsic-width
    /// measurement has to account for.
    pub(super) fn is_forced_break(&self, node_id: FormattingNodeId) -> bool {
        self.formatting
            .get(node_id)
            .and_then(|node| node.source)
            .is_some_and(|source| {
                matches!(
                    self.dom.node(source).map(Node::kind),
                    Some(NodeKind::Element(data)) if data.local_name == "br"
                )
            })
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
    ) {
        let typography = inline_typography(&atom);
        if run.as_ref().is_some_and(|run| {
            run.formatting_node != atom.formatting_node
                || (run.y - y).abs() > f32::EPSILON
                || run.typography != typography
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
            typography,
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
        let metrics = self.text_measurer.measure_spaced(
            &run.text,
            run.typography.style,
            run.typography.spacing,
        );
        let rect = PhysicalRect::new(run.x, run.y, run.width, run.typography.style.line_height);
        if let Some(fragment) = self.allocate_fragment(
            run.formatting_node,
            run.source,
            rect,
            FragmentKind::Text(TextFragmentData {
                text: run.text,
                baseline: run.y + metrics.ascent,
                font_size: run.typography.style.font_size,
            }),
        ) {
            fragments.push(fragment);
        }
    }
}
