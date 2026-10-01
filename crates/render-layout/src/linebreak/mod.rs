//! UAX #14: the Unicode line breaking algorithm.
//!
//! CSS Text 3 §5 says the line breaking rules are "not fully defined" by CSS
//! and defers to [`UAX14`], "which defines a baseline behavior for line breaking
//! for all scripts in Unicode, which is expected to be further tailored". This
//! module is that baseline plus the two tailorings CSS Text 3 §5.1 and §5.2
//! name. Nothing here decides *where* a line actually breaks: it produces the
//! set of positions where a break is allowed, and CSS 2.1 §9.4.2 and CSS Text 3
//! §5 ("Wrapping is only performed at an allowed break point") leave the choice
//! among them to layout.
//!
//! # What is implemented
//!
//! UAX #14 §6.1, the non-tailorable rules, in full: `LB1` to `LB12a`.
//!
//! UAX #14 §6.2, the tailorable rules, in full for the classes this engine
//! distinguishes: `LB13` to `LB31`, including the Hangul syllable rules `LB26` and
//! `LB27`, the regional indicator pairing of `LB30a` and the emoji modifier rule
//! `LB30b`.
//!
//! # What is deliberately absent
//!
//! These are absences, not approximations. An approximate line breaker produces
//! wrong layout silently, which is the failure this engine treats as worst, so
//! each case is left to the default class `AL` and named here rather than
//! guessed at:
//!
//! * **Rule `LB28a`, Brahmic orthographic syllables.** `AK`, `AP`, `AS`, `VI`
//!   and `VF`
//!   resolve to `AL`, so a Devanagari or Balinese syllable can break where it
//!   should not. Keeping a syllable together needs syllable segmentation, which
//!   is a segmenter and not a table.
//! * **`SA`, dictionary breaking.** Thai, Lao, Khmer and Myanmar resolve to
//!   `AL`, which is `LB1`'s own default for a non-`Mn`/`Mc` `SA` character.
//!   UAX #14 §5.1's `SA` description and §8.2's first example both say
//!   resolution needs a dictionary, and there is none.
//! * **The `QU` sub-classes `Pi` and `Pf`.** Rules `LB15a`, `LB15b`, `LB19` and
//!   `LB19a` are evaluated as one rule, because `LB19a`'s two East-Asian conditions
//!   hold of both halves of a boundary a quotation mark sits on and so subsume
//!   the `SP*` forms of `LB15a` and `LB15b`. A quote is therefore never split from
//!   the text on either side unless both neighbours are East Asian.
//! * **The tailoring §5.2 lists as desirable for interoperability** (no break
//!   between `!` and a letter, or `/` and a letter, or `|` and a letter). §5.2
//!   lists those as options, and each one removes a break opportunity UAX #14
//!   mandates, so taking them is a tailoring decision that belongs with
//!   measurement rather than with a first implementation.
//! * **CLDR tailoring** beyond the `East_Asian_Width` property itself. §5 requires
//!   the property, and [`tables`] has it; §8's locale-specific tailorings and
//!   §5.2's note that the writing system must be tagged are not implemented. This
//!   engine reads the strictness keyword, not the document language.
//! * **Per-line-length variation.** §5.2's `auto` is "the UA determines the set
//!   of line-breaking restrictions to use, and it may vary the restrictions based
//!   on the length of the line". `auto` is `normal` here.
//!
//! # The CSS tailorings
//!
//! * [`WordBreak`] is CSS Text 3 §5.1: `break-all` treats letters and digits as
//!   `ID`, and `keep-all` prohibits a break between two letter units.
//! * [`LineBreakStrictness`] is CSS Text 3 §5.2's kinsoku shori, applied as a
//!   class resolution because that is the formulation UAX #14 §5.1's `CJ`
//!   description prescribes: "Treating characters of class `CJ` as class `NS` will
//!   give CSS strict line breaking; treating them as class `ID` will give CSS
//!   normal breaking." `loose` resolves to `ID` every class §5.2 says `loose`
//!   may break next to, and `anywhere` makes every position a break opportunity
//!   "disregarding any prohibition against line breaks, even those introduced by
//!   characters with the `GL`, `WJ`, or `ZWJ` line breaking classes".
//!
//! Kinsoku is therefore a *narrowing* of the opportunity set rather than a
//! second pass over the chosen break: a position kinsoku forbids simply is not
//! an opportunity, so the caller's line filler moves to the next one. No
//! measurement depends on which opportunity was taken.
//!
//! [`UAX14`]: https://www.unicode.org/reports/tr14/

mod tables;

#[cfg(test)]
mod tests;

use std::cmp::Ordering;

use tables::{EAST_ASIAN_WIDE, EAST_ASIAN_WIDE_OR_AMBIGUOUS, LINE_BREAK};

/// The line breaking classes of UAX #14 Table 1 that this engine keeps
/// distinct. A code point in no [`tables::LINE_BREAK`] range is
/// [`Al`](Self::Al), which is `LB1`'s resolution of `AI`, `SA`, `SG`, `XX` and of
/// the Brahmic classes rule `LB28a` would have used; the module header names
/// each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LineBreakClass {
    /// Mandatory Break.
    Bk,
    /// Carriage Return.
    Cr,
    /// Line Feed.
    Lf,
    /// Next Line.
    Nl,
    /// Combining Mark.
    Cm,
    /// Word Joiner.
    Wj,
    /// Zero Width Space.
    Zw,
    /// Non-breaking ("Glue").
    Gl,
    /// Space.
    Sp,
    /// Zero Width Joiner.
    Zwj,
    /// Break Opportunity Before and After.
    B2,
    /// Break After.
    Ba,
    /// Break Before.
    Bb,
    /// Hyphen.
    Hy,
    /// Unambiguous Hyphen.
    Hh,
    /// Inseparable.
    In,
    /// Close Punctuation.
    Cl,
    /// Close Parenthesis.
    Cp,
    /// Exclamation / Interrogation.
    Ex,
    /// Nonstarter.
    Ns,
    /// Open Punctuation.
    Op,
    /// Quotation.
    Qu,
    /// Infix Numeric Separator.
    Is,
    /// Numeric.
    Nu,
    /// Postfix Numeric.
    Po,
    /// Prefix Numeric.
    Pr,
    /// Symbols Allowing Break After.
    Sy,
    /// Ideographic.
    Id,
    /// Conditional Japanese Starter.
    Cj,
    /// Hebrew Letter.
    Hl,
    /// Hangul LV Syllable.
    H2,
    /// Hangul LVT Syllable.
    H3,
    /// Hangul L Jamo.
    Jl,
    /// Hangul V Jamo.
    Jv,
    /// Hangul T Jamo.
    Jt,
    /// Regional Indicator.
    Ri,
    /// Emoji Base.
    Eb,
    /// Emoji Modifier.
    Em,
    /// Alphabetic. Also the default, and the resolution of every class `LB1`
    /// resolves away.
    Al,
}

impl LineBreakClass {
    /// UAX #14 Table 1's spelling, so a diagnostic names the class the
    /// specification uses rather than the variant this file uses.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bk => "BK",
            Self::Cr => "CR",
            Self::Lf => "LF",
            Self::Nl => "NL",
            Self::Cm => "CM",
            Self::Wj => "WJ",
            Self::Zw => "ZW",
            Self::Gl => "GL",
            Self::Sp => "SP",
            Self::Zwj => "ZWJ",
            Self::B2 => "B2",
            Self::Ba => "BA",
            Self::Bb => "BB",
            Self::Hy => "HY",
            Self::Hh => "HH",
            Self::In => "IN",
            Self::Cl => "CL",
            Self::Cp => "CP",
            Self::Ex => "EX",
            Self::Ns => "NS",
            Self::Op => "OP",
            Self::Qu => "QU",
            Self::Is => "IS",
            Self::Nu => "NU",
            Self::Po => "PO",
            Self::Pr => "PR",
            Self::Sy => "SY",
            Self::Id => "ID",
            Self::Cj => "CJ",
            Self::Hl => "HL",
            Self::H2 => "H2",
            Self::H3 => "H3",
            Self::Jl => "JL",
            Self::Jv => "JV",
            Self::Jt => "JT",
            Self::Ri => "RI",
            Self::Eb => "EB",
            Self::Em => "EM",
            Self::Al => "AL",
        }
    }

    /// UAX #14 rule `LB9`'s exception: "X is any line break class except `BK`, `CR`,
    /// `LF`, `NL`, `SP`, or `ZW`". A combining mark following one of those is not part
    /// of an `X (CM | ZWJ)*` sequence, and `LB10` makes it `AL` instead.
    const fn absorbs_combining(self) -> bool {
        !matches!(
            self,
            Self::Bk | Self::Cr | Self::Lf | Self::Nl | Self::Sp | Self::Zw
        )
    }
}

/// Whether a code point is in the inclusive ranges of a generated table.
fn in_ranges(table: &[(u32, u32)], code: u32) -> bool {
    table
        .binary_search_by(|(first, last)| {
            if code < *first {
                Ordering::Greater
            } else if code > *last {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// UAX #14 §5: the `Line_Break` property value of `character`, which the UCD
/// assigns normatively. Anything the table does not list takes `AL`.
fn line_break_class(character: char) -> LineBreakClass {
    let code = u32::from(character);
    let mut low = 0_usize;
    let mut high = LINE_BREAK.len();
    while low < high {
        let middle = low + (high - low) / 2;
        let (first, last, class) = LINE_BREAK[middle];
        if code < first {
            high = middle;
        } else if code > last {
            low = middle + 1;
        } else {
            return class;
        }
    }
    LineBreakClass::Al
}

/// UAX #14 §6's `$EastAsian`: the characters with `East_Asian_Width` F, W or H,
/// which rules `LB19a` and `LB30` test.
fn is_east_asian(character: char) -> bool {
    in_ranges(EAST_ASIAN_WIDE, u32::from(character))
}

/// CSS Text 3 §5.2's test for its `PO` and `PR` rows: `East_Asian_Width`
/// Ambiguous, Fullwidth or Wide.
fn is_east_asian_or_ambiguous(character: char) -> bool {
    in_ranges(EAST_ASIAN_WIDE_OR_AMBIGUOUS, u32::from(character))
}

/// CSS Text 3 §5.1's `word-break`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WordBreak {
    /// "Words break according to their customary rules": UAX #14 unchanged.
    #[default]
    Normal,
    /// Letters and digits are "instead treated as `ID` for the purpose of
    /// line-breaking", which permits a break between any two of them.
    BreakAll,
    /// Breaks between two typographic letter units "are suppressed, i.e. breaks
    /// are prohibited between pairs of such characters (regardless of
    /// line-break settings other than anywhere)".
    KeepAll,
    /// The deprecated keyword, which §5.1 defines as `normal` plus
    /// `overflow-wrap: anywhere`. `overflow-wrap` is not implemented, so this
    /// resolves to the opportunities [`Self::Normal`] produces.
    BreakWord,
}

/// CSS Text 3 §5.2's `line-break`: the levels of kinsoku shori.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineBreakStrictness {
    /// "The UA determines the set of line-breaking restrictions to use". This
    /// engine has no length-dependent tailoring, so it is `normal`.
    #[default]
    Auto,
    /// "Breaks text using the least restrictive set of line-breaking rules."
    Loose,
    /// "Breaks text using the most common set of line-breaking rules."
    Normal,
    /// "Breaks text using the most stringent set of line-breaking rules."
    Strict,
    /// "There is a soft wrap opportunity around every typographic character unit
    /// ... disregarding any prohibition against line breaks".
    Anywhere,
}

impl LineBreakStrictness {
    /// `auto` and `normal` are one answer here, because §5.2 only says they
    /// *may* differ. Making them differ would be a guess.
    const fn normal(self) -> Self {
        match self {
            Self::Auto | Self::Normal => Self::Normal,
            other => other,
        }
    }
}

/// Everything the text properties say about where a line may break.
///
/// [`Self::wrap`] is CSS Text 3 §3 rather than §5: `white-space: pre` and
/// `nowrap` "do not allow wrapping" and every other value does. §5's rule is
/// that "When wrapping is enabled (see white-space), the UA must minimize the
/// amount of content overflowing a line by wrapping the line at a soft wrap
/// opportunity, if one exists", so a `white-space` that forbids wrapping
/// suppresses the opportunities rather than changing them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineBreakOptions {
    pub word_break: WordBreak,
    pub line_break: LineBreakStrictness,
    pub wrap: bool,
}

impl LineBreakOptions {
    /// The options for text whose `white-space` permits wrapping.
    #[must_use]
    pub const fn wrapping(word_break: WordBreak, line_break: LineBreakStrictness) -> Self {
        Self {
            word_break,
            line_break,
            wrap: true,
        }
    }
}

/// What the algorithm found at one position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Break {
    /// No line may end here.
    Prohibited,
    /// A line may end here.
    Allowed,
    /// A line must end here: UAX #14 rules `LB3`, `LB4` and `LB5`.
    Mandatory,
}

impl Break {
    /// Whether a line may end here. A mandatory break is a break.
    #[must_use]
    pub const fn is_break(self) -> bool {
        !matches!(self, Self::Prohibited)
    }
}

/// One typographic character unit, after UAX #14 rule `LB9` has absorbed any
/// combining marks into the base they follow.
#[derive(Clone, Copy, Debug)]
struct Unit {
    character: char,
    class: LineBreakClass,
    /// Whether this unit is a combining mark or joiner that rule `LB9` attached
    /// to the previous one. `LB9` then says "In subsequent rules, any `CM` or
    /// `ZWJ` characters affected by this rule are ignored", and §8.2's sixth
    /// example is that tailoring written out: "Tailor to prevent line breaks
    /// from falling within default grapheme clusters".
    attached: bool,
}

/// The resolved units of a text run, from which every boundary is decided.
struct Context {
    units: Vec<Unit>,
    /// The options that apply at each position, from `options_with`. There is
    /// one entry per unit; the end of the text reuses the last.
    options: Vec<LineBreakOptions>,
}

impl Context {
    /// UAX #14 rules `LB1` and `LB9`: assign a class to every code point, then
    /// "treat X (`CM` | `ZWJ`)* as if it were X", with rule `LB10` handling a
    /// combining mark that has no base to attach to.
    ///
    /// `options_for` supplies the tailoring for each character, because CSS
    /// Text 3 §5.1 and §5.2 both apply to "text" and an inline formatting
    /// context can contain several elements with different values. A run with
    /// one value throughout passes [`Self::uniform`].
    fn resolve(characters: &[char], options_for: impl Fn(usize) -> LineBreakOptions) -> Self {
        let mut units: Vec<Unit> = Vec::with_capacity(characters.len());
        let mut options: Vec<LineBreakOptions> = Vec::with_capacity(characters.len());
        // The class of the sequence currently open, which is what a following
        // combining mark joins. `None` is start of text, where LB10 applies.
        let mut open: Option<LineBreakClass> = None;
        for (index, &character) in characters.iter().enumerate() {
            let class = line_break_class(character);
            let combined = matches!(class, LineBreakClass::Cm | LineBreakClass::Zwj);
            let unit = if combined {
                match open {
                    Some(base) if base.absorbs_combining() => Unit {
                        character,
                        class: base,
                        attached: true,
                    },
                    // LB10: "Treat any remaining CM or ZWJ as if it had the
                    // properties of U+0041 A LATIN CAPITAL LETTER A".
                    _ => {
                        open = Some(LineBreakClass::Al);
                        Unit {
                            character,
                            class: LineBreakClass::Al,
                            attached: false,
                        }
                    }
                }
            } else {
                open = Some(class);
                Unit {
                    character,
                    class,
                    attached: false,
                }
            };
            units.push(unit);
            options.push(options_for(index));
        }
        let mut context = Self { units, options };
        context.tailor();
        context
    }

    /// CSS Text 3 §5.1 and §5.2, applied as a class resolution, which is how
    /// UAX #14 §5.1's `CJ` description says the strictness tailoring is done
    /// and the only formulation under which `strict` and `loose` interact with
    /// the rules rather than overriding them.
    ///
    /// CSS Text 3 §1.5 is why the options are per character rather than per
    /// run: "For the purpose of determining adjacency for text processing
    /// (such as white space processing, text transformation, line-breaking,
    /// etc.), and thus in general within this specification, intervening inline
    /// box boundaries and out-of-flow elements must be ignored." The adjacency
    /// is one sequence; the values are not.
    fn tailor(&mut self) {
        let strictness: Vec<LineBreakStrictness> = self
            .options
            .iter()
            .map(|options| options.line_break.normal())
            .collect();
        let break_all: Vec<bool> = self
            .options
            .iter()
            .map(|options| options.word_break == WordBreak::BreakAll)
            .collect();
        for (index, unit) in self.units.iter_mut().enumerate() {
            if unit.attached {
                // A combining mark is not a character a break can land next to,
                // and §5.1 and §5.2 both speak about characters a line can end
                // beside, so it takes its base's class unchanged.
                continue;
            }
            if break_all[index]
                && matches!(
                    unit.class,
                    LineBreakClass::Al | LineBreakClass::Hl | LineBreakClass::Nu
                )
            {
                // §5.1: "any typographic letter units (and any typographic
                // character units resolving to the NU, AL, or SA line breaking
                // classes) are instead treated as ID". `HL` is one of the letter
                // classes §5.1 counts as a letter.
                unit.class = LineBreakClass::Id;
            }
            match strictness[index] {
                // §5.2 forbids breaking before "Japanese small kana or the
                // Katakana-Hiragana prolonged sound mark, i.e. characters from
                // the Unicode line breaking class CJ" for `normal` and
                // `strict`; the iteration marks of the next row are already
                // `NS`, which the rules prohibit a break before.
                LineBreakStrictness::Strict | LineBreakStrictness::Normal => {
                    if unit.class == LineBreakClass::Cj {
                        unit.class = LineBreakClass::Ns;
                    }
                    // §5.2 requires `normal` and `loose` to allow a break
                    // before U+301C and U+30A0 "if the writing system is Chinese
                    // or Japanese". Those two characters are `NS`, so allowing
                    // the break is exactly the class change below.
                    if strictness[index] == LineBreakStrictness::Normal
                        && matches!(unit.character, '\u{301c}' | '\u{30a0}')
                    {
                        unit.class = LineBreakClass::Id;
                    }
                }
                LineBreakStrictness::Loose => {
                    // §5.2's `loose` rows, each naming a class a stricter level
                    // prohibits a break next to: small kana and the prolonged
                    // sound mark, the iteration marks, the inseparable
                    // characters, and the centred punctuation marks, which §5.1
                    // classes as `EX` or `NS`.
                    if matches!(
                        unit.class,
                        LineBreakClass::Ns
                            | LineBreakClass::Cj
                            | LineBreakClass::In
                            | LineBreakClass::Ex
                    ) {
                        unit.class = LineBreakClass::Id;
                    }
                    // "breaks before hyphens: ‐ U+2010, – U+2013", which are
                    // `HH` and `BA`.
                    if matches!(unit.character, '\u{2010}' | '\u{2013}') {
                        unit.class = LineBreakClass::Id;
                    }
                    // "breaks before suffixes: characters with the Unicode line
                    // breaking class PO and the East Asian Width property
                    // Ambiguous, Fullwidth or Wide", and the same after a `PR`.
                    if matches!(unit.class, LineBreakClass::Po | LineBreakClass::Pr)
                        && is_east_asian_or_ambiguous(unit.character)
                    {
                        unit.class = LineBreakClass::Id;
                    }
                }
                // §5.2: `anywhere` disregards every prohibition, so the classes
                // would make no difference to its opportunities and are left as
                // they are.
                LineBreakStrictness::Auto | LineBreakStrictness::Anywhere => {}
            }
        }
    }

    /// The unit before position `at`, or `None` at the start of the text.
    fn previous(&self, at: usize) -> Option<&Unit> {
        self.units.get(at.checked_sub(1)?)
    }

    /// The unit at position `at`, or `None` at the end of the text.
    fn next(&self, at: usize) -> Option<&Unit> {
        self.units.get(at)
    }

    /// The nearest unit before `at` that is not `SP`, which is what the `SP*`
    /// of rules `LB8`, `LB14`, `LB16` and `LB17` skip over.
    fn previous_skipping_spaces(&self, at: usize) -> Option<&Unit> {
        self.units[..at]
            .iter()
            .rev()
            .find(|unit| unit.class != LineBreakClass::Sp)
    }

    /// The unit two positions before the boundary at `at`, which is what rules
    /// `LB20a` and `LB21a` read: both are three-unit contexts. `None` means the
    /// text begins with the two units after it, which §6.1 spells `sot`.
    fn two_before(&self, at: usize) -> Option<&Unit> {
        self.units.get(at.checked_sub(2)?)
    }

    /// Whether the units before `at` form `NU (SY | IS)*`, the prefix four of
    /// rule `LB25`'s arms require. The run is the maximal one over those three
    /// classes, and it qualifies as soon as it contains a digit: `1,234` has one
    /// and `(12)` also has one, because the parenthesis is outside the run and
    /// the arms read `NU (SY | IS)* (CL | CP) × PO`.
    fn preceded_by_number(&self, at: usize) -> bool {
        let mut index = at;
        let mut seen_digit = false;
        while index > 0 {
            index -= 1;
            match self.units[index].class {
                LineBreakClass::Nu => seen_digit = true,
                LineBreakClass::Sy | LineBreakClass::Is => {}
                _ => break,
            }
        }
        seen_digit
    }

    /// Rule `LB30a`'s parity: `sot (RI RI)* RI × RI` and `[^RI] (RI RI)* RI × RI`
    /// both break, so the question is whether the run of regional indicators
    /// immediately before the position has even length.
    fn preceded_by_even_regional_indicators(&self, at: usize) -> bool {
        self.units[..at]
            .iter()
            .take_while(|unit| unit.class == LineBreakClass::Ri)
            .count()
            % 2
            == 0
    }
}

/// CSS Text 3 §5.1's "typographic letter unit ... or other typographic
/// character units belonging to the `NU`, `AL`, `AI`, or `ID` Unicode line breaking
/// classes", which `keep-all` treats as one unbreakable word.
const fn is_letter_like(class: LineBreakClass) -> bool {
    matches!(
        class,
        LineBreakClass::Al
            | LineBreakClass::Hl
            | LineBreakClass::Nu
            | LineBreakClass::Id
            | LineBreakClass::Cj
    )
}

/// UAX #14 rules `LB19` and `LB19a`, the quotation mark rules, which refine each
/// other: `LB19` reads `× [QU - Pi]` and `[QU - Pf] ×`, and `LB19a` then adds
/// `[^$EastAsian] × QU`, `× QU ([^$EastAsian] | eot)`, `QU × [^$EastAsian]` and
/// `(sot | [^$EastAsian]) QU ×`. Both halves of a boundary a quotation mark sits
/// on are covered by the same pair of conditions - the neighbour before it and
/// the neighbour after it must both be East Asian or no break is allowed - so
/// the two rules are one here, and the `SP*` forms of `LB15a` and `LB15b` follow
/// from the same pair because a space is not East Asian.
///
/// §6.1 writes `sot` and `eot` as though they were classes, so a missing
/// neighbour reads as a non-East-Asian one and both conditions hold there. The
/// relaxation therefore never fires for a real character: the `QU` class holds
/// only the ambiguous-width quotation marks, and none of them has a Fullwidth,
/// Wide or Halfwidth `East_Asian_Width`. A quotation mark so has no break on
/// either side, which is what this returns.
fn quotation_prohibits(context: &Context, at: usize) -> bool {
    for side in [
        context.previous(at).map(|unit| (at - 1, unit)),
        context.next(at).map(|unit| (at, unit)),
    ] {
        let Some((index, unit)) = side else {
            continue;
        };
        if unit.class != LineBreakClass::Qu {
            continue;
        }
        let before_is_east_asian = index
            .checked_sub(1)
            .and_then(|before| context.units.get(before))
            .is_some_and(|unit| is_east_asian(unit.character));
        let after_is_east_asian = context
            .units
            .get(index + 1)
            .is_some_and(|unit| is_east_asian(unit.character));
        if !before_is_east_asian || !after_is_east_asian {
            return true;
        }
    }
    false
}

/// UAX #14 §6: the rules, applied in order. `at` is a position between
/// typographic character units; a missing neighbour is `sot` or `eot`.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per rule, in the order §6 lists them, is the readable form"
)]
fn decide(context: &Context, at: usize) -> Break {
    // Rule LB9's second half: "any CM or ZWJ characters affected by this rule
    // are ignored", so a boundary immediately after one is not a boundary.
    if context.next(at).is_some_and(|unit| unit.attached) {
        return Break::Prohibited;
    }
    // LB2: never break at the start of text.
    let Some(previous) = context.previous(at) else {
        return Break::Prohibited;
    };
    // LB3: always break at the end of text.
    let Some(next) = context.next(at) else {
        return Break::Mandatory;
    };
    let before = previous.class;
    let after = next.class;
    let before_text = previous.character;
    let after_text = next.character;

    // LB4 and LB5: a hard line break ends the line. `CR × LF` is the no-break
    // half of LB5, and it has to be tested before the general `CR !`.
    let hard_break_ends_the_line = matches!(
        before,
        LineBreakClass::Bk | LineBreakClass::Nl | LineBreakClass::Lf
    ) || before == LineBreakClass::Cr && after != LineBreakClass::Lf;
    if hard_break_ends_the_line {
        return Break::Mandatory;
    }
    // LB6: do not break before a hard line break.
    if matches!(
        after,
        LineBreakClass::Bk | LineBreakClass::Cr | LineBreakClass::Lf | LineBreakClass::Nl
    ) {
        return Break::Prohibited;
    }
    // LB7: do not break before a space or a zero width space.
    if matches!(after, LineBreakClass::Sp | LineBreakClass::Zw) {
        return Break::Prohibited;
    }
    // LB8: break after a zero width space, even across spaces.
    if context
        .previous_skipping_spaces(at)
        .is_some_and(|unit| unit.class == LineBreakClass::Zw)
    {
        return Break::Allowed;
    }
    // LB8a: do not break after a zero width joiner.
    if before == LineBreakClass::Zwj {
        return Break::Prohibited;
    }
    // LB11 and LB12: do not break before or after a word joiner, and do not
    // break after a non-breaking character.
    if matches!(before, LineBreakClass::Wj | LineBreakClass::Gl) || after == LineBreakClass::Wj {
        return Break::Prohibited;
    }
    // LB12a: do not break before a non-breaking character, except after a space
    // or a hyphen, which "matches widespread implementation practice and
    // supports a common way of handling special line breaking of explicit
    // hyphens".
    if after == LineBreakClass::Gl
        && !matches!(
            before,
            LineBreakClass::Sp | LineBreakClass::Hy | LineBreakClass::Hh
        )
    {
        return Break::Prohibited;
    }
    // LB13: do not break before closing punctuation, a close parenthesis, an
    // exclamation or interrogation mark, or a solidus.
    if matches!(
        after,
        LineBreakClass::Cl | LineBreakClass::Cp | LineBreakClass::Ex | LineBreakClass::Sy
    ) {
        return Break::Prohibited;
    }
    // LB14: do not break after an opening punctuation, even across spaces.
    if context
        .previous_skipping_spaces(at)
        .is_some_and(|unit| unit.class == LineBreakClass::Op)
    {
        return Break::Prohibited;
    }
    // LB15c: break before a decimal mark that follows a space, as in
    // "subtract .5".
    if before == LineBreakClass::Sp
        && after == LineBreakClass::Is
        && context
            .next(at + 1)
            .is_some_and(|unit| unit.class == LineBreakClass::Nu)
    {
        return Break::Allowed;
    }
    // LB15d: otherwise do not break before an infix numeric separator.
    if after == LineBreakClass::Is {
        return Break::Prohibited;
    }
    // LB16: do not break between closing punctuation and a nonstarter.
    if after == LineBreakClass::Ns
        && context
            .previous_skipping_spaces(at)
            .is_some_and(|unit| matches!(unit.class, LineBreakClass::Cl | LineBreakClass::Cp))
    {
        return Break::Prohibited;
    }
    // LB17: do not break within a run of em dashes.
    if after == LineBreakClass::B2
        && context
            .previous_skipping_spaces(at)
            .is_some_and(|unit| unit.class == LineBreakClass::B2)
    {
        return Break::Prohibited;
    }
    // LB18: break after spaces. The rules are applied in order, so the
    // prohibitions in LB13 to LB17 make them indirect breaks: a space does not
    // make one of them breakable.
    if before == LineBreakClass::Sp {
        return Break::Allowed;
    }
    // LB20a: do not break after a word-initial hyphen. `None` is `sot`, which
    // the rule's first alternative lists.
    if matches!(before, LineBreakClass::Hy | LineBreakClass::Hh)
        && matches!(after, LineBreakClass::Al | LineBreakClass::Hl)
        && context.two_before(at).is_none_or(|unit| {
            matches!(
                unit.class,
                LineBreakClass::Bk
                    | LineBreakClass::Cr
                    | LineBreakClass::Lf
                    | LineBreakClass::Nl
                    | LineBreakClass::Sp
                    | LineBreakClass::Zw
                    | LineBreakClass::Gl
            )
        })
    {
        return Break::Prohibited;
    }
    // LB21: do not break before a break-after character, a hyphen or a
    // nonstarter, and do not break after a break-before character.
    if matches!(
        after,
        LineBreakClass::Ba | LineBreakClass::Hh | LineBreakClass::Hy | LineBreakClass::Ns
    ) || before == LineBreakClass::Bb
    {
        return Break::Prohibited;
    }
    // LB21a: do not break after the hyphen in Hebrew + Hyphen + non-Hebrew.
    if matches!(before, LineBreakClass::Hy | LineBreakClass::Hh)
        && after != LineBreakClass::Hl
        && context
            .two_before(at)
            .is_some_and(|unit| unit.class == LineBreakClass::Hl)
    {
        return Break::Prohibited;
    }
    // LB21b: do not break between a solidus and a Hebrew letter.
    if before == LineBreakClass::Sy && after == LineBreakClass::Hl {
        return Break::Prohibited;
    }
    // LB22: do not break before an inseparable character.
    if after == LineBreakClass::In {
        return Break::Prohibited;
    }
    // LB23: do not break between digits and letters.
    if matches!(before, LineBreakClass::Al | LineBreakClass::Hl) && after == LineBreakClass::Nu
        || before == LineBreakClass::Nu && matches!(after, LineBreakClass::Al | LineBreakClass::Hl)
    {
        return Break::Prohibited;
    }
    // LB23a: do not break between a numeric prefix and an ideograph, an emoji
    // base or a modifier, or between those and a numeric postfix.
    if before == LineBreakClass::Pr
        && matches!(
            after,
            LineBreakClass::Id | LineBreakClass::Eb | LineBreakClass::Em
        )
        || matches!(
            before,
            LineBreakClass::Id | LineBreakClass::Eb | LineBreakClass::Em
        ) && after == LineBreakClass::Po
    {
        return Break::Prohibited;
    }
    // LB24: do not break between a numeric prefix or postfix and a letter.
    if matches!(before, LineBreakClass::Pr | LineBreakClass::Po)
        && matches!(after, LineBreakClass::Al | LineBreakClass::Hl)
        || matches!(before, LineBreakClass::Al | LineBreakClass::Hl)
            && matches!(after, LineBreakClass::Pr | LineBreakClass::Po)
    {
        return Break::Prohibited;
    }
    // LB25: do not break numbers. The four arms that end in a prefix or a
    // postfix read the `NU (SY | IS)*` prefix optionally closed by `CL` or `CP`,
    // which is what `(12)%` needs.
    if matches!(after, LineBreakClass::Po | LineBreakClass::Pr)
        && (context.preceded_by_number(at)
            || (matches!(before, LineBreakClass::Cl | LineBreakClass::Cp)
                && context.preceded_by_number(at.saturating_sub(1))))
    {
        return Break::Prohibited;
    }
    if matches!(before, LineBreakClass::Po | LineBreakClass::Pr)
        && (after == LineBreakClass::Nu
            || (after == LineBreakClass::Op
                && context.next(at + 1).is_some_and(|unit| {
                    matches!(unit.class, LineBreakClass::Nu | LineBreakClass::Is)
                })))
    {
        return Break::Prohibited;
    }
    if matches!(before, LineBreakClass::Hy | LineBreakClass::Is)
        || after == LineBreakClass::Nu && context.preceded_by_number(at)
    {
        return Break::Prohibited;
    }
    // LB26: do not break a Korean syllable block.
    if before == LineBreakClass::Jl
        && matches!(
            after,
            LineBreakClass::Jl | LineBreakClass::Jv | LineBreakClass::H2 | LineBreakClass::H3
        )
        || matches!(before, LineBreakClass::Jv | LineBreakClass::H2)
            && matches!(after, LineBreakClass::Jv | LineBreakClass::Jt)
        || matches!(before, LineBreakClass::Jt | LineBreakClass::H3) && after == LineBreakClass::Jt
    {
        return Break::Prohibited;
    }
    // LB27: treat a Korean syllable block the same as an ideograph, so the
    // numeric rules apply to it too.
    if matches!(
        before,
        LineBreakClass::Jl
            | LineBreakClass::Jv
            | LineBreakClass::Jt
            | LineBreakClass::H2
            | LineBreakClass::H3
    ) && after == LineBreakClass::Po
        || before == LineBreakClass::Pr
            && matches!(
                after,
                LineBreakClass::Jl
                    | LineBreakClass::Jv
                    | LineBreakClass::Jt
                    | LineBreakClass::H2
                    | LineBreakClass::H3
            )
    {
        return Break::Prohibited;
    }
    // LB28: do not break between alphabetics.
    if matches!(before, LineBreakClass::Al | LineBreakClass::Hl)
        && matches!(after, LineBreakClass::Al | LineBreakClass::Hl)
    {
        return Break::Prohibited;
    }
    // LB29: do not break between numeric punctuation and alphabetics, as in
    // "e.g.".
    if before == LineBreakClass::Is && matches!(after, LineBreakClass::Al | LineBreakClass::Hl) {
        return Break::Prohibited;
    }
    // LB30: do not break between a letter, digit or ordinary symbol and an
    // opening or closing parenthesis. The specification identifies the excluded
    // East Asian cases as "East_Asian_Width values of Fullwidth, Wide, or
    // Halfwidth", which is what the two `$EastAsian` tests read.
    if after == LineBreakClass::Op
        && !is_east_asian(after_text)
        && matches!(
            before,
            LineBreakClass::Al | LineBreakClass::Hl | LineBreakClass::Nu
        )
    {
        return Break::Prohibited;
    }
    if before == LineBreakClass::Cp
        && !is_east_asian(before_text)
        && matches!(
            after,
            LineBreakClass::Al | LineBreakClass::Hl | LineBreakClass::Nu
        )
    {
        return Break::Prohibited;
    }
    // LB30a: break between two regional indicators "if and only if there are an
    // even number of regional indicators preceding the position of the break",
    // which the rule's own `sot (RI RI)* RI × RI` shows as the prohibition for
    // the odd case. Two indicators are therefore one flag and the break falls
    // between flags.
    if before == LineBreakClass::Ri && after == LineBreakClass::Ri {
        return if context.preceded_by_even_regional_indicators(at) {
            Break::Allowed
        } else {
            Break::Prohibited
        };
    }
    // LB30b: do not break between an emoji base and an emoji modifier.
    if before == LineBreakClass::Eb && after == LineBreakClass::Em {
        return Break::Prohibited;
    }
    if quotation_prohibits(context, at) {
        return Break::Prohibited;
    }
    // LB31: break everywhere else.
    Break::Allowed
}

/// Every soft wrap opportunity in `characters`: one entry per position, so the
/// result has `characters.len() + 1` entries. Entry `i` describes the position
/// *before* `characters[i]`, and the last entry describes the end of the text.
///
/// CSS Text 3 §5: "Wrapping is only performed at an allowed break point". A
/// `white-space` that forbids wrapping therefore gets no opportunities at all,
/// which is what makes `nowrap` and `pre` differ from the values that wrap.
#[must_use]
pub fn opportunities(characters: &[char], options: LineBreakOptions) -> Vec<Break> {
    opportunities_with(characters, |_| options)
}

/// [`opportunities`] with the options resolved per character, which an inline
/// formatting context needs because CSS Text 3 §1.5 ignores inline box
/// boundaries when determining adjacency while §5.1 and §5.2 still apply to the
/// text of each element.
///
/// `options_for` is called once per character with its index; the position at
/// the end of the text reuses the last character's options, which is what
/// "wrapping is forbidden" has to mean for the final break.
#[must_use]
pub fn opportunities_with(
    characters: &[char],
    options_for: impl Fn(usize) -> LineBreakOptions,
) -> Vec<Break> {
    let context = Context::resolve(characters, options_for);
    if !context.options.last().is_some_and(|options| options.wrap) {
        return vec![Break::Prohibited; characters.len() + 1];
    }
    (0..=characters.len())
        .map(|at| {
            // A `white-space` that forbids wrapping removes the opportunity in
            // front of its own text only: a run of `nowrap` text in a wrapping
            // paragraph still breaks where the wrapping text resumes.
            let wraps = context
                .options
                .get(at)
                .or_else(|| context.options.last())
                .is_some_and(|options| options.wrap);
            if !wraps {
                return Break::Prohibited;
            }
            let break_at = decide(&context, at);
            if context
                .options
                .get(at)
                .is_some_and(|options| options.line_break == LineBreakStrictness::Anywhere)
            {
                // §5.2: a soft wrap opportunity around every typographic
                // character unit, "disregarding any prohibition against line
                // breaks, even those introduced by characters with the GL, WJ,
                // or ZWJ line breaking classes or mandated by the word-break
                // property". A prohibition is what it disregards, so a
                // mandatory break - which mandates one - is untouched, as is
                // LB2's "never break at the start of text".
                return match (at, break_at) {
                    (0, _) => Break::Prohibited,
                    (_, Break::Mandatory) => Break::Mandatory,
                    _ => Break::Allowed,
                };
            }
            if !break_at.is_break() || at == 0 || at == characters.len() {
                return break_at;
            }
            // §5.1's `keep-all`: implicit soft wrap opportunities between
            // typographic letter units "are suppressed, i.e. breaks are
            // prohibited between pairs of such characters (regardless of
            // line-break settings other than anywhere)". A mandatory break is
            // not an implicit opportunity, so a hard line break still ends the
            // line, and `anywhere` is already out of this branch.
            let pair_is_letter_like =
                context
                    .previous(at)
                    .zip(context.next(at))
                    .is_some_and(|(previous, next)| {
                        is_letter_like(previous.class) && is_letter_like(next.class)
                    });
            if context
                .options
                .get(at)
                .is_some_and(|options| options.word_break == WordBreak::KeepAll)
                && pair_is_letter_like
            {
                Break::Prohibited
            } else {
                break_at
            }
        })
        .collect()
}

/// The width of the widest run of `characters` that no soft wrap opportunity can
/// split.
///
/// CSS 2.1 §10.3.5 defines a float's "preferred minimum width" as the width
/// found "by trying all possible line breaks", and §10.3.5's preferred width as
/// the one found "by formatting the content without breaking lines other than
/// where explicit line breaks occur". Where a run has no opportunity in it the
/// two coincide, so the answer is the width of the whole run. A script with no
/// word separators has an opportunity between most of its characters, so its
/// widest unbreakable run is one character rather than a whole paragraph -
/// which is what an unspaced Chinese paragraph has to measure as.
///
/// The white space at the end of a run hangs (CSS Text 3 §3, "End-of-line
/// spaces: Hang"), so it is not counted.
#[must_use]
pub fn widest_unbreakable_run(
    characters: &[char],
    options: LineBreakOptions,
    mut measure: impl FnMut(&str) -> f32,
) -> f32 {
    let breaks = opportunities(characters, options);
    let mut widest = 0.0_f32;
    let mut start = 0_usize;
    for (at, break_at) in breaks.iter().enumerate().skip(1) {
        if break_at.is_break() {
            widest = widest.max(measure_run(characters, start, at, &mut measure));
            start = at;
        }
    }
    // Reached when wrapping is forbidden, where no position is a break and the
    // whole run is one unbreakable run.
    if start < characters.len() {
        widest = widest.max(measure_run(
            characters,
            start,
            characters.len(),
            &mut measure,
        ));
    }
    widest
}

/// The width of `characters[start..end]` with its hanging white space removed.
fn measure_run(
    characters: &[char],
    start: usize,
    end: usize,
    measure: &mut impl FnMut(&str) -> f32,
) -> f32 {
    let mut trimmed = end;
    while trimmed > start && characters[trimmed - 1].is_whitespace() {
        trimmed -= 1;
    }
    if trimmed == start {
        return 0.0;
    }
    let run: String = characters[start..trimmed].iter().collect();
    measure(&run)
}
