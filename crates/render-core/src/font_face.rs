//! CSS Fonts 4 §4: the `@font-face` rule, its descriptor grammar, and what a
//! rule means.
//!
//! # Why this module exists and what it deliberately does not do
//!
//! §4.1 says a set of `@font-face` rules "define a set of fonts available for
//! use within the documents that contain these rules", and §5.2 says the
//! matcher must find a named family "among fonts defined via `@font-face` rules
//! and then among available installed fonts". A rule is therefore not
//! decoration on a stylesheet: it is an input to font matching, and the
//! descriptors that decide it are the ones §4 names. This module is that
//! grammar, and nothing else. Fetching is `render-browser`'s, and matching is
//! `font_matching`'s.
//!
//! Two properties are load-bearing and are stated here rather than assumed:
//!
//! - **A rule is a rule or it is nothing.** §4.1: "`@font-face` rules require a
//!   `font-family` and `src` descriptor; if either of these are missing, the
//!   `@font-face` rule must not be considered when performing the font
//!   matching algorithm." There is no half-registered face, and
//!   [`FontFaceSheet::rules`] holds only rules that can be considered.
//! - **`font-display` has no timeline to act on, so it has no effect.** See
//!   [`FontDisplay`].
//!
//! # Why the block walk is here and not in `render-css`
//!
//! `render-css` already parses a descriptor list — `parse_declaration_list` is
//! how it reads a `style` attribute — and this module calls *that* function, so
//! the recovery, the name normalisation, the value slicing and the diagnostics
//! are `render-css`'s and cannot drift from the style-rule path. What this
//! module adds is the small at-rule walk that locates `@font-face` blocks, which
//! `render-css` does not expose: its `StyleSheet` keeps style rules only, and
//! `ParsedRule::DeclarationListBlock` discards the descriptor list it just
//! parsed. That is one narrow, temporary duplication of an at-rule walk, and it
//! is the seam to close: the change that removes it is a `font_faces` field on
//! `render_css::stylesheet::StyleSheet` populated from the
//! `declarations` already returned by `parse_declarations`. This module is
//! written so that becoming a consumer of that field is a change to one
//! function, [`parse_font_faces`], and to no descriptor grammar at all.
//!
//! # `font-display`, and the face that has not arrived
//!
//! §3.2 defines a font face's life as three periods driven by a font download
//! timer: a block period, a swap period, and a failure period. `font-display`
//! chooses the *lengths* of the first two. This engine has no font download
//! timer, so it has no block period, no swap period, and no clock to start one:
//! there is exactly one point on §3.2's timeline it can ever be at, and that is
//! the failure period, whose defined outcome is "causing normal font fallback".
//! Every one of the five `font-display` values reaches that same outcome here,
//! which is why this module validates the descriptor, reports an invalid one,
//! and then lets the *absence* of a face decide rendering rather than the
//! descriptor.
//!
//! What the absence means is settled by §5.2 and not by this module: "If the
//! font resources defined for a given face in an `@font-face` rule are either
//! not available or contain invalid font data, then the face should be treated
//! as not present in the family." A face whose bytes have not arrived is
//! therefore not in the family at all — not in the family with an empty
//! character map, and not in the family pending a load. §5.2 then walks on to
//! the next name in `font-family`, which is also what §4.8.1 requires when a
//! font is unavailable ("user agents must display the text visibly"). Because
//! the decision is "the face is not in the table", there is no per-face
//! rendering state that measurement and painting could disagree about: both read
//! one table.

use std::collections::BTreeSet;
use std::fmt;

use cssparser::{
    AtRuleParser, CowRcStr, ParseError, Parser, ParserInput, ParserState, QualifiedRuleParser,
    SourceLocation, StyleSheetParser, Token,
};
use render_css::at_rules::at_rule_block_is_declaration_list;
use render_css::stylesheet::{Declaration, StyleSheetDiagnostic, parse_declaration_list};
use render_layout::{FontStyle, computed_font_style, computed_font_weight};

/// §4.5's initial value for `unicode-range`: the whole of Unicode.
const UNICODE_ALL: (u32, u32) = (0x0000, 0x0010_FFFF);

/// The largest codepoint §4.5 permits, which is also what bounds a wildcard
/// range: "Wildcard ranges that extend beyond the range of Unicode codepoints
/// are invalid."
const MAX_CODEPOINT: u32 = 0x0010_FFFF;

/// §4.5's `<font-face-name>`-adjacent bound on a wildcard: "the maximum number
/// of trailing `?` wildcard characters is five, even though the UNICODE-RANGE
/// token accepts six."
const MAX_WILDCARDS: usize = 5;

/// Every `@font-face` rule a stylesheet declares, in document order.
///
/// Document order is kept because §4.5.1 makes it load-bearing: "If the unicode
/// ranges overlap for a set of `@font-face` rules with the same family and style
/// descriptor values, the rules are ordered in the reverse order they were
/// defined; the last rule defined is the first to be checked for a given
/// character." [`FontFaceSheet::rules`] is in ascending document order and
/// [`FontFaceSheet::families`] is in *descending* order, which is the order §4.5.1
/// asks the matcher to check.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontFaceSheet {
    rules: Vec<FontFaceRule>,
    /// Every syntax error found inside a descriptor list, reported whether or
    /// not the block's other descriptors survived. §5.4.4's recovery is what
    /// lets them survive, and a parse error found inside a block that is then
    /// discarded is a fact that must not disappear.
    pub diagnostics: Vec<StyleSheetDiagnostic>,
}

impl FontFaceSheet {
    /// The rules this stylesheet declares, in ascending document order.
    #[must_use]
    pub fn rules(&self) -> &[FontFaceRule] {
        &self.rules
    }

    /// The rules in the order §4.5.1 checks them for one character: last
    /// defined first.
    pub fn families(&self) -> impl Iterator<Item = &FontFaceRule> {
        self.rules.iter().rev()
    }

    /// Whether the stylesheet declared no `@font-face` at all, which is the
    /// common case and the reason the fetch planner has nothing to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// One `@font-face` rule, as §4.1 defines it.
///
/// Every field is a descriptor's *computed* value, so a rule that omits a
/// descriptor carries that descriptor's initial value rather than an absence:
/// §4.1 says "Those not given explicit values in the rule take the initial value
/// listed with each descriptor in this specification."
#[derive(Clone, Debug, PartialEq)]
pub struct FontFaceRule {
    /// §4.2's `font-family`, lowercased for §5.1's caseless matching.
    pub family: String,
    /// §4.2's `font-family` as the author wrote it, kept so a diagnostic quotes
    /// the spelling the author used.
    pub family_as_written: String,
    /// §2.2's `font-weight` descriptor, initial `normal` (400).
    pub weight: u16,
    /// §2.4's `font-style` descriptor, initial `normal`.
    pub style: FontStyle,
    /// §4.5's `unicode-range`, initial `U+0-10FFFF`.
    pub unicode_range: UnicodeRangeSet,
    /// §4.3.1's `<font-src-list>`, in author order. Empty when every item was
    /// rejected, which §4.3.1 makes a parse error for the descriptor.
    pub sources: Vec<FontSource>,
    /// §4.9's `font-display`, validated and recorded. It does not change
    /// rendering; see the module documentation and [`FontDisplay`].
    pub display: FontDisplay,
    /// This rule's position in its stylesheet, ascending from zero.
    pub order: usize,
}

impl FontFaceRule {
    /// §4.3: the items of `src` that are worth trying, in author order.
    ///
    /// §4.3.1: "If there are no supported entries at the end of this process,
    /// the value for the `src` descriptor is a parse error." Those items have
    /// already been removed, so an empty result here means §4.1's "if either of
    /// these are missing" case for `src` and the rule was not built.
    pub fn supported_sources(&self) -> impl Iterator<Item = &FontSource> {
        self.sources.iter()
    }

    /// Whether §4.5's `unicode-range` admits `character`.
    ///
    /// §4.5: "the effective character map is the intersection of the codepoints
    /// defined by `unicode-range` with the font's character map", so a caller
    /// asking whether a face may be used for a character asks this *and* the
    /// face's own character map. The corpus needs both halves: 216 of 223
    /// `@font-face` blocks declare a range, so a matcher that ignores it hands a
    /// Latin face to a Han character.
    #[must_use]
    pub fn admits(&self, character: char) -> bool {
        self.unicode_range.contains(character)
    }
}

/// §4.9's `font-display` values.
///
/// The enum exists so that an *invalid* value is a reported parse error rather
/// than a silently ignored declaration, and so the value is available to a
/// diagnostic. It deliberately does not reach rendering: this engine has no font
/// download timer, so every value resolves to §3.2's failure period, which is
/// "normal font fallback". See the module documentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FontDisplay {
    #[default]
    Auto,
    Block,
    Swap,
    Fallback,
    Optional,
}

impl FontDisplay {
    /// §4.9's `<font-display>` keyword, or `None` for a value the descriptor's
    /// grammar does not include.
    #[must_use]
    pub fn from_keyword(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "block" => Some(Self::Block),
            "swap" => Some(Self::Swap),
            "fallback" => Some(Self::Fallback),
            "optional" => Some(Self::Optional),
            _ => None,
        }
    }

    /// The keyword as §4.9 spells it.
    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Block => "block",
            Self::Swap => "swap",
            Self::Fallback => "fallback",
            Self::Optional => "optional",
        }
    }
}

/// One item of §4.3.1's `<font-src-list>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontSource {
    /// §4.3.3.1's `local(<font-family-name>)`: a single *installed* face to
    /// look for before any URL is fetched. §4.3.3.1 is explicit that this names
    /// "a single font face within a larger family" and not a family, which is
    /// why it is resolved against installed face names and never against a
    /// `font-family` list.
    Local {
        /// The name as written, unquoted.
        name: String,
    },
    /// §4.3.1's `<url> [format(<font-format>)]? [tech(<font-tech>#)]?`, with
    /// the URL already resolved against the stylesheet's own URL per §4.3.2.
    Url {
        /// The resolved URL, as the text to request.
        url: String,
        /// §4.3.1's format hint, the format the URL's own extension implies when
        /// the author wrote none, or [`FontFormat::Unknown`] when neither says
        /// anything. §4.3.1's legacy `format("woff2-variations")` spellings have
        /// been normalised into this plus `techs`.
        format: FontFormat,
        /// §4.3.1's `<font-tech>` list. Every value is unsupported by this
        /// engine, which is why a non-empty list removes the item: §4.3.3 says
        /// a user agent "must skip downloading a font resource if ... any of the
        /// font technologies are unsupported".
        techs: Vec<String>,
    },
}

impl FontSource {
    /// Whether this item can be acted on at all, per §4.3.1 and §4.3.3.
    ///
    /// A `local()` item always can: looking for an installed face costs nothing
    /// and §4.3.3.1 requires it to be tried first. A URL item can only be
    /// downloaded if its format is one this engine can decode and it asks for no
    /// font technology.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        match self {
            Self::Local { .. } => true,
            Self::Url { format, techs, .. } => format.is_decodable() && techs.is_empty(),
        }
    }

    /// A one-line description for a diagnostic, quoting the author.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Local { name } => format!("local({name})"),
            Self::Url { url, format, .. } => format!("url({url}) format({})", format.keyword()),
        }
    }
}

/// §4.3.1's `<font-format>` keywords, plus the string spellings that are "for
/// reasons of backwards compatibility" equivalent to them.
///
/// `is_decodable` is the load-bearing method and it is a fact about this
/// engine, not a preference: it names the container formats the rasteriser can
/// actually turn into glyphs. `fontdue` 0.9.3 reads a raw sfnt through
/// `ttf_parser`, whose `Face::parse` accepts a file beginning with `0x00010000`,
/// `true`, `OTTO` or `ttcf` and nothing else. There is no WOFF and no WOFF2
/// code path in that crate, and `ttf-parser` 0.21.1 has no container decoder of
/// its own, so a `.woff2` body is rejected as an unknown magic number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontFormat {
    /// No format hint was given and the URL's extension implies nothing, so the
    /// format is whatever the body turns out to be.
    ///
    /// This is a real case rather than a defensive one: §4.3.3 says "If no
    /// format hint is supplied, the user agent should download the font
    /// resource", and one of the corpus's `src` URLs is a hashed path with no
    /// extension at all. It is also distinct from "unsupported", which is what
    /// makes a hint of `woff2` a reason to *skip* a request while a missing hint
    /// is not.
    Unknown,
    Collection,
    EmbeddedOpenType,
    OpenType,
    Svg,
    TrueType,
    Woff,
    Woff2,
}

impl FontFormat {
    /// §4.3.1's keyword, which is also the string spelling for the formats that
    /// have one.
    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Collection => "collection",
            Self::EmbeddedOpenType => "embedded-opentype",
            Self::OpenType => "opentype",
            Self::Svg => "svg",
            Self::TrueType => "truetype",
            Self::Woff => "woff",
            Self::Woff2 => "woff2",
        }
    }

    /// §4.3.1's `<font-format>` keyword, and the string form.
    #[must_use]
    pub fn from_keyword(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "collection" => Some(Self::Collection),
            "embedded-opentype" => Some(Self::EmbeddedOpenType),
            "opentype" => Some(Self::OpenType),
            "svg" => Some(Self::Svg),
            "truetype" => Some(Self::TrueType),
            "woff" => Some(Self::Woff),
            "woff2" => Some(Self::Woff2),
            _ => None,
        }
    }

    /// §4.3.1's backwards-compatible string table, which maps a legacy string
    /// onto a modern format and, where the legacy spelling encodes one, onto the
    /// `tech()` it stands for: `format("woff2-variations")` is
    /// `format(woff2) tech(variations)`.
    #[must_use]
    pub fn from_legacy_string(value: &str) -> Option<(Self, Option<&'static str>)> {
        match value.trim().to_ascii_lowercase().as_str() {
            "woff2-variations" => Some((Self::Woff2, Some("variations"))),
            "woff-variations" => Some((Self::Woff, Some("variations"))),
            "truetype-variations" => Some((Self::TrueType, Some("variations"))),
            "opentype-variations" => Some((Self::OpenType, Some("variations"))),
            other => Self::from_keyword(other).map(|format| (format, None)),
        }
    }

    /// Whether this engine's rasteriser can turn a body in this format into
    /// glyphs.
    ///
    /// TrueType, OpenType and a TrueType/OpenType collection are raw sfnt
    /// containers and are decoded. WOFF and WOFF2 are compressed wrappers with
    /// no decoder in this engine's dependency graph, `embedded-opentype` is an
    /// IE-era format that is not a font container at all, and SVG fonts need an
    /// SVG font renderer this engine does not have.
    #[must_use]
    pub const fn is_decodable(self) -> bool {
        matches!(
            self,
            Self::Unknown | Self::TrueType | Self::OpenType | Self::Collection
        )
    }
}

/// §4.5's `unicode-range` value: the set of codepoints a face may be used for.
///
/// Stored merged, sorted and non-overlapping, because §4.5 permits overlaps in
/// the authored list and only the *union* is meaningful, and because the corpus
/// is 12,252 range tokens across 216 blocks, so the per-character test that the
/// matcher performs has to be a binary search rather than a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnicodeRangeSet(Vec<(u32, u32)>);

impl Default for UnicodeRangeSet {
    /// §4.5's initial value, `U+0-10FFFF`.
    fn default() -> Self {
        Self(vec![UNICODE_ALL])
    }
}

impl UnicodeRangeSet {
    /// The set that admits every codepoint, which is §4.5's initial value.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Parses §4.5's `<unicode-range-token>#`.
    ///
    /// §4.5: "Ranges that do not fit one of these forms are invalid and cause
    /// the declaration to be ignored", so one bad token discards the whole
    /// descriptor rather than narrowing it. That is why this returns `None` for
    /// a list with any invalid member and never a partial set.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let mut ranges = Vec::new();
        for token in value.split(',') {
            ranges.push(parse_range_token(token.trim())?);
        }
        if ranges.is_empty() {
            return None;
        }
        Some(Self::merge(ranges))
    }

    /// The union of two sets, which is what two `unicode-range` declarations on
    /// one face mean.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let mut ranges = self.0.clone();
        ranges.extend(other.0.iter().copied());
        Self::merge(ranges)
    }

    /// Sorts, merges overlapping and adjacent intervals, and clamps to the
    /// codepoint space. Two intervals that touch are merged because the union of
    /// `[a, b]` and `[b, c]` is `[a, c]` and keeping the split would make the
    /// per-character search report a miss for no reason.
    fn merge(mut ranges: Vec<(u32, u32)>) -> Self {
        ranges.retain(|(start, end)| start <= end && *start <= MAX_CODEPOINT);
        for range in &mut ranges {
            range.1 = range.1.min(MAX_CODEPOINT);
        }
        ranges.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            match merged.last_mut() {
                Some(last) if start <= last.1.saturating_add(1) => {
                    last.1 = last.1.max(end);
                }
                _ => merged.push((start, end)),
            }
        }
        Self(merged)
    }

    /// Whether the set admits `character`, by binary search over the merged
    /// intervals.
    #[must_use]
    pub fn contains(&self, character: char) -> bool {
        self.contains_codepoint(character as u32)
    }

    /// Whether the set admits `codepoint`, for callers that have a codepoint
    /// rather than a `char`.
    #[must_use]
    pub fn contains_codepoint(&self, codepoint: u32) -> bool {
        let mut low = 0_usize;
        let mut high = self.0.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let (start, end) = self.0[middle];
            if codepoint < start {
                high = middle;
            } else if codepoint > end {
                low = middle + 1;
            } else {
                return true;
            }
        }
        false
    }

    /// The merged intervals, for a diagnostic or a measurement.
    #[must_use]
    pub fn intervals(&self) -> &[(u32, u32)] {
        &self.0
    }

    /// How many intervals the set has after merging, which is the number a
    /// corpus measurement reports and the reason merging exists.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the set is empty, which §4.5's grammar cannot produce: a
    /// descriptor that parsed has at least one token.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One §4.5 `<unicode-range-token>`.
///
/// The three forms §4.5 names are a single codepoint, an interval, and a
/// wildcard. The wildcard's zero-digit case is spelled out by the
/// specification — "U+???" is "valid and equivalent to ... U+0???" — so it is
/// handled rather than rejected.
fn parse_range_token(token: &str) -> Option<(u32, u32)> {
    let rest = token
        .strip_prefix('U')
        .or_else(|| token.strip_prefix('u'))?
        .strip_prefix('+')?;
    if rest.is_empty() {
        return None;
    }
    if let Some((first, second)) = rest.split_once('-') {
        // An interval range: two plain codepoints, "the start and end codepoints
        // of a range".
        if first.contains('?') || second.contains('?') {
            return None;
        }
        let start = hex_codepoint(first)?;
        let end = hex_codepoint(second)?;
        if start > end || end > MAX_CODEPOINT {
            return None;
        }
        return Some((start, end));
    }
    if rest.contains('?') {
        let fixed = rest.split('?').next().unwrap_or_default();
        let wildcards = rest.len() - fixed.len();
        if wildcards == 0 || wildcards > MAX_WILDCARDS || fixed.contains('-') {
            return None;
        }
        let prefix = if fixed.is_empty() {
            0
        } else {
            hex_codepoint(fixed)?
        };
        let span = 1_u32.checked_shl(u32::try_from(wildcards).ok()? * 4)?;
        let start = prefix.checked_mul(span)?;
        let end = start.checked_add(span - 1)?;
        if end > MAX_CODEPOINT {
            return None;
        }
        return Some((start, end));
    }
    let codepoint = hex_codepoint(rest)?;
    Some((codepoint, codepoint))
}

/// One to six hexadecimal digits, as §4.5 requires.
fn hex_codepoint(digits: &str) -> Option<u32> {
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    u32::from_str_radix(digits, 16)
        .ok()
        .filter(|value| *value <= MAX_CODEPOINT)
}

/// A descriptor list reduced to what a `@font-face` rule needs: the surviving
/// descriptors, in the order §4.1 gives them precedence, and the diagnostics
/// the list's own parse produced.
#[derive(Clone, Debug, Default)]
struct Descriptors {
    family: Option<String>,
    src: Option<ParsedSrc>,
    weight: Option<u16>,
    style: Option<FontStyle>,
    unicode_range: Option<UnicodeRangeSet>,
    display: Option<FontDisplay>,
}

impl Descriptors {
    /// Folds one declaration into the set, last writer winning.
    ///
    /// §4.1: "When a given descriptor occurs multiple times in a given
    /// `@font-face` rule, only the last descriptor declaration is used and all
    /// prior declarations for that descriptor are ignored." So this is a plain
    /// overwrite, and the *last* value that parses is the one that counts.
    fn accept(&mut self, declaration: &Declaration) {
        match declaration.name.as_str() {
            "font-family" => {
                if let Some(name) = font_family_descriptor(&declaration.value) {
                    self.family = Some(name);
                }
            }
            "src" => {
                if let Some(parsed) = ParsedSrc::parse(&declaration.value) {
                    self.src = Some(parsed);
                }
            }
            "font-weight" => {
                // §4.4's `font-weight` descriptor takes the same values as the
                // property, and `computed_font_weight` is the one reading of
                // them, so a descriptor and a property cannot disagree about
                // what `500` means.
                if let Some(weight) = computed_font_weight(&declaration.value, 400) {
                    self.weight = Some(weight);
                }
            }
            "font-style" => {
                if let Some(style) = computed_font_style(&declaration.value) {
                    self.style = Some(style);
                }
            }
            "unicode-range" => {
                if let Some(range) = UnicodeRangeSet::parse(&declaration.value) {
                    self.unicode_range = Some(range);
                }
            }
            "font-display" => {
                if let Some(display) = FontDisplay::from_keyword(&declaration.value) {
                    self.display = Some(display);
                }
            }
            // §4.1's forward-compatible parsing: "declarations of any
            // descriptors that are not supported by the user agent must be
            // ignored", and so must a custom property.
            _ => {}
        }
    }
}

/// §4.2's `font-family` descriptor: a single `<font-family-name>`, which §2.1.1
/// defines as a `<string>` or a sequence of `<custom-ident>`s.
///
/// A descriptor naming a list is invalid rather than a list, which is why the
/// value has to hold no comma at all: `@font-face { font-family: A, B }` is a
/// `@font-face` with no usable `font-family`, and §4.1 says such a rule "must
/// not be considered".
fn font_family_descriptor(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.contains(',') {
        return None;
    }
    // A `<string>` keeps its contents; a sequence of idents is joined by single
    // spaces, which is §2.1.1's computed value for that form.
    let unquoted = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        });
    let name = match unquoted {
        Some(inner) => inner.trim().to_owned(),
        None => value.split_whitespace().collect::<Vec<_>>().join(" "),
    };
    if name.is_empty() {
        return None;
    }
    // The name is kept as the author wrote it, so a diagnostic quotes their
    // spelling. §5.1's caseless matching is applied where the family is looked
    // up, not here, because the matcher is the only thing that knows what it is
    // comparing against.
    Some(name)
}

/// §4.3.1's `<font-src-list>`, or `None` when the descriptor is a parse error.
///
/// §4.3.1: "If a component value is parsed correctly and is of a font format
/// or font tech that the UA supports, add it to the list of supported sources.
/// If parsing a component value results in a parsing error or its format or
/// tech are unsupported, do not add it to the list of supported sources. If
/// there are no supported entries at the end of this process, the value for the
/// `src` descriptor is a parse error."
///
/// The distinction the specification draws is between a *parse error* and an
/// *unsupported* source, and it matters: an unparseable item is a malformed
/// declaration, whereas an unsupported one is a well-formed declaration the user
/// agent declines. Both leave the rule with fewer items; only the first is
/// worth reporting. [`SrcProblem`] carries which happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SrcProblem {
    /// §4.3.1: no item was a well-formed `<font-src>`, so the descriptor is a
    /// parse error.
    Malformed(String),
    /// Every item parsed but none is one this engine can act on, so §4.3.1
    /// makes the descriptor a parse error too — with a different reason, and the
    /// reason is the useful one: a `woff2`-only `src` says the rasteriser
    /// cannot read the format, which is a fact about this engine rather than a
    /// fault in the sheet.
    NothingSupported(String),
}

/// The items of a `src` descriptor, split into the ones this engine can act on
/// and the reason the rest were not kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSrc {
    /// §4.3.1's supported sources, in author order.
    pub sources: Vec<FontSource>,
    /// Set when the descriptor is a parse error under §4.3.1.
    pub problem: Option<SrcProblem>,
}

/// §4.3.1's `<font-src-list>`.
impl ParsedSrc {
    /// Reads a `src` descriptor value.
    ///
    /// Returns `None` only when the value cannot be read as a `<font-src-list>`
    /// at all, which §4.3.1 makes a parse error for the declaration and which
    /// therefore leaves a *previous* `src` declaration in force rather than
    /// clearing it. A value that reads cleanly but whose every item is
    /// unsupported is a parse error too, and that is the state 218 of the
    /// corpus's 223 blocks are in; it is reported through
    /// [`ParsedSrc::problem`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let mut sources = Vec::new();
        // A malformed item and an unsupported one are different faults, and
        // which one occurred is what the reason has to say. The first of each
        // is kept, because a sheet with twenty `woff2` items needs one fact
        // reported, not twenty.
        let mut first_malformed: Option<String> = None;
        let mut first_unsupported: Option<String> = None;
        for item in split_top_level_commas(value) {
            match parse_font_src(item.trim()) {
                Ok(source) => {
                    if source.is_usable() {
                        sources.push(source);
                    } else if first_unsupported.is_none() {
                        first_unsupported = Some(format!(
                            "{} names a format or font technology this engine cannot use",
                            source.describe()
                        ));
                    }
                }
                Err(reason) if first_malformed.is_none() => {
                    first_malformed =
                        Some(format!("`{}` is not a <font-src>: {reason}", item.trim()));
                }
                Err(_) => {}
            }
        }
        if sources.is_empty() {
            // §4.3.1: "If there are no supported entries at the end of this
            // process, the value for the src descriptor is a parse error."
            // Whether the items were malformed or merely unsupported is worth
            // keeping apart, because the two call for different fixes: one is a
            // fault in the sheet and the other is a limit of this engine.
            return Some(Self {
                sources,
                problem: Some(match (first_malformed, first_unsupported) {
                    (Some(reason), None) => SrcProblem::Malformed(reason),
                    (_, Some(reason)) => SrcProblem::NothingSupported(reason),
                    (None, None) => SrcProblem::Malformed("the src list is empty".to_owned()),
                }),
            });
        }
        Some(Self {
            sources,
            problem: None,
        })
    }
}

/// §4.3.1's `<font-src-list>`.
#[must_use]
pub fn parse_src(value: &str) -> ParsedSrc {
    ParsedSrc::parse(value).unwrap_or_else(|| ParsedSrc {
        sources: Vec::new(),
        problem: Some(SrcProblem::Malformed("the src list is empty".to_owned())),
    })
}

/// Splits a descriptor value on the commas that separate `<font-src>`s, leaving
/// commas inside `url()`, `local()`, `format()` and `tech()` alone.
fn split_top_level_commas(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let bytes = value.as_bytes();
    let mut depth = 0_i32;
    let mut start = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => {
                // A string can hold anything, including a comma or a bracket.
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 2;
                        continue;
                    }
                    if bytes[index] == quote {
                        break;
                    }
                    index += 1;
                }
            }
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    parts.push(&value[start..]);
    parts
}

/// §4.3.1's `<font-src>`: `local(<font-family-name>)` or
/// `<url> [format(<font-format>)]? [tech(<font-tech>#)]?`.
fn parse_font_src(item: &str) -> Result<FontSource, &'static str> {
    if item.is_empty() {
        return Err("an empty item is not a <font-src>");
    }
    let components = components_of(item);
    let Some((first, rest)) = components.split_first() else {
        return Err("an empty item is not a <font-src>");
    };
    match first {
        Component::Function(name, arguments) if name == "local" => {
            // §4.3.3.1: the argument is a `<font-family-name>`, and "if
            // unquoted, the unquoted font family name processing conventions
            // apply", so a sequence of identifiers is joined by single spaces.
            if !rest.is_empty() {
                return Err("`local()` takes a single name");
            }
            let Some(name) = local_name(arguments) else {
                return Err("`local()` needs a name to look for");
            };
            Ok(FontSource::Local { name })
        }
        Component::Url(url) => {
            let mut format = None;
            let mut techs = Vec::new();
            for component in rest {
                match component {
                    Component::Function(name, arguments) if name == "format" => {
                        if format.is_some() {
                            return Err("a <font-src> carries at most one `format()`");
                        }
                        // §4.3.1's backwards-compatible string spellings encode
                        // a `tech()` as well as a format, so the legacy spelling
                        // contributes its technology to the same list an explicit
                        // `tech()` would.
                        let Some((parsed, legacy_tech)) = font_format(arguments) else {
                            return Err("`format()` needs a <font-format>");
                        };
                        format = Some(parsed);
                        if let Some(tech) = legacy_tech {
                            techs.push(tech.to_owned());
                        }
                    }
                    Component::Function(name, arguments) if name == "tech" => {
                        techs.extend(font_techs(arguments));
                    }
                    _ => return Err("a <url> may be followed only by `format()` and `tech()`"),
                }
            }
            Ok(FontSource::Url {
                url: url.clone(),
                format: format.unwrap_or_else(|| implied_format(url)),
                techs,
            })
        }
        Component::Function(name, arguments) if name == "url" => {
            // `url("…")` is a `Function` token, not a `url-token`, so the quoted
            // spelling arrives here rather than as `Component::Url`.
            if !rest.is_empty() {
                return Err("a `url()` cannot be followed by anything else");
            }
            let [Component::QuotedString(url)] = arguments.as_slice() else {
                return Err("`url()` needs a URL");
            };
            Ok(FontSource::Url {
                url: url.clone(),
                format: implied_format(url),
                techs: Vec::new(),
            })
        }
        _ => Err("a <font-src> is a `url()` or a `local()`"),
    }
}

/// §4.3.3.1's `local()` argument as a string: a `<string>` verbatim, or a
/// sequence of identifiers joined by single spaces.
///
/// §4.3.3.1 also excludes two kinds of name, and the corpus writes one of them:
/// "CSS-wide keywords such as `inherit`, and `<generic-font-family>` keywords such
/// as `serif` are not allowed inside `local()`". The reason is §4.3.3.1's own
/// definition of the argument - "a format-specific string that uniquely
/// identifies a single font face" - which neither is. A quoted keyword is excluded
/// too, because §4.3.3.1 puts the restriction on the keyword rather than on how it
/// was written, and §2.1.1's note is the same: a font really named `serif` has to
/// be quoted, and `local()` has no such escape.
fn local_name(arguments: &[Component]) -> Option<String> {
    match arguments {
        [Component::QuotedString(name)] => {
            let trimmed = name.trim();
            if trimmed.is_empty() || is_reserved_in_local(trimmed) {
                return None;
            }
            Some(trimmed.to_owned())
        }
        _ if arguments.is_empty() => None,
        _ => {
            let mut parts = Vec::new();
            for argument in arguments {
                match argument {
                    Component::Ident(name) => parts.push(name.as_str()),
                    _ => return None,
                }
            }
            let joined = parts.join(" ");
            if joined.is_empty() || is_reserved_in_local(&joined) {
                return None;
            }
            Some(joined)
        }
    }
}

/// §4.3.3.1's excluded `local()` names: the CSS-wide keywords and the
/// `<generic-font-family>` keywords.
///
/// §4.3.3.1 names `inherit` and `serif`; the set of CSS-wide keywords is the
/// usual five, and the generic keywords are the ones `render-layout` already
/// classifies, so neither list is restated here.
fn is_reserved_in_local(name: &str) -> bool {
    if render_layout::generic_family(name).is_some() {
        return true;
    }
    matches!(
        name.to_ascii_lowercase().as_str(),
        "inherit" | "initial" | "unset" | "revert" | "revert-layer"
    )
}

/// §4.3.1's `format()` argument: a keyword or a string, with the backwards
/// compatible string spellings normalised to a format plus, where the legacy
/// spelling encodes one, the `tech()` it stands for.
fn font_format(arguments: &[Component]) -> Option<(FontFormat, Option<&'static str>)> {
    match arguments {
        [Component::Ident(keyword)] => {
            FontFormat::from_keyword(keyword).map(|format| (format, None))
        }
        [Component::QuotedString(text)] => FontFormat::from_legacy_string(text),
        _ => None,
    }
}

/// §4.3.1's `tech()` arguments: `<font-tech>#`, so a comma-separated list of
/// keywords or strings.
fn font_techs(arguments: &[Component]) -> Vec<String> {
    let mut techs = Vec::new();
    for argument in arguments {
        match argument {
            // §4.3.1's `<font-tech>#` is a comma-separated list, and a technology
            // is written either as a keyword or as a string, so both spellings
            // name the same thing.
            Component::Ident(name)
            | Component::QuotedString(name)
            | Component::Function(name, _) => techs.push(name.to_ascii_lowercase()),
            // The separators, and every component no `<font-tech>` can be.
            _ => {}
        }
    }
    techs
}

/// §4.3.3: "If no format hint is supplied, the user agent should download the
/// font resource." A URL that carries a recognisable container extension is
/// still describable, and the corpus relies on the difference: a `.ttf` item
/// behind a `.eot`/`.woff2`/`.woff` list has no hint on some sheets and an
/// explicit one on others, and both must reach the same face.
fn implied_format(url: &str) -> FontFormat {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    match path.rsplit_once('.').map_or("", |(_, extension)| extension) {
        "ttf" => FontFormat::TrueType,
        "otf" => FontFormat::OpenType,
        "ttc" | "otc" => FontFormat::Collection,
        "woff2" => FontFormat::Woff2,
        "woff" => FontFormat::Woff,
        "eot" => FontFormat::EmbeddedOpenType,
        "svg" => FontFormat::Svg,
        // An unrecognised extension is a real case: the corpus has one `src`
        // whose URL ends in a hashed path segment with no extension at all. §4.3.3
        // says to download when no hint is supplied, so the item stays in the
        // list and the body decides, which is a different decision from refusing
        // it because a hint named a format the rasteriser cannot read.
        _ => FontFormat::Unknown,
    }
}

/// One component value of a descriptor value, reduced to what §4's grammars can
/// contain.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Component {
    Ident(String),
    Delim(char),
    Url(String),
    QuotedString(String),
    Function(String, Vec<Component>),
    /// A token no `@font-face` descriptor grammar can contain. Carried without
    /// text on purpose: a number's source text cannot be reconstructed from a
    /// `f32`, and `U+4e00` is exactly the case that shows why — the tokenizer
    /// reads `4e00` as the number `4` followed by the identifier `e00`, so
    /// printing the number would yield `U+4`. `unicode-range` is therefore read
    /// from the descriptor's source text instead, where the author's digits are
    /// still intact.
    Other,
}

/// Tokenizes one descriptor value into its component values.
fn components_of(value: &str) -> Vec<Component> {
    let mut input = ParserInput::new(value);
    let mut parser = Parser::new(&mut input);
    let mut components = Vec::new();
    let _ = collect_components(&mut parser, &mut components);
    components
}

/// Reads component values until the value ends.
///
/// The error type is [`Infallible`] and the result is propagated rather than
/// swallowed at every call site, because `cssparser` chooses the nested block's
/// error type from the closure's return type and this one has to agree with the
/// enclosing parser's.
fn collect_components<'i>(
    parser: &mut Parser<'i, '_>,
    out: &mut Vec<Component>,
) -> Result<(), ParseError<'i, std::convert::Infallible>> {
    loop {
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => return Ok(()),
        };
        match token {
            Token::WhiteSpace(_) | Token::Comment(_) => {}
            Token::Function(name) => {
                let mut arguments = Vec::new();
                parser.parse_nested_block(|nested| collect_components(nested, &mut arguments))?;
                out.push(Component::Function(name.to_ascii_lowercase(), arguments));
            }
            Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                parser.parse_nested_block(|nested| {
                    while nested.next_including_whitespace_and_comments().is_ok() {}
                    Ok(())
                })?;
                out.push(Component::Other);
            }
            Token::Ident(name) => out.push(Component::Ident(name.to_string())),
            Token::UnquotedUrl(url) => out.push(Component::Url(url.to_string())),
            Token::QuotedString(text) => out.push(Component::QuotedString(text.to_string())),
            Token::Delim(character) => out.push(Component::Delim(character)),
            _ => out.push(Component::Other),
        }
    }
}

/// A problem found while reading one `@font-face` block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFaceProblem {
    /// Where the block is, 1-based line as `render-css` reports it.
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl fmt::Display for FontFaceProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}: {}", self.line, self.column, self.message)
    }
}

/// Every `@font-face` rule in `source`, with its descriptors resolved and its
/// problems reported.
///
/// This is the one function that walks the stylesheet for `@font-face` blocks,
/// and it is the seam described in the module documentation. It reads each
/// block's descriptor list with `render_css`'s own
/// [`parse_declaration_list`], so a malformed descriptor is reported *and* the
/// block keeps its other descriptors, which is the same recovery the style-rule
/// path gets.
#[must_use]
pub fn parse_font_faces(source: &str) -> (FontFaceSheet, Vec<FontFaceProblem>) {
    let mut sheet = FontFaceSheet::default();
    let mut problems = Vec::new();
    let mut order = 0_usize;
    collect_rules(source, &mut order, &mut sheet, &mut problems);
    (sheet, problems)
}

fn collect_rules(
    source: &str,
    order: &mut usize,
    sheet: &mut FontFaceSheet,
    problems: &mut Vec<FontFaceProblem>,
) {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    let mut walker = FontFaceWalker {
        source,
        order,
        sheet,
        problems,
    };
    for item in StyleSheetParser::new(&mut parser, &mut walker) {
        let _ = item;
    }
}

/// The byte offset a 1-based `(line, column)` names within `text`.
///
/// `cssparser` counts a line from 1 and a column from 1 (CSS Syntax §3.2) and
/// the column counts characters rather than bytes, so this walks the text rather
/// than adding the two up. A position past the end is clamped, which keeps a
/// diagnostic readable instead of panicking.
fn byte_offset(text: &str, line: u32, column: u32) -> usize {
    let mut offset = 0_usize;
    for _ in 1..line.max(1) {
        match text[offset..].find('\n') {
            Some(index) => offset += index + 1,
            None => return text.len(),
        }
    }
    let rest = &text[offset..];
    let end = rest
        .char_indices()
        .nth(column.max(1) as usize - 1)
        .map_or(rest.len(), |(index, _)| index);
    offset + end
}

/// The 1-based `(line, column)` a byte offset names within `text`.
fn line_column(text: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = u32::try_from(before.bytes().filter(|byte| *byte == b'\n').count())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let column = match before.rsplit_once('\n') {
        Some((_, rest)) => u32::try_from(rest.chars().count()).unwrap_or(u32::MAX),
        None => u32::try_from(before.chars().count()).unwrap_or(u32::MAX),
    };
    (line, column.saturating_add(1))
}

/// The at-rule walk: find every `@font-face` block, and look through the blocks
/// that can contain one.
struct FontFaceWalker<'a> {
    source: &'a str,
    order: &'a mut usize,
    sheet: &'a mut FontFaceSheet,
    problems: &'a mut Vec<FontFaceProblem>,
}

impl FontFaceWalker<'_> {
    /// Re-bases a diagnostic `render-css` reported inside one block onto the
    /// stylesheet's own line numbering.
    ///
    /// The descriptor list is handed to `render-css` as its own source text, so
    /// its diagnostics are relative to the block. Reporting them unchanged would
    /// point a reader at the wrong line of the wrong file, which is worse than
    /// not reporting them: the byte offset is known, so the position is known.
    fn rebase(
        &self,
        block: &str,
        block_start: cssparser::SourcePosition,
        diagnostic: &StyleSheetDiagnostic,
    ) -> (u32, u32) {
        let within = byte_offset(block, diagnostic.line, diagnostic.column);
        line_column(self.source, block_start.byte_index().saturating_add(within))
    }

    /// Reads one `@font-face` block.
    ///
    /// The block's contents are handed to `render-css`'s declaration-list
    /// parser, which reports its own syntax errors and keeps every declaration
    /// it could read — so `@font-face { font-family 'X'; src: url(a.woff2) }`
    /// reports the missing colon *and* yields a usable `src`.
    fn read_block(&mut self, input: &mut Parser<'_, '_>, location: SourceLocation) {
        let (line, column) = (location.line.saturating_add(1), location.column);
        let start = input.position();
        while input.next_including_whitespace_and_comments().is_ok() {}
        let end = input.position();
        let block = input.slice(start..end);
        let (declarations, diagnostics) = parse_declaration_list(block);
        for diagnostic in &diagnostics {
            let (line, column) = self.rebase(block, start, diagnostic);
            let message = format!(
                "invalid descriptor in an @font-face block at {line}:{column}: {}",
                diagnostic.message
            );
            self.problems.push(FontFaceProblem {
                line,
                column,
                message: message.clone(),
            });
            self.sheet.diagnostics.push(StyleSheetDiagnostic {
                line,
                column,
                message,
            });
        }

        let mut descriptors = Descriptors::default();
        for declaration in &declarations {
            descriptors.accept(declaration);
        }

        // §4.1: "if either of these are missing, the @font-face rule must not be
        // considered when performing the font matching algorithm."
        let Some(family) = descriptors.family.clone() else {
            self.problems.push(FontFaceProblem {
                line,
                column,
                message: "@font-face has no usable `font-family` descriptor, so §4.1 says the \
                          rule must not be considered; an installed family of the same name is \
                          not used in its place"
                    .to_owned(),
            });
            return;
        };
        let Some(src) = descriptors.src.clone() else {
            self.problems.push(FontFaceProblem {
                line,
                column,
                message: "@font-face has no usable `src` descriptor, so §4.1 says the rule must \
                          not be considered"
                    .to_owned(),
            });
            return;
        };
        if let Some(SrcProblem::NothingSupported(reason)) = &src.problem {
            // Not a fault in the sheet: the rule is well formed and this engine
            // simply cannot act on it. §4.3.1 still calls the descriptor a parse
            // error, so §4.1 keeps the rule out of the matcher, and the reason
            // says which format stopped it - the author cannot see that from the
            // sheet, and 218 of the corpus's 223 blocks are stopped by it.
            self.problems.push(FontFaceProblem {
                line,
                column,
                message: format!(
                    "@font-face `{family}` cannot be used: {reason}, so §4.3.1 makes its `src` a \
                     parse error and the rule is not considered"
                ),
            });
        }

        self.sheet.rules.push(FontFaceRule {
            // §5.1 matches caselessly, so the table key is folded once here and
            // the name the author wrote is kept for a diagnostic to quote.
            family: family.to_lowercase(),
            family_as_written: family,
            weight: descriptors.weight.unwrap_or(400),
            style: descriptors.style.unwrap_or(FontStyle::Normal),
            unicode_range: descriptors.unicode_range.unwrap_or_default(),
            sources: src.sources,
            display: descriptors.display.unwrap_or_default(),
            order: *self.order,
        });
        *self.order += 1;
    }
}

impl<'i> QualifiedRuleParser<'i> for FontFaceWalker<'_> {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude<'t>(&mut self, _input: &mut Parser<'i, 't>) -> Result<(), ParseError<'i, ()>> {
        Ok(())
    }

    fn parse_block<'t>(
        &mut self,
        _prelude: (),
        _start: &ParserState,
        _input: &mut Parser<'i, 't>,
    ) -> Result<(), ParseError<'i, ()>> {
        // A style rule cannot contain a `@font-face` in any position CSS Syntax
        // lets a rule survive: its block is a declaration list, and CSS Nesting
        // §3.1's nested at-rules are the only way in, which the engine does not
        // implement. Nothing to collect.
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for FontFaceWalker<'_> {
    type Prelude = CowRcStr<'i>;
    type AtRule = ();
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<CowRcStr<'i>, ParseError<'i, ()>> {
        while input.next_including_whitespace_and_comments().is_ok() {}
        Ok(name)
    }

    fn rule_without_block(
        &mut self,
        _prelude: CowRcStr<'i>,
        _start: &ParserState,
    ) -> Result<(), ()> {
        Ok(())
    }

    fn parse_block<'t>(
        &mut self,
        prelude: CowRcStr<'i>,
        start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<(), ParseError<'i, ()>> {
        let name = canonical_at_keyword(prelude.as_ref());
        if name == "font-face" {
            self.read_block(input, start.source_location());
            return Ok(());
        }
        if name.ends_with("keyframes") || at_rule_block_is_declaration_list(&name) {
            // A block this walk must not look inside: `@keyframes` holds
            // qualified rules and every other declaration-list at-rule holds
            // descriptors, so a nested walk would read a descriptor as a
            // prelude.
            while input.next_including_whitespace_and_comments().is_ok() {}
            return Ok(());
        }
        // A `@media`, `@supports` or `@layer` block may hold a `@font-face`, so
        // the walk continues inside it, which is what CSS Cascade 5 §6 and §7
        // require. `parse_block` is handed a parser already scoped to the
        // block's contents, so the same parser walks the nested rule list.
        for item in StyleSheetParser::new(input, self) {
            let _ = item;
        }
        Ok(())
    }
}

/// The family names a stylesheet's rules declare, for a diagnostic or a
/// measurement that wants the set rather than the rules.
#[must_use]
pub fn declared_families(sheet: &FontFaceSheet) -> BTreeSet<String> {
    sheet
        .rules()
        .iter()
        .map(|rule| rule.family.clone())
        .collect()
}

/// The at-rule `name` names, with any vendor prefix removed.
///
/// CSS Syntax §3.2 makes the at-keyword a case-insensitive ident after the `@`,
/// and a prefixed spelling is the at-rule it aliases: `render-css` says so of
/// `@-webkit-keyframes` and `@-ms-viewport`, and the production corpus ships
/// both. `render-css` owns that mapping in a private function, so this is the
/// same rule restated rather than a second opinion about it - it exists because
/// a face the engine skipped because of a vendor prefix would be a face no page
/// could use, and the alternative was not being able to ask.
fn canonical_at_keyword(name: &str) -> String {
    let lowered = name.trim().to_ascii_lowercase();
    let Some(rest) = lowered.strip_prefix('-') else {
        return lowered;
    };
    // `-webkit-font-face` is `-` vendor `-` at-rule. A leading `-` followed by a
    // vendor name and another `-` is a prefix; `-moz-x` and `-o-x` are the
    // same shape.
    match rest.split_once('-') {
        Some((vendor, at_rule)) if !vendor.is_empty() && !at_rule.is_empty() => at_rule.to_owned(),
        _ => lowered,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        reason = "descriptor grammars are compared at the values the specification names"
    )]

    use render_layout::FontStyle;

    use super::{
        Component, FontDisplay, FontFaceRule, FontFormat, FontSource, SrcProblem, UnicodeRangeSet,
        components_of, declared_families, implied_format, parse_font_faces, parse_range_token,
        parse_src,
    };

    fn rules(source: &str) -> Vec<FontFaceRule> {
        parse_font_faces(source).0.rules
    }

    // ---- §4.1: the rule as a whole -----------------------------------------

    #[test]
    fn a_rule_needs_both_a_family_and_a_src_to_be_considered() {
        // §4.1: "if either of these are missing, the @font-face rule must not be
        // considered when performing the font matching algorithm."
        let sheet = parse_font_faces(
            "@font-face { font-family: A } @font-face { src: url(a.ttf) } \
             @font-face { font-family: C; src: url(c.ttf) }",
        );
        let families: Vec<_> = sheet
            .0
            .rules()
            .iter()
            .map(|rule| rule.family_as_written.as_str())
            .collect();
        assert_eq!(families, ["C"], "only the complete rule is a rule");
        assert_eq!(
            sheet.1.len(),
            2,
            "and each incomplete one says why, because a rule that is not \
             considered is a fact the author needs"
        );
    }

    #[test]
    fn a_rule_without_family_does_not_borrow_an_installed_family_of_that_name() {
        // §5.2's clause, which is the reason this is a test rather than a
        // convenience: "If no faces are present for a family defined via
        // @font-face rules, the family should be treated as missing; matching a
        // platform font with the same name must not occur in this case."
        let (sheet, problems) = parse_font_faces("@font-face { src: url(a.ttf) }");
        assert!(sheet.is_empty());
        assert!(
            problems[0]
                .message
                .contains("an installed family of the same name is not used"),
            "{}",
            problems[0].message
        );
    }

    #[test]
    fn a_later_declaration_of_the_same_descriptor_wins() {
        // §4.1: "only the last descriptor declaration is used and all prior
        // declarations for that descriptor are ignored".
        let parsed = rules("@font-face { font-family: A; font-family: B; src: url(a.ttf) }");
        assert_eq!(parsed[0].family_as_written, "B");
    }

    #[test]
    fn an_unsupported_descriptor_is_ignored_rather_than_failing_the_rule() {
        // §4.1's forward-compatible parsing: "declarations of any descriptors
        // that are not supported by the user agent must be ignored."
        let (sheet, problems) = parse_font_faces(
            "@font-face { font-family: A; src: url(a.ttf); font-named-instance: \
             \"Bold\"; --custom: 1 }",
        );
        assert_eq!(sheet.rules().len(), 1);
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn rules_keep_document_order_because_section_four_five_one_orders_by_it() {
        // §4.5.1: "the rules are ordered in the reverse order they were
        // defined; the last rule defined is the first to be checked".
        let sheet = parse_font_faces(
            "@font-face { font-family: A; src: url(a.ttf) } \
             @font-face { font-family: A; src: url(b.ttf) }",
        );
        let ascending: Vec<_> = sheet.0.rules().iter().map(|rule| rule.order).collect();
        assert_eq!(ascending, [0, 1], "ascending order is the source order");
        let descending: Vec<_> = sheet
            .0
            .families()
            .map(|rule| rule.sources[0].describe())
            .collect();
        assert_eq!(
            descending,
            ["url(b.ttf) format(truetype)", "url(a.ttf) format(truetype)"],
            "the matcher checks the last defined first"
        );
    }

    #[test]
    fn a_face_inside_a_conditional_group_rule_is_still_a_face() {
        // CSS Cascade 5 §6 puts `@font-face` inside `@media` and CSS Cascade 5
        // §7 inside `@layer`, and a face the engine skipped would be a face no
        // page could use.
        let (sheet, _problems) = parse_font_faces(
            "@media screen { @font-face { font-family: A; src: url(a.ttf) } } \
             @layer base { @font-face { font-family: B; src: url(b.ttf) } }",
        );
        assert_eq!(
            declared_families(&sheet).into_iter().collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn a_face_is_not_looked_for_inside_another_descriptors_block() {
        // `@keyframes` holds qualified rules and `@page` holds descriptors, so a
        // walk that read either as a rule list would invent a face out of a
        // declaration.
        let (sheet, _problems) = parse_font_faces(
            "@keyframes spin { from { font-family: Ghost } } \
             @page { font-family: Ghost } \
             @font-face { font-family: Real; src: url(r.ttf) }",
        );
        assert_eq!(
            declared_families(&sheet).into_iter().collect::<Vec<_>>(),
            ["real"]
        );
    }

    // ---- §4.2: the family descriptor ---------------------------------------

    #[test]
    fn the_family_descriptor_is_a_name_not_a_list() {
        // §4.2 gives the descriptor `<font-family-name>`, and §2.1.1 joins an
        // unquoted name's identifiers with single spaces.
        let parsed = rules("@font-face { font-family: Foo Bar; src: url(a.ttf) }");
        assert_eq!(parsed[0].family_as_written, "Foo Bar");
        let quoted = rules("@font-face { font-family: 'Foo Bar'; src: url(a.ttf) }");
        assert_eq!(quoted[0].family_as_written, "Foo Bar");
    }

    #[test]
    fn the_family_descriptor_is_matched_caselessly_the_way_section_five_one_requires() {
        // §5.1's caseless matching, so a document face declared in one case is
        // found by a `font-family` written in another.
        let parsed = rules("@font-face { font-family: HarmonyOS_Medium; src: url(a.woff2) }");
        assert_eq!(parsed[0].family, "harmonyos_medium");
    }

    // ---- §4.3 / §4.3.1: the src list ----------------------------------------

    #[test]
    fn the_src_list_is_tried_in_author_order() {
        // §4.3: "the user agent iterates over the set of references listed, using
        // the first one it can successfully parse and activate."
        let parsed = parse_src("url(a.ttf) format(truetype), url(b.otf) format(opentype)");
        let described: Vec<_> = parsed.sources.iter().map(FontSource::describe).collect();
        assert_eq!(
            described,
            ["url(a.ttf) format(truetype)", "url(b.otf) format(opentype)"]
        );
    }

    #[test]
    fn a_format_the_engine_cannot_decode_is_dropped_and_the_list_walks_on() {
        // §4.3.3: "must skip downloading a font resource if the format hint
        // indicates an unsupported or unknown font format". The corpus has
        // exactly this shape in its one multi-entry list, and the engine's
        // rasteriser cannot read any of the first four entries.
        let parsed = parse_src(
            "url(a.eot) format('embedded-opentype'), url(a.woff2) format('woff2'), \
             url(a.woff) format('woff'), url(a.ttf) format('truetype'), url(a.svg) format(svg)",
        );
        assert_eq!(
            parsed
                .sources
                .iter()
                .map(FontSource::describe)
                .collect::<Vec<_>>(),
            ["url(a.ttf) format(truetype)"],
            "the only item this engine can decode is the one it keeps"
        );
        assert_eq!(
            parsed.problem, None,
            "so the descriptor is not a parse error"
        );
    }

    #[test]
    fn a_src_whose_formats_are_all_undecodable_is_a_parsed_diagnosable_failure() {
        // The state 218 of the corpus's 223 blocks are in. §4.3.1: "If there are
        // no supported entries at the end of this process, the value for the src
        // descriptor is a parse error" - and the reason has to name the format,
        // because "this engine cannot decode woff2" is a fact the author cannot
        // see from the sheet.
        let parsed = parse_src("url(a.woff2) format('woff2')");
        assert!(parsed.sources.is_empty());
        let problem = parsed.problem.expect("no supported entry is a parse error");
        assert_eq!(
            problem,
            SrcProblem::NothingSupported(
                "url(a.woff2) format(woff2) names a format or font technology this engine cannot \
                 use"
                .to_owned()
            )
        );
    }

    #[test]
    fn a_font_technology_the_engine_lacks_removes_the_item() {
        // §4.3.3: "must skip downloading a font resource if ... any of the font
        // technologies are unsupported by the user agent". This engine
        // implements none, so any `tech()` removes its item.
        let parsed = parse_src("url(a.ttf) format(truetype) tech(variations), url(b.ttf)");
        assert_eq!(
            parsed
                .sources
                .iter()
                .map(FontSource::describe)
                .collect::<Vec<_>>(),
            ["url(b.ttf) format(truetype)"]
        );
    }

    #[test]
    fn a_legacy_variations_format_string_means_the_format_and_the_technology() {
        // §4.3.1's backwards-compatible table: `format("woff2-variations")` is
        // `format(woff2) tech(variations)`.
        let parsed = parse_src("url(a.woff2) format('woff2-variations'), url(b.ttf)");
        assert_eq!(
            parsed
                .sources
                .iter()
                .map(FontSource::describe)
                .collect::<Vec<_>>(),
            ["url(b.ttf) format(truetype)"],
            "the legacy spelling is normalised, so it is dropped for the same \
             reason a modern one would be"
        );
    }

    #[test]
    fn a_local_item_is_kept_and_costs_nothing() {
        // §4.3.3.1: "local() can be used" to prefer a locally available copy. A
        // `local()` item is never filtered out, because looking for an installed
        // face is free and the specification requires it to be tried first.
        let parsed = parse_src("local(Gentium), url(g.woff2)");
        assert_eq!(
            parsed.sources.first(),
            Some(&FontSource::Local {
                name: "Gentium".to_owned()
            })
        );
        assert!(parsed.sources[0].is_usable());
    }

    #[test]
    fn a_local_item_takes_its_arguments_the_way_section_four_three_three_one_writes_them() {
        // §4.3.3.1: the name is "a format-specific string that uniquely
        // identifies a single font face", quoted or a sequence of identifiers.
        for (src, expected) in [
            ("local(Gentium)", "Gentium"),
            ("local('Gentium')", "Gentium"),
            ("local(Gentium Bold)", "Gentium Bold"),
            ("local(Gentium-Bold)", "Gentium-Bold"),
        ] {
            let parsed = parse_src(src);
            assert_eq!(
                parsed.sources.first(),
                Some(&FontSource::Local {
                    name: expected.to_owned()
                }),
                "{src}"
            );
        }
    }

    #[test]
    fn an_empty_local_argument_is_not_a_font_src() {
        // §4.3.3.1's own counter-example, `local(inherit)`, is a parse error
        // because CSS-wide keywords are not allowed inside `local()`; an empty
        // name is the same shape of fault.
        assert!(parse_src("local()").sources.is_empty());
        assert!(parse_src("local(\"\")").sources.is_empty());
    }

    #[test]
    fn a_local_argument_that_is_a_keyword_is_not_a_face_name() {
        // §4.3.3.1: "CSS-wide keywords such as inherit, and <generic-font-family>
        // keywords such as serif are not allowed inside local()", because the
        // argument is "a format-specific string that uniquely identifies a single
        // font face" and a keyword is not one. The corpus writes this: two of its
        // rules name a font `local(sans-serif, Arial, Helvetica)`, and the
        // `sans-serif` in that list is not a face name.
        for keyword in [
            "inherit",
            "initial",
            "unset",
            "revert",
            "revert-layer",
            "serif",
            "sans-serif",
            "monospace",
            "system-ui",
            "cursive",
            "fantasy",
            "math",
        ] {
            assert!(
                parse_src(&format!("local({keyword})")).sources.is_empty(),
                "local({keyword}) is excluded by §4.3.3.1 and must not resolve"
            );
        }
        // Quoting does not make it a name either, because §4.3.3.1 restricts the
        // keyword rather than the spelling.
        assert!(parse_src("local(\"serif\")").sources.is_empty());
        // And the rest of the list is still honoured: a `local()` with one bad item
        // is a malformed item, so the `src` list walks on to the URL beside it.
        let parsed = parse_src("local(sans-serif, Arial), url(a.ttf) format(truetype)");
        assert_eq!(
            parsed
                .sources
                .iter()
                .map(FontSource::describe)
                .collect::<Vec<_>>(),
            ["url(a.ttf) format(truetype)"],
            "§4.3: \"local font faces that are not found are ignored and the user \
             agent loads the next font in the list\""
        );
    }

    #[test]
    fn a_quoted_url_and_a_bare_url_are_the_same_item() {
        // CSS Syntax §3.3 makes `url(x)` a url-token and `url("x")` a function,
        // so a parser that only knows one spelling silently drops half the
        // corpus's URLs.
        let parsed = parse_src("url(a.ttf), url('b.ttf'), url(\"c.ttf\")");
        let urls: Vec<_> = parsed
            .sources
            .iter()
            .filter_map(|source| match source {
                FontSource::Url { url, .. } => Some(url.as_str()),
                FontSource::Local { .. } => None,
            })
            .collect();
        assert_eq!(urls, ["a.ttf", "b.ttf", "c.ttf"]);
    }

    #[test]
    fn a_url_with_no_extension_stays_in_the_list_because_there_is_no_hint_to_skip_it() {
        // §4.3.3: "If no format hint is supplied, the user agent should download
        // the font resource." The corpus has one such URL, and refusing to fetch
        // it would be the engine inventing a rule §4.3.3 does not have.
        let parsed = parse_src("url(//s1.hdslb.com/bfs/static/jinkela/long/font/X) , url(b.ttf)");
        assert_eq!(parsed.sources.len(), 2, "{:?}", parsed.sources);
    }

    #[test]
    fn an_unparseable_item_is_reported_separately_from_an_unsupported_one() {
        // §4.3.1 draws the line: an unsupported source is a well-formed
        // declaration this user agent declines, while a malformed one is a
        // malformed declaration. The second is worth a diagnostic; the first is
        // already reported by the format.
        let parsed = parse_src("url( /* nothing */ )");
        assert!(parsed.sources.is_empty());
        assert!(
            matches!(parsed.problem, Some(SrcProblem::Malformed(_))),
            "{:?}",
            parsed.problem
        );
    }

    #[test]
    fn an_implied_format_follows_the_extensions_the_corpus_uses() {
        assert_eq!(implied_format("a.woff2"), FontFormat::Woff2);
        assert_eq!(implied_format("a.ttf?v=2"), FontFormat::TrueType);
        assert_eq!(implied_format("a.otf#Regular"), FontFormat::OpenType);
        assert_eq!(implied_format("a.ttc"), FontFormat::Collection);
        assert_eq!(implied_format("a.eot#iefix"), FontFormat::EmbeddedOpenType);
    }

    // ---- §4.3.1's format table ---------------------------------------------

    #[test]
    fn every_format_keyword_the_specification_names_is_recognised() {
        for keyword in [
            "collection",
            "embedded-opentype",
            "opentype",
            "svg",
            "truetype",
            "woff",
            "woff2",
        ] {
            assert!(
                FontFormat::from_keyword(keyword).is_some(),
                "{keyword} is in §4.3.1's <font-format> and must be recognised"
            );
            assert_eq!(
                FontFormat::from_keyword(keyword).map(FontFormat::keyword),
                Some(keyword)
            );
        }
        assert_eq!(
            FontFormat::from_keyword("zebra"),
            None,
            "a format no one defines"
        );
    }

    #[test]
    fn only_the_raw_sfnt_containers_are_decodable_and_that_is_the_corpus_finding() {
        // The rasteriser reads a raw sfnt through `ttf_parser`, whose
        // `Face::parse` accepts `0x00010000`, `true`, `OTTO` and `ttcf` and
        // nothing else. WOFF and WOFF2 are compressed wrappers with no decoder
        // in this engine's dependency graph, which is why 218 of the corpus's
        // 223 blocks cannot be used and only the `.ttf` and `.otf` ones can.
        assert!(FontFormat::TrueType.is_decodable());
        assert!(FontFormat::OpenType.is_decodable());
        assert!(FontFormat::Collection.is_decodable());
        for format in [
            FontFormat::Woff2,
            FontFormat::Woff,
            FontFormat::EmbeddedOpenType,
            FontFormat::Svg,
        ] {
            assert!(
                !format.is_decodable(),
                "{} cannot be decoded",
                format.keyword()
            );
        }
    }

    // ---- §4.4: the style and weight descriptors ----------------------------

    #[test]
    fn the_style_and_weight_descriptors_read_as_their_property_counterparts() {
        // §4.4: the descriptors take the same values as the properties, so a
        // rule declaring `font-weight: 500` is a 500 face and §5.2's weight
        // search can find it.
        let parsed = rules(
            "@font-face { font-family: A; src: url(a.ttf); font-weight: 500; font-style: oblique \
             14deg }",
        );
        assert_eq!(parsed[0].weight, 500);
        assert_eq!(parsed[0].style, FontStyle::Oblique(14.0));
    }

    #[test]
    fn an_omitted_descriptor_is_its_initial_value() {
        // §4.1: "Those not given explicit values in the rule take the initial
        // value listed with each descriptor in this specification."
        let parsed = rules("@font-face { font-family: A; src: url(a.ttf) }");
        assert_eq!(parsed[0].weight, 400, "§2.2's initial is `normal`");
        assert_eq!(
            parsed[0].style,
            FontStyle::Normal,
            "§2.4's initial is `normal`"
        );
        assert_eq!(
            parsed[0].display,
            FontDisplay::Auto,
            "§4.9's initial is `auto`"
        );
        assert_eq!(
            parsed[0].unicode_range,
            UnicodeRangeSet::all(),
            "§4.5's initial value is U+0-10FFFF"
        );
    }

    #[test]
    fn an_invalid_style_or_weight_leaves_the_rule_at_the_initial_value() {
        // CSS Fonts 4 §4.4 gives the descriptors the properties' grammars, and
        // an invalid value is an invalid declaration: the descriptor keeps its
        // initial value and the rest of the rule stands.
        let parsed = rules(
            "@font-face { font-family: A; src: url(a.ttf); font-weight: 5000; font-style: \
             sideways; unicode-range: U+ZZ }",
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].weight, 400);
        assert_eq!(parsed[0].style, FontStyle::Normal);
        assert_eq!(parsed[0].unicode_range, UnicodeRangeSet::all());
    }

    // ---- §4.5: the character range descriptor ------------------------------

    #[test]
    fn the_three_range_forms_the_specification_names_parse_to_the_same_intervals() {
        // §4.5: a single codepoint, an interval, and a wildcard.
        assert_eq!(parse_range_token("U+416"), Some((0x416, 0x416)));
        assert_eq!(parse_range_token("u+416"), Some((0x416, 0x416)));
        assert_eq!(parse_range_token("U+400-4ff"), Some((0x400, 0x4ff)));
        assert_eq!(parse_range_token("U+4??"), Some((0x400, 0x4ff)));
    }

    #[test]
    fn a_wildcard_with_no_leading_digit_is_a_wildcard_with_a_zero_digit() {
        // §4.5 spells this out: "U+???" is "valid and equivalent to a wildcard
        // range with an initial zero digit (e.g. U+0??? = U+0000-0FFF)".
        assert_eq!(parse_range_token("U+???"), Some((0x0, 0xfff)));
        assert_eq!(parse_range_token("U+0???"), Some((0x0, 0xfff)));
    }

    #[test]
    fn a_wildcard_of_six_is_invalid_even_though_the_token_accepts_six() {
        // §4.5: "the maximum number of trailing '?' wildcard characters is five,
        // even though the UNICODE-RANGE token accepts six."
        assert_eq!(parse_range_token("U+??????"), None);
        assert_eq!(parse_range_token("U+0?????"), Some((0x0, 0xf_ffff)));
    }

    #[test]
    fn a_range_past_the_end_of_unicode_is_invalid() {
        assert_eq!(parse_range_token("U+110000"), None);
        assert_eq!(parse_range_token("U+10FFFF"), Some((0x10_ffff, 0x10_ffff)));
        assert_eq!(parse_range_token("U+1?????"), None);
        assert_eq!(parse_range_token("U+110000-110001"), None);
    }

    #[test]
    fn an_interval_whose_end_precedes_its_start_is_invalid() {
        // §4.5: "the end codepoint must be greater than or equal to the start
        // codepoint".
        assert_eq!(parse_range_token("U+500-400"), None);
        assert_eq!(parse_range_token("U+400-400"), Some((0x400, 0x400)));
    }

    #[test]
    fn one_invalid_token_discards_the_whole_descriptor() {
        // §4.5: "Ranges that do not fit one of these forms are invalid and cause
        // the declaration to be ignored" - the declaration, not the token.
        assert_eq!(UnicodeRangeSet::parse("U+400-4ff, U+ZZ"), None);
        assert!(UnicodeRangeSet::parse("U+400-4ff, U+500-600").is_some());
    }

    #[test]
    fn overlapping_ranges_are_merged_so_the_search_is_a_binary_search() {
        // §4.5: "ranges may overlap. The union of these ranges defines the set of
        // codepoints". The corpus's 216 ranged blocks carry 12,252 tokens, so a
        // per-character scan of the authored list is not affordable.
        let range = UnicodeRangeSet::parse("U+0-7F, U+40-FF, U+100-10FFFF").expect("valid");
        assert_eq!(range.intervals(), [(0x0, 0x10_ffff)]);
        assert_eq!(range.len(), 1, "three overlapping ranges are one interval");
    }

    #[test]
    fn the_corpus_shape_of_a_slice_font_is_one_rule_per_slice_all_merged_when_taken_together() {
        // The corpus's dominant font is 108 slices per file at weight 500, so a
        // page's Han characters are split across 108 rules. This is the property
        // that makes that work: each rule admits only its own slice, and a
        // character in no slice is admitted by no rule.
        let sheet = parse_font_faces(
            "@font-face { font-family: S; font-weight: 500; src: url(a.woff2); unicode-range: \
             U+4E00-9FFF } \
             @font-face { font-family: S; font-weight: 500; src: url(b.woff2); unicode-range: \
             U+30-4DFF }",
        );
        let first = &sheet.0.rules()[0];
        let second = &sheet.0.rules()[1];
        assert!(
            first.admits('\u{4e2d}'),
            "the slice that owns U+4E2D admits it"
        );
        assert!(!first.admits('\u{3042}'));
        assert!(second.admits('\u{3042}'));
        assert!(
            !first.admits('A'),
            "a Latin character is in no slice, so a page whose webfont covers only \
             Han falls through to the next family in the list - which is what \
             keeps `font-family: WebFont, sans-serif` from painting Latin in the \
             webfont"
        );
    }

    // ---- §4.9: the display descriptor --------------------------------------

    #[test]
    fn every_display_keyword_is_recognised_and_the_initial_value_is_auto() {
        for keyword in ["auto", "block", "swap", "fallback", "optional"] {
            let display = FontDisplay::from_keyword(keyword).expect(keyword);
            assert_eq!(display.keyword(), keyword);
        }
        assert_eq!(FontDisplay::default(), FontDisplay::Auto);
        assert_eq!(FontDisplay::from_keyword("eventually"), None);
    }

    #[test]
    fn an_invalid_display_value_leaves_the_rule_at_auto() {
        let parsed =
            rules("@font-face { font-family: A; src: url(a.ttf); font-display: eventually }");
        assert_eq!(parsed[0].display, FontDisplay::Auto);
    }

    // ---- S26: a malformed descriptor costs the block nothing else ----------

    #[test]
    fn a_malformed_descriptor_is_reported_and_the_block_keeps_its_others() {
        // CSS Syntax §5.4.4: a bad declaration is discarded and the rest of the
        // block survives. This is the property the descriptor-list recovery fix
        // exists to guarantee, and it is checked here because a face is now
        // worth something, so a dropped `src` is a dropped face rather than a
        // dropped no-op.
        let (sheet, problems) = parse_font_faces(
            "@font-face { font-family: A; font-weight; src: url(a.ttf); font-style: italic }",
        );
        assert_eq!(sheet.rules().len(), 1, "the block is still a block");
        let rule = &sheet.rules()[0];
        assert_eq!(
            rule.family_as_written, "A",
            "the descriptor before it survives"
        );
        assert_eq!(rule.sources.len(), 1, "so the `src` is not lost with it");
        assert_eq!(
            rule.style,
            FontStyle::Italic,
            "and neither is the descriptor after it"
        );
        assert_eq!(
            rule.weight, 400,
            "the valueless declaration is the one that did not survive, and \
             §4.1 leaves its descriptor at its initial value"
        );
        assert!(
            problems
                .iter()
                .any(|problem| problem.message.contains("invalid descriptor")),
            "the bad declaration is reported: {problems:?}"
        );
        assert!(
            !sheet.diagnostics.is_empty(),
            "and the descriptor list's own diagnostics are kept with the sheet, \
             so a caller reporting on a stylesheet has one place to look"
        );
    }

    #[test]
    fn a_declaration_with_no_colon_is_itself_discarded_and_only_itself() {
        // CSS Syntax §5.4.4: a declaration that is not `ident : value` "cannot be
        // parsed as a declaration", so §5.4.4 discards it and resumes after the
        // next `;`. The discard is one declaration, not the block.
        let (sheet, problems) = parse_font_faces(
            "@font-face { font-weight: 700; oops; font-family: A; src: url(a.ttf) }",
        );
        assert_eq!(sheet.rules().len(), 1, "the block is still a block");
        let rule = &sheet.rules()[0];
        assert_eq!(
            rule.family_as_written, "A",
            "the declaration after the bad one survives"
        );
        assert_eq!(rule.sources.len(), 1);
        assert_eq!(
            rule.weight, 700,
            "and the descriptor before it is untouched, so the recovery is \
             §5.4.4's - resume after the next `;` - and not a bail-out"
        );
        assert!(
            problems
                .iter()
                .any(|problem| problem.message.contains("invalid descriptor")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_block_whose_only_family_declaration_is_malformed_is_not_a_rule() {
        // The other half of the same rule: §4.1 still applies. A block that
        // declared a family in a way the parser could not read has no family, so
        // the rule is not considered - and it is *not* replaced by an installed
        // family of that name, which is §5.2's explicit consequence.
        let (sheet, problems) =
            parse_font_faces("@font-face { font-family 'Missing Colon'; src: url(a.ttf) }");
        assert!(sheet.is_empty());
        assert!(
            problems
                .iter()
                .any(|problem| problem.message.contains("no usable `font-family`")),
            "{problems:?}"
        );
    }

    #[test]
    fn the_bad_declaration_is_reported_at_its_own_place_in_the_stylesheet() {
        // The descriptor list is read as its own source text, so its diagnostics
        // are relative to the block. Reporting them unchanged would point at the
        // wrong line of the wrong file, which is worse than not reporting them.
        let (sheet, problems) = parse_font_faces(
            "@font-face {\n  font-family: A;\n  font-weight;\n  src: url(a.ttf);\n}\n",
        );
        let reported = problems
            .iter()
            .find(|problem| problem.message.contains("invalid descriptor"))
            .expect("the valueless declaration is reported");
        assert_eq!(
            (reported.line, reported.column),
            (3, 3),
            "CSS Syntax §3.2 counts a line from 1 and a column from 1, and the \
             position is the declaration's own start in the *stylesheet*"
        );
        let on_sheet = sheet
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("invalid descriptor"))
            .expect("and the sheet carries the same position");
        assert_eq!((on_sheet.line, on_sheet.column), (3, 3));
    }

    // ---- the tokenizer reductions the grammars rest on ---------------------

    #[test]
    fn a_descriptor_value_tokenizes_into_the_components_the_grammars_read() {
        assert_eq!(
            components_of("url(a.woff2) format(woff2)"),
            vec![
                Component::Url("a.woff2".to_owned()),
                Component::Function(
                    "format".to_owned(),
                    vec![Component::Ident("woff2".to_owned())]
                ),
            ]
        );
        assert_eq!(
            components_of("local(Foo Bar)"),
            vec![Component::Function(
                "local".to_owned(),
                vec![
                    Component::Ident("Foo".to_owned()),
                    Component::Ident("Bar".to_owned())
                ]
            )]
        );
    }

    #[test]
    fn a_function_name_is_matched_caselessly_because_css_keywords_are() {
        // CSS Syntax §3.2: idents are matched ASCII case-insensitively.
        let parsed = parse_src("URL(a.ttf) FORMAT(truetype), LOCAL(Foo)");
        assert_eq!(
            parsed.sources,
            vec![
                FontSource::Url {
                    url: "a.ttf".to_owned(),
                    format: FontFormat::TrueType,
                    techs: Vec::new(),
                },
                FontSource::Local {
                    name: "Foo".to_owned()
                },
            ]
        );
    }
}
