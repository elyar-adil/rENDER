//! Deterministic reference layout for block and inline formatting contexts.

use crate::font::{
    FontRequest, FontStyle, computed_font_style, computed_font_synthesis, computed_font_weight,
};
use crate::fragment::FragmentId;
use crate::fragment::FragmentKind;
use crate::fragment::StoredFontRequest;
use crate::fragment::TextFragmentData;
use crate::geometry::PhysicalRect;
use crate::geometry::PhysicalSize;
use crate::linebreak::Break;
use crate::linebreak::LineBreakOptions;
use crate::linebreak::LineBreakStrictness;
use crate::linebreak::WordBreak;
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
use render_css::computed::ComputedValue;
use render_css::properties::AlignItems;
use render_css::properties::BoxSizing;
use render_css::properties::Float;
use render_css::properties::JustifyContent;
use render_css::properties::LineBreak;
use render_css::properties::Overflow;
use render_css::properties::TextAlign;
use render_css::properties::TypedPropertyValue;
use render_css::properties::WordBreak as WordBreakProperty;
use render_dom::Node;
use render_dom::NodeId;
use render_dom::NodeKind;
use std::collections::BTreeMap;

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
            || next.break_before
        {
            break;
        }
        end += 1;
    }
    end
}

/// The soft wrap opportunities of a whole inline sequence, as one flag per atom.
///
/// CSS Text 3 §1.5: "For the purpose of determining adjacency for text
/// processing (such as ... line-breaking ...), intervening inline box boundaries
/// and out-of-flow elements must be ignored." So the opportunity before a
/// character depends on the character before it even when the two are in
/// different elements, which is why this runs over the atom sequence rather than
/// over each text node.
///
/// The `<br>` and atomic-inline placeholders stay in the sequence so that the
/// indices line up; their own boundaries are decided by the surrounding rules
/// anyway, and the two `is_some` arms of [`inline_segment_end`] are what the
/// solver uses for them.
pub(super) fn inline_break_opportunities(atoms: &[InlineAtom]) -> Vec<bool> {
    let characters: Vec<char> = atoms.iter().map(|atom| atom.character).collect();
    crate::linebreak::opportunities_with(&characters, |index| atoms[index].line_breaking)
        .into_iter()
        .map(Break::is_break)
        .collect()
}

/// The character CSS Overflow 3 §3.1 substitutes for clipped inline text.
const ELLIPSIS: char = '\u{2026}';

/// Everything text measurement needs about one inline box's typography: what
/// the measurer resolves glyph advances from, plus the CSS Text 3 §7 spacing
/// that changes the run's total advance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct InlineTextStyle<'a> {
    pub style: TextStyle<'a>,
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
fn inline_typography<'a>(atom: &InlineAtom<'a>) -> InlineTextStyle<'a> {
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
    style: InlineTextStyle<'_>,
    advance: f32,
    atomic: bool,
    previous: Option<&PreviousUnit<'_>>,
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

// The lifetime cannot be elided: `inline_text_style` returns a `TextStyle` that
// borrows the computed style, so the impl needs to name the solver's lifetime to
// say so.
#[allow(
    clippy::elidable_lifetime_names,
    reason = "the methods below return values borrowing the solver's lifetime"
)]
impl<'a> Solver<'a> {
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
        let mut atoms = self.collect_inline_content_atoms(roots, depth);
        if atoms.is_empty()
            || atoms.iter().all(|atom| {
                atom.atomic.is_none() && !atom.forced_break && atom.character.is_whitespace()
            })
        {
            return (Vec::new(), 0.0);
        }
        // The UAX #14 opportunities for the whole sequence, which is what CSS
        // Text 3 §1.5 asks for: adjacency ignores the inline box boundaries
        // between the atoms. `break_before` is a field rather than a separate
        // array so that the line filler below reads the opportunity and the
        // character from one place.
        let opportunities = inline_break_opportunities(&atoms);
        for (atom, break_before) in atoms.iter_mut().zip(opportunities) {
            atom.break_before = break_before;
        }
        let atoms = atoms;
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
                font: FontRequest::initial(),
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
            // CSS 2.1 §9.4.2 and CSS Text 3 §5: when a line cannot take the
            // whole of an unbreakable run it overflows rather than breaking it.
            // The run here is `segment`, which ends at the first break
            // opportunity, so it is unbreakable exactly when nothing inside it
            // is one. A run that *does* contain an opportunity is left to the
            // character loop below, which breaks at the last candidate that
            // fits - which is what kinsoku requires, because a forbidden line
            // start simply is not a candidate and the break moves on to the
            // next one.
            //
            // A `white-space` that forbids wrapping is not a wrapping
            // opportunity at all (§3: `pre` and `nowrap` "do not allow
            // wrapping"), so it must not reach this at all: the run overflows
            // rather than starting a new line.
            let breakable_inside = atoms[cursor + 1..segment_end]
                .iter()
                .any(|atom| atom.break_before);
            if segment[0].line_breaking.wrap
                && !breakable_inside
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
                let wraps = atom.break_before && overflows && line_x > line_left;
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

    /// The three text properties that decide where a line may break, read off
    /// the element the text belongs to.
    ///
    /// * `white-space` is CSS Text 3 §3. `pre` and `nowrap` "do not allow
    ///   wrapping"; `normal`, `pre-line`, `pre-wrap` and `break-spaces` do.
    /// * `word-break` is §5.1 and `line-break` is §5.2; both are read as the
    ///   registered typed value, and both inherit, so a descendant of a
    ///   `word-break: keep-all` ancestor keeps it without a declaration.
    ///
    /// The three are read together because §5 and §5.1 do not treat them
    /// independently: §5.1's `keep-all` is stated to hold "regardless of
    /// line-break settings other than anywhere", and §5.2's `anywhere`
    /// disregards what `word-break` mandates. Returning one value keeps that
    /// relationship in one place instead of in the line filler.
    pub(super) fn line_break_options(&self, source: Option<NodeId>) -> LineBreakOptions {
        let Some(style) = source.and_then(|source| self.styles.get(&source)) else {
            return LineBreakOptions::default();
        };
        let word_break = match style.typed("word-break") {
            Some(TypedPropertyValue::WordBreak(WordBreakProperty::KeepAll)) => WordBreak::KeepAll,
            Some(TypedPropertyValue::WordBreak(WordBreakProperty::BreakAll)) => WordBreak::BreakAll,
            Some(TypedPropertyValue::WordBreak(WordBreakProperty::BreakWord)) => {
                WordBreak::BreakWord
            }
            // §5.1's initial value, and the value of a `word-break` this
            // document declared that the grammar rejected: the computed stage
            // falls back to the initial value in that case.
            Some(TypedPropertyValue::WordBreak(WordBreakProperty::Normal)) | None => {
                WordBreak::Normal
            }
            _ => WordBreak::Normal,
        };
        let line_break = match style.typed("line-break") {
            Some(TypedPropertyValue::LineBreak(LineBreak::Loose)) => LineBreakStrictness::Loose,
            Some(TypedPropertyValue::LineBreak(LineBreak::Strict)) => LineBreakStrictness::Strict,
            Some(TypedPropertyValue::LineBreak(LineBreak::Anywhere)) => {
                LineBreakStrictness::Anywhere
            }
            // §5.2's initial value is `auto`, and `auto` is resolved here rather
            // than in the line breaker because §5.2 says the UA "may vary the
            // restrictions based on the length of the line" and this engine has
            // no length-dependent tailoring, so `auto` is the common set.
            Some(TypedPropertyValue::LineBreak(LineBreak::Normal)) | None => {
                LineBreakStrictness::Auto
            }
            _ => LineBreakStrictness::Auto,
        };
        // §3: `pre` and `nowrap` are the two values whose "Text Wrapping" cell
        // in §3's informative table reads "No wrap". A `white-space` the
        // document did not declare, or one the grammar rejected, is the initial
        // value `normal`, which wraps.
        let wrap = style
            .get("white-space")
            .is_none_or(|value| !matches!(value.css_text().trim(), "pre" | "nowrap"));
        LineBreakOptions {
            word_break,
            line_break,
            wrap,
        }
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
    ) -> Vec<InlineAtom<'a>> {
        let mut atoms = Vec::new();
        for root in roots {
            self.collect_inline_atoms(*root, &mut atoms, depth);
        }
        atoms
    }

    pub(super) fn measure_inline_segment(
        &self,
        segment: &[InlineAtom<'a>],
        previous: Option<&PreviousUnit<'a>>,
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

    pub(super) fn measure_inline_character(&self, character: char, style: TextStyle<'_>) -> f32 {
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
        style: InlineTextStyle<'a>,
        atomic: bool,
        previous: Option<&PreviousUnit<'a>>,
    ) -> f32 {
        let advance = self.measure_inline_character(character, style.style);
        let spacing = spacing_before(character, style, advance, atomic, previous);
        (advance + spacing).max(0.0)
    }

    pub(super) fn intrinsic_text_width(
        &self,
        text: &str,
        source: Option<NodeId>,
        style: InlineTextStyle<'a>,
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
        let mut previous: Option<PreviousUnit<'a>> = None;
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
    pub(super) fn inline_text_style(&mut self, source: Option<NodeId>) -> InlineTextStyle<'a> {
        // Copied out of `self` so the resulting borrows carry `'a` rather than
        // the `&mut self` this method holds: the style outlives the call, and
        // `text_spacing` below still needs the receiver mutably.
        let styles: &'a BTreeMap<NodeId, ComputedStyle> = self.styles;
        let computed = source.and_then(|source| styles.get(&source));
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
                font: self.font_request(computed, source),
            },
            spacing: self.text_spacing(computed, source, font_size),
        }
    }

    /// The CSS Fonts 4 font request for `source`.
    ///
    /// §2.1.1 makes `font-family`, §2.2 `font-weight`, §2.4 `font-style` and
    /// §2.8 the `font-synthesis` longhands all inherited, and the cascade
    /// resolves none of them into the value this needs: `font-family` keeps the
    /// author's list, `font-weight` keeps the `bolder`/`lighter` keyword that
    /// §2.2 defines relative to the parent's weight, and `font-synthesis` is
    /// not registered at all. So all four are read from the computed values
    /// here, in the same place and from the same `ComputedStyle` as
    /// `font-size` and `line-height`, which is what keeps measurement and paint
    /// from disagreeing about which face a run belongs to.
    fn font_request(
        &self,
        computed: Option<&'a ComputedStyle>,
        source: Option<NodeId>,
    ) -> FontRequest<'a> {
        let declared = |property: &str| {
            computed
                .and_then(|style| style.get(property))
                .map(ComputedValue::css_text)
        };
        let family = match declared("font-family") {
            Some(family) if !family.trim().is_empty() => family,
            // §2.1's initial value is user-agent defined; an element that
            // declared none, and one whose declaration is only whitespace, both
            // arrive at the same request.
            _ => FontRequest::INITIAL_FAMILY,
        };
        let weight = declared("font-weight")
            .map(|value| (value, self.inherited_font_weight(source)))
            .and_then(|(value, inherited)| computed_font_weight(value, inherited))
            .unwrap_or(400);
        let style = declared("font-style")
            .and_then(computed_font_style)
            .unwrap_or(FontStyle::Normal);
        let synthesis = computed_font_synthesis(
            declared("font-synthesis-weight"),
            declared("font-synthesis-style"),
        );
        FontRequest {
            family,
            weight,
            style,
            synthesis,
        }
    }

    /// §2.2.1's relative weights are defined against "the inherited
    /// font-weight value", so `bolder` has to be resolved against the nearest
    /// ancestor element that declares one. A chain with no declaration anywhere
    /// resolves at the initial 400, which is what `font-weight: normal`
    /// computes to.
    fn inherited_font_weight(&self, source: Option<NodeId>) -> u16 {
        let styles: &'a BTreeMap<NodeId, ComputedStyle> = self.styles;
        let mut node = source.and_then(|source| self.dom.parent(source));
        while let Some(current) = node {
            let inherited = styles
                .get(&current)
                .and_then(|style| style.get("font-weight"))
                .and_then(|value| computed_font_weight(value.css_text(), 400));
            if let Some(inherited) = inherited {
                return inherited;
            }
            node = self.dom.parent(current);
        }
        400
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
        atoms: &mut Vec<InlineAtom<'a>>,
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
            let line_breaking = self.line_break_options(node.style_source);
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
                    break_before: false,
                    atomic: None,
                    line_breaking,
                    style: typography.style,
                    spacing: typography.spacing,
                });
            }
            return;
        }
        let typography = self.inline_text_style(node.style_source);
        let line_breaking = self.line_break_options(node.style_source);
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
                break_before: false,
                atomic: None,
                line_breaking,
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
                break_before: true,
                atomic: Some(node_id),
                line_breaking,
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
            || self.shrink_to_fit_width(node_id, containing_width),
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

    /// CSS 2.1 §10.3.5: a float's used width is the shrink-to-fit width,
    /// `min(max(preferred minimum width, available width), preferred width)`.
    /// The min-content floor is the "preferred minimum width" - §10.3.5's "width
    /// found by trying all possible line breaks" - and it is not merely a tie
    /// between the two ends of the clamp: without it a shrink-to-fit box
    /// narrower than its own min-content comes out narrower than any line it
    /// could hold, and its text wraps inside itself instead of the box growing
    /// to fit the longest word.
    ///
    /// §10.3.9 makes an `inline-block`'s `width: auto` "the shrink-to-fit width
    /// as for floating elements", so the same formula answers for both, and this
    /// is where they are answered.
    fn shrink_to_fit_width(&mut self, node_id: FormattingNodeId, available: f32) -> f32 {
        let preferred = self.atomic_inline_intrinsic_width(node_id);
        let preferred_minimum = self.atomic_inline_min_content_width(node_id);
        f32::min(preferred, f32::max(preferred_minimum, available))
    }

    /// The "preferred minimum width" of an `inline-block`, as a content width:
    /// the same walk as [`Self::atomic_inline_intrinsic_width`] with each
    /// in-flow child measured by its min-content width instead of its
    /// max-content one.
    pub(super) fn atomic_inline_min_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let children = self
            .formatting
            .get(node_id)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        let mut widest = 0.0_f32;
        let mut float_run = 0.0_f32;
        for child in children {
            let child_float = self.float_side(child);
            let child_width = self.min_content_width(child);
            if child_float == Float::None {
                if child_width > f32::EPSILON {
                    float_run = 0.0;
                    widest = widest.max(child_width);
                }
            } else {
                float_run += self.atomic_outer_intrinsic_width(child, true);
                widest = widest.max(float_run);
            }
        }
        widest
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
    /// is the width of the widest content that cannot be broken, which the
    /// specification describes as the width "found by trying all possible line
    /// breaks". That is [`crate::linebreak::widest_unbreakable_run`] over the
    /// text's own soft wrap opportunities, so a run that has none - a Latin word,
    /// or a whole paragraph under `white-space: nowrap` - measures as itself and
    /// an unspaced run measures per character, because UAX #14 gives it an
    /// opportunity between most of its characters.
    ///
    /// §10.3.5's preferred *width* is the other half of the same pair, and
    /// [`Self::max_content_width`] is it: a line with no break taken at all, so
    /// the two only differ where there is an opportunity to take.
    pub(super) fn min_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        self.min_content_width_at(node_id, 0)
    }

    fn min_content_width_at(&mut self, node_id: FormattingNodeId, depth: usize) -> f32 {
        if depth > self.options.limits.max_depth {
            return 0.0;
        }
        let Some(node) = self.formatting.get(node_id).cloned() else {
            return 0.0;
        };
        if let FormattingNodeKind::Text(text) = &node.kind {
            let style = self.inline_text_style(node.style_source);
            let options = self.line_break_options(node.style_source);
            let spacing = style.spacing;
            let measurer = self.text_measurer;
            return crate::linebreak::widest_unbreakable_run(
                &text.chars().collect::<Vec<char>>(),
                options,
                |run| measurer.measure_spaced(run, style.style, spacing).advance,
            );
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
        if matches!(
            node.kind,
            FormattingNodeKind::AnonymousBlock | FormattingNodeKind::Inline
        ) {
            // §10.3.5's "preferred minimum width" is a property of the whole
            // sequence, not of its widest child: two adjacent inline boxes with
            // no break opportunity between them form one unbreakable run, and a
            // fold over the children would measure each of them alone. That is
            // invisible on a single text node and visible on
            // `<p>aaa<b>bbb</b>ccc</p>`, where UAX #14 forbids the break before
            // the `<b>` and the answer is all nine characters wide.
            return self.inline_sequence_min_content_width(&node.children, depth);
        }
        node.children
            .into_iter()
            .map(|child| self.min_content_width_at(child, depth.saturating_add(1)))
            .fold(0.0_f32, f32::max)
    }

    /// The atoms an inline sequence contributes to an intrinsic width
    /// measurement.
    ///
    /// [`Self::collect_inline_atoms`] cannot be reused here: it charges the
    /// layout pass's inline character budget and reports a diagnostic once that
    /// budget is spent, and an intrinsic measurement must do neither. Layout
    /// runs the same measurement again afterwards, so spending the budget here
    /// would make a document that merely *fits* report a limit it never hit.
    fn collect_measurement_atoms(
        &mut self,
        node_id: FormattingNodeId,
        atoms: &mut Vec<InlineAtom<'a>>,
        depth: usize,
    ) {
        if depth > self.options.limits.max_depth {
            return;
        }
        let Some(node) = self.formatting.get(node_id).cloned() else {
            return;
        };
        let typography = self.inline_text_style(node.style_source);
        let line_breaking = self.line_break_options(node.style_source);
        if let FormattingNodeKind::Text(text) = node.kind {
            for character in text.chars() {
                atoms.push(InlineAtom {
                    formatting_node: node_id,
                    source: node.source,
                    character,
                    forced_break: false,
                    break_before: false,
                    atomic: None,
                    line_breaking,
                    style: typography.style,
                    spacing: typography.spacing,
                });
            }
            return;
        }
        if self.is_forced_break(node_id) {
            atoms.push(InlineAtom {
                formatting_node: node_id,
                source: node.source,
                character: '\n',
                forced_break: true,
                break_before: false,
                atomic: None,
                line_breaking,
                style: typography.style,
                spacing: typography.spacing,
            });
            return;
        }
        if matches!(node.kind, FormattingNodeKind::AtomicInline { .. }) {
            atoms.push(InlineAtom {
                formatting_node: node_id,
                source: node.source,
                // The placeholder character takes the atomic box's own width,
                // so its class never decides an opportunity that matters: an
                // atomic inline ends every run it is in and starts the next.
                character: '\0',
                forced_break: false,
                break_before: true,
                atomic: Some(node_id),
                line_breaking,
                style: typography.style,
                spacing: typography.spacing,
            });
            return;
        }
        for child in node.children {
            self.collect_measurement_atoms(child, atoms, depth.saturating_add(1));
        }
    }

    /// The width of the widest unbreakable run of an inline sequence, which is
    /// §10.3.5's "preferred minimum width" of that sequence.
    pub(super) fn inline_sequence_min_content_width(
        &mut self,
        children: &[FormattingNodeId],
        depth: usize,
    ) -> f32 {
        if depth > self.options.limits.max_depth {
            return 0.0;
        }
        let mut atoms: Vec<InlineAtom<'a>> = Vec::new();
        for child in children.iter().copied() {
            self.collect_measurement_atoms(child, &mut atoms, depth.saturating_add(1));
        }
        self.atoms_min_content_width(&atoms)
    }

    /// The width of the widest run of `atoms` that no soft wrap opportunity can
    /// split.
    ///
    /// §10.3.5 describes the preferred minimum width as what "trying all
    /// possible line breaks" leaves behind, which is exactly this: the runs
    /// between consecutive opportunities, plus a forced break and an atomic
    /// inline boundary, both of which end a run whatever UAX #14 says.
    fn atoms_min_content_width(&mut self, atoms: &[InlineAtom<'a>]) -> f32 {
        if atoms.is_empty() {
            return 0.0;
        }
        let opportunities = inline_break_opportunities(atoms);
        let mut widest = 0.0_f32;
        let mut start = 0_usize;
        for at in 1..atoms.len() {
            if opportunities[at]
                || atoms[at].forced_break
                || atoms[at].atomic.is_some()
                || atoms[at - 1].atomic.is_some()
            {
                widest = widest.max(self.unbreakable_run_width(atoms, start, at));
                start = at;
            }
        }
        widest.max(self.unbreakable_run_width(atoms, start, atoms.len()))
    }

    /// The width of `atoms[start..end]`, with its trailing white space hanging.
    fn unbreakable_run_width(&mut self, atoms: &[InlineAtom<'a>], start: usize, end: usize) -> f32 {
        // CSS Text 3 §3: "end-of-line spaces hang", so the white space that
        // follows the last opportunity is not part of the run it ends. A forced
        // break ends the run too and occupies none of it.
        let mut trimmed = end;
        while trimmed > start
            && (atoms[trimmed - 1].forced_break
                || (atoms[trimmed - 1].atomic.is_none()
                    && atoms[trimmed - 1].character.is_whitespace()))
        {
            trimmed -= 1;
        }
        let mut width = 0.0_f32;
        let mut previous: Option<PreviousUnit<'a>> = None;
        for atom in &atoms[start..trimmed] {
            let typography = inline_typography(atom);
            let atomic = atom.atomic;
            width += match atomic {
                // §7.2 treats a consecutive run of atomic inlines as a single
                // typographic character unit, so nothing goes inside one.
                Some(node) => self.atomic_outer_max_content_width(node),
                None => self.measure_inline_unit(atom.character, typography, false, previous.as_ref()),
            };
            previous = Some(PreviousUnit {
                typography,
                atomic: atomic.is_some(),
            });
        }
        width
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
        self.atomic_outer_intrinsic_width(node_id, false)
    }

    /// The outer (margin box) max-content width of a replaced or atomic box,
    /// or its min-content width when `minimum` is set.
    ///
    /// §10.3.5's two halves of a shrink-to-fit width share every step of this
    /// measurement except the one that picks the intrinsic width of the content,
    /// so the flag is the only thing that differs.
    pub(super) fn atomic_outer_intrinsic_width(
        &mut self,
        node_id: FormattingNodeId,
        minimum: bool,
    ) -> f32 {
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
        let children = node.map(|node| node.children).unwrap_or_default();
        let content_width = specified.map_or_else(
            || {
                children
                    .into_iter()
                    .map(|child| {
                        if minimum {
                            self.min_content_width_at(child, 0)
                        } else {
                            self.max_content_width(child)
                        }
                    })
                    .fold(0.0_f32, f32::max)
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
        run: &mut Option<TextRun<'a>>,
        fragments: &mut Vec<FragmentId>,
        atom: InlineAtom<'a>,
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
        run: &mut Option<TextRun<'a>>,
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
                font: StoredFontRequest::new(run.typography.style.font),
            }),
        ) {
            fragments.push(fragment);
        }
    }
}
