//! The CSS Fonts Level 4 font request: family, weight, style, and synthesis.
//!
//! Everything here is a *request*. Selecting a face from it is the font
//! matching algorithm of CSS Fonts 4 §5, which needs a table of installed
//! faces to search and therefore belongs to the font backend, not here. What
//! lives here is the part that is the same for every backend: how the four
//! properties are read out of a computed style, what each computed value means,
//! and the font-free approximation the reference path measures with.
//!
//! The distinction matters more than usual. [`TextStyle`] is a `Copy` value
//! that is rebuilt for *every* typographic character unit during inline layout,
//! so it borrows the family list from the computed style instead of owning it;
//! the owned copy that outlives the cascade lives on the text fragment
//! ([`crate::fragment::TextFragmentData`]).
//!
//! [`TextStyle`]: crate::solver::TextStyle

/// The character CSS Unicode ranges that occupy a full em: the East Asian
/// Wide and Fullwidth blocks, Hangul syllables and jamo, the CJK compatibility
/// and supplementary ideographic planes, and the emoji planes.
///
/// Whether a glyph is actually one em wide is a property of the face, so a real
/// backend asks the font. This is the reference path's answer, and it is the
/// same answer line breaking uses, which is what keeps a wide character's
/// measurement and its line-break opportunity from disagreeing.
#[must_use]
pub const fn is_wide_character(character: char) -> bool {
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

/// The computed value of `font-style` (CSS Fonts 4 §2.4).
///
/// §2.4 resolves the keywords before they get here, so this covers the whole
/// computed domain: `left` and `right` are "a font that is labeled as an italic
/// face, with a positive (clockwise) slant" and its negative, and a bare
/// `oblique` is `oblique 14deg`, so both are written as angles. Only `italic`
/// stays distinct, because §5.2 gives it a different search and because "For
/// user agents that treat these values distinctly, synthesis must not be
/// performed for italic."
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontStyle {
    /// `normal`, which §2.4 places at an oblique value of 0.
    Normal,
    /// `italic`, whose angle and direction of slant §2.4 leaves unspecified.
    Italic,
    /// `oblique <angle>`, `left` (positive) or `right` (negative).
    Oblique(f32),
}

impl FontStyle {
    /// The angle §2.4 gives `oblique` when no `<angle>` is written.
    pub const DEFAULT_OBLIQUE_DEGREES: f32 = 14.0;

    /// The slant of the requested style on the scale §5.2 lets a user agent
    /// collapse italic and oblique onto.
    ///
    /// §5.2 requires only one mapping constraint - "an italic value of 1 must
    /// map to the same value that an oblique angle of 11deg maps to" - and that
    /// is the one used here. It is what makes a search expressible as an
    /// ordered list of target slants rather than as two interleaved sweeps.
    #[must_use]
    pub fn slant_degrees(self) -> f32 {
        match self {
            Self::Normal => 0.0,
            Self::Italic => 11.0,
            Self::Oblique(degrees) => degrees,
        }
    }
}

/// Whether the user agent may synthesize a face the family does not have
/// (CSS Fonts 4 §2.8, the `font-synthesis` properties).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontSynthesis {
    /// §2.8.1 `font-synthesis-weight`.
    pub weight: bool,
    /// §2.8.2 `font-synthesis-style`.
    pub style: bool,
}

impl Default for FontSynthesis {
    /// §2.8: both properties are `auto` initially, which permits synthesis.
    fn default() -> Self {
        Self {
            weight: true,
            style: true,
        }
    }
}

/// One font request: what §5 needs in order to pick a face.
///
/// The family list is kept as authored rather than pre-split, because §2.1's
/// computed value is "a list, each item a string and/or `<generic-font-family>`
/// keywords" and reading the list needs the tokenizer, which lives in
/// render-css.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontRequest<'a> {
    /// The `font-family` list. An element that declared none arrives here with
    /// the user agent's initial value, which is `system-ui`; see
    /// [`FontRequest::initial`].
    pub family: &'a str,
    /// §2.2: 1 to 1000, already resolved from `bolder`/`lighter` against the
    /// inherited value.
    pub weight: u16,
    pub style: FontStyle,
    pub synthesis: FontSynthesis,
}

impl Default for FontRequest<'_> {
    fn default() -> Self {
        Self::initial()
    }
}

impl<'a> FontRequest<'a> {
    /// The name of the generic family this engine's initial `font-family` is.
    ///
    /// §2.1 leaves the initial value user-agent defined. `system-ui` is what
    /// every shipping engine uses, and it is the only choice that keeps the
    /// reference path and a real platform agreeing about what "no declaration"
    /// looks like.
    pub const INITIAL_FAMILY: &'a str = "system-ui";

    /// The request an element that declares nothing makes: the user agent's
    /// initial family, weight 400 (§2.2) and `normal` style (§2.4).
    #[must_use]
    pub const fn initial() -> Self {
        Self {
            family: Self::INITIAL_FAMILY,
            weight: 400,
            style: FontStyle::Normal,
            synthesis: FontSynthesis {
                weight: true,
                style: true,
            },
        }
    }

    /// A request for one family, at the initial weight, style, and synthesis.
    #[must_use]
    pub const fn of_family(family: &'a str) -> Self {
        Self {
            family,
            weight: 400,
            style: FontStyle::Normal,
            synthesis: FontSynthesis {
                weight: true,
                style: true,
            },
        }
    }

    /// The `font-family` list in author order, one raw entry per comma.
    ///
    /// §2.1 makes the list a prioritized sequence that §5.2 walks from the
    /// front, so the order this yields is load-bearing: it is the fallback
    /// chain. Entries are returned as written, quotes and all, because
    /// [`family_entry`] needs the quotes to tell a `<font-family-name>` from a
    /// `<generic-font-family>` keyword (§2.1.2).
    pub fn families(&self) -> impl Iterator<Item = &'a str> {
        self.family
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }
}

/// One entry of a `font-family` list: a `<generic-font-family>` keyword or a
/// `<font-family-name>`, which §2.1.1 defines as a `<string>` or a sequence of
/// `<custom-ident>`s.
///
/// The two are kept apart because §2.1.2 says a generic keyword "cannot be
/// quoted (otherwise they are interpreted as a `<font-family-name>`)" and §5.4
/// says a Private Use Area character must only be matched against families that
/// are *not* generic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FamilyName<'a> {
    Generic(GenericFamily),
    Named(&'a str),
}

/// One of the `<generic-font-family>` keywords (CSS Fonts 4 §2.1.2).
///
/// The `ui-*` and `generic(...)` forms are separate values rather than aliases,
/// because §2.1.5 says they "may not match to a locally installed font on some
/// systems" and a backend has to be able to fall through them rather than
/// silently treat them as `sans-serif`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenericFamily {
    Serif,
    SansSerif,
    SystemUi,
    Cursive,
    Fantasy,
    Math,
    Monospace,
    UiSerif,
    UiSansSerif,
    UiMonospace,
    UiRounded,
    /// `generic(fangsong)`, §2.1.5.
    Fangsong,
    /// `generic(kai)`, §2.1.5.
    Kai,
    /// `generic(khmer-mul)`, §2.1.5.
    KhmerMul,
    /// `generic(nastaliq)`, §2.1.5.
    Nastaliq,
}

/// A `font-family` name as §2.1 writes one: quoted, or one or more
/// `<custom-ident>`s joined by single spaces.
///
/// Quotes are stripped but nothing inside them is, so a quoted `serif` stays a
/// `<font-family-name>` and does not become the generic family, which is what
/// §2.1.1 requires.
#[must_use]
pub fn unquote_family_name(name: &str) -> Option<&str> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let quoted = name
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            name.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        });
    match quoted {
        Some(inner) => {
            let inner = inner.trim();
            if inner.is_empty() { None } else { Some(inner) }
        }
        // §2.1.1: an unquoted name is a sequence of identifiers joined by
        // spaces. Commas have already been split off, so nothing else needs
        // removing.
        None => Some(name),
    }
}

/// The `<generic-font-family>` keyword `name` denotes, if it is one.
///
/// §2.1.2: keywords are matched case-insensitively like every other CSS
/// keyword, and the functional `generic(...)` forms are matched on their inner
/// name.
#[must_use]
pub fn generic_family(name: &str) -> Option<GenericFamily> {
    let name = name.trim();
    if let Some(inner) = name
        .strip_prefix("generic(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return match inner.trim().to_ascii_lowercase().as_str() {
            "fangsong" => Some(GenericFamily::Fangsong),
            "kai" => Some(GenericFamily::Kai),
            "khmer-mul" => Some(GenericFamily::KhmerMul),
            "nastaliq" => Some(GenericFamily::Nastaliq),
            _ => None,
        };
    }
    match name.to_ascii_lowercase().as_str() {
        "serif" => Some(GenericFamily::Serif),
        "sans-serif" => Some(GenericFamily::SansSerif),
        "system-ui" => Some(GenericFamily::SystemUi),
        "cursive" => Some(GenericFamily::Cursive),
        "fantasy" => Some(GenericFamily::Fantasy),
        "math" => Some(GenericFamily::Math),
        "monospace" => Some(GenericFamily::Monospace),
        "ui-serif" => Some(GenericFamily::UiSerif),
        "ui-sans-serif" => Some(GenericFamily::UiSansSerif),
        "ui-monospace" => Some(GenericFamily::UiMonospace),
        "ui-rounded" => Some(GenericFamily::UiRounded),
        _ => None,
    }
}

/// One entry of a `font-family` list, classified as §2.1 defines each kind.
///
/// The name is unquoted first, so a quoted `serif` is a `<font-family-name>`
/// naming a font called `serif` and not the generic family, which is exactly
/// what §2.1.1's note about quoting requires.
#[must_use]
pub fn family_entry(name: &str) -> Option<FamilyName<'_>> {
    let bytes = name.as_bytes();
    let quoted = bytes.len() >= 2
        && matches!(
            (bytes.first(), bytes.last()),
            (Some(b'"' | b'\''), Some(b'"' | b'\''))
        );
    let unquoted = unquote_family_name(name)?;
    match generic_family(unquoted) {
        Some(generic) if !quoted => Some(FamilyName::Generic(generic)),
        _ => Some(FamilyName::Named(unquoted)),
    }
}

/// Every entry of `request`'s `font-family` list, classified, in author order.
pub fn family_entries<'a>(request: &FontRequest<'a>) -> impl Iterator<Item = FamilyName<'a>> {
    request.families().filter_map(family_entry)
}

/// §5.1 Default Caseless Matching, as far as a self-contained implementation
/// can reach it.
///
/// The specification requires the case mappings with Unicode `CaseFolding`
/// status `C` or `F`, "applied without normalizing the strings involved and
/// without applying any language-specific tailorings". Simple lowercase
/// mapping is used
/// instead of full case folding, which is the one documented deviation: the two
/// disagree only for characters whose full folding is multi-character (the
/// German sharp s among them), and no font family name is named after one. No
/// normalization is applied either, so §5.1's own warning - that `a` followed
/// by U+030A does not match U+00E5 - still holds, which is why a platform
/// routine that normalizes is not what this is standing in for.
#[must_use]
pub fn caseless_match(candidate: &str, requested: &str) -> bool {
    candidate.to_lowercase() == requested.to_lowercase()
}

/// The computed `font-weight` of CSS Fonts 4 §2.2.
///
/// `normal` and `bold` are "same as" 400 and 700. The relative keywords are
/// resolved against `inherited` through the §2.2.1 table, because §2.2 defines
/// them relative to the parent's computed value and the cascade does not
/// resolve them. A `<number>` is valid only in `[1, 1000]`; anything else is
/// invalid, and an invalid declaration leaves the value alone.
#[must_use]
pub fn computed_font_weight(value: &str, inherited: u16) -> Option<u16> {
    let value = value.trim();
    match value.to_ascii_lowercase().as_str() {
        "normal" => return Some(400),
        "bold" => return Some(700),
        "bolder" => return Some(relative_font_weight(inherited, true)),
        "lighter" => return Some(relative_font_weight(inherited, false)),
        _ => {}
    }
    let number = value.parse::<f32>().ok()?;
    if !number.is_finite() || !(1.0..=1000.0).contains(&number) {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the range check above leaves exactly 1.0..=1000.0"
    )]
    Some(number as u16)
}

/// §2.2.1's table of relative weights, for the inherited weight `inherited`.
///
/// `w < 100` has no row value for `lighter` other than "No change"; the
/// inherited weight is already inside §2.2's valid 1..=1000 range, so it is
/// returned unchanged.
#[must_use]
pub const fn relative_font_weight(inherited: u16, bolder: bool) -> u16 {
    let weight = inherited as u32;
    if weight < 100 {
        if bolder { 400 } else { inherited }
    } else if weight < 350 {
        if bolder { 400 } else { 100 }
    } else if weight < 550 {
        if bolder { 700 } else { 100 }
    } else if weight < 750 {
        if bolder { 900 } else { 400 }
    } else if weight < 900 {
        if bolder { 900 } else { 700 }
    } else if bolder {
        inherited
    } else {
        700
    }
}

/// The computed `font-style` of CSS Fonts 4 §2.4.
#[must_use]
pub fn computed_font_style(value: &str) -> Option<FontStyle> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "normal" => Some(FontStyle::Normal),
        "italic" => Some(FontStyle::Italic),
        "left" => Some(FontStyle::Oblique(FontStyle::DEFAULT_OBLIQUE_DEGREES)),
        "right" => Some(FontStyle::Oblique(-FontStyle::DEFAULT_OBLIQUE_DEGREES)),
        _ => {
            let degrees = value.strip_prefix("oblique")?.trim();
            if degrees.is_empty() {
                return Some(FontStyle::Oblique(FontStyle::DEFAULT_OBLIQUE_DEGREES));
            }
            let angle = degrees.strip_suffix("deg")?.trim();
            let degrees = angle.parse::<f32>().ok()?;
            if !degrees.is_finite() || !(-90.0..=90.0).contains(&degrees) {
                return None;
            }
            Some(FontStyle::Oblique(degrees))
        }
    }
}

/// The computed `font-synthesis-weight` and `font-synthesis-style` of CSS Fonts
/// 4 §2.8.
///
/// Both are `auto` initially and `auto` is the only value that permits
/// synthesis. `font-synthesis-style` also has `oblique-only`, which permits
/// oblique synthesis but not italic, and that is a distinction the engine does
/// not draw: §2.8.2 describes it as disabling the synthesis of italic faces, and
/// §2.4 says synthesis must not be performed for italic anyway, so
/// `oblique-only` and `auto` are the same decision here.
#[must_use]
pub fn computed_font_synthesis(weight: Option<&str>, style: Option<&str>) -> FontSynthesis {
    FontSynthesis {
        weight: permits_synthesis(weight),
        style: permits_synthesis(style),
    }
}

fn permits_synthesis(value: Option<&str>) -> bool {
    value.is_none_or(|value| !value.trim().eq_ignore_ascii_case("none"))
}

/// The two classes of face the reference path distinguishes.
///
/// The reference path has no font files, so it cannot run §5. What it can do is
/// honour the one property §2.1.5 states as a *definition* rather than a
/// preference: "The sole criterion of a monospace font is that all glyphs have
/// the same fixed width." That is a claim a font-free model can actually make
/// true, so `Monospace` gets one equal advance for every narrow character and
/// two cells for a wide one. Every other family is a proportional face and
/// shares the single nominal advance below, which is why the reference path is
/// a geometry fixture and not a font: two proportional families are the same
/// size to it, and no weight or style changes it at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NominalFace {
    Proportional,
    Monospace,
}

impl NominalFace {
    /// The advance of a narrow character in a proportional face, in em.
    const NARROW_ADVANCE_EM: f32 = 0.5;
    /// The advance of one monospace cell, in em. §2.1.5's sole criterion is
    /// that every glyph shares it.
    const MONOSPACE_ADVANCE_EM: f32 = 0.6;
    /// The advance of a space in a proportional face, in em.
    const PROPORTIONAL_SPACE_EM: f32 = 0.25;
    /// The nominal ascent of every reference face, in em.
    const ASCENT_EM: f32 = 0.8;
    /// The nominal descent of every reference face, in em.
    const DESCENT_EM: f32 = 0.2;

    /// The advance of `character` in this face, in em.
    #[must_use]
    pub fn advance_em(self, character: char) -> f32 {
        let wide = is_wide_character(character);
        match self {
            Self::Proportional => {
                if character.is_whitespace() {
                    Self::PROPORTIONAL_SPACE_EM
                } else if wide {
                    1.0
                } else {
                    Self::NARROW_ADVANCE_EM
                }
            }
            Self::Monospace => {
                // A monospace grid is one cell per narrow character and two for
                // a wide one, which is what keeps CJK code aligned inside an
                // otherwise fixed-width line.
                if wide {
                    Self::MONOSPACE_ADVANCE_EM * 2.0
                } else {
                    Self::MONOSPACE_ADVANCE_EM
                }
            }
        }
    }

    /// The nominal ascent of this face, in em.
    #[must_use]
    pub const fn ascent_em(self) -> f32 {
        Self::ASCENT_EM
    }

    /// The nominal descent of this face, in em.
    #[must_use]
    pub const fn descent_em(self) -> f32 {
        Self::DESCENT_EM
    }
}

/// The face the reference path measures `request` with.
///
/// §5.2 walks the `font-family` list from the front and moves to the next name
/// when a family does not exist. The reference path has no installed fonts at
/// all, so every `<font-family-name>` is absent by construction and the walk
/// finds a family only where the author named a `<generic-font-family>` -
/// which §2.1.5 defines as "an alias for an existing installed font family
/// present on the system", a promise the engine can keep without one. A list
/// that names only families the engine does not have, and a list that names
/// none, both end at the same place: the user agent's default face, which is
/// what §5 calls installed font fallback and explicitly leaves to the user
/// agent.
#[must_use]
pub fn nominal_face(request: &FontRequest<'_>) -> NominalFace {
    for entry in family_entries(request) {
        let FamilyName::Generic(generic) = entry else {
            continue;
        };
        return match generic {
            GenericFamily::Monospace | GenericFamily::UiMonospace => NominalFace::Monospace,
            GenericFamily::Serif
            | GenericFamily::SansSerif
            | GenericFamily::SystemUi
            | GenericFamily::Cursive
            | GenericFamily::Fantasy
            | GenericFamily::Math
            | GenericFamily::UiSerif
            | GenericFamily::UiSansSerif
            | GenericFamily::UiRounded
            | GenericFamily::Fangsong
            | GenericFamily::Kai
            | GenericFamily::KhmerMul
            | GenericFamily::Nastaliq => NominalFace::Proportional,
        };
    }
    NominalFace::Proportional
}

/// The total advance of `text` in `face` at `font_size`.
///
/// Both halves of the reference path measure and shape through this, so the
/// geometry the solver computes and the glyph advances the shaper emits come
/// from one number rather than from two that have to be kept in step.
#[must_use]
pub fn nominal_advance(face: NominalFace, text: &str, font_size: f32) -> f32 {
    text.chars()
        .map(|character| face.advance_em(character) * font_size)
        .sum()
}

#[cfg(test)]
mod tests {
    // The nominal advances and slants below are exact constants, not measurements,
    // so the assertions are about equality and not about a tolerance.
    #![allow(
        clippy::float_cmp,
        reason = "the reference path's advances and slants are exact constants"
    )]

    use super::{
        FamilyName, FontRequest, FontStyle, GenericFamily, NominalFace, caseless_match,
        computed_font_style, computed_font_synthesis, computed_font_weight, family_entries,
        nominal_advance, nominal_face, relative_font_weight, unquote_family_name,
    };

    /// Every `(inherited, bolder)` case of the §2.2.1 table, read straight off
    /// its published columns so the test does not restate the function.
    const SECTION_2_2_1: [(u16, u16, u16); 10] = [
        (50, 400, 50),
        (99, 400, 99),
        (100, 400, 100),
        (349, 400, 100),
        (350, 700, 100),
        (549, 700, 100),
        (550, 900, 400),
        (749, 900, 400),
        (750, 900, 700),
        (899, 900, 700),
    ];

    #[test]
    fn relative_weights_are_the_section_two_two_one_columns() {
        for (inherited, bolder_column, lighter_column) in SECTION_2_2_1 {
            assert_eq!(
                relative_font_weight(inherited, true),
                bolder_column,
                "bolder than {inherited}"
            );
            assert_eq!(
                relative_font_weight(inherited, false),
                lighter_column,
                "lighter than {inherited}"
            );
            assert_eq!(
                computed_font_weight("bolder", inherited),
                Some(bolder_column)
            );
            assert_eq!(
                computed_font_weight("lighter", inherited),
                Some(lighter_column)
            );
        }
    }

    #[test]
    fn the_heaviest_row_is_bolder_no_change_and_lighter_700() {
        for inherited in [900_u16, 950, 1000] {
            assert_eq!(computed_font_weight("bolder", inherited), Some(inherited));
            assert_eq!(computed_font_weight("lighter", inherited), Some(700));
        }
    }

    #[test]
    fn font_weight_keywords_resolve_to_their_numbers() {
        assert_eq!(computed_font_weight("normal", 700), Some(400));
        assert_eq!(computed_font_weight("bold", 400), Some(700));
        assert_eq!(computed_font_weight("BOLD", 400), Some(700));
        assert_eq!(computed_font_weight("700", 400), Some(700));
        assert_eq!(computed_font_weight("450", 400), Some(450));
        assert_eq!(computed_font_weight(" 550 ", 400), Some(550));
    }

    #[test]
    fn a_font_weight_number_outside_one_to_thousand_is_invalid() {
        assert_eq!(computed_font_weight("0", 400), None);
        assert_eq!(computed_font_weight("1001", 400), None);
        assert_eq!(computed_font_weight("-100", 400), None);
        assert_eq!(computed_font_weight("heavy", 400), None);
        assert_eq!(computed_font_weight("calc(1 + 1)", 400), None);
    }

    #[test]
    fn font_style_keywords_resolve_to_their_angles() {
        assert_eq!(computed_font_style("normal"), Some(FontStyle::Normal));
        assert_eq!(computed_font_style("italic"), Some(FontStyle::Italic));
        assert_eq!(computed_font_style("ITALIC"), Some(FontStyle::Italic));
        assert_eq!(
            computed_font_style("oblique"),
            Some(FontStyle::Oblique(14.0))
        );
        assert_eq!(
            computed_font_style("oblique 30deg"),
            Some(FontStyle::Oblique(30.0))
        );
        assert_eq!(
            computed_font_style("oblique -14DEG"),
            Some(FontStyle::Oblique(-14.0))
        );
        assert_eq!(computed_font_style("left"), Some(FontStyle::Oblique(14.0)));
        assert_eq!(
            computed_font_style("right"),
            Some(FontStyle::Oblique(-14.0))
        );
    }

    #[test]
    fn an_oblique_angle_outside_ninety_degrees_is_invalid() {
        assert_eq!(computed_font_style("oblique 91deg"), None);
        assert_eq!(computed_font_style("oblique -91deg"), None);
        assert_eq!(computed_font_style("oblique 14rad"), None);
        assert_eq!(computed_font_style("upright"), None);
    }

    #[test]
    fn italic_collapses_onto_the_eleven_degree_oblique_value() {
        // §5.2's one mapping constraint, and the reason the reference model's
        // slant is a single number per style.
        assert_eq!(FontStyle::Italic.slant_degrees(), 11.0);
        assert_eq!(FontStyle::Normal.slant_degrees(), 0.0);
        assert_eq!(FontStyle::Oblique(11.0).slant_degrees(), 11.0);
    }

    #[test]
    fn font_synthesis_none_disables_only_its_own_axis() {
        let weight_only = computed_font_synthesis(Some("none"), None);
        assert!(!weight_only.weight, "font-synthesis-weight: none");
        assert!(weight_only.style, "font-synthesis-style is still auto");

        let style_only = computed_font_synthesis(None, Some("NONE"));
        assert!(style_only.weight);
        assert!(!style_only.style);

        let both = computed_font_synthesis(Some("auto"), Some("auto"));
        assert!(both.weight);
        assert!(both.style);
    }

    #[test]
    fn a_quoted_generic_keyword_is_a_family_name() {
        assert_eq!(unquote_family_name("\"monospace\""), Some("monospace"));
        assert_eq!(
            family_entries(&FontRequest::of_family("\"monospace\"")).next(),
            Some(FamilyName::Named("monospace"))
        );
        assert_eq!(
            family_entries(&FontRequest::of_family("monospace")).next(),
            Some(FamilyName::Generic(GenericFamily::Monospace))
        );
    }

    #[test]
    fn a_quoted_family_name_keeps_its_inner_spaces_and_drops_its_quotes() {
        assert_eq!(unquote_family_name("\"Segoe UI\""), Some("Segoe UI"));
        assert_eq!(unquote_family_name("'Segoe UI'"), Some("Segoe UI"));
        assert_eq!(unquote_family_name("   "), None);
        assert_eq!(unquote_family_name("\"\""), None);
    }

    #[test]
    fn a_multi_identifier_name_is_kept_as_one_family() {
        assert_eq!(
            family_entries(&FontRequest::of_family("Lucida Grande, monospace")).collect::<Vec<_>>(),
            vec![
                FamilyName::Named("Lucida Grande"),
                FamilyName::Generic(GenericFamily::Monospace),
            ]
        );
    }

    #[test]
    fn the_family_list_keeps_author_order_because_it_is_the_fallback_chain() {
        assert_eq!(
            family_entries(&FontRequest::of_family(
                "\"PingFang SC\", \"Microsoft YaHei\", sans-serif"
            ))
            .collect::<Vec<_>>(),
            vec![
                FamilyName::Named("PingFang SC"),
                FamilyName::Named("Microsoft YaHei"),
                FamilyName::Generic(GenericFamily::SansSerif),
            ]
        );
    }

    #[test]
    fn an_undeclared_family_is_this_engines_initial_value() {
        assert_eq!(
            family_entries(&FontRequest::initial()).collect::<Vec<_>>(),
            vec![FamilyName::Generic(GenericFamily::SystemUi)]
        );
    }

    #[test]
    fn generic_families_are_matched_case_insensitively() {
        assert_eq!(
            family_entries(&FontRequest::of_family("MONOSPACE")).next(),
            Some(FamilyName::Generic(GenericFamily::Monospace))
        );
        assert_eq!(
            family_entries(&FontRequest::of_family("ui-monospace")).next(),
            Some(FamilyName::Generic(GenericFamily::UiMonospace))
        );
    }

    #[test]
    fn the_script_specific_generic_families_are_their_own_keywords() {
        for (name, expected) in [
            ("generic(fangsong)", GenericFamily::Fangsong),
            ("generic(kai)", GenericFamily::Kai),
            ("generic(khmer-mul)", GenericFamily::KhmerMul),
            ("generic(nastaliq)", GenericFamily::Nastaliq),
        ] {
            assert_eq!(
                family_entries(&FontRequest::of_family(name)).next(),
                Some(FamilyName::Generic(expected)),
                "{name}"
            );
        }
        // §2.1.2's grammar has no other `generic()` name, so an unknown one is
        // a `<font-family-name>` like any other unrecognised identifier.
        assert_eq!(
            family_entries(&FontRequest::of_family("generic(arabic)")).next(),
            Some(FamilyName::Named("generic(arabic)"))
        );
    }

    #[test]
    fn a_named_family_the_engine_does_not_have_falls_through_to_the_next() {
        assert_eq!(
            nominal_face(&FontRequest::of_family("\"PingFang SC\", monospace")),
            NominalFace::Monospace
        );
        assert_eq!(
            nominal_face(&FontRequest::of_family(
                "\"PingFang SC\", \"Microsoft YaHei\", sans-serif"
            )),
            NominalFace::Proportional
        );
    }

    #[test]
    fn a_list_with_no_available_family_ends_at_the_default_face() {
        assert_eq!(
            nominal_face(&FontRequest::of_family(
                "\"No Such Font\", \"Also Missing\""
            )),
            NominalFace::Proportional
        );
        assert_eq!(
            nominal_face(&FontRequest::initial()),
            NominalFace::Proportional
        );
    }

    #[test]
    fn a_monospace_face_gives_every_narrow_glyph_the_same_advance() {
        let advances = ['a', 'W', 'i', ' ', '0', '.']
            .map(|character| NominalFace::Monospace.advance_em(character));
        assert!(advances.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn a_monospace_face_gives_a_wide_glyph_two_cells() {
        assert_eq!(
            NominalFace::Monospace.advance_em('\u{6e32}'),
            NominalFace::Monospace.advance_em('a') * 2.0
        );
    }

    #[test]
    fn a_proportional_face_is_narrower_for_a_narrow_glyph_than_a_wide_one() {
        assert!(
            NominalFace::Proportional.advance_em('i')
                < NominalFace::Proportional.advance_em('\u{6e32}')
        );
        assert!(
            NominalFace::Proportional.advance_em(' ') < NominalFace::Proportional.advance_em('i')
        );
    }

    #[test]
    fn the_two_families_the_acceptance_probe_compares_measure_differently() {
        // `tests/real_site_tasks` compares a `sans-serif` and a `monospace`
        // paragraph; the reference path has to disagree about them for the
        // probe to mean anything.
        let width = |family: &'static str| {
            nominal_advance(
                nominal_face(&FontRequest::of_family(family)),
                "Rendering \u{6e32}\u{67d3}",
                32.0,
            )
        };
        let (sans, mono) = (width("sans-serif"), width("monospace"));
        assert!(
            (sans - mono).abs() > 0.5,
            "sans-serif and monospace are the same size to the reference path: {sans} and {mono}"
        );
    }

    #[test]
    fn family_names_match_caselessly_and_without_normalizing() {
        assert!(caseless_match("Segoe UI", "segoe ui"));
        assert!(caseless_match("SEGOE UI", "segoe ui"));
        assert!(!caseless_match("Segoe UI", "Segoe"));
        // §5.1 forbids normalization, so a decomposed and a precomposed name
        // are two names.
        assert!(!caseless_match("a\u{030a}ngstrom", "\u{e5}ngstrom"));
    }
}
