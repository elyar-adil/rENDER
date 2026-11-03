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
use crate::solver::InlineBidiPlacement;
use crate::solver::LayoutDiagnostic;
use crate::solver::LayoutDiagnosticCode;
use crate::solver::PreviousUnit;
use crate::solver::Solver;
use crate::solver::TextRun;
use crate::solver::TextSpacing;
use crate::solver::TextStyle;
use crate::solver::block::content_visibility_hidden;
use crate::solver::block::has_inline_size_containment;
use crate::solver::block::inline_float_band;
use crate::solver::block::overflow_axes;
use crate::solver::is_word_separator;
use crate::solver::resolve::count_as_f32;
use crate::tree::FormattingContextKind;
use crate::tree::FormattingNodeId;
use crate::tree::FormattingNodeKind;
use render_css::computed::ComputedStyle;
use render_css::computed::ComputedValue;
use render_css::length::{LengthContext, resolve_length_expr};
use render_css::properties::AlignItems;
use render_css::properties::BoxSizing;
use render_css::properties::Direction;
use render_css::properties::Display;
use render_css::properties::DisplayOutside;
use render_css::properties::Float;
use render_css::properties::JustifyContent;
use render_css::properties::LineBreak;
use render_css::properties::Overflow;
use render_css::properties::TextAlign;
use render_css::properties::TypedPropertyValue;
use render_css::properties::VerticalAlign;
use render_css::properties::WordBreak as WordBreakProperty;
use render_dom::Namespace;
use render_dom::Node;
use render_dom::NodeId;
use render_dom::NodeKind;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use unicode_bidi::BidiInfo;
use unicode_bidi::Level;

/// Whether the intrinsic-sizing trace is switched on.
///
/// `RENDER_DEBUG_INTRINSIC` is one of the project's **documented observability
/// seams**, not scaffolding: `PROGRESS.md`'s "Debug switches" table lists it as
/// verified live and says to prefer it over adding a new print statement, so
/// [`Solver::atomic_inline_intrinsic_width`]'s per-child trace belongs here and
/// stays. It is read through a `OnceLock` because that function runs once per
/// child of every shrink-to-fit box in the document and `std::env::var_os`
/// allocates and takes the environment lock on each call - a real cost on the
/// sizing path even when the switch is off, which is the state it is in for
/// every page that is not being debugged.
fn intrinsic_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RENDER_DEBUG_INTRINSIC").is_some())
}

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

/// The length-resolution context for `text-indent` and the text spacing
/// properties.
///
/// `parse_text_length` has one basis because its other callers resolve a
/// font-relative value whose root and element sizes are the same. `text-indent`
/// and the text spacing properties are element-relative lengths on inherited
/// properties, so an `em` follows the element's computed font size while a
/// `rem` follows the root font size. Passing the viewport through also makes
/// the standard viewport units resolve the same way they do for box lengths.
fn text_length_context(
    font_size: f32,
    percentage_base: Option<f32>,
    root_font_size: f32,
    viewport: PhysicalSize,
) -> LengthContext {
    LengthContext {
        percentage_base: percentage_base.map(f64::from),
        em_base: f64::from(font_size),
        rem_base: f64::from(root_font_size),
        viewport_width: f64::from(viewport.width),
        viewport_height: f64::from(viewport.height),
        ..LengthContext::default()
    }
}

/// Layout geometry is `f32`, so the resolver's `f64` used value is narrowed at
/// the one boundary where it enters the solver.
#[allow(
    clippy::cast_possible_truncation,
    reason = "layout geometry is f32 and every CSS length consumer stores f32"
)]
fn length_to_f32(length: f64) -> f32 {
    length as f32
}

/// The computed length of a CSS Text 3 §7 spacing value.
///
/// `letter-spacing` and `word-spacing` both compute to "an absolute length",
/// so what reaches layout is the specified length with `em` and `rem` still to
/// resolve against the element's own font size and the root font size
/// respectively. The shared length resolver handles every unit it supports,
/// including `calc()`; a percentage is not part of this grammar, so
/// `percentage_base` is absent and a percentage resolves to the initial value.
fn text_spacing_length(value: &str, context: &LengthContext) -> f32 {
    let value = value.trim();
    if value.eq_ignore_ascii_case("normal") {
        return 0.0;
    }
    match resolve_length_expr(value, context).map(length_to_f32) {
        Ok(length) if length.is_finite() => length,
        _ => 0.0,
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

/// The used value of CSS 2.1 §10.8.1 `vertical-align` for an inline-level box.
///
/// §10.8.1 has two families of values. `baseline`, `middle`, `text-top`,
/// `text-bottom` and a `<length>`/`<percentage>` shift all position the box by
/// its own baseline and therefore contribute an ascent and a descent to the
/// line box. `top` and `bottom` pin the box to the line box itself, so they
/// contribute their height instead. Resolving the `<length>`/`<percentage>`
/// form at collection time turns every value into one of a few primitive
/// shifts the alignment pass can apply without re-reading the cascade.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum InlineVerticalAlign {
    #[default]
    Baseline,
    Middle,
    Top,
    Bottom,
    TextTop,
    TextBottom,
    /// Raise the box's baseline by this many pixels. Negative values lower
    /// it. The `sub` and `super` keywords are UA-defined shifts; they resolve
    /// to this form so the line pass has one representation.
    Offset(f32),
}

/// The alignment requested by an inline-level box and the parent metrics its
/// keyword needs: the parent's content-area ascent/descent for `text-top` and
/// `text-bottom`, and the parent's x-height for `middle`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct VerticalAlignContext {
    pub align: InlineVerticalAlign,
    pub parent_ascent: f32,
    pub parent_descent: f32,
    pub parent_x_height: f32,
}

/// One inline-level box on a line, captured before §10.8.1 alignment moves
/// it. `baseline` is measured from `top`, the box's current top edge, and
/// `height` is the box's inline-level height: its `line-height` for a text run
/// or its margin box for an atomic inline.
#[derive(Clone, Copy, Debug)]
pub(super) struct InlineLineItem {
    fragment: FragmentId,
    top: f32,
    baseline: f32,
    height: f32,
    /// Whether this item is a text run rather than an atomic inline box.
    text: bool,
    vertical_align: VerticalAlignContext,
}

/// The ascent and descent of a line's strut, relative to its baseline.
#[derive(Clone, Copy, Debug)]
struct LineStrut {
    ascent: f32,
    descent: f32,
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
    rtl: bool,
) -> (f32, f32) {
    let (left, right) = inline_float_band(floats, containing, line_y, line_height, min_width);
    if rtl {
        // `text-indent` is a margin on the *start* edge (CSS Text 3 §8.1),
        // which is the right edge in an RTL paragraph.
        (left, (right - indent).max(left))
    } else {
        (left + indent, right)
    }
}

/// The paragraph base direction from the block container's `direction`.
fn inline_direction(style: Option<&ComputedStyle>) -> bool {
    matches!(
        style.and_then(|style| style.typed("direction")),
        Some(TypedPropertyValue::Direction(Direction::Rtl))
    )
}

/// Assign every atom the embedding level UAX #9 resolves for it.
///
/// The text is built in logical order: an atomic inline contributes U+FFFC
/// (OBJECT REPLACEMENT CHARACTER), which is what a replaced box is in the
/// bidirectional algorithm, and a forced break contributes a paragraph
/// separator. The CSS `direction` value is the paragraph embedding level even
/// when the first strong character disagrees (CSS Writing Modes 3 §2.1).
fn assign_bidi_levels(atoms: &mut [InlineAtom], base_rtl: bool) {
    // The overwhelmingly common case is an LTR paragraph whose text is ASCII.
    // It cannot contain a right-to-left character, so every level is zero and
    // running the full algorithm would only allocate per-byte vectors for
    // every paragraph on every page.
    if !base_rtl
        && atoms
            .iter()
            .all(|atom| atom.character.is_ascii() || atom.forced_break || atom.atomic.is_some())
    {
        for atom in atoms.iter_mut() {
            atom.bidi_level = 0;
        }
        return;
    }
    let mut text = String::with_capacity(atoms.len());
    let mut offsets = Vec::with_capacity(atoms.len());
    for atom in atoms.iter() {
        offsets.push(text.len());
        if atom.forced_break {
            text.push('\u{2028}');
        } else if atom.atomic.is_some() {
            text.push('\u{FFFC}');
        } else {
            text.push(atom.character);
        }
    }
    let base = if base_rtl { Level::rtl() } else { Level::ltr() };
    let info = BidiInfo::new(&text, Some(base));
    for (atom, offset) in atoms.iter_mut().zip(offsets) {
        atom.bidi_level = info.levels.get(offset).copied().unwrap_or(base).number();
    }
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
        // CSS Text 3 §8.1's grammar is
        // `[ <length-percentage> ] && [ hanging || each-line ]?`, and `&&`
        // does not prescribe an order, so the length may follow either
        // keyword. A `calc()` expression contains whitespace, so the
        // non-keyword components are put back together and resolved as one
        // length expression rather than each token being treated as a length.
        let mut length = String::new();
        let mut has_length = false;
        let mut each_line = false;
        let mut hanging = false;
        for component in value.split_whitespace() {
            match component.to_ascii_lowercase().as_str() {
                "each-line" => each_line = true,
                "hanging" => hanging = true,
                _ => {
                    if has_length {
                        length.push(' ');
                    }
                    length.push_str(component);
                    has_length = true;
                }
            }
        }
        if !has_length {
            self.diagnostics.push(LayoutDiagnostic {
                node: source.style_source,
                code: LayoutDiagnosticCode::UnresolvedUsedValue,
                message: format!("could not resolve 'text-indent': {value}"),
            });
            return FirstLineIndent::default();
        }
        let font_size = source
            .style_source
            .and_then(|style_source| self.styles.get(&style_source))
            .and_then(|style| style.get("font-size"))
            .and_then(|value| parse_font_size(value.css_text(), self.options.root_font_size))
            .unwrap_or(self.options.root_font_size);
        let context = text_length_context(
            font_size,
            Some(inline_size),
            self.options.root_font_size,
            self.options.viewport,
        );
        let resolved = match resolve_length_expr(&length, &context) {
            Ok(length) => {
                let length = length_to_f32(length);
                if !length.is_finite() {
                    self.diagnostics.push(LayoutDiagnostic {
                        node: source.style_source,
                        code: LayoutDiagnosticCode::UnresolvedUsedValue,
                        message: format!("could not resolve 'text-indent': {value}: not finite"),
                    });
                    return FirstLineIndent::default();
                }
                length
            }
            Err(error) => {
                self.diagnostics.push(LayoutDiagnostic {
                    node: source.style_source,
                    code: LayoutDiagnosticCode::UnresolvedUsedValue,
                    message: format!("could not resolve 'text-indent': {value}: {error}"),
                });
                return FirstLineIndent::default();
            }
        };
        FirstLineIndent {
            length: resolved,
            each_line,
            hanging,
        }
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
    #[allow(
        clippy::too_many_arguments,
        reason = "inline layout carries the paragraph geometry, positioning geometry and float band"
    )]
    #[allow(clippy::too_many_lines)]
    pub(super) fn layout_inline_content(
        &mut self,
        roots: &[FormattingNodeId],
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        positioning_containing_rtl: bool,
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
        let rtl = inline_direction(
            source
                .style_source
                .and_then(|source| self.styles.get(&source)),
        );
        assign_bidi_levels(&mut atoms, rtl);
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
        // CSS 2.1 §9.4.2: a line box exists only for in-flow inline content.
        // An out-of-flow box keeps an atom so it can be laid out at its static
        // position, but by itself it must not create the line box whose height
        // an auto-height ancestor would otherwise absorb.
        let has_in_flow_line_content = atoms.iter().any(|atom| match atom.atomic {
            Some(node) => !self.is_out_of_flow(node),
            None => atom.forced_break || !atom.character.is_whitespace(),
        });
        let ends_with_forced_break = atoms
            .iter()
            .rev()
            // An out-of-flow placeholder after the break is not line content,
            // so it must not hide the forced break that ended the last line.
            .filter(|atom| !atom.atomic.is_some_and(|node| self.is_out_of_flow(node)))
            .find(|atom| atom.forced_break || !atom.character.is_whitespace())
            .is_some_and(|atom| atom.forced_break);

        let mut text = self.inline_text_context(source, containing.size.width);
        let default_style = InlineTextStyle {
            style: TextStyle {
                font_size: self.options.root_font_size,
                line_height: self.options.default_line_height,
                font: FontRequest::initial(),
            },
            spacing: TextSpacing::default(),
        };
        let first_style = atoms.first().map_or(default_style, inline_typography);
        let strut = self.line_strut(source.style_source);
        let mut fragments = Vec::new();
        let mut line_items: Vec<InlineLineItem> = Vec::new();
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
            rtl,
        );
        // The available inline band of each line, for the alignment pass after
        // bidi reordering. It is not always the full containing block: floats
        // narrow it and `text-indent` moves one of its edges, so alignment
        // must use the band the line was actually filled into.
        let mut line_bands = vec![(line_left, line_right)];
        let mut line_top = line_y;
        // CSS Overflow 3 §3.1: an ellipsis marks overflow, so it is drawn only
        // when the content does not fit the line box. Without this test every
        // character is checked against the room reserved for the ellipsis, and
        // text that fits exactly would be cut as if it overflowed.
        if text.ellipsis
            && self.inline_sequence_max_content_width(roots, depth) <= line_right - line_left
        {
            text.ellipsis = false;
        }
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
                self.flush_text_run(&mut current_run, &mut fragments, &mut line_items);
                line_y += self.align_inline_line(&line_items, line_top, strut, current_line_height);
                line_items.clear();
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
                    rtl,
                );
                line_top = line_y;
                line_bands.push((line_left, line_right));
                line_x = line_left;
                pending_space = None;
                previous_unit = None;
                cursor += 1;
                continue;
            }
            if let Some(atomic) = atom.atomic {
                self.flush_text_run(&mut current_run, &mut fragments, &mut line_items);
                if let Some(space) = pending_space.take()
                    && line_x > line_left
                {
                    let (width, spacing) = self.measure_inline_unit_with_spacing(
                        ' ',
                        inline_typography(&space),
                        false,
                        previous_unit.as_ref(),
                    );
                    self.push_character(
                        &mut current_run,
                        &mut fragments,
                        &mut line_items,
                        space,
                        ' ',
                        line_x,
                        line_y,
                        width,
                        spacing,
                        line_index,
                    );
                    self.flush_text_run(&mut current_run, &mut fragments, &mut line_items);
                    line_x += width;
                    previous_unit = Some(PreviousUnit {
                        typography: inline_typography(&space),
                        atomic: false,
                    });
                }
                if let Some((fragment, outer)) = self.layout_atomic_inline(
                    atomic,
                    containing,
                    rtl,
                    positioning_containing,
                    positioning_containing_rtl,
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
                            rtl,
                        );
                        line_top = line_y;
                        line_x = line_left;
                        self.translate_fragment_subtree(
                            fragment,
                            line_x - outer.origin.x,
                            line_y - outer.origin.y,
                        );
                    }
                    if line_x > line_left && line_x + outer.size.width > line_right {
                        self.flush_text_run(&mut current_run, &mut fragments, &mut line_items);
                        line_y += self.align_inline_line(
                            &line_items,
                            line_top,
                            strut,
                            current_line_height,
                        );
                        line_items.clear();
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
                            rtl,
                        );
                        line_top = line_y;
                        line_bands.push((line_left, line_right));
                        line_x = line_left;
                        self.translate_fragment_subtree(
                            fragment,
                            line_x - outer.origin.x,
                            line_y - outer.origin.y,
                        );
                    }
                    let baseline = self.atomic_inline_baseline(
                        fragment,
                        atom.source,
                        line_y,
                        outer.size.height,
                    );
                    // An inline-block with no in-flow line box has no text
                    // baseline to align. The engine keeps such a box (an empty
                    // spacer, an image-only card) at the line top, as it did
                    // before §10.8.1 alignment existed; only a box with a real
                    // baseline takes part in baseline alignment.
                    let vertical_align = if baseline.is_none()
                        && atom.vertical_align.align == InlineVerticalAlign::Baseline
                    {
                        VerticalAlignContext {
                            align: InlineVerticalAlign::Top,
                            ..atom.vertical_align
                        }
                    } else {
                        atom.vertical_align
                    };
                    line_items.push(InlineLineItem {
                        fragment,
                        top: line_y,
                        baseline: baseline.unwrap_or(0.0),
                        height: outer.size.height,
                        text: false,
                        vertical_align,
                    });
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
                    self.inline_bidi.insert(
                        fragment,
                        InlineBidiPlacement {
                            line: line_index,
                            level: atom.bidi_level,
                        },
                    );
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
                    rtl,
                );
                line_top = line_y;
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
                self.flush_text_run(&mut current_run, &mut fragments, &mut line_items);
                line_y += self.align_inline_line(&line_items, line_top, strut, current_line_height);
                line_items.clear();
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
                    rtl,
                );
                line_top = line_y;
                line_bands.push((line_left, line_right));
                line_x = line_left;
                pending_space = None;
                previous_unit = None;
            }

            if let Some(space) = pending_space.take()
                && line_x > line_left
            {
                let (width, spacing) = self.measure_inline_unit_with_spacing(
                    ' ',
                    inline_typography(&space),
                    false,
                    previous_unit.as_ref(),
                );
                self.push_character(
                    &mut current_run,
                    &mut fragments,
                    &mut line_items,
                    space,
                    ' ',
                    line_x,
                    line_y,
                    width,
                    spacing,
                    line_index,
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
                    Some(self.measure_inline_unit_with_spacing(
                        ELLIPSIS,
                        typography,
                        false,
                        previous_unit.as_ref(),
                    ))
                } else {
                    None
                };
                let limit = ellipsis.map_or(line_right, |(ellipsis, _)| {
                    (line_right - ellipsis).max(line_left)
                });
                let overflows = line_x + bare + extra > limit;
                let wraps = atom.break_before && overflows && line_x > line_left;
                let width;
                let leading_spacing;
                if wraps {
                    self.flush_text_run(&mut current_run, &mut fragments, &mut line_items);
                    line_y +=
                        self.align_inline_line(&line_items, line_top, strut, current_line_height);
                    line_items.clear();
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
                        rtl,
                    );
                    line_top = line_y;
                    line_bands.push((line_left, line_right));
                    line_x = line_left;
                    width = bare.max(0.0);
                    // §7.2 excludes tracking at the start of a line, which is
                    // exactly what the character a wrap just moved starts.
                    leading_spacing = 0.0;
                } else {
                    if let Some((ellipsis, ellipsis_spacing)) = ellipsis
                        && overflows
                        && line_x > line_left
                    {
                        // The line cannot wrap, so it really does overflow, and
                        // the ellipsis takes the place of the clipped text. It
                        // reuses the measurement that reserved its room, so it
                        // ends exactly where the text it replaced would have.
                        self.flush_text_run(&mut current_run, &mut fragments, &mut line_items);
                        self.push_character(
                            &mut current_run,
                            &mut fragments,
                            &mut line_items,
                            *atom,
                            ELLIPSIS,
                            line_x,
                            line_y,
                            ellipsis,
                            ellipsis_spacing,
                            line_index,
                        );
                        self.flush_text_run(&mut current_run, &mut fragments, &mut line_items);
                        line_y += self.align_inline_line(
                            &line_items,
                            line_top,
                            strut,
                            current_line_height,
                        );
                        line_items.clear();
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
                            rtl,
                        );
                        line_top = line_y;
                        line_bands.push((line_left, line_right));
                        line_x = line_left;
                        previous_unit = None;
                        ellipsized = true;
                        break;
                    }
                    width = (bare + extra).max(0.0);
                    leading_spacing = extra;
                }
                current_line_height = current_line_height.max(atom.style.line_height);
                self.push_character(
                    &mut current_run,
                    &mut fragments,
                    &mut line_items,
                    *atom,
                    atom.character,
                    line_x,
                    line_y,
                    width,
                    leading_spacing,
                    line_index,
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
        self.flush_text_run(&mut current_run, &mut fragments, &mut line_items);
        let trailing_line_height = if ends_with_forced_break || !has_in_flow_line_content {
            0.0
        } else {
            self.align_inline_line(&line_items, line_top, strut, current_line_height)
        };
        let height = line_y - containing.origin.y + trailing_line_height;
        let text_align = match (self.text_align(source.style_source), rtl) {
            // `start` is the right edge in RTL; `end` is the right edge in
            // LTR; and `justify` has no justification pass yet, so it falls
            // back to the paragraph's start edge.
            (TextAlign::Start | TextAlign::Justify, true) | (TextAlign::End, false) => {
                TextAlign::Right
            }
            (TextAlign::End, true) => TextAlign::Left,
            (value, _) => value,
        };
        self.finish_inline_lines(&fragments, containing, text_align, &line_bands);
        (fragments, height)
    }

    /// The metrics every line box starts with: a zero-width inline box with
    /// the block container's font and `line-height`, CSS 2.1 §10.8's "strut".
    fn line_strut(&mut self, source: Option<NodeId>) -> LineStrut {
        let style = self.inline_text_style(source);
        let metrics = self.text_measurer.measure("", style.style);
        let half_leading = (style.style.line_height - (metrics.ascent + metrics.descent)) / 2.0;
        LineStrut {
            ascent: half_leading + metrics.ascent,
            descent: half_leading + metrics.descent,
        }
    }

    /// CSS 2.1 §10.8.1: align every inline-level box on one line and return
    /// the line box height.
    ///
    /// The whole line shares one baseline. Baseline-relative values contribute
    /// an ascent and a descent around it; `top` and `bottom` are pinned to the
    /// line box edges and only contribute their height. The line box is the
    /// union of those extents, so a box taller than the line grows it (and
    /// `top`/`bottom` boxes follow the final edges). `line_top` is the first
    /// line box position in document space and is the top of the finished line
    /// box, which is what makes a line of stacked blocks start at its
    /// containing block's content edge.
    fn align_inline_line(
        &mut self,
        items: &[InlineLineItem],
        line_top: f32,
        strut: LineStrut,
        minimum_height: f32,
    ) -> f32 {
        // A line box starts with the container's strut. The engine has always
        // let a line made only of atomic inline boxes be sized by those boxes
        // themselves (their own text baselines or their bottom edges), which is
        // also what keeps an empty inline-block from growing a line by the
        // container's descender. Keep that behaviour: the strut's ascent and
        // descent join the line when a direct text run is present, and an
        // otherwise empty line still needs its line-height.
        let (mut ascent, mut descent) = if items.is_empty() || items.iter().any(|item| item.text) {
            (strut.ascent, strut.descent)
        } else {
            (0.0, 0.0)
        };
        let mut tallest_top = 0.0_f32;
        let mut tallest_bottom = 0.0_f32;
        for item in items {
            match item.vertical_align.align {
                InlineVerticalAlign::Baseline => {
                    ascent = ascent.max(item.baseline);
                    descent = descent.max(item.height - item.baseline);
                }
                InlineVerticalAlign::Middle => {
                    let half_x = item.vertical_align.parent_x_height / 2.0;
                    ascent = ascent.max(item.height / 2.0 - half_x);
                    descent = descent.max(item.height / 2.0 + half_x);
                }
                InlineVerticalAlign::TextTop => {
                    let parent_ascent = item.vertical_align.parent_ascent;
                    ascent = ascent.max(parent_ascent);
                    descent = descent.max(item.height - parent_ascent);
                }
                InlineVerticalAlign::TextBottom => {
                    let parent_descent = item.vertical_align.parent_descent;
                    descent = descent.max(parent_descent);
                    ascent = ascent.max(item.height - parent_descent);
                }
                InlineVerticalAlign::Offset(offset) => {
                    ascent = ascent.max(offset + item.baseline);
                    descent = descent.max(item.height - item.baseline - offset);
                }
                InlineVerticalAlign::Top => {
                    tallest_top = tallest_top.max(item.height);
                }
                InlineVerticalAlign::Bottom => {
                    tallest_bottom = tallest_bottom.max(item.height);
                }
            }
        }
        let line_height = (ascent + descent)
            .max(tallest_top)
            .max(tallest_bottom)
            .max(minimum_height);
        let baseline = line_top + ascent;
        for item in items {
            let desired_top = match item.vertical_align.align {
                InlineVerticalAlign::Baseline => baseline - item.baseline,
                InlineVerticalAlign::Middle => {
                    baseline + item.vertical_align.parent_x_height / 2.0 - item.height / 2.0
                }
                InlineVerticalAlign::Top => line_top,
                InlineVerticalAlign::Bottom => line_top + line_height - item.height,
                InlineVerticalAlign::TextTop => baseline - item.vertical_align.parent_ascent,
                InlineVerticalAlign::TextBottom => {
                    baseline + item.vertical_align.parent_descent - item.height
                }
                InlineVerticalAlign::Offset(offset) => baseline - offset - item.baseline,
            };
            let dy = desired_top - item.top;
            if dy.abs() > 0.01 {
                self.translate_fragment_subtree(item.fragment, 0.0, dy);
            }
        }
        line_height
    }

    /// The baseline of an atomic inline box, measured from its margin box top.
    ///
    /// CSS 2.1 §10.8.1: this is the baseline of its last line box in the
    /// normal flow, or its bottom margin edge when it has no in-flow line boxes
    /// or its `overflow` is not `visible`. Floats and absolutely positioned
    /// descendants do not generate line boxes in the normal flow, so text
    /// inside them must not become the box's baseline: a positioned badge in
    /// an inline-block used to drag the whole box down by the badge's offset.
    fn atomic_inline_baseline(
        &self,
        fragment: FragmentId,
        source: Option<NodeId>,
        top: f32,
        height: f32,
    ) -> Option<f32> {
        let style = source.and_then(|source| self.styles.get(&source));
        let (overflow_x, overflow_y) = overflow_axes(style);
        if overflow_x != Overflow::Visible || overflow_y != Overflow::Visible {
            return Some(height);
        }
        self.fragment_last_text_baseline(fragment)
            .map(|baseline| (baseline - top).max(0.0))
    }

    /// The bottom-most text baseline inside `root`'s normal flow, in document
    /// coordinates.
    fn fragment_last_text_baseline(&self, root: FragmentId) -> Option<f32> {
        let mut pending = vec![root];
        let mut baseline: Option<f32> = None;
        while let Some(id) = pending.pop() {
            let Some(fragment) = self.fragments.get(id.as_u32() as usize) else {
                continue;
            };
            if id != root
                && matches!(fragment.kind, FragmentKind::Box(_))
                && (self.source_is_out_of_flow(fragment.source)
                    || self.float_side(fragment.formatting_node) != Float::None)
            {
                continue;
            }
            if let FragmentKind::Text(text) = &fragment.kind {
                baseline =
                    Some(baseline.map_or(text.baseline, |current| current.max(text.baseline)));
            }
            pending.extend(fragment.children.iter().copied());
        }
        baseline
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

    /// Reorder each line's inline fragments with UAX #9 rule L2, then apply
    /// `text-align` to the line box.
    ///
    /// The layout above fills every line in logical order. This pass is where
    /// the paragraph's resolved levels become geometry: level runs are placed
    /// in visual order, and a run at an odd level is displayed right-to-left
    /// (its characters are reversed for the left-to-right painter). Atomic
    /// inlines take part as single units, so an `inline-block` moves with the
    /// surrounding run without its own content being reversed. Alignment is
    /// against the line's own band, not the whole containing block, because a
    /// float can leave one side of the line box shorter; in an RTL paragraph a
    /// right float's left edge is the start edge of the remaining text.
    pub(super) fn finish_inline_lines(
        &mut self,
        fragments: &[FragmentId],
        containing: PhysicalRect,
        text_align: TextAlign,
        line_bands: &[(f32, f32)],
    ) {
        let needs_work = line_bands.iter().any(|(left, right)| {
            (*left - containing.origin.x).abs() > f32::EPSILON
                || (*right - containing.right()).abs() > f32::EPSILON
        }) || matches!(
            text_align,
            TextAlign::Center | TextAlign::End | TextAlign::Right
        ) || fragments.iter().any(|fragment| {
            self.inline_bidi
                .get(fragment)
                .is_some_and(|placement| placement.level % 2 == 1)
        });
        if !needs_work {
            for fragment in fragments {
                self.inline_bidi.remove(fragment);
            }
            return;
        }
        let mut lines: BTreeMap<usize, Vec<(FragmentId, u8)>> = BTreeMap::new();
        for fragment in fragments {
            if let Some(placement) = self.inline_bidi.remove(fragment) {
                lines
                    .entry(placement.line)
                    .or_default()
                    .push((*fragment, placement.level));
            }
        }
        for (line, units) in lines {
            let Some(start_x) = units
                .iter()
                .filter_map(|(fragment, _)| self.inline_unit_rect(*fragment))
                .map(|rect| rect.origin.x)
                .reduce(f32::min)
            else {
                continue;
            };
            let levels: Vec<Level> = units.iter().map(|(_, level)| Level::from(*level)).collect();
            let order = BidiInfo::reorder_visual(&levels);
            let mut cursor_x = start_x;
            for index in order {
                let (fragment_id, level) = units[index];
                let Some(rect) = self.inline_unit_rect(fragment_id) else {
                    continue;
                };
                if level % 2 == 1
                    && let Some(fragment) = self.fragment_mut(fragment_id)
                    && let FragmentKind::Text(data) = &mut fragment.kind
                {
                    // UAX #9 L2: the run's characters are displayed from the
                    // right, so a left-to-right painter receives them in
                    // reverse. L3 (combining marks) and L4 (mirroring) are
                    // not applied yet.
                    data.text = data.text.chars().rev().collect();
                    // The tracking between the logical units is part of the
                    // run box. Reversing the run puts the gap that preceded its
                    // first logical unit after its last visual one, so the
                    // painted glyphs keep the fragment rect they were measured
                    // against.
                    std::mem::swap(&mut data.leading_spacing, &mut data.trailing_spacing);
                }
                let dx = cursor_x - rect.origin.x;
                if dx != 0.0 {
                    self.translate_fragment_subtree(fragment_id, dx, 0.0);
                }
                cursor_x += rect.size.width;
            }
            let line_width = cursor_x - start_x;
            let (band_left, band_right) = line_bands
                .get(line)
                .copied()
                .unwrap_or((containing.origin.x, containing.right()));
            let free = (band_right - band_left - line_width).max(0.0);
            let target_left = match text_align {
                TextAlign::Start | TextAlign::Left | TextAlign::Justify => band_left,
                TextAlign::End | TextAlign::Right => band_left + free,
                TextAlign::Center => band_left + free / 2.0,
            };
            let offset = target_left - start_x;
            if offset != 0.0 {
                for (fragment, _) in &units {
                    self.translate_fragment_subtree(*fragment, offset, 0.0);
                }
            }
        }
    }

    fn inline_unit_rect(&self, id: FragmentId) -> Option<PhysicalRect> {
        let fragment = self.fragments.get(usize::try_from(id.as_u32()).ok()?)?;
        match fragment.kind {
            FragmentKind::Box(_) => self.fragment_outer_rect(id),
            FragmentKind::Text(_) => Some(fragment.rect),
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
        self.measure_inline_unit_with_spacing(character, style, atomic, previous)
            .0
    }

    /// The advance of one typographic character unit and the part of it that is
    /// CSS Text 3 §7 spacing rather than the glyph's own advance.
    ///
    /// Layout has to keep the two apart because the painter places each glyph
    /// inside the character box: the box starts where the previous unit's
    /// advance ended, while the glyph starts after this unit's leading gap.
    /// Returning the pair lets every caller that pushes a character give
    /// [`Self::push_character`] the gap without measuring the character twice.
    pub(super) fn measure_inline_unit_with_spacing(
        &self,
        character: char,
        style: InlineTextStyle<'a>,
        atomic: bool,
        previous: Option<&PreviousUnit<'a>>,
    ) -> (f32, f32) {
        let advance = self.measure_inline_character(character, style.style);
        let spacing = spacing_before(character, style, advance, atomic, previous);
        ((advance + spacing).max(0.0), spacing)
    }

    /// Whether `white-space` preserves the spaces of `source` rather than
    /// collapsing runs of them (CSS Text 3 §3).
    fn preserves_whitespace(&self, source: Option<NodeId>) -> bool {
        source
            .and_then(|source| self.styles.get(&source))
            .and_then(|style| style.get("white-space"))
            .is_some_and(|value| {
                matches!(
                    value.css_text().to_ascii_lowercase().as_str(),
                    "pre" | "pre-wrap" | "break-spaces"
                )
            })
    }

    pub(super) fn intrinsic_text_width(
        &self,
        text: &str,
        source: Option<NodeId>,
        style: InlineTextStyle<'a>,
    ) -> f32 {
        let preserves_whitespace = self.preserves_whitespace(source);
        if preserves_whitespace {
            return self
                .text_measurer
                .measure_spaced(text, style.style, style.spacing)
                .advance;
        }
        // With no letter or word spacing the total is the plain sum of the
        // run's advances. Collapse the white space exactly as the per-character
        // path below does and measure the result in one call: the measurer
        // accumulates advances left to right, so this is the same f32 sum it
        // would produce one character at a time, without a measurement call per
        // character. Spacing still takes the per-character path because §7
        // tracking depends on the neighbours of each unit.
        if style.spacing.letter_spacing == 0.0 && style.spacing.word_spacing == 0.0 {
            let mut collapsed = String::with_capacity(text.len());
            let mut pending_space = false;
            let mut has_content = false;
            for character in text.chars() {
                if character.is_whitespace() {
                    pending_space |= has_content;
                    continue;
                }
                if pending_space {
                    collapsed.push(' ');
                    pending_space = false;
                }
                collapsed.push(character);
                has_content = true;
            }
            if !has_content {
                return 0.0;
            }
            return self
                .text_measurer
                .measure_spaced(&collapsed, style.style, style.spacing)
                .advance;
        }
        let mut width = 0.0;
        if std::env::var_os("RENDER_DEBUG_TEXT").is_some() {
            eprintln!(
                "intrinsic_text text={text:?} size={} width_before={width}",
                style.style.font_size
            );
        }
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
        if std::env::var_os("RENDER_DEBUG_TEXT").is_some() {
            eprintln!("intrinsic_text done text={text:?} width={width}");
        }
        width
    }

    /// The typography text measurement needs for `source`: the font inputs the
    /// measurer resolves advances from, plus the CSS Text 3 §7 spacing that
    /// changes the total.
    ///
    /// The answer depends only on the computed style of `source`, which is
    /// immutable during a layout pass, so one resolution is reused by every
    /// atom, intrinsic measurement and line in the subtree. Diagnostics are
    /// replayed on a hit so a repeated call reports what the first one did.
    pub(super) fn inline_text_style(&mut self, source: Option<NodeId>) -> InlineTextStyle<'a> {
        if let Some((style, diagnostics)) = self.inline_text_styles.get(&source) {
            if !diagnostics.is_empty() {
                self.diagnostics.extend(diagnostics.iter().cloned());
            }
            return *style;
        }
        let (style, diagnostics) = self.compute_inline_text_style(source);
        if !diagnostics.is_empty() {
            self.diagnostics.extend(diagnostics.iter().cloned());
        }
        self.inline_text_styles.insert(source, (style, diagnostics));
        style
    }

    fn compute_inline_text_style(
        &mut self,
        source: Option<NodeId>,
    ) -> (InlineTextStyle<'a>, Vec<LayoutDiagnostic>) {
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
        let font = self.font_request(computed, source);
        let line_height = match computed.and_then(|style| style.get("line-height")) {
            Some(value) if value.css_text().trim().eq_ignore_ascii_case("normal") => {
                self.text_measurer.normal_line_height(font_size, &font)
            }
            Some(value) => {
                parse_line_height(value.css_text(), font_size).unwrap_or(font_size * 1.2)
            }
            None => self.text_measurer.normal_line_height(font_size, &font),
        }
        .clamp(font_size, 1_024.0);
        let (spacing, diagnostics) = self.text_spacing(computed, source, font_size);
        (
            InlineTextStyle {
                style: TextStyle {
                    font_size,
                    line_height,
                    font,
                },
                spacing,
            },
            diagnostics,
        )
    }

    /// The used `vertical-align` of an inline-level box and the parent metrics
    /// the baseline-relative keywords need.
    ///
    /// CSS 2.1 §10.8.1 resolves the `<length>`/`<percentage>` form against the
    /// element's own `line-height`, and `sub`/`super` are UA-defined baseline
    /// shifts (§10.8.1 says only that they are "the proper position" for
    /// subscripts and superscripts). The two here follow the long-standing
    /// browser convention of 0.2em down and 0.33em up. `middle`, `text-top`
    /// and `text-bottom` read the parent inline box's font metrics, not the
    /// line-height, because §10.8.1 names the parent's content area and
    /// x-height.
    /// Whether `source` generates an inline-level box, which is the only kind
    /// of box CSS 2.1 §10.8.1 lets `vertical-align` move.
    ///
    /// Text directly inside a block container lives in an anonymous inline
    /// box. §9.2.2.1 gives anonymous boxes the initial value of every
    /// non-inherited property, so a `vertical-align` declared on the block -
    /// including a table cell's, which §17.5.3 consumes for cell alignment -
    /// must not leak into the text's line box. Text inside a `<span>` has the
    /// span as its style source and does inherit the declaration through the
    /// span's inline box.
    fn inline_level_style(&self, source: Option<NodeId>) -> bool {
        matches!(
            source
                .and_then(|source| self.styles.get(&source))
                .and_then(|style| style.typed("display")),
            Some(TypedPropertyValue::Display(Display::Normal {
                outside: DisplayOutside::Inline,
                ..
            }))
        )
    }

    fn vertical_align_context(
        &mut self,
        source: Option<NodeId>,
        typography: InlineTextStyle<'a>,
    ) -> VerticalAlignContext {
        let vertical_align = source
            .and_then(|source| self.styles.get(&source))
            .and_then(|style| style.typed("vertical-align"))
            .cloned();
        let align = match vertical_align {
            Some(TypedPropertyValue::VerticalAlign(VerticalAlign::Middle)) => {
                InlineVerticalAlign::Middle
            }
            Some(TypedPropertyValue::VerticalAlign(VerticalAlign::Top)) => InlineVerticalAlign::Top,
            Some(TypedPropertyValue::VerticalAlign(VerticalAlign::Bottom)) => {
                InlineVerticalAlign::Bottom
            }
            Some(TypedPropertyValue::VerticalAlign(VerticalAlign::TextTop)) => {
                InlineVerticalAlign::TextTop
            }
            Some(TypedPropertyValue::VerticalAlign(VerticalAlign::TextBottom)) => {
                InlineVerticalAlign::TextBottom
            }
            Some(TypedPropertyValue::VerticalAlign(VerticalAlign::Sub)) => {
                InlineVerticalAlign::Offset(-0.2 * typography.style.font_size)
            }
            Some(TypedPropertyValue::VerticalAlign(VerticalAlign::Super)) => {
                InlineVerticalAlign::Offset(0.33 * typography.style.font_size)
            }
            Some(TypedPropertyValue::VerticalAlign(VerticalAlign::Offset(value))) => {
                InlineVerticalAlign::Offset(self.resolve_length(
                    &value,
                    typography.style.line_height,
                    source,
                    "vertical-align",
                ))
            }
            Some(TypedPropertyValue::VerticalAlign(VerticalAlign::Baseline)) | None => {
                InlineVerticalAlign::Baseline
            }
            Some(_) => InlineVerticalAlign::Baseline,
        };
        let parent_style = self.inline_text_style(self.parent_style_source(source));
        let metrics = self.text_measurer.measure("", parent_style.style);
        let parent_x_height = self
            .text_measurer
            .x_height(parent_style.style.font_size, &parent_style.style.font);
        VerticalAlignContext {
            align,
            parent_ascent: metrics.ascent,
            parent_descent: metrics.descent,
            parent_x_height,
        }
    }

    /// The nearest ancestor element whose computed style the solver has. This
    /// is an inline-level box's parent for the §10.8.1 keywords that read the
    /// parent's content area or x-height.
    fn parent_style_source(&self, source: Option<NodeId>) -> Option<NodeId> {
        let mut current = source.and_then(|source| self.dom.parent(source));
        while let Some(node) = current {
            if self.styles.contains_key(&node) {
                return Some(node);
            }
            current = self.dom.parent(node);
        }
        None
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
    /// own computed font size while a `rem` follows the root. The shared length
    /// resolver supports the same units and `calc()` expressions as every
    /// other length in layout. A value this cannot read is the initial value -
    /// no additional spacing - which is the conservative reading, and the one
    /// that keeps a malformed declaration from moving text.
    fn text_spacing(
        &self,
        computed: Option<&ComputedStyle>,
        source: Option<NodeId>,
        font_size: f32,
    ) -> (TextSpacing, Vec<LayoutDiagnostic>) {
        let mut diagnostics = Vec::new();
        let context = text_length_context(
            font_size,
            None,
            self.options.root_font_size,
            self.options.viewport,
        );
        let length = |property: &str| {
            computed
                .and_then(|style| style.get(property))
                .map_or(0.0, |value| text_spacing_length(value.css_text(), &context))
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
                diagnostics.push(LayoutDiagnostic {
                    node: source,
                    code: LayoutDiagnosticCode::UnresolvedUsedValue,
                    message: format!("could not resolve '{property}': not a finite length"),
                });
            }
        }
        (spacing, diagnostics)
    }

    /// Append one atom per character of a text node. Split out of
    /// [`Self::collect_inline_atoms`] so the walker stays under the line
    /// budget after `vertical-align` added per-run alignment context.
    fn collect_text_atoms(
        &mut self,
        node_id: FormattingNodeId,
        source: Option<NodeId>,
        text: &str,
        style_source: Option<NodeId>,
        atoms: &mut Vec<InlineAtom<'a>>,
    ) {
        let typography = self.inline_text_style(style_source);
        let line_breaking = self.line_break_options(style_source);
        let vertical_align = if self.inline_level_style(style_source) {
            self.vertical_align_context(style_source, typography)
        } else {
            VerticalAlignContext::default()
        };
        for character in text.chars() {
            if self.inline_characters >= self.options.limits.max_inline_characters {
                self.diagnostics.push(LayoutDiagnostic {
                    node: source,
                    code: LayoutDiagnosticCode::InlineTextLimit,
                    message: "inline character limit exceeded".to_owned(),
                });
                return;
            }
            self.inline_characters += 1;
            atoms.push(InlineAtom {
                formatting_node: node_id,
                source,
                character,
                forced_break: false,
                break_before: false,
                atomic: None,
                line_breaking,
                style: typography.style,
                spacing: typography.spacing,
                vertical_align,
                bidi_level: 0,
            });
        }
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
        if content_visibility_hidden(
            node.style_source
                .and_then(|source| self.styles.get(&source)),
        ) {
            // `content-visibility: hidden` skips the element's contents
            // (CSS Containment 2 §4.1); its own inline box has nothing to
            // contribute to this inline formatting context.
            return;
        }
        if let FormattingNodeKind::Text(text) = node.kind {
            self.collect_text_atoms(node_id, node.source, &text, node.style_source, atoms);
            return;
        }
        let typography = self.inline_text_style(node.style_source);
        let line_breaking = self.line_break_options(node.style_source);
        let vertical_align = self.vertical_align_context(node.style_source, typography);
        // CSS Position 3 §2: an absolutely or fixed positioned box "does not
        // participate in its parent's formatting context". A block-level one
        // can be a child of an inline box instead of a sibling of the
        // anonymous block, so it still reaches this walk; it keeps a
        // placeholder for its static position but none of its content may
        // become a line atom.
        if self.is_out_of_flow(node_id) {
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
                vertical_align,
                bidi_level: 0,
            });
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
                break_before: false,
                atomic: None,
                line_breaking,
                style: typography.style,
                spacing: typography.spacing,
                vertical_align,
                bidi_level: 0,
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
                vertical_align,
                bidi_level: 0,
            });
            return;
        }
        for child in node.children {
            self.collect_inline_atoms(child, atoms, depth.saturating_add(1));
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "an atomic inline needs both containing-block rectangles, their directions and its line position"
    )]
    pub(super) fn layout_atomic_inline(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        containing_rtl: bool,
        positioning_containing: PhysicalRect,
        positioning_containing_rtl: bool,
        x: f32,
        y: f32,
        depth: usize,
    ) -> Option<(FragmentId, PhysicalRect)> {
        let out_of_flow = self.is_out_of_flow(node_id);
        // CSS Position 3 §3.5 and §4 size an absolutely positioned box from its
        // inset-modified containing block, not from the line it would have
        // occupied. Passing the atomic inline's shrink-to-fit width here would
        // override that algorithm, so only in-flow atomic boxes get one.
        let content_width = if out_of_flow {
            None
        } else {
            Some(self.atomic_inline_content_width(node_id, containing.size.width))
        };
        let result = self.layout_block(
            node_id,
            PhysicalRect::new(
                x,
                containing.origin.y,
                containing.size.width,
                containing.size.height,
            ),
            containing_rtl,
            positioning_containing,
            positioning_containing_rtl,
            y,
            depth,
            content_width,
        )?;
        let mut outer = self.fragment_outer_rect(result.fragment)?;
        if out_of_flow {
            // CSS Position 3 §3.5.3 "Inline Layout": when both insets in an
            // axis are `auto`, the box is aligned into a static-position
            // rectangle taken from the line box its hypothetical inline box
            // would have occupied. The block layout above resolves a definite
            // inset itself, so only the both-auto axes are moved to the inline
            // cursor; this engine approximates the hypothetical line box with
            // the current line and its top edge.
            let node = self.formatting.get(node_id).cloned();
            let source = node.as_ref().and_then(|node| node.source);
            let style = node
                .as_ref()
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source))
                .cloned();
            let left = self.resolve_inset(
                style.as_ref(),
                "left",
                positioning_containing.size.width,
                source,
            );
            let right = self.resolve_inset(
                style.as_ref(),
                "right",
                positioning_containing.size.width,
                source,
            );
            let top = self.resolve_inset(
                style.as_ref(),
                "top",
                positioning_containing.size.height,
                source,
            );
            let bottom = self.resolve_inset(
                style.as_ref(),
                "bottom",
                positioning_containing.size.height,
                source,
            );
            let dx = if left.is_none() && right.is_none() {
                x - outer.origin.x
            } else {
                0.0
            };
            let dy = if top.is_none() && bottom.is_none() {
                y - outer.origin.y
            } else {
                0.0
            };
            if dx != 0.0 || dy != 0.0 {
                self.translate_fragment_subtree(result.fragment, dx, dy);
                outer = self.fragment_outer_rect(result.fragment)?;
            }
        }
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
        // An inline `svg` is a replaced element. Its intrinsic size is the raster
        // the document pipeline installed for it, which is sized from the
        // element's own `width`/`height`/`viewBox`, and it has no HTML dimension
        // attributes. Without a raster it is not replaced, so its size stays the
        // presentation-hint size it had before.
        let is_svg = element.namespace == Namespace::Svg && element.local_name == "svg";
        let is_frame = element.namespace == Namespace::Html && element.local_name == "iframe";
        if !is_svg
            && !matches!(
                element.local_name.as_str(),
                "img" | "video" | "canvas" | "iframe"
            )
        {
            return None;
        }
        let html_width = if is_svg {
            None
        } else {
            self.html_image_dimension(source, "width")
        };
        let html_height = if is_svg {
            None
        } else {
            self.html_image_dimension(source, "height")
        };
        // An iframe's size is its HTML dimension attributes or the 300x150
        // default; the nested document's pixels must not make layout depend on
        // the previous frame's raster surface.
        let intrinsic = if is_frame {
            None
        } else {
            self.images
                .and_then(|images| images.intrinsic_size_for_node(source))
                .map(|(width, height)| {
                    (
                        image_dimension_to_f32(width),
                        image_dimension_to_f32(height),
                    )
                })
        };
        if is_svg && intrinsic.is_none() {
            return None;
        }
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
        // CSS Containment 3 §3: a box with inline-size containment has an
        // intrinsic inline size of zero, so its shrink-to-fit width is zero
        // unless a specified width (handled by the caller) replaces it.
        let style = self
            .formatting
            .get(node_id)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        if has_inline_size_containment(style) {
            return 0.0;
        }
        let preferred = self.atomic_inline_intrinsic_width(node_id);
        let preferred_minimum = self.atomic_inline_min_content_width(node_id);
        f32::min(preferred, f32::max(preferred_minimum, available))
    }

    /// The "preferred minimum width" of an `inline-block`, as a content width:
    /// the same walk as [`Self::atomic_inline_intrinsic_width`] with each
    /// in-flow child measured by its min-content width instead of its
    /// max-content one.
    pub(super) fn atomic_inline_min_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        if self.is_row_flex_container(node_id) {
            return self.flex_intrinsic_size(node_id, true, self.options.viewport.width, 0, true);
        }
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
                    // In-flow content shares the line with a preceding float:
                    // the float's outer width is part of the preferred minimum
                    // width, not a separate candidate that the content has to
                    // beat. Resetting the run before adding it made an
                    // inline-block with a leading icon measure only the label
                    // and then wrap the label under the icon in real layout.
                    widest = widest.max(float_run + child_width);
                    float_run = 0.0;
                }
            } else {
                float_run += self.atomic_outer_intrinsic_width(child, true);
                widest = widest.max(float_run);
            }
        }
        widest
    }

    /// Whether `node_id` is a row-axis flex container.
    ///
    /// Both the block-level and inline-level kind count; an `inline-flex`
    /// reaches the atomic-inline measurement before `max_content_width`'s
    /// block-container branch can see it.
    fn is_row_flex_container(&self, node_id: FormattingNodeId) -> bool {
        let Some(node) = self.formatting.get(node_id) else {
            return false;
        };
        if !matches!(
            node.kind,
            FormattingNodeKind::AtomicInline {
                context: FormattingContextKind::Flex
            } | FormattingNodeKind::BlockContainer {
                context: FormattingContextKind::Flex
            }
        ) {
            return false;
        }
        node.style_source
            .and_then(|source| self.styles.get(&source))
            .and_then(|style| style.get("flex-direction"))
            .is_none_or(|direction| {
                matches!(direction.css_text().trim(), "" | "row" | "row-reverse")
            })
    }

    pub(super) fn atomic_inline_intrinsic_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let trace = intrinsic_trace_enabled();
        let children = self
            .formatting
            .get(node_id)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        // An inline-level flex container is still a flex container: its
        // intrinsic main size is the sum of its items, not the widest child.
        // `max_content_width` handles the block-level flex container, but an
        // `AtomicInline { context: Flex }` reaches this function before that
        // branch, so it must ask the flex solver here.
        if self.is_row_flex_container(node_id) {
            return self.flex_intrinsic_size(node_id, true, self.options.viewport.width, 0, false);
        }
        let mut widest = 0.0_f32;
        let mut float_run = 0.0_f32;
        for child in children {
            let child_float = self.float_side(child);
            let child_width = self.max_content_width(child);
            if trace {
                eprintln!("  child={child:?} float={child_float:?} max={child_width}");
            }
            if child_float == Float::None {
                // Whitespace between inline/floating children is represented
                // by empty anonymous blocks. It does not terminate a run of
                // adjacent floats; doing so makes every link in a real
                // navigation bar start on a new line.
                if child_width > f32::EPSILON {
                    // The first in-flow content after a float occupies the
                    // remaining part of the same line, so the preferred width
                    // is the float run plus that content. See the matching
                    // comment in `atomic_inline_min_content_width`.
                    widest = widest.max(float_run + child_width);
                    float_run = 0.0;
                }
            } else {
                // Adjacent floats share a line in a shrink-to-fit box. The
                // preferred width is their combined outer width, including
                // margins, rather than the width of only the widest float.
                float_run += self.atomic_outer_max_content_width(child);
                widest = widest.max(float_run);
            }
        }
        if trace {
            eprintln!("intrinsic float node={node_id:?} width={widest}");
        }
        widest
    }

    pub(super) fn max_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        if let Some(width) = self.max_content_widths.get(&node_id) {
            return *width;
        }
        let width = self.max_content_width_uncached(node_id);
        self.max_content_widths.insert(node_id, width);
        width
    }

    fn max_content_width_uncached(&mut self, node_id: FormattingNodeId) -> f32 {
        let Some(node) = self.formatting.get(node_id).cloned() else {
            return 0.0;
        };
        // CSS Containment 3 §3: inline-size containment makes the intrinsic
        // inline size zero, as if the box had no contents. Atomic boxes keep
        // their own width contribution and are handled below.
        if !matches!(node.kind, FormattingNodeKind::AtomicInline { .. })
            && has_inline_size_containment(
                node.style_source
                    .and_then(|source| self.styles.get(&source)),
            )
        {
            return 0.0;
        }
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
        // A flex container's content is its items along the main axis, not its
        // block children stacked vertically. A row container is as wide as the
        // sum of its items (Bing's `换一批` button measured as its widest child
        // and then wrapped the label inside the too-narrow button).
        if matches!(
            node.kind,
            FormattingNodeKind::BlockContainer {
                context: FormattingContextKind::Flex
            }
        ) {
            let style = node
                .style_source
                .and_then(|source| self.styles.get(&source));
            let row = style
                .and_then(|style| style.get("flex-direction"))
                .is_none_or(|direction| {
                    matches!(direction.css_text().trim(), "" | "row" | "row-reverse")
                });
            if row {
                return self.flex_intrinsic_size(
                    node_id,
                    true,
                    self.options.viewport.width,
                    0,
                    false,
                );
            }
        }
        // CSS Sizing 3 §5.1: a definite specified size caps the intrinsic
        // contribution, so an empty `width: 14px` item is 14px wide even though
        // it has no contents. Without this the icon contributed zero and the
        // surrounding inline-block/flex container collapsed to its label; the
        // same omission made a table cell ignore a fixed-width descendant.
        if let Some(source) = node.source
            && let Some(style) = node
                .style_source
                .and_then(|style_source| self.styles.get(&style_source))
            && let Some(width) = self.resolve_size_against(Some(style), "width", None, Some(source))
        {
            return width.max(0.0);
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
        if let Some(width) = self.min_content_widths.get(&node_id) {
            return *width;
        }
        let width = self.min_content_width_at(node_id, 0);
        self.min_content_widths.insert(node_id, width);
        width
    }

    fn min_content_width_at(&mut self, node_id: FormattingNodeId, depth: usize) -> f32 {
        if depth > self.options.limits.max_depth {
            return 0.0;
        }
        let Some(node) = self.formatting.get(node_id).cloned() else {
            return 0.0;
        };
        // CSS Containment 3 §3: inline-size containment zeroes the intrinsic
        // inline size of the box in the same way for min- and max-content.
        if !matches!(node.kind, FormattingNodeKind::AtomicInline { .. })
            && has_inline_size_containment(
                node.style_source
                    .and_then(|source| self.styles.get(&source)),
            )
        {
            return 0.0;
        }
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
        // CSS 2.1 §9.3.1: absolutely positioned boxes are taken out of the
        // inline formatting context. They neither participate in its line
        // breaking nor contribute to its max-content or min-content width.
        if self.is_out_of_flow(node_id) {
            return;
        }
        if content_visibility_hidden(
            node.style_source
                .and_then(|source| self.styles.get(&source)),
        ) {
            return;
        }
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
                    vertical_align: VerticalAlignContext::default(),
                    bidi_level: 0,
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
                vertical_align: VerticalAlignContext::default(),
                bidi_level: 0,
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
                vertical_align: VerticalAlignContext::default(),
                bidi_level: 0,
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
                None => {
                    self.measure_inline_unit(atom.character, typography, false, previous.as_ref())
                }
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
    ///
    /// Like [`Self::inline_sequence_min_content_width`], this measures the
    /// atoms of the whole sequence rather than folding over each child's own
    /// max-content width. Adjacent inline boxes and text nodes are adjacent
    /// typographic units: CSS Text 3 §1.5 ignores the box boundaries, so a
    /// `letter-spacing` gap belongs between the last unit of one child and the
    /// first unit of the next, and folding over children would lose it. That
    /// also keeps `min-content <= max-content` for tracking, which the
    /// shrink-to-fit formula depends on.
    pub(super) fn inline_sequence_max_content_width(
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
        let mut widest = 0.0_f32;
        let mut line = 0.0_f32;
        let mut previous: Option<PreviousUnit<'a>> = None;
        let mut pending_space: Option<InlineAtom<'a>> = None;
        let mut has_content = false;
        for atom in atoms {
            if atom.forced_break {
                widest = widest.max(line);
                line = 0.0;
                previous = None;
                pending_space = None;
                has_content = false;
                continue;
            }
            if let Some(node) = atom.atomic {
                // A pending collapsed separator still counts before an atomic
                // inline, exactly as the line filler places it.
                if let Some(space) = pending_space.take()
                    && has_content
                {
                    line += self.measure_inline_unit(
                        ' ',
                        inline_typography(&space),
                        false,
                        previous.as_ref(),
                    );
                }
                line += self.atomic_outer_max_content_width(node);
                previous = Some(PreviousUnit {
                    typography: inline_typography(&atom),
                    atomic: true,
                });
                has_content = true;
                continue;
            }
            if atom.character.is_whitespace() {
                if has_content {
                    pending_space = Some(atom);
                }
                continue;
            }
            if let Some(space) = pending_space.take() {
                // One collapsed separator stands for the whole run of white
                // space, and it is a character unit like any other, so
                // tracking goes before it too.
                line += self.measure_inline_unit(
                    ' ',
                    inline_typography(&space),
                    false,
                    previous.as_ref(),
                );
                previous = Some(PreviousUnit {
                    typography: inline_typography(&space),
                    atomic: false,
                });
            }
            line += self.measure_inline_unit(
                atom.character,
                inline_typography(&atom),
                false,
                previous.as_ref(),
            );
            previous = Some(PreviousUnit {
                typography: inline_typography(&atom),
                atomic: false,
            });
            has_content = true;
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
        let contained = has_inline_size_containment(style.as_ref());
        let children = node.map(|node| node.children).unwrap_or_default();
        let content_width = specified.map_or_else(
            || {
                // CSS Containment 3 §3: an auto width sizes the box as if it
                // had no contents, while a specified width keeps its own used
                // size plus the padding, border, and margin below.
                if contained {
                    return 0.0;
                }
                // An inline-flex container's content is the sum of its flex
                // items along the main axis, not the widest child. Folding
                // `max` here under-measured a search row by one item's width
                // and then wrapped the last item onto a second line.
                if self.is_row_flex_container(node_id) {
                    return self.flex_intrinsic_size(node_id, true, basis, 0, minimum);
                }
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
        line_items: &mut Vec<InlineLineItem>,
        atom: InlineAtom<'a>,
        character: char,
        x: f32,
        y: f32,
        width: f32,
        leading_spacing: f32,
        line: usize,
    ) {
        let typography = inline_typography(&atom);
        if run.as_ref().is_some_and(|run| {
            run.formatting_node != atom.formatting_node
                || (run.y - y).abs() > f32::EPSILON
                || run.typography != typography
                || run.vertical_align != atom.vertical_align
                || run.bidi_level != atom.bidi_level
                || run.line != line
        }) {
            self.flush_text_run(run, fragments, line_items);
        }
        let level = atom.bidi_level;
        let run = run.get_or_insert_with(|| TextRun {
            formatting_node: atom.formatting_node,
            source: atom.source,
            text: String::new(),
            x,
            y,
            width: 0.0,
            typography,
            vertical_align: atom.vertical_align,
            leading_spacing,
            bidi_level: level,
            line,
        });
        run.text.push(character);
        run.width += width;
    }

    pub(super) fn flush_text_run(
        &mut self,
        run: &mut Option<TextRun<'a>>,
        fragments: &mut Vec<FragmentId>,
        line_items: &mut Vec<InlineLineItem>,
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
        // CSS 2.1 §10.8.1: the difference between `line-height` and the font's
        // ascent + descent is half-leading, split equally above and below the
        // text. Painting the baseline at the line-box top plus the ascent
        // leaves all of the leading below the glyphs, so text in a line box
        // taller than its font sits too high and is clipped by an ancestor's
        // `overflow:hidden` edge (the hot-search row is 23px tall around a
        // 13px font).
        let half_leading =
            (run.typography.style.line_height - (metrics.ascent + metrics.descent)) / 2.0;
        let baseline_offset = half_leading + metrics.ascent;
        if let Some(fragment) = self.allocate_fragment(
            run.formatting_node,
            run.source,
            rect,
            FragmentKind::Text(TextFragmentData {
                text: run.text,
                baseline: run.y + baseline_offset,
                font_size: run.typography.style.font_size,
                font: StoredFontRequest::new(run.typography.style.font),
                spacing: run.typography.spacing,
                leading_spacing: run.leading_spacing,
                trailing_spacing: 0.0,
            }),
        ) {
            line_items.push(InlineLineItem {
                fragment,
                top: run.y,
                baseline: baseline_offset,
                height: run.typography.style.line_height,
                text: true,
                vertical_align: run.vertical_align,
            });
            self.inline_bidi.insert(
                fragment,
                InlineBidiPlacement {
                    line: run.line,
                    level: run.bidi_level,
                },
            );
            fragments.push(fragment);
        }
    }
}
