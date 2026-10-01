//! Tests for the UAX #14 line breaking algorithm and the CSS Text 3 tailorings.
//!
//! Every assertion states what a rule says, not what this implementation happens
//! to produce, so a rule implemented more faithfully later cannot fail one of
//! them. The notation is UAX #14 §6's: `÷` is a break opportunity, `×` is a
//! non-break position and `!` is a mandatory break.

//! `run.chars().count() as f32` is the measurement every width test below uses:
//! the widths are whole character counts of two to five, so the precision loss
//! the cast invites cannot be reached.
#![allow(
    clippy::cast_precision_loss,
    reason = "test measurements are character counts of one to five"
)]
//! The widths are exact small integers of the reference measurer's table, so an
//! equality is the strongest available assertion.
#![allow(clippy::float_cmp, reason = "exact small widths from a table")]

use super::{
    Break, LineBreakOptions, LineBreakStrictness, WordBreak, opportunities, widest_unbreakable_run,
};

/// The classes UAX #14 §6 names in a rule's own summary: an ideograph, a small
/// kana, a digit and a letter.
const HAN: &str = "\u{4e00}";
const SMALL_KANA: &str = "\u{3041}";

fn plain() -> LineBreakOptions {
    LineBreakOptions::wrapping(WordBreak::Normal, LineBreakStrictness::Normal)
}

fn strictness(level: LineBreakStrictness) -> LineBreakOptions {
    LineBreakOptions::wrapping(WordBreak::Normal, level)
}

fn word_break(mode: WordBreak) -> LineBreakOptions {
    LineBreakOptions::wrapping(mode, LineBreakStrictness::Normal)
}

fn characters(text: &str) -> Vec<char> {
    text.chars().collect()
}

/// The position *between* each adjacent pair of characters, so one mark per
/// character: the first is the start of the text and the last is the position
/// before the final character. `÷` allowed, `×` prohibited, `!` mandatory.
fn marks(text: &str, options: LineBreakOptions) -> String {
    opportunities(&characters(text), options)[..text.chars().count()]
        .iter()
        .map(|break_at| match break_at {
            Break::Prohibited => '\u{d7}',
            Break::Allowed => '\u{f7}',
            Break::Mandatory => '!',
        })
        .collect()
}

/// Whether a break is allowed before the character at `index`.
fn breaks_before(text: &str, index: usize, options: LineBreakOptions) -> bool {
    opportunities(&characters(text), options)[index].is_break()
}

/// That no position strictly inside `text` is a break, which is what "a number
/// does not break" and "a word does not break" mean.
fn unbroken_inside(text: &str, options: LineBreakOptions) -> bool {
    let opportunities = opportunities(&characters(text), options);
    opportunities[1..text.chars().count()]
        .iter()
        .all(|break_at| matches!(break_at, Break::Prohibited))
}

#[test]
fn lb2_never_breaks_at_the_start_of_text_and_lb3_always_breaks_at_the_end() {
    let found = opportunities(&characters("ab"), plain());
    assert_eq!(found[0], Break::Prohibited, "LB2 at the start of text");
    assert_eq!(
        found[found.len() - 1],
        Break::Mandatory,
        "LB3 at the end of text"
    );
}

#[test]
fn lb4_and_lb5_make_a_hard_line_break_mandatory() {
    // U+000A is LF and U+2028 is BK; each ends the line outright.
    for hard_break in ['\n', '\u{2028}'] {
        let text = format!("ab{hard_break}cd");
        let found = opportunities(&characters(&text), plain());
        assert_eq!(
            found[3],
            Break::Mandatory,
            "a break after {hard_break:?} is mandatory"
        );
    }
    // `CR × LF` is the no-break half of LB5: the pair is one hard line break.
    assert_eq!(marks("a\r\nb", plain()), "\u{d7}\u{d7}\u{d7}!", "CR LF");
}

#[test]
fn lb7_never_breaks_before_a_space_and_lb18_always_breaks_after_one() {
    // "A 9": no break before the space, a break after it.
    assert_eq!(marks("A 9", plain()), "\u{d7}\u{d7}\u{f7}", "A 9");
}

#[test]
fn lb7_lb8_and_lb8a_place_the_zero_width_controls_where_the_specification_says() {
    // LB7 `× ZW`, LB8 `ZW SP* ÷`, LB8a `ZWJ ×`. So a zero width space is an
    // opportunity on both sides of what follows it, and a zero width joiner
    // removes the opportunity after it.
    assert_eq!(
        marks("\u{200b}\u{200d}A", plain()),
        "\u{d7}\u{f7}\u{d7}",
        "ZW ZWJ A"
    );
    // LB8 skips spaces: `ZW SP* ÷`.
    assert_eq!(
        marks("\u{200b} A", plain()),
        "\u{d7}\u{d7}\u{f7}",
        "ZW SP A"
    );
}

#[test]
fn lb11_and_lb12_prohibit_a_break_around_a_word_joiner_and_a_no_break_space() {
    // `× WJ`, `WJ ×`, `GL ×` and LB12a's `[^SP HY HH] × GL`.
    assert_eq!(
        marks("A\u{2060}A\u{00a0}A", plain()),
        "\u{d7}\u{d7}\u{d7}\u{d7}\u{d7}",
        "WJ and NBSP are non-break on both sides"
    );
}

#[test]
fn lb13_never_breaks_before_closing_punctuation_or_a_solidus() {
    for closer in [")", "\u{ff09}", "!", "/"] {
        let text = format!("A{closer}A");
        assert!(
            !breaks_before(&text, 1, plain()),
            "no break before {closer:?}"
        );
    }
}

#[test]
fn lb30_keeps_a_narrow_close_parenthesis_with_the_letter_after_it() {
    // `[CP - $EastAsian] × (AL | HL | NU)`, which is what stops a break inside
    // "person(s)". The exclusion is on the *parenthesis*: `)` is CP and narrow,
    // so the arm applies, while `）` is CL and neither arm of LB30 reaches it,
    // leaving an ordinary opportunity after it.
    assert!(
        !breaks_before("A)A", 2, plain()),
        "narrow CP holds the letter"
    );
    assert!(breaks_before("A）A", 2, plain()), "fullwidth CL does not");
    // LB14 keeps an opening parenthesis with what follows it at every width.
    assert!(!breaks_before("A（A", 2, plain()), "LB14 after an OP");
}

#[test]
fn lb14_never_breaks_after_an_opening_punctuation_even_across_spaces() {
    // `OP SP* ×`, so the position between the space and `A` is a non-break too.
    assert_eq!(marks("( A", plain()), "\u{d7}\u{d7}\u{d7}", "( SP A");
}

#[test]
fn lb15c_breaks_before_a_decimal_mark_that_follows_a_space() {
    // `SP ÷ IS NU`, as in "subtract .5". The `.` and the `5` still hold
    // together, which is LB15d's `× IS` and LB25's `IS × NU`.
    assert_eq!(
        marks("A .5", plain()),
        "\u{d7}\u{d7}\u{f7}\u{d7}",
        "A SP . 5"
    );
}

#[test]
fn lb16_keeps_a_nonstarter_with_the_closing_punctuation_before_it() {
    // `(CL | CP) SP* × NS`: U+FF09 is CP and U+3005 is NS.
    let text = "A\u{ff09}\u{3005}A";
    assert!(!breaks_before(text, 2, plain()), "no break before the NS");
    assert!(
        breaks_before(text, 3, plain()),
        "the run ends after the nonstarter"
    );
}

#[test]
fn a_closing_punctuation_mark_cannot_start_a_line() {
    // The recognisable CJK case. U+3001 and U+3002 are CL, and LB13's `× CL`
    // keeps the mark with the character before it; the opportunity is after it.
    for mark in ['\u{3001}', '\u{3002}', '\u{ff0c}', '\u{ff0e}'] {
        let text = format!("{HAN}{mark}{HAN}");
        assert!(
            !breaks_before(&text, 1, plain()),
            "no break before {mark:?}"
        );
        assert!(breaks_before(&text, 2, plain()), "a break after {mark:?}");
    }
}

#[test]
fn lb21_never_breaks_before_a_nonstarter() {
    // U+3005 IDEOGRAPHIC ITERATION MARK is NS, so it cannot open a line either.
    let text = format!("{HAN}\u{3005}{HAN}");
    assert!(!breaks_before(&text, 1, plain()), "no break before the NS");
    assert!(breaks_before(&text, 2, plain()), "a break after the NS");
}

#[test]
fn lb25_never_breaks_a_number() {
    for number in ["1,234", "12.54", "1/2", "-1", "(12)%", "1.5%", "12:59"] {
        assert!(unbroken_inside(number, plain()), "{number} must not break");
    }
}

#[test]
fn lb26_never_breaks_a_hangul_syllable_block() {
    // Conjoining jamo: L, V and T form one syllable block, so no break falls
    // inside `L V T L`.
    let text = "\u{1100}\u{1161}\u{11a8}\u{1100}";
    assert_eq!(marks(text, plain()), "\u{d7}\u{d7}\u{d7}\u{f7}", "L V T L");
}

#[test]
fn lb28_never_breaks_between_two_latin_letters() {
    // This is what makes an unspaced Latin word one unbreakable run.
    assert!(unbroken_inside("abc", plain()), "a word does not break");
}

#[test]
fn lb31_breaks_between_two_ideographs() {
    // The case the whole module exists for: an unspaced Chinese paragraph has
    // an opportunity between most of its characters.
    assert_eq!(
        marks("\u{4e00}\u{4e8c}\u{4e09}", plain()),
        "\u{d7}\u{f7}\u{f7}",
        "three ideographs"
    );
}

#[test]
fn lb30a_keeps_regional_indicator_pairs_together() {
    // A flag is two regional indicators and the break falls between flags.
    let flag = "\u{1f1e8}\u{1f1f3}";
    assert_eq!(
        marks(&format!("{flag}{flag}"), plain()),
        "\u{d7}\u{d7}\u{f7}\u{d7}"
    );
}

#[test]
fn lb30b_keeps_an_emoji_modifier_with_its_base() {
    // `EB × EM`: U+261D is EB and U+1F3FB is EM.
    let text = "\u{261d}\u{1f3fb}\u{261d}";
    assert_eq!(marks(text, plain()), "\u{d7}\u{d7}\u{f7}", "EB EM EB");
}

#[test]
fn lb9_keeps_a_combining_mark_with_its_base() {
    // "Treat X (CM | ZWJ)* as if it were X", and §8.2's sixth example makes
    // that the grapheme-cluster tailoring, so no break falls inside one.
    assert!(
        unbroken_inside("A\u{301}A", plain()),
        "a combining mark stays"
    );
}

#[test]
fn lb10_makes_a_lone_combining_mark_alphabetic_and_lb28_then_keeps_it() {
    // A combining mark with no base is AL rather than a break opportunity.
    assert!(
        unbroken_inside("\u{301}A", plain()),
        "a lone CM joins the letter"
    );
}

#[test]
fn lb19_and_lb19a_prohibit_a_break_on_either_side_of_a_quotation_mark() {
    // LB19a relaxes only where the mark is "surrounded by East Asian
    // characters", and the `QU` class holds only ambiguous-width quotation
    // marks, so the relaxation never fires and both sides are non-break.
    assert_eq!(
        marks("A\u{201c}B\u{201d}", plain()),
        "\u{d7}\u{d7}\u{d7}\u{d7}",
        "A open-quote B close-quote"
    );
}

#[test]
fn word_break_break_all_permits_a_break_inside_a_latin_word() {
    // §5.1: letters are "instead treated as ID for the purpose of
    // line-breaking".
    assert_eq!(
        marks("abc", word_break(WordBreak::BreakAll)),
        "\u{d7}\u{f7}\u{f7}",
        "abc"
    );
}

#[test]
fn word_break_break_all_still_respects_punctuation() {
    // §5.1's own note: "This value does not affect whether there are soft wrap
    // opportunities around punctuation characters."
    assert_eq!(
        marks("ab)", word_break(WordBreak::BreakAll)),
        "\u{d7}\u{f7}\u{d7}",
        "ab)"
    );
}

#[test]
fn word_break_keep_all_prohibits_a_break_between_two_ideographs() {
    // §5.1: "In this style, sequences of CJK characters do not break."
    assert!(unbroken_inside(
        "\u{4e00}\u{4e8c}\u{4e09}",
        word_break(WordBreak::KeepAll)
    ));
    // A space is still an opportunity, because `keep-all` is about letters.
    assert_eq!(
        marks(&format!("{HAN} {HAN}"), word_break(WordBreak::KeepAll)),
        "\u{d7}\u{d7}\u{f7}",
        "a space still breaks under keep-all"
    );
}

#[test]
fn word_break_keep_all_does_not_override_line_break_anywhere() {
    // §5.1: the suppression applies "regardless of line-break settings other
    // than anywhere".
    let text = "\u{4e00}\u{4e8c}";
    let options = LineBreakOptions::wrapping(WordBreak::KeepAll, LineBreakStrictness::Anywhere);
    assert!(breaks_before(text, 1, options), "anywhere still breaks");
}

#[test]
fn line_break_strict_and_normal_forbid_a_break_before_a_small_kana_and_loose_allows_it() {
    // §5.2: breaks before "characters from the Unicode line breaking class CJ"
    // are "forbidden for normal and strict line breaking and allowed in loose".
    // UAX #14 §5.1 gives the implementation: CJ as NS is strict, CJ as ID is
    // normal, and the table above shows the same character in each.
    let text = format!("{HAN}{SMALL_KANA}{HAN}");
    for level in [
        LineBreakStrictness::Strict,
        LineBreakStrictness::Normal,
        LineBreakStrictness::Auto,
    ] {
        assert!(
            !breaks_before(&text, 1, strictness(level)),
            "{level:?} must forbid a break before a small kana"
        );
    }
    assert!(breaks_before(
        &text,
        1,
        strictness(LineBreakStrictness::Loose)
    ));
}

#[test]
fn line_break_normal_allows_the_two_characters_that_strict_forbids() {
    // §5.2 requires "breaks before certain CJK hyphen-like characters: 〜 U+301C,
    // ゠ U+30A0" to be "allowed for normal and loose line breaking if the
    // writing system is Chinese or Japanese, and are otherwise forbidden".
    for character in ['\u{301c}', '\u{30a0}'] {
        let text = format!("{HAN}{character}{HAN}");
        for level in [LineBreakStrictness::Normal, LineBreakStrictness::Loose] {
            assert!(
                breaks_before(&text, 1, strictness(level)),
                "{level:?} must allow a break before {character:?}"
            );
        }
        assert!(
            !breaks_before(&text, 1, strictness(LineBreakStrictness::Strict)),
            "strict must forbid a break before {character:?}"
        );
    }
}

#[test]
fn line_break_loose_allows_the_punctuation_that_normal_and_strict_forbid() {
    // §5.2's row: breaks before the centred punctuation marks and the doubled
    // punctuation are "allowed for loose line breaking if the writing system is
    // Chinese or Japanese, and are otherwise forbidden".
    for character in [
        '\u{30fb}', '\u{ff1a}', '\u{ff1b}', '\u{ff65}', '\u{203c}', '\u{2047}', '\u{2048}',
        '\u{2049}', '\u{ff01}', '\u{ff1f}',
    ] {
        let text = format!("{HAN}{character}{HAN}");
        for level in [LineBreakStrictness::Normal, LineBreakStrictness::Strict] {
            assert!(
                !breaks_before(&text, 1, strictness(level)),
                "{level:?} must forbid a break before {character:?}"
            );
        }
        assert!(
            breaks_before(&text, 1, strictness(LineBreakStrictness::Loose)),
            "loose must allow a break before {character:?}"
        );
    }
}

#[test]
fn line_break_loose_allows_a_break_around_an_ambiguous_width_numeric_affix() {
    // §5.2: breaks before a `PO` with East_Asian_Width Ambiguous, Fullwidth or
    // Wide are allowed for loose, and the same after a `PR`. U+FF05 is
    // FULLWIDTH PERCENT SIGN, class PO; U+FFE5 is FULLWIDTH YEN SIGN, class PR.
    let postfix = format!("{HAN}\u{ff05}");
    assert!(!breaks_before(
        &postfix,
        1,
        strictness(LineBreakStrictness::Normal)
    ));
    assert!(breaks_before(
        &postfix,
        1,
        strictness(LineBreakStrictness::Loose)
    ));
    let prefix = format!("\u{ffe5}{HAN}");
    assert!(!breaks_before(
        &prefix,
        1,
        strictness(LineBreakStrictness::Normal)
    ));
    assert!(breaks_before(
        &prefix,
        1,
        strictness(LineBreakStrictness::Loose)
    ));
}

#[test]
fn line_break_loose_allows_a_break_between_inseparable_characters() {
    // §5.2: breaks "between inseparable characters (such as ‥ U+2025,
    // … U+2026), i.e. characters from the Unicode line breaking class IN" are
    // "forbidden for normal and strict line breaking and allowed in loose".
    let text = "A\u{2025}\u{2026}";
    assert!(!breaks_before(
        text,
        1,
        strictness(LineBreakStrictness::Normal)
    ));
    assert!(!breaks_before(
        text,
        2,
        strictness(LineBreakStrictness::Normal)
    ));
    assert!(breaks_before(
        text,
        1,
        strictness(LineBreakStrictness::Loose)
    ));
    assert!(breaks_before(
        text,
        2,
        strictness(LineBreakStrictness::Loose)
    ));
}

#[test]
fn line_break_loose_allows_a_break_before_a_hyphen() {
    // §5.2: breaks before "‐ U+2010, – U+2013" are allowed for loose "if the
    // preceding character belongs to the Unicode line breaking class ID".
    let text = format!("{HAN}\u{2010}{HAN}");
    assert!(!breaks_before(
        &text,
        1,
        strictness(LineBreakStrictness::Normal)
    ));
    assert!(breaks_before(
        &text,
        1,
        strictness(LineBreakStrictness::Loose)
    ));
}

#[test]
fn line_break_anywhere_breaks_between_every_pair_of_typographic_character_units() {
    // §5.2: "disregarding any prohibition against line breaks, even those
    // introduced by characters with the GL, WJ, or ZWJ line breaking classes",
    // which is why a word joiner and a no-break space are in the input.
    assert_eq!(
        marks(
            "ab\u{2060}\u{00a0}c",
            strictness(LineBreakStrictness::Anywhere)
        ),
        "\u{d7}\u{f7}\u{f7}\u{f7}\u{f7}",
        "anywhere disregards GL and WJ"
    );
}

#[test]
fn the_non_tailorable_prohibitions_survive_line_break_anywhere() {
    // §5.2 disclaims GL, WJ and ZWJ, and nothing else: the non-tailorable rules
    // of §6.1 that are not about those three still hold. A hard line break
    // cannot be softened.
    let text = "a\u{2028}b";
    let found = opportunities(&characters(text), strictness(LineBreakStrictness::Anywhere));
    assert_eq!(found[2], Break::Mandatory, "LB4 is not tailorable");
}

#[test]
fn line_break_strict_never_breaks_before_a_closing_parenthesis_in_latin_text() {
    // The Western case of the same rule: `)` is CP and LB13 applies at every
    // strictness level, including `loose`, whose §5.2 rows do not relax it. Only
    // `anywhere` disregards it, and that has its own test.
    for level in [
        LineBreakStrictness::Strict,
        LineBreakStrictness::Normal,
        LineBreakStrictness::Loose,
    ] {
        assert!(
            !breaks_before("ab)", 2, strictness(level)),
            "{level:?} must forbid a break before a close parenthesis"
        );
    }
}

#[test]
fn a_white_space_that_forbids_wrapping_has_no_opportunity_at_all() {
    // CSS Text 3 §3: `pre` and `nowrap` "do not allow wrapping". §5: "Wrapping
    // is only performed at an allowed break point".
    let nowrap = LineBreakOptions {
        wrap: false,
        ..plain()
    };
    assert_eq!(marks("a b", nowrap), "\u{d7}\u{d7}\u{d7}", "nowrap");
    // The space itself is never a break position (LB7 `× SP`); the opportunity
    // is the one after it (LB18 `SP ÷`).
    assert!(!breaks_before("a b", 1, plain()), "LB7 before the space");
    assert!(breaks_before("a b", 2, plain()), "LB18 after the space");
}

#[test]
fn a_run_with_no_opportunity_in_it_measures_as_one_unbreakable_run() {
    // CSS 2.1 §10.3.5: the preferred minimum width is "the width found by
    // trying all possible line breaks", which for a word with none is the word.
    let width = widest_unbreakable_run(&characters("word"), plain(), |run| {
        run.chars().count() as f32
    });
    assert_eq!(width, 4.0);
}

#[test]
fn an_unspaced_run_measures_per_character_and_not_as_a_whole_paragraph() {
    // The defect this fixes. A Chinese paragraph has an opportunity between
    // most of its characters, so its widest unbreakable run is one character
    // rather than the whole paragraph.
    let text = characters("\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}");
    let width = widest_unbreakable_run(&text, plain(), |run| run.chars().count() as f32);
    assert_eq!(width, 1.0, "an ideographic run breaks per character");
}

#[test]
fn a_run_whose_breaks_are_all_forbidden_measures_as_the_whole_run() {
    // `word-break: keep-all` removes the opportunities, so the run is the whole
    // string again. §5.1: "The effects of word-break are taken into account
    // when computing intrinsic sizes."
    let text = characters("\u{4e00}\u{4e8c}\u{4e09}");
    let width = widest_unbreakable_run(&text, word_break(WordBreak::KeepAll), |run| {
        run.chars().count() as f32
    });
    assert_eq!(width, 3.0);
}

#[test]
fn a_white_space_that_forbids_wrapping_measures_as_one_run() {
    // The same rule applied to `nowrap`: with no opportunity anywhere, the
    // unbreakable run is the whole text.
    let nowrap = LineBreakOptions {
        wrap: false,
        ..plain()
    };
    let width = widest_unbreakable_run(&characters("ab cd"), nowrap, |run| {
        run.chars()
            .filter(|character| !character.is_whitespace())
            .count() as f32
    });
    assert_eq!(width, 4.0);
}

#[test]
fn a_trailing_space_hangs_and_is_not_counted() {
    // CSS Text 3 §3's table says "End-of-line spaces: Hang" for `normal`.
    let width = widest_unbreakable_run(&characters("ab "), plain(), |run| {
        run.chars()
            .filter(|character| !character.is_whitespace())
            .count() as f32
    });
    assert_eq!(width, 2.0);
}

#[test]
fn a_mandatory_break_ends_a_run() {
    // UAX #14 LB4: a mandatory break is a break opportunity, so the runs either
    // side of it are measured independently.
    let width = widest_unbreakable_run(&characters("ab\ncd"), plain(), |run| {
        run.chars().filter(|character| *character != '\n').count() as f32
    });
    assert_eq!(width, 2.0);
}

#[test]
fn a_run_of_only_white_space_measures_zero() {
    assert_eq!(
        widest_unbreakable_run(&characters("  "), plain(), |run| run.chars().count() as f32),
        0.0
    );
}

#[test]
fn an_empty_run_measures_zero() {
    assert_eq!(
        widest_unbreakable_run(&[], plain(), |run| run.chars().count() as f32),
        0.0
    );
    assert_eq!(
        opportunities(&[], plain()),
        [Break::Prohibited],
        "LB2 at the start"
    );
}
