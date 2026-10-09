//! The engine's string model, and the one place it is decided.
//!
//! ECMA-262 §6.1.4 defines a String value as a **sequence of UTF-16 code
//! units**, and §6.1.4.1 is explicit that "the ECMAScript String type ... does
//! not interpret the contents as Unicode characters (code points)". The
//! specification then uses *two* different notions on purpose, and conflating
//! them is the defect this module exists to prevent:
//!
//! * **Code units address a string.** `length`, every `String.prototype`
//!   index and range operation, and the indexed own properties of a String
//!   exotic object all number positions in code units. A request that lands
//!   between the two halves of a surrogate pair therefore *does* return half a
//!   pair, and that is the specified answer rather than an accident.
//! * **Code points walk a string.** `String.prototype[Symbol.iterator]`
//!   (§22.1.3.27) yields "the individual code points", so `for...of`, spread
//!   and destructuring see `'😀'` as one element.
//!
//! A Rust `str` is a sequence of Unicode scalar values, so it can hold neither
//! a lone surrogate nor "two halves of a pair" as distinct values. This crate
//! represents an **unpaired surrogate** as a private-use placeholder in
//! `U+F0000..=U+F07FF` (see `crate::lexer::surrogate_placeholder`), which makes
//! the representation a faithful image of the code-unit sequence in both
//! directions:
//!
//! | in a Rust `str`                       | code units        |
//! |--------------------------------------|-------------------|
//! | a placeholder (`0xF0000 + d - 0xD800`) | one surrogate     |
//! | any other scalar below `U+10000`       | one unit          |
//! | any scalar at or above `U+10000`       | a surrogate pair  |
//!
//! Because the placeholder occupies one code unit and an astral scalar
//! occupies two, an engine string's code-unit length is *not* its `chars()`
//! count. That difference is the whole bug this module fixes, so every
//! conversion lives here rather than being re-derived at each call site: an
//! operation that addresses a string calls [`utf16_units`]/[`utf16_length`],
//! and an operation that walks one calls `str::chars` directly.
//!
//! Nothing here depends on layout or text segmentation. `render-layout`
//! counts one typographic unit per code point and is right to; the defect was
//! that script was shown the wrong units, not that the engine thought in code
//! points.

use crate::lexer::surrogate_placeholder;

/// First private-use scalar standing in for an unpaired surrogate.
const PLACEHOLDER_BASE: u32 = 0xF_0000;
/// Last private-use scalar standing in for an unpaired surrogate.
const PLACEHOLDER_END: u32 = 0xF_07FF;

/// The first surrogate code unit, `U+D800`.
const LEADING_SURROGATE: u32 = 0xD800;
/// The last surrogate code unit, `U+DFFF`.
const TRAILING_SURROGATE: u32 = 0xDFFF;

/// Whether `character` is the placeholder standing in for one unpaired
/// surrogate, and so occupies exactly one code unit.
pub(crate) fn is_placeholder(character: char) -> bool {
    (PLACEHOLDER_BASE..=PLACEHOLDER_END).contains(&u32::from(character))
}

/// The single code unit a placeholder stands for, or `None` for any other
/// scalar (which is one or two units, so it has no single-unit answer).
pub(crate) fn placeholder_unit(character: char) -> Option<u16> {
    let value = u32::from(character);
    // `checked_sub` rather than arithmetic: the offset only exists for a
    // placeholder, and `bool::then_some` would evaluate it for every scalar
    // including ones far below the private-use plane.
    is_placeholder(character)
        .then(|| LEADING_SURROGATE + (value - PLACEHOLDER_BASE))
        .and_then(|surrogate| u16::try_from(surrogate).ok())
}

/// §6.1.4 `StringValue` as this engine stores it: a Rust `str` whose
/// placeholders stand in for unpaired surrogates.
pub(crate) fn string_from_utf16(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|unit| {
            unit.unwrap_or_else(|error| {
                surrogate_placeholder(u32::from(error.unpaired_surrogate()))
            })
        })
        .collect()
}

/// One code unit as a string. This is what `charAt`, `at` and every substring
/// whose range happens to end mid-pair produce, and it is why the engine must
/// be able to hold a lone surrogate at all.
pub(crate) fn string_from_unit(unit: u16) -> String {
    string_from_utf16(std::slice::from_ref(&unit))
}

/// The code units of `text`, in order. Every operation that *addresses* a
/// string works on this.
pub(crate) fn utf16_units(text: &str) -> Vec<u16> {
    let mut units = Vec::with_capacity(text.len());
    for character in text.chars() {
        if let Some(unit) = placeholder_unit(character) {
            units.push(unit);
        } else {
            let mut encoded = [0u16; 2];
            units.extend_from_slice(character.encode_utf16(&mut encoded));
        }
    }
    units
}

/// §6.1.4 `StringValue`'s length, in code units. This is what `length`,
/// `LengthOfArrayLike` and every range bound must use; it is *not*
/// `text.chars().count()`, which answers how many Unicode scalar values the
/// string holds.
pub(crate) fn utf16_length(text: &str) -> usize {
    text.chars().map(character_code_units).sum()
}

/// How many UTF-16 code units one engine character occupies: a placeholder is
/// one, because it stands for exactly one surrogate.
fn character_code_units(character: char) -> usize {
    if is_placeholder(character) {
        1
    } else {
        character.len_utf16()
    }
}

/// §11.1.4 `CodePointAt`, the whole of it: read the code point *starting with*
/// the code unit at `position`.
///
/// A position holding a trailing surrogate, a position whose partner is not a
/// surrogate at all, and a position one short of a pair's end all report that
/// single code unit's own value - they do not report `undefined`, and they do
/// not pair a leading surrogate with a non-surrogate that follows it. This is
/// the one operation where both notions are visible at once: the *index* is a
/// code-unit offset and the *answer* is a code point.
pub(crate) fn code_point_at(units: &[u16], position: usize) -> u32 {
    let Some(&first) = units.get(position) else {
        return 0;
    };
    let leading = u32::from(first);
    if !(LEADING_SURROGATE..=0xDBFF).contains(&leading) {
        return u32::from(first);
    }
    // A leading surrogate with no trailing partner is itself the answer.
    let Some(&second) = units.get(position + 1) else {
        return leading;
    };
    let trailing = u32::from(second);
    if !(0xDC00..=TRAILING_SURROGATE).contains(&trailing) {
        return leading;
    }
    0x1_0000 + ((leading - LEADING_SURROGATE) << 10) + (trailing - 0xDC00)
}

#[cfg(test)]
mod tests {
    use super::{code_point_at, string_from_unit, utf16_length, utf16_units};
    use crate::lexer::surrogate_placeholder;

    /// The code-unit view agrees with `str::encode_utf16` for any string with
    /// no unpaired surrogate, which is the invariant the placeholder scheme
    /// must not disturb.
    #[test]
    fn plain_strings_match_std_utf16() {
        for text in ["", "abc", "\u{4f60}\u{597d}", "caf\u{e9}", "a\u{1f600}b"] {
            let expected: Vec<u16> = text.encode_utf16().collect();
            assert_eq!(utf16_units(text), expected, "{text:?}");
            assert_eq!(utf16_length(text), expected.len(), "{text:?}");
        }
    }

    /// An unpaired surrogate is one code unit, and a placeholder is one code
    /// unit, so `'😀'.length` is 2 while the scalar count is 1.
    #[test]
    fn a_placeholder_is_one_code_unit() {
        let lone = surrogate_placeholder(0xd83d).to_string();
        assert_eq!(utf16_length(&lone), 1);
        assert_eq!(utf16_units(&lone), vec![0xd83d]);
    }

    #[test]
    fn an_astral_scalar_is_two_code_units() {
        let emoji = "\u{1f600}";
        assert_eq!(utf16_length(emoji), 2);
        assert_eq!(utf16_units(emoji), vec![0xd83d, 0xde00]);
    }

    #[test]
    fn units_round_trip_through_the_placeholder_mapping() {
        for units in [
            vec![],
            vec![0x61],
            vec![0xd83d, 0xde00],
            vec![0xd83d],
            vec![0xde00],
            vec![0xd83d, 0x61, 0xdfff],
        ] {
            let text = super::string_from_utf16(&units);
            assert_eq!(utf16_units(&text), units, "{units:?}");
            assert_eq!(utf16_length(&text), units.len(), "{units:?}");
        }
    }

    /// §11.1.4 `CodePointAt`: pair only when a leading surrogate is *actually*
    /// followed by a trailing one.
    #[test]
    fn code_point_at_follows_the_specified_pairing() {
        let paired = utf16_units("\u{1f600}");
        assert_eq!(code_point_at(&paired, 0), 0x1f600);
        // A trailing surrogate in the lead position is its own answer.
        assert_eq!(code_point_at(&paired, 1), 0xde00);
        let lone = utf16_units(&surrogate_placeholder(0xd83d).to_string());
        assert_eq!(code_point_at(&lone, 0), 0xd83d);
        // A leading surrogate followed by a non-surrogate stays unpaired.
        let mixed = utf16_units("a\u{1f600}");
        assert_eq!(code_point_at(&mixed, 0), 0x61);
        assert_eq!(code_point_at(&mixed, 1), 0x1f600);
        let followed = utf16_units("\u{1f600}a");
        assert_eq!(code_point_at(&followed, 0), 0x1f600);
        assert_eq!(code_point_at(&followed, 2), 0x61);
    }

    #[test]
    fn one_unit_becomes_a_one_unit_string() {
        assert_eq!(string_from_unit(0x61), "a");
        assert_eq!(utf16_length(&string_from_unit(0xd83d)), 1);
        assert_eq!(utf16_length(&string_from_unit(0xde00)), 1);
    }
}
