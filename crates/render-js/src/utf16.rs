//! The engine's string value: a sequence of UTF-16 code units.
//!
//! ECMA-262 §6.1.4 defines a String value as "a finite ordered sequence of
//! zero or more 16-bit unsigned integer values", and §6.1.4.1 is explicit that
//! the sequence is not interpreted as Unicode characters. Every spec-visible
//! string operation therefore works on code units: `length`, indexing,
//! `charCodeAt`, slicing, comparison (§6.1.4.1 `IsLessThan` compares code units)
//! and the property-key identity of a string. Code points are used only where
//! the standard asks for them, such as `String.prototype[@@iterator]` and
//! `codePointAt`.
//!
//! [`JsString`] stores exactly that sequence. A Rust `str` cannot hold a lone
//! surrogate, so the conversions are explicit and lossless in one direction
//! only:
//!
//! * [`JsString::from`] takes real Unicode text (a host string, a source
//!   identifier, a Rust literal) and encodes it with `encode_utf16`. No text is
//!   reinterpreted.
//! * [`JsString::to_lossy`] is the host-boundary conversion (DOM, URL, console
//!   output): unpaired surrogates become U+FFFD, the WHATWG well-formed
//!   replacement.
//! * [`JsString::to_key`] and [`JsString::from_key`] are a bijection between
//!   code-unit sequences and Rust `String`s, used only for property keys (which
//!   the object model stores as `String`). A lone surrogate is written as one
//!   private-use scalar from `U+F0000..=U+F07FF`, and a real scalar in that
//!   range (or the escape scalar `U+10FFFF`) is written behind an escape, so a
//!   key that spells a real private-use character cannot collide with a key
//!   that spells a lone surrogate.

use std::fmt;
use std::sync::Arc;

/// The first key scalar standing for an unpaired surrogate: `U+D800` maps to
/// `U+F0000`, and so on through `U+DFFF` at `U+F07FF`.
const KEY_SURROGATE_BASE: u32 = 0xF_0000;
/// The last key scalar standing for an unpaired surrogate.
const KEY_SURROGATE_END: u32 = 0xF_07FF;
/// The escape scalar in property-key text. It precedes any real scalar that
/// would otherwise be read as a surrogate marker or as the escape itself.
const KEY_ESCAPE: char = '\u{10_FFFF}';

/// An ECMAScript String value (ECMA-262 §6.1.4): an immutable sequence of
/// UTF-16 code units. Clones share the buffer. The buffer is `Arc` so that
/// parsed scripts, which hold string literals, stay `Send` and `Sync`.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsString(Arc<[u16]>);

impl JsString {
    /// The empty string.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A string holding exactly `units`, surrogates included.
    #[must_use]
    pub fn from_units(units: impl AsRef<[u16]>) -> Self {
        Self(Arc::from(units.as_ref()))
    }

    /// The one-unit string holding `unit`, which may be a lone surrogate.
    #[must_use]
    pub fn from_unit(unit: u16) -> Self {
        Self::from_units([unit])
    }

    /// A string owning `units`.
    #[must_use]
    pub fn from_unit_vec(units: Vec<u16>) -> Self {
        Self(Arc::from(units))
    }

    /// The code units of this string, in order.
    #[must_use]
    pub fn units(&self) -> &[u16] {
        &self.0
    }

    /// §6.1.4 `length`: the number of code units.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the string has no code units.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The code unit at `index`, if there is one.
    #[must_use]
    pub fn unit_at(&self, index: usize) -> Option<u16> {
        self.0.get(index).copied()
    }

    /// The code units from `start` up to `end` (both code-unit offsets,
    /// clamped to the string).
    #[must_use]
    pub fn substring(&self, start: usize, end: usize) -> Self {
        let end = end.min(self.0.len());
        let start = start.min(end);
        Self::from_units(&self.0[start..end])
    }

    /// The concatenation of `self` and `other`.
    #[must_use]
    pub fn concat(&self, other: &Self) -> Self {
        let mut units = Vec::with_capacity(self.0.len() + other.0.len());
        units.extend_from_slice(&self.0);
        units.extend_from_slice(&other.0);
        Self::from_unit_vec(units)
    }

    /// Whether the string contains no unpaired surrogate (ECMA-262
    /// `IsStringWellFormedUnicode`).
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        char::decode_utf16(self.0.iter().copied()).all(|unit| unit.is_ok())
    }

    /// The host-boundary text of this string: each unpaired surrogate becomes
    /// U+FFFD, as `WebIDL`'s `USVString` conversion does. Use it wherever the text
    /// leaves the engine for storage or display in a Rust `String`.
    #[must_use]
    pub fn to_lossy(&self) -> String {
        char::decode_utf16(self.0.iter().copied())
            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }

    /// The property-key form of this string: a `String` that round-trips
    /// through [`JsString::from_key`] to exactly the same code units. Two
    /// strings have equal keys exactly when they have equal code units.
    #[must_use]
    pub fn to_key(&self) -> String {
        let mut key = String::with_capacity(self.0.len());
        for unit in char::decode_utf16(self.0.iter().copied()) {
            match unit {
                Ok(character) if character == KEY_ESCAPE || is_key_surrogate(character) => {
                    key.push(KEY_ESCAPE);
                    key.push(character);
                }
                Ok(character) => key.push(character),
                Err(error) => {
                    let offset = u32::from(error.unpaired_surrogate()) - 0xD800;
                    key.push(char::from_u32(KEY_SURROGATE_BASE + offset).unwrap_or('\u{fffd}'));
                }
            }
        }
        key
    }

    /// The inverse of [`JsString::to_key`].
    #[must_use]
    pub fn from_key(key: &str) -> Self {
        let mut units = Vec::with_capacity(key.len());
        let mut characters = key.chars();
        while let Some(character) = characters.next() {
            if character == KEY_ESCAPE {
                // An escape is always followed by the real scalar it protects.
                // A bare trailing escape cannot come from `to_key`; it is kept
                // as itself rather than dropped.
                let real = characters.next().unwrap_or(KEY_ESCAPE);
                push_scalar(&mut units, real);
            } else if is_key_surrogate(character) {
                // `is_key_surrogate` bounds the offset to 0x800 values, so the
                // unit is always a surrogate and the fallback never applies.
                let offset = u32::from(character) - KEY_SURROGATE_BASE;
                units.push(u16::try_from(0xD800 + offset).unwrap_or(0xFFFD));
            } else {
                push_scalar(&mut units, character);
            }
        }
        Self::from_unit_vec(units)
    }

    /// The code points of this string as one-element-or-two-unit strings, in
    /// order (ECMA-262 22.1.3.27 `String.prototype[@@iterator]`): a valid pair
    /// is one element, and an unpaired surrogate is an element of its own.
    #[must_use]
    pub fn code_point_strings(&self) -> Vec<Self> {
        let mut elements = Vec::with_capacity(self.0.len());
        let mut index = 0;
        while index < self.0.len() {
            let width = if code_point_at(&self.0, index) > 0xFFFF {
                2
            } else {
                1
            };
            elements.push(self.substring(index, index + width));
            index += width;
        }
        elements
    }

    /// The code point that starts at code-unit `position` (ECMA-262 11.1.4
    /// `CodePointAt`): a valid pair yields the astral scalar, and any other
    /// unit yields its own value. Callers check the bound first.
    #[must_use]
    pub fn code_point_at(&self, position: usize) -> u32 {
        code_point_at(&self.0, position)
    }
}

/// Whether `character` is a key scalar that stands for one unpaired surrogate.
fn is_key_surrogate(character: char) -> bool {
    (KEY_SURROGATE_BASE..=KEY_SURROGATE_END).contains(&u32::from(character))
}

/// Append the code units of one real Unicode scalar.
fn push_scalar(units: &mut Vec<u16>, character: char) {
    let mut encoded = [0_u16; 2];
    units.extend_from_slice(character.encode_utf16(&mut encoded));
}

/// §11.1.4 `CodePointAt`, the whole of it: the code point *starting with* the
/// code unit at `position`. A position holding a trailing surrogate, a
/// position whose partner is not a trailing surrogate, and a position at the
/// end all report that single unit's value; a pair is never split or joined
/// across a non-surrogate.
#[must_use]
pub fn code_point_at(units: &[u16], position: usize) -> u32 {
    let Some(&first) = units.get(position) else {
        return 0;
    };
    let leading = u32::from(first);
    if !(0xD800..=0xDBFF).contains(&leading) {
        return leading;
    }
    let Some(&second) = units.get(position + 1) else {
        return leading;
    };
    let trailing = u32::from(second);
    if !(0xDC00..=0xDFFF).contains(&trailing) {
        return leading;
    }
    0x1_0000 + ((leading - 0xD800) << 10) + (trailing - 0xDC00)
}

/// Whether a code point written as an escape (`\u{...}` or `\uXXXX`) is a
/// surrogate code unit rather than a scalar.
#[must_use]
pub fn is_surrogate_code_point(code_point: u32) -> bool {
    (0xD800..=0xDFFF).contains(&code_point)
}

/// Append one code point to `units`. A surrogate value is kept as a single
/// unit; a scalar is encoded as one or two units. This is the one place a
/// lexer escape (`\uD83D`, `\u{1F600}`) turns into code units.
pub fn push_code_point(units: &mut Vec<u16>, code_point: u32) {
    if is_surrogate_code_point(code_point) {
        units.extend(u16::try_from(code_point).ok());
    } else if let Some(character) = char::from_u32(code_point) {
        push_scalar(units, character);
    }
}

impl From<&str> for JsString {
    /// Real Unicode text, encoded as UTF-16. Nothing in `text` is read as a
    /// surrogate marker, so this is lossless for every Rust string.
    fn from(text: &str) -> Self {
        let units: Vec<u16> = text.encode_utf16().collect();
        Self::from_unit_vec(units)
    }
}

impl From<String> for JsString {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<&String> for JsString {
    fn from(text: &String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<Vec<u16>> for JsString {
    fn from(units: Vec<u16>) -> Self {
        Self::from_unit_vec(units)
    }
}

impl PartialEq<str> for JsString {
    fn eq(&self, other: &str) -> bool {
        self.0.iter().copied().eq(other.encode_utf16())
    }
}

impl PartialEq<&str> for JsString {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl PartialEq<String> for JsString {
    fn eq(&self, other: &String) -> bool {
        self == other.as_str()
    }
}

impl fmt::Display for JsString {
    /// Host-boundary text (see [`JsString::to_lossy`]), for messages and
    /// formatting only; it is not an ECMAScript conversion.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_lossy())
    }
}

impl fmt::Debug for JsString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.to_lossy(), formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::{JsString, code_point_at, push_code_point};

    /// Code units of an astral string are the surrogate pair, so its length is
    /// two while its scalar count is one.
    #[test]
    fn length_of_an_astral_string_counts_code_units() {
        let emoji = JsString::from("\u{1f600}");
        assert_eq!(emoji.len(), 2);
        assert_eq!(emoji.units(), &[0xd83d, 0xde00]);
        assert_eq!(JsString::from("a\u{1f600}b").len(), 4);
    }

    /// `charCodeAt` reads the two halves of a pair separately, and
    /// `codePointAt` reads the pair at its leading unit and the trailing unit
    /// on its own.
    #[test]
    fn code_point_at_on_a_surrogate_pair() {
        let emoji = JsString::from("\u{1f600}");
        assert_eq!(emoji.unit_at(0), Some(0xd83d));
        assert_eq!(emoji.unit_at(1), Some(0xde00));
        assert_eq!(emoji.code_point_at(0), 0x1_f600);
        assert_eq!(emoji.code_point_at(1), 0xde00);
        assert_eq!(emoji.unit_at(2), None);
    }

    /// A lone surrogate survives every conversion that is meant to keep it:
    /// the units, the key form, and the concatenation of its halves.
    #[test]
    fn lone_surrogate_round_trips_through_units_and_keys() {
        let lone = JsString::from_units([0xd83d]);
        assert_eq!(lone.len(), 1);
        assert!(!lone.is_well_formed());
        assert_eq!(JsString::from_key(&lone.to_key()), lone);
        assert_eq!(lone.to_lossy(), "\u{fffd}");

        let halves = JsString::from_units([0xd83d, 0xde00]);
        assert_eq!(halves, JsString::from("\u{1f600}"));
        assert!(halves.is_well_formed());
        // A trailing surrogate on its own is also a lone unit.
        let trailing = JsString::from_units([0xde00]);
        assert_eq!(JsString::from_key(&trailing.to_key()), trailing);
    }

    /// Every unit sequence in a mixed sample maps to a key that reads back to
    /// the same units.
    #[test]
    fn keys_are_a_bijection_on_code_units() {
        for units in [
            vec![],
            vec![0x61],
            vec![0xd83d, 0xde00],
            vec![0xd83d],
            vec![0xde00],
            vec![0xd83d, 0x61, 0xdfff],
            vec![0xdb80, 0xdc00],
        ] {
            let string = JsString::from_unit_vec(units.clone());
            assert_eq!(
                JsString::from_key(&string.to_key()).units(),
                units.as_slice()
            );
        }
    }

    /// U+F0000 is an astral private-use scalar, two code units long. A lone
    /// surrogate also has a private-use scalar in its key, and the two must not
    /// collide, either as values or as property keys.
    #[test]
    fn private_use_scalar_does_not_collide_with_a_lone_surrogate() {
        let private_use = JsString::from("\u{f0000}");
        assert_eq!(private_use.len(), 2);
        let lone = JsString::from_units([0xd800]);
        assert_ne!(private_use, lone);
        assert_ne!(private_use.to_key(), lone.to_key());
        assert_eq!(JsString::from_key(&private_use.to_key()), private_use);
        assert_eq!(JsString::from_key(&lone.to_key()), lone);

        // The escape scalar itself, and a real scalar that happens to match a
        // key surrogate slot, also keep their own identity.
        let escape = JsString::from("\u{10ffff}");
        assert_eq!(JsString::from_key(&escape.to_key()), escape);
        let edge = JsString::from_units([0xdfff]);
        assert_eq!(JsString::from_key(&edge.to_key()), edge);
    }

    /// Comparison is by code units, not by code points: U+FFFF sorts above an
    /// astral scalar whose lead unit is smaller, while code points would say
    /// the opposite.
    #[test]
    fn comparison_is_by_code_units() {
        let bmp = JsString::from("\u{ffff}");
        let astral = JsString::from("\u{1f600}");
        assert!(astral < bmp);
        assert!("\u{1f600}".chars().next() > "\u{ffff}".chars().next());
        let lone = JsString::from_units([0xd800]);
        let private_area = JsString::from("\u{e000}");
        assert!(lone < private_area);
    }

    /// Escapes that name a surrogate keep it as a single unit, and an escaped
    /// scalar contributes its UTF-16 encoding.
    #[test]
    fn push_code_point_keeps_surrogates_as_single_units() {
        let mut units = Vec::new();
        push_code_point(&mut units, 0xd83d);
        push_code_point(&mut units, 0x1_f600);
        assert_eq!(units, vec![0xd83d, 0xd83d, 0xde00]);
    }

    #[test]
    fn code_point_at_reads_a_lone_unit_as_itself() {
        assert_eq!(code_point_at(&[0xd83d, 0x61], 0), 0xd83d);
        assert_eq!(code_point_at(&[0x61, 0xde00], 1), 0xde00);
        assert_eq!(code_point_at(&[], 0), 0);
    }
}
