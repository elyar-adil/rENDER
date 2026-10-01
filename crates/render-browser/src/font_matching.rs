//! The CSS Fonts 4 §5 font matching algorithm.
//!
//! §5 is a search over *faces*, so it is written here as a pure function over a
//! face table rather than over font files. That is what makes it testable: the
//! platform table in `font_backend` supplies real faces on a developer's
//! machine, and this module is exercised against tables that exist only in a
//! test, which is the only way to pin down the order the specification mandates.
//!
//! The order the specification mandates, and the reason the order matters, is
//! worth stating once here because it is easy to get backwards:
//!
//! 1. **Family.** §5.2 starts at the first name in `font-family` and moves to
//!    the next when a family has no face for the character. The list is the
//!    fallback chain and is walked in author order.
//! 2. **Style.** §5.2 tries `font-style` *next*, before `font-weight`. (The
//!    CSS Fonts 3 ordering was weight-then-style; this specification reversed
//!    it, and §5.2 says so explicitly: "font-style is tried next" precedes
//!    "font-weight is matched next".)
//! 3. **Weight.** §5.2 then narrows what is left by `font-weight`, using the
//!    three-branch search its own distance graphs illustrate.
//! 4. **Coverage.** A face is only usable if the character is in its effective
//!    character map. §5.2 is explicit that "Glyphs from other faces in the family
//!    are not considered", so a family that yields a face without the glyph is a
//!    family that fails, not a family to keep searching within. §4.5 halves that
//!    further: a `@font-face` face's effective map is its `unicode-range`
//!    intersected with its own character map.
//!
//! `font-width` is §5.2's first axis and is absent here: `font-width` has no
//! consumer anywhere in this engine, and matching on an axis nothing can set
//! would be untestable machinery. Everything after it is implemented.
//!
//! # The table is unified, not layered
//!
//! The table holds a document's `@font-face` faces and the installed faces
//! together, and §5.2 decides the relationship between them rather than
//! preference: for a named family the user agent looks "among fonts defined via
//! `@font-face` rules and then among available installed fonts", and §10.2 says
//! a web font "shadow[s]" an installed font of the same name, so the installed
//! one is not accessible. The consequence worth stating because it is easy to
//! get wrong is that the shadowing holds even when the document's faces have not
//! arrived: §5.2 says "If no faces are present for a family defined via
//! `@font-face` rules, the family should be treated as missing; matching a
//! platform font with the same name must not occur in this case." A document
//! family that declares `font-family: Arial` therefore gets the author's Arial
//! or nothing, never the machine's.

use std::collections::BTreeSet;

use render_core::font_face::UnicodeRangeSet;
use render_core::layout::{
    FamilyName, FontRequest, FontStyle, GenericFamily, caseless_match, family_entries,
};

/// A handle on one of the table's interned `unicode-range` sets.
///
/// The sets are interned rather than held per face because the production corpus
/// declares 12,252 range tokens across 216 `@font-face` blocks, and a face needs
/// a pointer to its set rather than a copy of it. Keeping the handle `Copy` is
/// also what lets [`Face`] stay `Copy`, which the matching searches rely on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnicodeRangeId(u32);

/// One face of one family: the two axes §5.2 searches after the family.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    /// §2.2's weight, 1 to 1000.
    pub weight: u16,
    /// §2.4's slant.
    pub style: FontStyle,
    /// §4.5's `unicode-range` descriptor, as a handle on the table's range sets.
    /// `None` is §4.5's initial value, `U+0-10FFFF`, which is also every
    /// installed face's: the descriptor is an `@font-face` thing and no
    /// installed font has one.
    pub unicode_range: Option<UnicodeRangeId>,
}

impl Face {
    /// A face with no `unicode-range`, which is §4.5's initial value.
    #[must_use]
    pub const fn new(weight: u16, style: FontStyle) -> Self {
        Self {
            weight,
            style,
            unicode_range: None,
        }
    }
}

impl Face {
    /// The oblique angle of an upright face, which §2.4 places at 0 and which
    /// §5.2 treats as the zero point of the oblique axis.
    const UPRIGHT_DEGREES: f32 = 0.0;

    /// The value of this face on the oblique axis.
    ///
    /// An italic face is not a point on the oblique axis: it is the italic
    /// axis, and §5.2 searches it separately from the oblique values. A face
    /// that is neither is the oblique axis alone, with 0 for upright.
    fn oblique_degrees(self) -> Option<f32> {
        match self.style {
            FontStyle::Italic => None,
            FontStyle::Normal => Some(Self::UPRIGHT_DEGREES),
            FontStyle::Oblique(degrees) => Some(degrees),
        }
    }

    /// Whether this face is on the italic axis.
    fn is_italic(self) -> bool {
        matches!(self.style, FontStyle::Italic)
    }
}

/// One family: its names, the generic keywords that resolve to it, and its
/// faces.
#[derive(Clone, Debug, PartialEq)]
pub struct Family {
    /// The canonical name, kept for diagnostics and for the reference path.
    pub name: String,
    /// Every name the platform answers to for this family, matched caselessly
    /// by §5.1. Lowercased once here so the per-character walk does not fold
    /// case again.
    pub aliases: Vec<String>,
    /// The `<generic-font-family>` keywords this family is the engine's choice
    /// for. §2.1.5 makes each generic "an alias for an existing installed font
    /// family present on the system", and §2.1.2 allows a generic to name more
    /// than one, so the list rather than a single keyword.
    pub generics: Vec<GenericFamily>,
    /// §2.1.4: a family name "only specifies a name given to a set of font
    /// faces; it does not specify an individual face."
    pub faces: Vec<Face>,
}

/// One face §5 accepted, with the synthesis §2.8 permits for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchedFace {
    /// Index of the family in the [`FaceTable`].
    pub family: usize,
    /// Index of the face within that family.
    pub face: usize,
    pub weight: u16,
    pub style: FontStyle,
    /// §2.8.1: thicken the outline, because the family has no face as heavy as
    /// the request.
    pub embolden: bool,
    /// §2.8.2: shear the outline by this many degrees, because the family has
    /// no face at the requested oblique angle. §2.4 forbids this for `italic`.
    pub shear_degrees: f32,
}

/// One family of §5.2's walk, already narrowed to a single face.
///
/// The narrowing is character-independent: §5.2 selects on family, then style,
/// then weight, and only then asks about the glyph. Doing the first three here
/// means the per-character work is one coverage test per family, which is what
/// makes a per-character walk affordable.
///
/// A `@font-face` family contributes one entry per face of its composite, in
/// §4.5.1's order, because the thing that tells two slices apart is
/// `unicode-range` and that is a question about the character.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WalkedFamily {
    /// Index of the family in the [`FaceTable`].
    pub family: usize,
    /// Index of the chosen face within the family.
    pub face: usize,
    /// Whether a `<generic-family>` keyword is what reached this family. §5.4
    /// forbids a Private Use Area character from being matched against such a
    /// family.
    pub via_generic: bool,
    /// §2.8.1's synthetic bold for this family.
    pub embolden: bool,
    /// §2.8.2's synthetic oblique for this family.
    pub shear_degrees: f32,
}

/// Every family §5 walked for one request, in the order it walked them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FamilyWalk {
    entries: Vec<WalkedFamily>,
    /// Where the author-named families end and installed font fallback begins.
    /// §5.4 forbids the latter for a Private Use Area character.
    named_len: usize,
}

impl FamilyWalk {
    /// The families §5 considered, in order.
    #[must_use]
    pub fn entries(&self) -> &[WalkedFamily] {
        &self.entries
    }
}

/// The faces §5 searches: the document's `@font-face` faces and the installed
/// faces, as one table.
///
/// The two are unified rather than layered, and that is a specification
/// requirement rather than a convenience. §5.2: "For other family names, the
/// user agent attempts to find the family name among fonts defined via
/// `@font-face` rules and then among available installed fonts", and §10.2:
/// "Web Fonts shadow Installed Fonts, so if an Installed Font has the same
/// family name as a Web Font, the Installed Font is not accessible." So a
/// document family does not sit in front of the installed ones - it *replaces*
/// any installed family of the same name, and the shadowing holds even when the
/// document family's faces have not arrived, because §5.2 also says "If no faces
/// are present for a family defined via `@font-face` rules, the family should be
/// treated as missing; matching a platform font with the same name must not
/// occur in this case."
///
/// Document families occupy indices `0..document_len` and installed families
/// `document_len..`, so the split is one number rather than a second collection
/// the walk has to consult, and §4.5.1's ordering is expressed by the order the
/// document families are added in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FaceTable {
    families: Vec<Family>,
    /// The interned `unicode-range` sets the document faces refer to, indexed by
    /// [`UnicodeRangeId`].
    ranges: Vec<UnicodeRangeSet>,
    /// How many of `families` came from `@font-face` rules. They are the leading
    /// entries, and this is where the installed ones begin.
    document_len: usize,
    /// The families installed font fallback visits, in order.
    ///
    /// §5.2: "If there are no more font families to be evaluated and no matching
    /// face has been found, then the user agent performs an installed font
    /// fallback procedure to find the best match for the character to be
    /// rendered. The result of this procedure can vary across user agents."
    /// A family is in this list when it is the best available coverage for
    /// characters the author's chain does not reach.
    ///
    /// Every entry is an installed family. §4.1 and §10.2 both forbid a
    /// downloaded font from being reached this way: "Downloaded fonts are only
    /// available to documents that reference them", because a font one page
    /// fetched affecting another page's rendering "would cause a security
    /// leak".
    fallback: Vec<usize>,
}

impl FaceTable {
    /// Builds a table of installed faces. Families keep the order they are
    /// given, which is the priority order: §2.1.5 says a generic family "may be
    /// a composite face" and §5.2 leaves the choice of a single face from a
    /// matching set to the user agent, so both are settled by a documented order
    /// rather than by whatever the platform enumerates first.
    #[must_use]
    pub fn new(families: Vec<Family>, fallback: Vec<usize>) -> Self {
        Self {
            families,
            ranges: Vec::new(),
            document_len: 0,
            fallback,
        }
    }

    /// An empty table, which a caller fills with a document's faces and then with
    /// the installed ones.
    ///
    /// Document families have to be added first because they occupy the leading
    /// indices: §5.2's shadowing is decided by the split between the two halves,
    /// and that split is a property of the index space rather than of a second
    /// collection the walk consults.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Adds one `@font-face` family and returns its index.
    ///
    /// Each face carries the `unicode-range` it was declared with, or `None` for
    /// §4.5's initial value, and the sets are interned here because a face's
    /// handle indexes the table's own arena.
    ///
    /// §4.5.1's order is the order families are added in - "the last rule defined
    /// is the first to be checked for a given character" - so a caller adds the
    /// rules in reverse document order and the walk checks them the way the
    /// specification says to.
    pub fn push_document_family(
        &mut self,
        name: &str,
        faces: Vec<(u16, FontStyle, Option<UnicodeRangeSet>)>,
    ) -> usize {
        let faces = faces
            .into_iter()
            .map(|(weight, style, range)| Face {
                weight,
                style,
                unicode_range: range.map(|range| self.intern_range(range)),
            })
            .collect();
        self.families.push(Family {
            name: name.to_owned(),
            aliases: vec![name.to_ascii_lowercase()],
            generics: Vec::new(),
            faces,
        });
        self.document_len += 1;
        self.families.len() - 1
    }

    /// Appends the installed families, after the document's.
    #[must_use]
    pub fn with_installed_families(mut self, installed: Vec<Family>) -> Self {
        self.families.extend(installed);
        self
    }

    /// Sets installed font fallback, naming the installed families by their index
    /// within the installed half.
    ///
    /// §4.1 and §10.2 forbid a downloaded face from being reachable this way, so
    /// the indices are offset past the document families and an index that names
    /// one of them cannot be written.
    #[must_use]
    pub fn with_fallback(mut self, installed_indices: &[usize]) -> Self {
        self.fallback = installed_indices
            .iter()
            .map(|index| index + self.document_len)
            .collect();
        self
    }

    /// Builds the unified table the engine searches: document families first, in
    /// the order §4.5.1 checks them, then the installed families.
    ///
    /// `fallback` names installed families by their index *within `installed`*,
    /// so a caller that builds the installed half once does not have to re-derive
    /// its indices every time a document face arrives.
    #[must_use]
    pub fn unified(document: Vec<Family>, installed: Vec<Family>, fallback: &[usize]) -> Self {
        let mut table = Self {
            families: document,
            ranges: Vec::new(),
            document_len: 0,
            fallback: Vec::new(),
        };
        table.document_len = table.families.len();
        table.families.extend(installed);
        table.with_fallback(fallback)
    }

    /// Adds a `unicode-range` set to the table's arena and returns its handle.
    ///
    /// An empty set is still interned rather than skipped, so a handle is
    /// returned for every call and a caller cannot accidentally get one that
    /// means something else.
    pub fn intern_range(&mut self, range: UnicodeRangeSet) -> UnicodeRangeId {
        // The corpus's slices are disjoint, so a set is almost never shared;
        // where two rules declare the same set, sharing it is free and correct
        // because the set is immutable once interned.
        if let Some(existing) = self.ranges.iter().position(|candidate| *candidate == range) {
            return UnicodeRangeId(u32::try_from(existing).unwrap_or(0));
        }
        self.ranges.push(range);
        UnicodeRangeId(u32::try_from(self.ranges.len() - 1).unwrap_or(0))
    }

    /// §4.5's set for one face, or `None` when the face declares none.
    ///
    /// §4.5: "the effective character map is the intersection of the codepoints
    /// defined by `unicode-range` with the font's character map", so this is one
    /// half of the coverage test and [`Self::admits`] is the other.
    #[must_use]
    pub fn range_of(&self, family: usize, face: usize) -> Option<&UnicodeRangeSet> {
        let id = self.families.get(family)?.faces.get(face)?.unicode_range?;
        self.ranges.get(id.0 as usize)
    }

    /// Whether §4.5's `unicode-range` for a face admits `character`.
    ///
    /// A face with no declared range admits everything, which is §4.5's initial
    /// value and the state of every installed face.
    #[must_use]
    pub fn admits(&self, family: usize, face: usize, character: char) -> bool {
        self.range_of(family, face)
            .is_none_or(|range| range.contains(character))
    }

    /// The families of the table.
    #[must_use]
    pub fn families(&self) -> &[Family] {
        &self.families
    }

    /// How many of the table's families came from `@font-face` rules.
    #[must_use]
    pub const fn document_family_count(&self) -> usize {
        self.document_len
    }

    /// §5.2's family walk for `request`.
    ///
    /// Walks `font-family` in author order, resolving a `<generic-family>`
    /// keyword to the engine's choice of family and leaving a
    /// `<font-family-name>` to §5.1 caseless matching, then appends installed
    /// font fallback. Each family that has a face at all is narrowed to one.
    #[must_use]
    pub fn walk(&self, request: &FontRequest<'_>) -> FamilyWalk {
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        for entry in family_entries(request) {
            match entry {
                FamilyName::Generic(generic) => {
                    for family in self.families_for_generic(generic) {
                        self.push_family(&mut entries, &mut seen, family, true, request);
                    }
                }
                FamilyName::Named(name) => {
                    for family in self.families_named(name) {
                        self.push_family(&mut entries, &mut seen, family, false, request);
                    }
                }
            }
        }
        let named_len = entries.len();
        for family in &self.fallback {
            self.push_family(&mut entries, &mut seen, *family, false, request);
        }
        FamilyWalk { entries, named_len }
    }

    fn push_family(
        &self,
        entries: &mut Vec<WalkedFamily>,
        seen: &mut BTreeSet<usize>,
        family: usize,
        via_generic: bool,
        request: &FontRequest<'_>,
    ) {
        if !seen.insert(family) {
            return;
        }
        let Some(faces) = self.families.get(family) else {
            return;
        };
        if family < self.document_len {
            // §5.2's composite face: "A group of faces defined via `@font-face`
            // rules with identical font descriptor values but differing
            // `unicode-range` values are considered to be a single composite
            // font face for this step." So the style and weight searches narrow
            // the group, not the family, and every face at the chosen weight and
            // style stays in the running - distinguished per character by §4.5.
            // The corpus needs this: its dominant font is 108 slices per
            // stylesheet at one weight and one style.
            for (face, embolden, shear_degrees) in select_composite(faces, request) {
                entries.push(WalkedFamily {
                    family,
                    face,
                    via_generic,
                    embolden,
                    shear_degrees,
                });
            }
            return;
        }
        if let Some((face, embolden, shear_degrees)) = select_face(faces, request) {
            entries.push(WalkedFamily {
                family,
                face,
                via_generic,
                embolden,
                shear_degrees,
            });
        }
    }

    fn families_for_generic(&self, generic: GenericFamily) -> Vec<usize> {
        self.families
            .iter()
            .enumerate()
            // A document family is never reached by a generic keyword: §2.1.2
            // says a generic is "an alias for an existing *installed* font
            // family present on the system", and a `@font-face` family is not
            // installed. A page wanting a webfont names it.
            .filter(|(index, family)| {
                *index >= self.document_len && family.generics.contains(&generic)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// §5.2's name lookup, with §5.2's shadowing.
    ///
    /// "the user agent attempts to find the family name among fonts defined via
    /// `@font-face` rules and then among available installed fonts" - and "If no
    /// faces are present for a family defined via `@font-face` rules, the family
    /// should be treated as missing; matching a platform font with the same name
    /// must not occur in this case."
    ///
    /// So a document family that matches the name *is* the answer, whether or
    /// not any of its faces has arrived, and the installed family of the same
    /// name is not consulted at all. A page that declares a webfont under the
    /// name of an installed family gets the webfont, and a page that declares
    /// one it never loads gets neither - which is the specified outcome and the
    /// reason the two cases cannot be told apart by "is it in the table".
    fn families_named(&self, name: &str) -> Vec<usize> {
        let matches_name = |family: &Family| {
            family
                .aliases
                .iter()
                .any(|alias| caseless_alias_matches(alias, name))
        };
        let document: Vec<usize> = self
            .families
            .iter()
            .enumerate()
            .filter(|(index, family)| *index < self.document_len && matches_name(family))
            .map(|(index, _)| index)
            .collect();
        if !document.is_empty() {
            return document;
        }
        self.families
            .iter()
            .enumerate()
            .filter(|(index, family)| *index >= self.document_len && matches_name(family))
            .map(|(index, _)| index)
            .collect()
    }

    /// The first face in `walk` that can render `character`, which is what §5.2
    /// asks for each character of a run.
    ///
    /// §5.2 answers this per character, so a run whose Latin and CJK characters
    /// live in different families is normal and expected, not an edge case.
    /// "A font is considered to support a given character if (1) the character is
    /// contained in the font's character map and (2) ..." - and §4.5 narrows the
    /// first of those to the effective character map, "the intersection of the
    /// codepoints defined by `unicode-range` with the font's character map". So
    /// both halves are asked here, and `has_glyph` is only the font's own half.
    #[must_use]
    pub fn face_for(
        &self,
        walk: &FamilyWalk,
        character: char,
        has_glyph: impl Fn(usize, usize) -> bool,
    ) -> Option<MatchedFace> {
        let private_use = is_private_use(character);
        let limit = if private_use {
            walk.named_len
        } else {
            walk.entries.len()
        };
        for entry in &walk.entries[..limit] {
            // §5.4: a Private Use Area character is matched only against
            // families the author named, never against a generic one. A document
            // family is only ever reached by name, so a self-hosted icon font is
            // still a candidate for a Private Use Area codepoint, which is what
            // §5.4's rule is for.
            if private_use && entry.via_generic {
                continue;
            }
            if !self.admits(entry.family, entry.face, character) {
                continue;
            }
            let family = self.families.get(entry.family)?;
            let face = *family.faces.get(entry.face)?;
            if has_glyph(entry.family, entry.face) {
                return Some(MatchedFace {
                    family: entry.family,
                    face: entry.face,
                    weight: face.weight,
                    style: face.style,
                    embolden: entry.embolden,
                    shear_degrees: entry.shear_degrees,
                });
            }
        }
        // §5: "If a particular character cannot be displayed using any font, the
        // user agent should indicate by some means that a character is not being
        // displayed." Leaving the character without a face is that indication:
        // the caller paints nothing rather than painting a wrong letterform.
        None
    }
}

/// §5.1 caseless matching against an already-lowercased alias.
fn caseless_alias_matches(alias: &str, requested: &str) -> bool {
    caseless_match(alias, requested)
}

/// §5.2's narrowing of one family to a single face.
///
/// Returns the face's index within `family`, whether §2.8.1 permits synthesizing
/// its weight, and by how many degrees §2.8.2 permits shearing it. `None` means
/// the family has no face to offer at all, which sends §5 on to the next name.
fn select_face(family: &Family, request: &FontRequest<'_>) -> Option<(usize, bool, f32)> {
    if family.faces.is_empty() {
        return None;
    }
    let style = select_style(family, request.style);
    let (face, embolden) = select_weight(family, &style, request);
    let shear = select_shear(family, &style, request, face);
    Some((face, embolden, shear))
}

/// §5.2's narrowing of a `@font-face` family to the faces that are one
/// composite face, in §4.5.1's order.
///
/// §5.2 treats "a group of faces defined via `@font-face` rules with identical
/// font descriptor values but differing `unicode-range` values" as a single face
/// for the style and weight steps. So the same searches run, and then every face
/// at the chosen weight and style is kept: they are told apart per character by
/// `unicode-range`, which is a question about the character and so cannot be
/// answered before the walk reaches one.
///
/// §4.5.1 fixes the order - "the last rule defined is the first to be checked for
/// a given character" - and it is honoured by the family being built in that
/// order, so this preserves it rather than imposing an order of its own.
fn select_composite(family: &Family, request: &FontRequest<'_>) -> Vec<(usize, bool, f32)> {
    let Some((chosen, embolden, shear_degrees)) = select_face(family, request) else {
        return Vec::new();
    };
    let Some(target) = family.faces.get(chosen) else {
        return Vec::new();
    };
    let (weight, style) = (target.weight, target.style);
    family
        .faces
        .iter()
        .enumerate()
        .filter(|(_, face)| face.weight == weight && face.style == style)
        .map(|(index, _)| (index, embolden, shear_degrees))
        .collect()
}

/// The slant values §5.2 will accept, in the order it will accept them.
///
/// The order is a function of the faces the family actually has, which is what
/// §5.2's distance graphs illustrate: the value chosen is the closest one
/// *present*, and "all fonts not including that value are eliminated".
#[derive(Clone, Debug, Default, PartialEq)]
struct StyleMatch {
    /// The oblique angle chosen, or `None` when an italic face was chosen.
    oblique: Option<f32>,
    /// The face indices that carry the chosen slant.
    faces: Vec<usize>,
}

impl StyleMatch {
    /// The chosen face, lowest index first, which is the deterministic choice
    /// §5.2 requires when more than one face remains.
    fn face(&self) -> Option<usize> {
        self.faces.first().copied()
    }
}

/// §5.2's `font-style` search, in the order the specification writes it.
fn select_style(family: &Family, requested: FontStyle) -> StyleMatch {
    match requested {
        FontStyle::Normal => {
            // "Oblique values greater than or equal to 0 are checked in
            // ascending order. If no match is found, italic values greater than
            // or equal to 0 are checked in ascending order. If no match is
            // found, oblique values less than 0deg are checked in descending
            // order until a match is found. If no match is found, italic values
            // less than 0 are checked in descending order."
            upright_oblique(family, 0.0, f32::INFINITY, true)
                .or_else(|| italic(family))
                .or_else(|| upright_oblique(family, f32::NEG_INFINITY, -f32::EPSILON, false))
                .unwrap_or_default()
        }
        FontStyle::Italic => {
            // "If the value of font-style is italic: If the matching set
            // includes faces with italic values containing the mapped value of
            // italic, then faces with italic values which do not include the
            // desired italic mapped value are removed from the matching set.
            // Otherwise, italic values above the desired italic value are checked
            // in ascending order followed by italic values below the desired
            // italic value, until 0 is hit. ... If no match is found, oblique
            // values greater than or equal to 11deg are checked in ascending
            // order followed by oblique values below 11deg in descending order,
            // until 0 is hit. ... If no match is found, oblique values less than
            // or equal to 0 are checked in descending order until a match is
            // found."
            //
            // §5.2 also collapses the two axes onto one scale, requiring only
            // that an italic value of 1 land where an oblique angle of 11deg
            // does, which is why the two sweeps below meet at 11.
            italic(family)
                .or_else(|| upright_oblique(family, 11.0, f32::INFINITY, true))
                .or_else(|| upright_oblique(family, POSITIVE_OBLIQUE, 11.0, false))
                .or_else(|| upright_oblique(family, f32::NEG_INFINITY, 0.0, false))
                .unwrap_or_default()
        }
        FontStyle::Oblique(degrees) if (0.0..11.0).contains(&degrees) => {
            // "Otherwise, oblique values below the desired oblique value are
            // checked in descending order until 0 is hit, followed by oblique
            // values above the desired oblique value. Only positive values of
            // oblique values are checked in this stage."
            upright_oblique(family, POSITIVE_OBLIQUE, degrees, false)
                .or_else(|| upright_oblique(family, degrees, f32::INFINITY, true))
                .or_else(|| italic(family))
                .or_else(|| upright_oblique(family, f32::NEG_INFINITY, 0.0, false))
                .unwrap_or_default()
        }
        FontStyle::Oblique(degrees) if degrees >= 11.0 => {
            // "Otherwise, oblique values above the desired oblique value are
            // checked in ascending order followed by oblique values below the
            // desired oblique value, until 0 is hit. Only positive values of
            // oblique values are checked in this stage. ... If no match is
            // found, italic values greater than or equal to 1 are checked in
            // ascending order ... If no match is found, oblique values less than
            // or equal to 0 are checked in descending order until a match is
            // found."
            upright_oblique(family, degrees, f32::INFINITY, true)
                .or_else(|| upright_oblique(family, POSITIVE_OBLIQUE, degrees, false))
                .or_else(|| italic(family))
                .or_else(|| upright_oblique(family, f32::NEG_INFINITY, 0.0, false))
                .unwrap_or_default()
        }
        FontStyle::Oblique(degrees) => {
            // "If the value of font-style is oblique and the requested angle is
            // less than 0deg and greater than -11deg, follow the steps above,
            // except with the negated values and opposite directions. If the
            // value of font-style is oblique and the requested angle is less than
            // or equal to -11deg, follow the steps above, except with the
            // negated values and opposite directions."
            //
            // The specification's own instruction is to run the procedure above
            // on the negated request, so that is literally what happens: the
            // face set is mirrored, the positive procedure runs, and the result
            // is mirrored back. Anything else would be a re-derivation of a
            // rule the specification states by reference.
            let mirrored: Vec<Face> = family
                .faces
                .iter()
                .map(|face| Face {
                    weight: face.weight,
                    style: match face.oblique_degrees() {
                        Some(degrees) => FontStyle::Oblique(-degrees),
                        None => FontStyle::Italic,
                    },
                    // The mirror is of the slant axis alone, so a face keeps the
                    // character range it was declared with.
                    unicode_range: face.unicode_range,
                })
                .collect();
            let mirrored_family = Family {
                name: family.name.clone(),
                aliases: family.aliases.clone(),
                generics: family.generics.clone(),
                faces: mirrored,
            };
            let found = select_style(&mirrored_family, FontStyle::Oblique(-degrees));
            let Some(oblique) = found.oblique else {
                return found;
            };
            StyleMatch {
                oblique: Some(-oblique),
                faces: found.faces,
            }
        }
    }
}

/// The lower bound of a search over the *positive* oblique values.
///
/// §5.2 writes those searches as applying to "positive values of oblique
/// values", and 0 is not positive, so the upright face is excluded from every
/// stage that a face of a slanted family would win. It reaches the upright face
/// only in §5.2's own last resort, which is what keeps `oblique 8deg` from
/// resolving to a face the author did not ask to be slanted at all.
const POSITIVE_OBLIQUE: f32 = f32::EPSILON;

/// The face indices whose oblique angle is in `[low, high]`, closest-first.
///
/// `ascending` picks which end of the range is nearest the request: §5.2 says
/// "for a requested angle greater or equal to 11deg, larger angles are
/// preferred; otherwise, smaller angles are preferred", so a sweep that starts at
/// `low` ascends and one that starts at `high` descends.
fn upright_oblique(family: &Family, low: f32, high: f32, ascending: bool) -> Option<StyleMatch> {
    let mut faces = Vec::new();
    for (index, face) in family.faces.iter().enumerate() {
        if let Some(degrees) = face.oblique_degrees()
            && degrees >= low
            && degrees <= high
        {
            faces.push((index, degrees));
        }
    }
    if faces.is_empty() {
        return None;
    }
    faces.sort_by(|left, right| {
        let ordering = left.1.total_cmp(&right.1);
        if ascending {
            ordering
        } else {
            ordering.reverse()
        }
    });
    let nearest = faces[0].1;
    Some(StyleMatch {
        oblique: Some(nearest),
        faces: faces
            .into_iter()
            .filter(|(_, degrees)| degrees.total_cmp(&nearest).is_eq())
            .map(|(index, _)| index)
            .collect(),
    })
}

fn italic(family: &Family) -> Option<StyleMatch> {
    let faces: Vec<usize> = family
        .faces
        .iter()
        .enumerate()
        .filter(|(_, face)| face.is_italic())
        .map(|(index, _)| index)
        .collect();
    if faces.is_empty() {
        return None;
    }
    Some(StyleMatch {
        oblique: None,
        faces,
    })
}

/// §5.2's `font-weight` search over the faces the style step left.
fn select_weight(family: &Family, style: &StyleMatch, request: &FontRequest<'_>) -> (usize, bool) {
    let weights: BTreeSet<u16> = style
        .faces
        .iter()
        .filter_map(|index| family.faces.get(*index))
        .map(|face| face.weight)
        .collect();
    let order = weight_search_order(request.weight, &weights);
    for weight in order {
        if let Some(index) = style.faces.iter().copied().find(|index| {
            family
                .faces
                .get(*index)
                .is_some_and(|face| face.weight == weight)
        }) {
            let matched = family.faces[index].weight;
            return (index, needs_synthesized_weight(family, request, matched));
        }
    }
    // Unreachable: `order` is a permutation of `weights`, and a non-empty style
    // match always has a weight. Handled rather than unwrapped so that a future
    // change to either search fails as a wrong face rather than a panic.
    let index = style.face().unwrap_or(0);
    (index, false)
}

/// §5.2's weight search order, given the weights the family actually has.
#[must_use]
pub fn weight_search_order(desired: u16, available: &BTreeSet<u16>) -> Vec<u16> {
    let ascending = available.iter().copied().collect::<Vec<_>>();
    let mut order = Vec::with_capacity(ascending.len());
    if (400..=500).contains(&desired) {
        // "If the desired weight is inclusively between 400 and 500, weights
        // greater than or equal to the target weight are checked in ascending
        // order until 500 is hit and checked, followed by weights less than the
        // target weight in descending order, followed by weights greater than
        // 500, until a match is found."
        order.extend(
            ascending
                .iter()
                .copied()
                .filter(|weight| *weight >= desired && *weight <= 500),
        );
        order.extend(
            ascending
                .iter()
                .rev()
                .copied()
                .filter(|weight| *weight < desired),
        );
        order.extend(ascending.iter().copied().filter(|weight| *weight > 500));
    } else if desired < 400 {
        // "If the desired weight is less than 400, weights less than or equal to
        // the desired weight are checked in descending order followed by weights
        // above the desired weight in ascending order until a match is found."
        order.extend(
            ascending
                .iter()
                .rev()
                .copied()
                .filter(|weight| *weight <= desired),
        );
        order.extend(ascending.iter().copied().filter(|weight| *weight > desired));
    } else {
        // "If the desired weight is greater than 500, weights greater than or
        // equal to the desired weight are checked in ascending order followed by
        // weights below the desired weight in descending order until a match is
        // found."
        order.extend(
            ascending
                .iter()
                .copied()
                .filter(|weight| *weight >= desired),
        );
        order.extend(
            ascending
                .iter()
                .rev()
                .copied()
                .filter(|weight| *weight < desired),
        );
    }
    order
}

/// §2.8.1: whether this request needs a face the family does not have, and so
/// permits one to be synthesized.
///
/// §2.2.2 is the rule: "bold faces are often synthesized by user agents for
/// families that lack actual bold faces. For the purposes of font matching,
/// these faces must be treated as if they exist within the family." A family
/// that *does* have a face as heavy as the request is not one that lacks bold
/// faces, so nothing is synthesized - which is also why `font-weight: 500`
/// against a 400-only family renders at 400, the same as every engine.
///
/// The 500 boundary is §5.2's own: the weight search treats 400 through 500 as
/// one region, so a request inside that region is asking for the regular face,
/// not for a heavier one.
fn needs_synthesized_weight(family: &Family, request: &FontRequest<'_>, matched: u16) -> bool {
    if !request.synthesis.weight || request.weight <= 500 || matched >= request.weight {
        return false;
    }
    !family
        .faces
        .iter()
        .any(|face| face.weight >= request.weight)
}

/// §2.8.2: the oblique angle to shear a face by, if any.
///
/// §5.2's `oblique` branch: "Otherwise, if font-synthesis-style has the value
/// auto, then a fallback match is produced by geometric shearing to the
/// specified oblique value."
///
/// §2.4 forbids this for `italic`: "For user agents that treat these values
/// distinctly, synthesis must not be performed for italic." This engine does
/// treat them distinctly - `italic` and `oblique` are different targets in
/// `select_style` - so `italic` with no italic face renders upright, which is
/// what §5.2's own last resort (`oblique` values less than or equal to 0,
/// descending) selects.
fn select_shear(
    family: &Family,
    style: &StyleMatch,
    request: &FontRequest<'_>,
    face: usize,
) -> f32 {
    if !request.synthesis.style {
        return 0.0;
    }
    let FontStyle::Oblique(desired) = request.style else {
        return 0.0;
    };
    // A family that already offers the requested angle needs no shearing, and
    // neither does one whose matched face already is that angle.
    if style
        .oblique
        .is_some_and(|found| (found - desired).abs() <= f32::EPSILON)
    {
        return 0.0;
    }
    let family_has_oblique = family
        .faces
        .iter()
        .any(|candidate| matches!(candidate.style, FontStyle::Oblique(_)));
    if family_has_oblique {
        // §5.2 found an oblique face and rejected it for its weight or its
        // position in the search. §2.8's "fallback match" is for a family that
        // lacks the face, not for one that has an unsuitable one.
        return 0.0;
    }
    let matched = family.faces.get(face);
    if matched.is_some_and(|face| {
        !face
            .oblique_degrees()
            .is_some_and(|found| found.abs() <= f32::EPSILON)
    }) {
        return 0.0;
    }
    desired
}

/// §5.4: "If a given character is a Private-Use Area Unicode codepoint, user
/// agents must only match font families named in the `font-family` list that are
/// not generic families."
#[must_use]
pub const fn is_private_use(character: char) -> bool {
    matches!(
        character as u32,
        0xe000..=0xf8ff | 0x000f_0000..=0x000f_fffd | 0x0010_0000..=0x0010_fffd
    )
}

#[cfg(test)]
mod tests {
    // The angles and weights the assertions compare are the values §5.2 names,
    // not measurements, so they are compared exactly.
    #![allow(
        clippy::float_cmp,
        reason = "the matching algorithm's inputs are the values the specification names"
    )]

    use std::collections::BTreeSet;

    use render_core::layout::{FontStyle, FontSynthesis, GenericFamily, TextStyle};

    use super::{
        Face, FaceTable, Family, FamilyWalk, is_private_use, select_face, weight_search_order,
    };
    use render_core::font_face::UnicodeRangeSet;

    const AUTO: FontSynthesis = FontSynthesis {
        weight: true,
        style: true,
    };

    fn style_of(
        table: &FaceTable,
        request: &render_core::layout::FontRequest<'_>,
    ) -> Option<FontStyle> {
        let walk = table.walk(request);
        let entry = walk.entries().first()?;
        table
            .families()
            .get(entry.family)?
            .faces
            .get(entry.face)
            .map(|face| face.style)
    }

    fn request(
        family: &'static str,
        weight: u16,
        style: FontStyle,
        synthesis: FontSynthesis,
    ) -> render_core::layout::FontRequest<'static> {
        render_core::layout::FontRequest {
            family,
            weight,
            style,
            synthesis,
        }
    }

    fn plain(family: &'static str) -> render_core::layout::FontRequest<'static> {
        request_with(family, 400, FontStyle::Normal)
    }

    fn family(
        name: &str,
        aliases: &[&str],
        generics: &[GenericFamily],
        faces: &[(u16, render_core::layout::FontStyle)],
    ) -> Family {
        Family {
            name: name.to_owned(),
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
            generics: generics.to_vec(),
            faces: faces
                .iter()
                .map(|(weight, style)| Face::new(*weight, *style))
                .collect(),
        }
    }

    fn weights(values: &[u16]) -> BTreeSet<u16> {
        values.iter().copied().collect()
    }

    fn request_with(
        family: &'static str,
        weight: u16,
        style: FontStyle,
    ) -> render_core::layout::FontRequest<'static> {
        request(family, weight, style, AUTO)
    }

    // ---- §4 / §5.2: document faces and installed faces in one table ---------

    /// A unified table whose document family is `WebFont`, with one face per
    /// listed `unicode-range` at weight 500 and `normal` - the shape the corpus's
    /// sliced fonts have - and whose installed families are the ones given.
    ///
    /// `ranges` is in *declaration* order and the table is built by adding them
    /// in reverse, which is what the resource pipeline does: §4.5.1 says "the
    /// last rule defined is the first to be checked for a given character", and
    /// `FontFaceSheet::families` already yields that order. So a test handing over
    /// `[wide, narrow]` gets a walk that checks `narrow` first, which is what
    /// makes the overlapping-slices test below a test of §4.5.1 rather than of
    /// this helper.
    fn table_with_document_slices(
        ranges: &[&str],
        installed: Vec<Family>,
        fallback: &[&str],
    ) -> FaceTable {
        let installed_names: Vec<String> = installed.iter().map(|f| f.name.clone()).collect();
        let fallback_indices: Vec<usize> = fallback
            .iter()
            .map(|name| {
                installed_names
                    .iter()
                    .position(|candidate| candidate == name)
                    .expect("the fallback family is one of the installed ones")
            })
            .collect();
        let mut table = FaceTable::empty();
        table.push_document_family(
            "WebFont",
            ranges
                .iter()
                .rev()
                .map(|range| (500, FontStyle::Normal, UnicodeRangeSet::parse(range)))
                .collect(),
        );
        table
            .with_installed_families(installed)
            .with_fallback(&fallback_indices)
    }

    /// Which family the §5 walk lands on for one character, by name.
    fn selected(
        table: &FaceTable,
        request: &render_core::layout::FontRequest<'_>,
        character: char,
    ) -> Option<String> {
        let walk = table.walk(request);
        let matched = table.face_for(&walk, character, |_, _| true)?;
        Some(table.families().get(matched.family)?.name.clone())
    }

    fn installed_sans() -> Family {
        family(
            "Installed Sans",
            &["installed sans"],
            &[GenericFamily::SansSerif],
            &[(400, FontStyle::Normal)],
        )
    }

    #[test]
    fn a_document_family_shadows_an_installed_family_of_the_same_name() {
        // §5.2: "the user agent attempts to find the family name among fonts
        // defined via @font-face rules and then among available installed
        // fonts", and §10.2: "Web Fonts shadow Installed Fonts, so if an Installed
        // Font has the same family name as a Web Font, the Installed Font is not
        // accessible."
        let table = FaceTable::unified(
            vec![family(
                "Shadowed",
                &["shadowed"],
                &[],
                &[(500, FontStyle::Normal)],
            )],
            vec![family(
                "Shadowed",
                &["shadowed"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            &[],
        );
        assert_eq!(
            table.document_family_count(),
            1,
            "the document family leads the table"
        );
        assert_eq!(
            selected(
                &table,
                &request_with("Shadowed", 400, FontStyle::Normal),
                'A'
            ),
            Some("Shadowed".to_owned())
        );
        // One entry, not two: the installed family is not reachable under that
        // name, so the walk has nothing to choose between.
        let walk = table.walk(&request_with("Shadowed", 400, FontStyle::Normal));
        assert_eq!(walk.entries().len(), 1);
        assert_eq!(
            table.families()[walk.entries()[0].family].faces[0].weight,
            500,
            "and the face that is reachable is the document's"
        );
    }

    #[test]
    fn a_document_family_whose_faces_have_not_arrived_still_hides_the_installed_one() {
        // §5.2: "If no faces are present for a family defined via @font-face
        // rules, the family should be treated as missing; matching a platform
        // font with the same name must not occur in this case." The family is in
        // the table with no faces, which is how the shadowing stays true while
        // the face is not yet in the family.
        let table = FaceTable::unified(
            vec![family("Shadowed", &["shadowed"], &[], &[])],
            vec![family(
                "Shadowed",
                &["shadowed"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            &[],
        );
        assert!(
            selected(
                &table,
                &request_with("Shadowed, monospace", 400, FontStyle::Normal),
                'A'
            )
            .is_none(),
            "neither the empty document family nor the installed family of that \
             name is used, so §5.2 walks on - here to a table with no fallback"
        );
        assert_eq!(
            table.document_family_count(),
            1,
            "and the family is still in the table, which is what makes the \
             shadowing checkable rather than an accident of omission"
        );
    }

    #[test]
    fn one_page_reaches_a_document_face_and_an_installed_face_in_the_same_run() {
        // The ordinary case, and the one that proves the table is unified rather
        // than layered: `font-family: WebFont, sans-serif` has to give a Han
        // character to the document's face and a Latin character to an installed
        // one, out of a single walk with a single §5 order.
        let table = table_with_document_slices(&["U+4E00-9FFF"], vec![installed_sans()], &[]);
        let request = request_with("WebFont, sans-serif", 500, FontStyle::Normal);
        assert_eq!(
            selected(&table, &request, '\u{4e2d}'),
            Some("WebFont".to_owned()),
            "the Han character is inside the document face's range"
        );
        assert_eq!(
            selected(&table, &request, 'A'),
            Some("Installed Sans".to_owned()),
            "and the Latin character is outside it, so §5.2 walks to the next \
             name in the same list - which is the installed face"
        );
    }

    #[test]
    fn a_slice_font_picks_the_slice_that_owns_the_character() {
        // §4.5.1: a composite font is built from slices and the slices'
        // `unicode-range` is what tells them apart. The corpus's dominant font is
        // 108 slices per stylesheet at one weight and one style, so this is
        // checked at that size rather than at a toy size.
        let ranges: Vec<String> = (0_u32..108)
            .map(|index| {
                let start = 0x3400 + index * 0x40;
                format!("U+{start:X}-{:X}", start + 0x3f)
            })
            .collect();
        let borrowed: Vec<&str> = ranges.iter().map(String::as_str).collect();
        let table = table_with_document_slices(&borrowed, vec![installed_sans()], &[]);
        let walk = table.walk(&request_with("WebFont", 500, FontStyle::Normal));
        assert_eq!(
            walk.entries().len(),
            108,
            "a composite face contributes all of its slices, because which one \
             applies is a question about the character"
        );
        for (declared, range) in borrowed.iter().enumerate() {
            let codepoint = 0x3400_u32 + u32::try_from(declared).expect("a small index") * 0x40;
            let character = char::from_u32(codepoint).expect("in the basic plane");
            let matched = table
                .face_for(&walk, character, |_, _| true)
                .expect("the slice that owns the character");
            assert_eq!(
                matched.face,
                107 - declared,
                "{range} owns U+{codepoint:04X}, and the faces are stored in \
                 §4.5.1's check order, so the last declared is the first face"
            );
        }
    }

    #[test]
    fn a_later_defined_slice_wins_when_two_ranges_overlap() {
        // §4.5.1: "If the unicode ranges overlap for a set of @font-face rules
        // with the same family and style descriptor values, the rules are ordered
        // in the reverse order they were defined; the last rule defined is the
        // first to be checked for a given character."
        let table = table_with_document_slices(
            &["U+4E00-9FFF", "U+4E00-4E7F"],
            vec![installed_sans()],
            &[],
        );
        let request = request_with("WebFont", 500, FontStyle::Normal);
        let matched = table
            .face_for(&table.walk(&request), '\u{4e2d}', |_, _| true)
            .expect("a slice owns the character");
        assert_eq!(
            matched.face, 0,
            "the narrow range is the *second* rule, so §4.5.1 puts it first in \
             the composite and it is the slice that is used"
        );
        // And the wide range alone would have been used had it been the only
        // rule, so this is the overlap changing the answer rather than a slice
        // that could never win.
        let only_wide = table_with_document_slices(&["U+4E00-9FFF"], vec![installed_sans()], &[]);
        let matched = only_wide
            .face_for(&only_wide.walk(&request), '\u{4e2d}', |_, _| true)
            .expect("the wide range covers the character");
        assert_eq!(
            matched.face, 0,
            "so face 0 here is the narrow one and not the only one"
        );
    }

    #[test]
    fn a_character_no_slice_admits_is_not_offered_the_document_face() {
        // §4.5: "User agents must not download or use the font for codepoints
        // outside this set." Without this, a webfont covering only Han would be
        // handed every Latin character in the document.
        let table = table_with_document_slices(&["U+4E00-9FFF"], vec![installed_sans()], &[]);
        let request = request_with("WebFont, sans-serif", 500, FontStyle::Normal);
        assert!(
            !table.admits(0, 0, 'A'),
            "§4.5: the face does not admit a codepoint outside its range, so the \
             engine must not use it for one"
        );
        assert_eq!(
            selected(&table, &request, 'A'),
            Some("Installed Sans".to_owned()),
            "so §5.2 walks past the document family to the installed one"
        );
        // And the document family is not consulted for the character at all: the
        // closure the caller supplies is what asks a font for a glyph, and a face
        // whose range excludes the character is never asked.
        let asked = std::cell::RefCell::new(Vec::new());
        let walk = table.walk(&request);
        let _ = table.face_for(&walk, 'A', |family, _| {
            asked.borrow_mut().push(family);
            true
        });
        let asked = asked.into_inner();
        assert_eq!(
            asked,
            [1],
            "only the installed family is asked, so a webfont that covers only \
             Han cannot claim the Latin text around it"
        );
    }

    #[test]
    fn a_document_family_is_never_reached_through_a_generic_keyword() {
        // §2.1.2: a generic family is "an alias for an existing installed font
        // family present on the system". A `@font-face` family is not installed,
        // so `sans-serif` cannot name it however its aliases are written.
        let table = FaceTable::unified(
            vec![family(
                "Sans-ish",
                &["sans-serif"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            vec![installed_sans()],
            &[],
        );
        let walk = table.walk(&request_with("sans-serif", 400, FontStyle::Normal));
        let names: Vec<_> = walk
            .entries()
            .iter()
            .map(|entry| table.families()[entry.family].name.as_str())
            .collect();
        assert_eq!(
            names,
            ["Installed Sans"],
            "a document face whose name spells a generic keyword is still only \
             reachable by that name quoted"
        );
    }

    #[test]
    fn a_document_face_is_not_installed_font_fallback() {
        // §4.1: "Downloaded fonts are only available to documents that reference
        // them", and using one in another document "would cause a security
        // leak". So a character no `font-family` names must not reach a
        // downloaded face through the fallback list.
        let table = FaceTable::unified(
            vec![family(
                "WebFont",
                &["webfont"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            vec![installed_sans()],
            &[0],
        );
        let names: Vec<_> = table
            .walk(&request_with("serif", 400, FontStyle::Normal))
            .entries()
            .iter()
            .map(|entry| table.families()[entry.family].name.as_str())
            .collect();
        assert!(
            names.contains(&"Installed Sans"),
            "the fallback index named the installed family, not the document's: {names:?}"
        );
        assert!(
            !names.contains(&"WebFont"),
            "and a downloaded face is never in installed font fallback: {names:?}"
        );
    }

    #[test]
    fn a_private_use_character_still_reaches_a_document_family() {
        // §5.4 restricts a Private Use Area codepoint to families the author named
        // and not generic ones. A `@font-face` icon family is named and not
        // generic, so §5.4 permits it - which is how an icon font is meant to
        // work, and the reason this rule is written the way it is.
        let table = FaceTable::unified(
            vec![family(
                "Icons",
                &["icons"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            vec![installed_sans()],
            &[0],
        );
        let request = request_with("Icons, sans-serif", 400, FontStyle::Normal);
        let matched = table
            .face_for(&table.walk(&request), '\u{e610}', |_, _| true)
            .expect("the named family is a candidate");
        assert_eq!(table.families()[matched.family].name, "Icons");
        // With only the generic named, §5.4 forbids the fallback, so the
        // character is not displayed rather than drawn from a generic family.
        assert!(
            table
                .face_for(
                    &table.walk(&request_with("sans-serif", 400, FontStyle::Normal)),
                    '\u{e610}',
                    |_, _| true
                )
                .is_none(),
            "§5.4: a Private Use Area character must only match font families \
             named in the font-family list that are not generic families"
        );
    }

    #[test]
    fn an_interned_range_is_shared_by_value_and_a_face_with_none_admits_everything() {
        // §4.5's initial value is U+0-10FFFF, which is also what a face with no
        // interned range means, so the common case - every installed face - costs
        // nothing.
        let mut table = FaceTable::new(
            vec![family("A", &["a"], &[], &[(400, FontStyle::Normal)])],
            Vec::new(),
        );
        assert!(table.admits(0, 0, 'A'), "a face with no range admits A");
        assert!(table.admits(0, 0, '\u{4e2d}'));
        let first = table.intern_range(UnicodeRangeSet::parse("U+0-7F").expect("valid"));
        let second = table.intern_range(UnicodeRangeSet::parse("U+0-7F").expect("valid"));
        assert_eq!(first, second, "the same set is interned once");
        let other = table.intern_range(UnicodeRangeSet::parse("U+4E00-9FFF").expect("valid"));
        assert_ne!(first, other, "and a different set gets its own handle");
    }

    // ---- Step 1: the family -------------------------------------------------

    #[test]
    fn a_named_family_is_matched_caselessly_per_section_five_one() {
        let table = FaceTable::new(
            vec![family(
                "Segoe UI",
                &["segoe ui"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            Vec::new(),
        );
        for spelling in ["segoe ui", "Segoe UI", "SEGOE UI", "SeGoE uI"] {
            let walk = table.walk(&plain(spelling));
            assert_eq!(walk.entries().len(), 1, "{spelling} did not match");
        }
    }

    #[test]
    fn a_family_that_does_not_exist_is_skipped_and_the_next_one_is_used() {
        let table = FaceTable::new(
            vec![family(
                "Arial",
                &["arial"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            Vec::new(),
        );
        let walk = table.walk(&plain("\"PingFang SC\", arial"));
        assert_eq!(
            walk.entries().len(),
            1,
            "an absent family must not stop the walk"
        );
        assert_eq!(table.families()[walk.entries()[0].family].name, "Arial");
    }

    #[test]
    fn the_family_list_is_walked_in_author_order() {
        let table = FaceTable::new(
            vec![
                family("First", &["first"], &[], &[(400, FontStyle::Normal)]),
                family("Second", &["second"], &[], &[(400, FontStyle::Normal)]),
            ],
            Vec::new(),
        );
        let walk = table.walk(&plain("first, second"));
        let names: Vec<&str> = walk
            .entries()
            .iter()
            .map(|entry| table.families()[entry.family].name.as_str())
            .collect();
        assert_eq!(names, ["First", "Second"]);
    }

    #[test]
    fn a_generic_family_keyword_resolves_to_the_family_that_claims_it() {
        let table = FaceTable::new(
            vec![
                family(
                    "Consolas",
                    &["consolas"],
                    &[GenericFamily::UiMonospace, GenericFamily::Monospace],
                    &[(400, FontStyle::Normal)],
                ),
                family(
                    "Courier New",
                    &["courier new"],
                    &[],
                    &[(400, FontStyle::Normal)],
                ),
            ],
            Vec::new(),
        );
        let walk = table.walk(&plain("monospace"));
        assert_eq!(walk.entries().len(), 1);
        assert_eq!(table.families()[walk.entries()[0].family].name, "Consolas");
        assert!(
            walk.entries()[0].via_generic,
            "a family reached through a keyword must be recorded as such, because \
             §5.4 excludes those families from Private Use Area matching"
        );
    }

    #[test]
    fn a_quoted_generic_keyword_names_a_font_rather_than_the_generic_family() {
        let table = FaceTable::new(
            vec![family(
                "monospace",
                &["monospace"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            Vec::new(),
        );
        let walk = table.walk(&plain("\"monospace\""));
        assert_eq!(walk.entries().len(), 1);
        assert!(
            !walk.entries()[0].via_generic,
            "a quoted keyword is a <font-family-name>, so §5.4 does not exclude it"
        );
    }

    #[test]
    fn a_family_with_no_faces_contributes_nothing_and_the_walk_continues() {
        let table = FaceTable::new(
            vec![
                family("Empty", &["empty"], &[], &[]),
                family("Real", &["real"], &[], &[(400, FontStyle::Normal)]),
            ],
            Vec::new(),
        );
        let walk = table.walk(&plain("empty, real"));
        assert_eq!(walk.entries().len(), 1);
        assert_eq!(table.families()[walk.entries()[0].family].name, "Real");
    }

    #[test]
    fn installed_font_fallback_runs_once_the_author_list_is_exhausted() {
        let table = FaceTable::new(
            vec![
                family("Arial", &["arial"], &[], &[(400, FontStyle::Normal)]),
                family("YaHei", &["yahei"], &[], &[(400, FontStyle::Normal)]),
            ],
            vec![1],
        );
        let walk = table.walk(&plain("\"No Such Font\""));
        assert_eq!(walk.entries().len(), 1, "§5.2's installed font fallback");
        assert_eq!(table.families()[walk.entries()[0].family].name, "YaHei");
    }

    #[test]
    fn a_family_named_twice_is_visited_once() {
        let table = FaceTable::new(
            vec![family(
                "Arial",
                &["arial"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            vec![0],
        );
        let walk = table.walk(&plain("arial, arial"));
        assert_eq!(walk.entries().len(), 1);
    }

    // ---- Step 2: the style, which §5.2 tries before the weight ---------------

    #[test]
    fn style_is_narrowed_before_weight_which_is_what_section_five_two_says() {
        // §5.2 tries `font-style` next and `font-weight` after it. A family with
        // an italic 400 and an upright 700 is the case that tells the two orders
        // apart:
        //   style first: the italic face is the only candidate, so 400 italic.
        //   weight first: 700 is a face of the requested weight, so 700 upright.
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[(400, FontStyle::Italic), (700, FontStyle::Normal)],
            )],
            Vec::new(),
        );
        let chosen = table.walk(&request_with("test", 700, FontStyle::Italic));
        let entry = chosen.entries().first().copied().expect("a face");
        let face = table.families()[entry.family].faces[entry.face];
        assert_eq!(
            (face.weight, face.style),
            (400, FontStyle::Italic),
            "§5.2 narrows by style before it narrows by weight"
        );
    }

    #[test]
    fn an_italic_request_prefers_an_italic_face_over_an_oblique_one() {
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[
                    (400, FontStyle::Normal),
                    (400, FontStyle::Oblique(20.0)),
                    (400, FontStyle::Italic),
                ],
            )],
            Vec::new(),
        );
        assert_eq!(
            style_of(&table, &request_with("test", 400, FontStyle::Italic)),
            Some(FontStyle::Italic)
        );
    }

    #[test]
    fn an_oblique_request_reaches_the_italic_axis_only_after_every_oblique_stage() {
        // §5.2's `oblique` branch: the oblique sweeps come first, and only "if no
        // match is found" are the italic values checked. An italic value of 1
        // lands where an oblique angle of 11deg does, so an italic face is a
        // legitimate match for an oblique request - and the last thing it does
        // is beat the upright face, which is the specification's own last resort
        // for both angle ranges.
        //
        // "The ital axis is not used to satisfy an oblique request" sits in the
        // same bullet as `slnt` and geometric shearing, so it is a statement
        // about how a *variable* font produces a slant, not about which faces of
        // a static family may match.
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[(400, FontStyle::Normal), (400, FontStyle::Italic)],
            )],
            Vec::new(),
        );
        for degrees in [20.0_f32, 5.0] {
            assert_eq!(
                style_of(
                    &table,
                    &request_with("test", 400, FontStyle::Oblique(degrees))
                ),
                Some(FontStyle::Italic),
                "oblique {degrees}deg: every oblique stage failed, and §5.2's next \
                 stage is the italic axis"
            );
        }
    }

    #[test]
    fn an_oblique_request_prefers_a_real_oblique_face_over_the_italic_axis() {
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[
                    (400, FontStyle::Normal),
                    (400, FontStyle::Oblique(9.0)),
                    (400, FontStyle::Italic),
                ],
            )],
            Vec::new(),
        );
        assert_eq!(
            style_of(&table, &request_with("test", 400, FontStyle::Oblique(5.0))),
            Some(FontStyle::Oblique(9.0)),
            "a real oblique face inside the searched band wins outright"
        );
    }

    #[test]
    fn an_oblique_request_at_or_above_eleven_degrees_prefers_the_larger_angle() {
        // §2.4: "In general, for a requested angle greater or equal to 11deg,
        // larger angles are preferred; otherwise, smaller angles are preferred."
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[
                    (400, FontStyle::Oblique(5.0)),
                    (400, FontStyle::Oblique(20.0)),
                ],
            )],
            Vec::new(),
        );
        assert_eq!(
            style_of(&table, &request_with("test", 400, FontStyle::Oblique(15.0))),
            Some(FontStyle::Oblique(20.0))
        );
    }

    #[test]
    fn an_oblique_request_below_eleven_degrees_prefers_the_smaller_angle() {
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[
                    (400, FontStyle::Oblique(5.0)),
                    (400, FontStyle::Oblique(20.0)),
                ],
            )],
            Vec::new(),
        );
        assert_eq!(
            style_of(&table, &request_with("test", 400, FontStyle::Oblique(8.0))),
            Some(FontStyle::Oblique(5.0))
        );
    }

    #[test]
    fn an_italic_request_with_only_upright_and_oblique_faces_prefers_the_oblique_one() {
        // §5.2's italic branch: "If no match is found, oblique values greater
        // than or equal to 11deg are checked in ascending order".
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[(400, FontStyle::Normal), (400, FontStyle::Oblique(14.0))],
            )],
            Vec::new(),
        );
        assert_eq!(
            style_of(&table, &request_with("test", 400, FontStyle::Italic)),
            Some(FontStyle::Oblique(14.0))
        );
    }

    #[test]
    fn a_normal_request_prefers_upright_over_every_oblique_face() {
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[(400, FontStyle::Oblique(1.0)), (400, FontStyle::Normal)],
            )],
            Vec::new(),
        );
        assert_eq!(
            style_of(&table, &request_with("test", 400, FontStyle::Normal)),
            Some(FontStyle::Normal)
        );
    }

    #[test]
    fn a_normal_request_with_no_upright_face_prefers_the_least_oblique_one() {
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[
                    (400, FontStyle::Oblique(5.0)),
                    (400, FontStyle::Oblique(20.0)),
                ],
            )],
            Vec::new(),
        );
        assert_eq!(
            style_of(&table, &request_with("test", 400, FontStyle::Normal)),
            Some(FontStyle::Oblique(5.0))
        );
    }

    #[test]
    fn a_backslant_oblique_is_the_mirror_of_a_forward_one() {
        // §5.2: a negative angle "follows the steps above, except with the
        // negated values and opposite directions".
        let table = FaceTable::new(
            vec![family(
                "Test",
                &["test"],
                &[],
                &[
                    (400, FontStyle::Normal),
                    (400, FontStyle::Oblique(20.0)),
                    (400, FontStyle::Oblique(-20.0)),
                ],
            )],
            Vec::new(),
        );
        assert_eq!(
            style_of(
                &table,
                &request_with("test", 400, FontStyle::Oblique(-20.0))
            ),
            Some(FontStyle::Oblique(-20.0))
        );
        assert_eq!(
            style_of(
                &table,
                &request_with("test", 400, FontStyle::Oblique(-10.0))
            ),
            Some(FontStyle::Oblique(-20.0)),
            "below eleven degrees a backslant prefers the smaller angle, so the \
             more negative one"
        );
    }

    #[test]
    fn the_weight_step_only_sees_faces_the_style_step_kept() {
        let family = family(
            "Test",
            &["test"],
            &[],
            &[
                (400, FontStyle::Normal),
                (700, FontStyle::Normal),
                (400, FontStyle::Italic),
                (700, FontStyle::Italic),
            ],
        );
        let (face, embolden, shear) =
            select_face(&family, &request_with("test", 700, FontStyle::Italic))
                .expect("the family has faces");
        assert_eq!(family.faces[face].weight, 700);
        assert_eq!(family.faces[face].style, FontStyle::Italic);
        assert!(!embolden);
        assert_eq!(shear, 0.0);
    }

    // ---- Step 3: the weight --------------------------------------------------

    #[test]
    fn the_weight_search_is_a_total_order_over_the_weights_present() {
        for (available, desired) in [
            (vec![400, 700, 900], 400_u16),
            (vec![300, 600], 300),
            (vec![300, 600], 500),
            (vec![100, 900], 100),
            (vec![100, 900], 900),
        ] {
            let order = weight_search_order(desired, &weights(&available));
            let mut sorted = order.clone();
            sorted.sort_unstable();
            let mut expected = available.clone();
            expected.sort_unstable();
            expected.dedup();
            assert_eq!(
                sorted, expected,
                "the search must consider every available weight exactly once \
                 for desired {desired} in {available:?}"
            );
        }
    }

    #[test]
    fn a_request_between_four_hundred_and_five_hundred_goes_down_before_it_goes_up() {
        // §5.2: "weights greater than or equal to the target weight are checked
        // in ascending order until 500 is hit and checked, followed by weights
        // less than the target weight in descending order, followed by weights
        // greater than 500".
        assert_eq!(
            weight_search_order(450, &weights(&[100, 400, 500, 700, 900])),
            [500, 400, 100, 700, 900]
        );
        assert_eq!(
            weight_search_order(400, &weights(&[100, 400, 500, 700, 900])),
            [400, 500, 100, 700, 900]
        );
    }

    #[test]
    fn a_request_below_four_hundred_goes_down_first() {
        // §5.2: "weights less than or equal to the desired weight are checked in
        // descending order followed by weights above the desired weight in
        // ascending order".
        assert_eq!(
            weight_search_order(300, &weights(&[100, 200, 500, 700])),
            [200, 100, 500, 700]
        );
    }

    #[test]
    fn a_request_above_five_hundred_goes_up_first() {
        // §5.2: "weights greater than or equal to the desired weight are checked
        // in ascending order followed by weights below the desired weight in
        // descending order".
        assert_eq!(
            weight_search_order(600, &weights(&[400, 500, 700, 900])),
            [700, 900, 500, 400]
        );
    }

    #[test]
    fn a_family_with_the_nine_step_weights_resolves_each_weight() {
        let family = family(
            "Test",
            &["test"],
            &[],
            &[
                (400, FontStyle::Normal),
                (500, FontStyle::Normal),
                (700, FontStyle::Normal),
                (900, FontStyle::Normal),
            ],
        );
        let weight_of = |desired: u16| {
            let (face, ..) =
                select_face(&family, &request_with("test", desired, FontStyle::Normal))
                    .expect("the family has faces");
            family.faces[face].weight
        };
        assert_eq!(weight_of(100), 400, "100 is below 400 so it goes up to 400");
        assert_eq!(weight_of(300), 400, "300 is below 400 so it goes up to 400");
        assert_eq!(weight_of(400), 400, "an exact match is used");
        assert_eq!(
            weight_of(450),
            500,
            "§5.2 checks the weights up to 500 before it checks any below the target"
        );
        assert_eq!(weight_of(500), 500, "an exact match is used");
        assert_eq!(weight_of(600), 700, "600 is above 500 so it goes up to 700");
        assert_eq!(weight_of(700), 700, "an exact match is used");
        assert_eq!(weight_of(900), 900, "an exact match is used");
        assert_eq!(
            weight_of(1000),
            900,
            "1000 has nothing above it and so descends"
        );
    }

    #[test]
    fn a_request_between_four_hundred_and_five_hundred_with_no_face_in_that_range_descends() {
        let family = family(
            "Test",
            &["test"],
            &[],
            &[
                (400, FontStyle::Normal),
                (700, FontStyle::Normal),
                (900, FontStyle::Normal),
            ],
        );
        let (face, ..) = select_face(&family, &request_with("test", 450, FontStyle::Normal))
            .expect("the family has faces");
        assert_eq!(
            family.faces[face].weight, 400,
            "700 and 900 are both past 500, so the search descends to 400 first"
        );
    }

    #[test]
    fn a_family_with_three_hundred_and_six_hundred_resolves_every_weight() {
        // §2.2.2's second figure: a family with 300 and 600 weight faces.
        let family = family(
            "Test",
            &["test"],
            &[],
            &[(300, FontStyle::Normal), (600, FontStyle::Normal)],
        );
        let weight_of = |desired: u16| {
            let (face, ..) =
                select_face(&family, &request_with("test", desired, FontStyle::Normal))
                    .expect("the family has faces");
            family.faces[face].weight
        };
        assert_eq!(weight_of(100), 300, "100 climbs to 300");
        assert_eq!(weight_of(300), 300);
        assert_eq!(
            weight_of(400),
            300,
            "400 has no exact face and 600 is past 500"
        );
        assert_eq!(
            weight_of(500),
            300,
            "500 has no exact face and 600 is past 500"
        );
        assert_eq!(weight_of(550), 600, "550 climbs to 600");
        assert_eq!(weight_of(600), 600);
        assert_eq!(
            weight_of(900),
            600,
            "900 has nothing above it and so descends"
        );
    }

    // ---- Missing faces and §2.8 synthesis ------------------------------------

    #[test]
    fn a_bold_request_with_no_bold_face_synthesizes_one() {
        let family = family("Test", &["test"], &[], &[(400, FontStyle::Normal)]);
        let (face, embolden, shear) =
            select_face(&family, &request_with("test", 700, FontStyle::Normal))
                .expect("the family has faces");
        assert_eq!(family.faces[face].weight, 400);
        assert!(
            embolden,
            "§2.2.2 requires a synthesized bold to be treated as if it existed in \
             the family, so bold text must not render as regular text"
        );
        assert_eq!(shear, 0.0);
    }

    #[test]
    fn font_synthesis_weight_none_forbids_the_synthetic_bold() {
        let family = family("Test", &["test"], &[], &[(400, FontStyle::Normal)]);
        let (face, embolden, _) = select_face(
            &family,
            &request(
                "test",
                700,
                FontStyle::Normal,
                FontSynthesis {
                    weight: false,
                    style: true,
                },
            ),
        )
        .expect("the family has faces");
        assert_eq!(family.faces[face].weight, 400);
        assert!(!embolden, "§2.8.1 `font-synthesis-weight: none`");
    }

    #[test]
    fn a_family_that_has_a_bold_face_is_never_synthesized_for_a_bold_request() {
        let family = family(
            "Test",
            &["test"],
            &[],
            &[(400, FontStyle::Normal), (700, FontStyle::Normal)],
        );
        let (face, embolden, _) =
            select_face(&family, &request_with("test", 700, FontStyle::Normal))
                .expect("the family has faces");
        assert_eq!(family.faces[face].weight, 700);
        assert!(!embolden);
    }

    #[test]
    fn a_heavier_request_uses_the_heaviest_face_and_synthesizes_on_top_of_it() {
        let family = family(
            "Test",
            &["test"],
            &[],
            &[(400, FontStyle::Normal), (700, FontStyle::Normal)],
        );
        let (face, embolden, _) =
            select_face(&family, &request_with("test", 900, FontStyle::Normal))
                .expect("the family has faces");
        assert_eq!(
            family.faces[face].weight, 700,
            "the weight search climbs to 700"
        );
        assert!(
            embolden,
            "and 900 is still bolder than anything the family has"
        );
    }

    #[test]
    fn a_request_inside_the_normal_region_is_never_synthesized() {
        // §5.2's weight search treats 400 through 500 as one region, so a 500
        // request against a 400-only family is a request for the regular face and
        // not a request for something heavier.
        let family = family("Test", &["test"], &[], &[(400, FontStyle::Normal)]);
        let (face, embolden, _) =
            select_face(&family, &request_with("test", 500, FontStyle::Normal))
                .expect("the family has faces");
        assert_eq!(family.faces[face].weight, 400);
        assert!(!embolden, "500 is not bolder than 400 in CSS's own scale");
    }

    #[test]
    fn an_oblique_request_with_no_oblique_face_is_sheared_to_the_angle() {
        // §5.2: "if font-synthesis-style has the value auto, then a fallback
        // match is produced by geometric shearing to the specified oblique value."
        let family = family("Test", &["test"], &[], &[(400, FontStyle::Normal)]);
        let (face, embolden, shear) = select_face(
            &family,
            &request_with("test", 400, FontStyle::Oblique(14.0)),
        )
        .expect("the family has faces");
        assert_eq!(family.faces[face].style, FontStyle::Normal);
        assert!(!embolden);
        assert!(
            (shear - 14.0).abs() < f32::EPSILON,
            "the shear must be the requested angle, not a guess"
        );
    }

    #[test]
    fn font_synthesis_style_none_forbids_the_synthetic_oblique() {
        let family = family("Test", &["test"], &[], &[(400, FontStyle::Normal)]);
        let (_, _, shear) = select_face(
            &family,
            &request(
                "test",
                400,
                FontStyle::Oblique(14.0),
                FontSynthesis {
                    weight: true,
                    style: false,
                },
            ),
        )
        .expect("the family has faces");
        assert_eq!(shear, 0.0, "§2.8.2 `font-synthesis-style: none`");
    }

    #[test]
    fn an_oblique_request_with_an_oblique_face_is_not_sheared() {
        let with_oblique = family(
            "Test",
            &["test"],
            &[],
            &[(400, FontStyle::Normal), (400, FontStyle::Oblique(14.0))],
        );
        let (_, _, shear) = select_face(
            &with_oblique,
            &request_with("test", 400, FontStyle::Oblique(14.0)),
        )
        .expect("the family has faces");
        assert_eq!(shear, 0.0, "a real oblique face needs no shearing");
        let upright_only = family("Test", &["test"], &[], &[(400, FontStyle::Normal)]);
        let (_, _, regular) =
            select_face(&upright_only, &plain("test")).expect("the family has faces");
        assert_eq!(regular, 0.0);
    }

    #[test]
    fn an_italic_request_is_never_synthesized() {
        // §2.4: "For user agents that treat these values distinctly, synthesis
        // must not be performed for italic." This engine does treat them
        // distinctly, so an italic request with no italic face resolves to the
        // upright face, which is what §5.2's last oblique stage selects.
        let family = family("Test", &["test"], &[], &[(400, FontStyle::Normal)]);
        let (face, _, shear) = select_face(&family, &request_with("test", 400, FontStyle::Italic))
            .expect("the family has faces");
        assert_eq!(family.faces[face].style, FontStyle::Normal);
        assert_eq!(shear, 0.0, "an italic request must not become a shear");
    }

    #[test]
    fn an_oblique_request_against_a_family_with_the_wrong_oblique_face_is_not_sheared() {
        // The family has an oblique face, so §2.8's "fallback match" for a family
        // that *lacks* the face does not apply; §5.2's own search decided.
        let family = family(
            "Test",
            &["test"],
            &[],
            &[(400, FontStyle::Normal), (400, FontStyle::Oblique(14.0))],
        );
        let (_, _, shear) = select_face(
            &family,
            &request_with("test", 400, FontStyle::Oblique(30.0)),
        )
        .expect("the family has faces");
        assert_eq!(shear, 0.0);
    }

    #[test]
    fn synthesis_composes_across_both_axes() {
        let family = family("Test", &["test"], &[], &[(400, FontStyle::Normal)]);
        let (_, embolden, shear) = select_face(
            &family,
            &request_with("test", 700, FontStyle::Oblique(20.0)),
        )
        .expect("the family has faces");
        assert!(
            embolden,
            "a bold oblique with neither face available is both"
        );
        assert!((shear - 20.0).abs() < f32::EPSILON);
    }

    // ---- Step 4: the character ----------------------------------------------

    fn covered(walk: &FamilyWalk, table: &FaceTable, character: char) -> Option<(usize, usize)> {
        table
            .face_for(walk, character, |_, _| true)
            .map(|matched| (matched.family, matched.face))
    }

    #[test]
    fn the_first_family_with_the_character_wins() {
        let table = FaceTable::new(
            vec![
                family("First", &["first"], &[], &[(400, FontStyle::Normal)]),
                family("Second", &["second"], &[], &[(400, FontStyle::Normal)]),
            ],
            Vec::new(),
        );
        let walk = table.walk(&plain("first, second"));
        assert_eq!(covered(&walk, &table, 'A'), Some((0, 0)));
    }

    #[test]
    fn glyphs_from_other_faces_in_the_same_family_are_not_considered() {
        // §5.2: "If no matching face exists or the matched face does not contain
        // a glyph for the character to be rendered, the next family name is
        // selected ... Glyphs from other faces in the family are not considered."
        let table = FaceTable::new(
            vec![
                family("Narrow", &["narrow"], &[], &[(400, FontStyle::Normal)]),
                family("Wide", &["wide"], &[], &[(400, FontStyle::Normal)]),
            ],
            Vec::new(),
        );
        let walk = table.walk(&plain("narrow, wide"));
        // The oracle answers yes for the second family and no for the first,
        // which is what a family whose chosen face lacks the character looks like.
        let matched = table
            .face_for(&walk, '\u{6e32}', |family, _| family == 1)
            .expect("the second family has the character");
        assert_eq!(matched.family, 1);
    }

    #[test]
    fn a_character_no_family_has_gets_no_face_at_all() {
        let table = FaceTable::new(
            vec![family(
                "Arial",
                &["arial"],
                &[],
                &[(400, FontStyle::Normal)],
            )],
            Vec::new(),
        );
        let walk = table.walk(&plain("arial"));
        assert!(
            table.face_for(&walk, '\u{6e32}', |_, _| false).is_none(),
            "§5 says a character no font can display must be indicated as not \
             displayed, not replaced with a wrong letterform"
        );
    }

    // ---- §5.4: the private use area -----------------------------------------

    #[test]
    fn a_private_use_character_never_reaches_a_generic_family() {
        let table = FaceTable::new(
            vec![
                family(
                    "Consolas",
                    &["consolas"],
                    &[GenericFamily::Monospace],
                    &[(400, FontStyle::Normal)],
                ),
                family("Icon Font", &["icons"], &[], &[(400, FontStyle::Normal)]),
            ],
            Vec::new(),
        );
        let walk = table.walk(&plain("\"icons\", monospace"));
        let private = '\u{e610}';
        assert!(is_private_use(private));
        assert_eq!(
            table
                .face_for(&walk, private, |family, _| family == 1)
                .map(|matched| matched.family),
            Some(1),
            "§5.4: only the families named in `font-family` that are not generic \
             may match a private-use codepoint"
        );
        assert!(
            table
                .face_for(&walk, private, |family, _| family == 0)
                .is_none(),
            "the generic family must be excluded even though it is in the walk"
        );
    }

    #[test]
    fn a_private_use_character_never_reaches_installed_font_fallback() {
        let table = FaceTable::new(
            vec![
                family("Arial", &["arial"], &[], &[(400, FontStyle::Normal)]),
                family("YaHei", &["yahei"], &[], &[(400, FontStyle::Normal)]),
            ],
            vec![1],
        );
        // A list that names nothing the engine has: the only families left in the
        // walk are the installed-fallback ones, which §5.4 puts off limits for a
        // private-use codepoint.
        let walk = table.walk(&plain("\"No Such Font\""));
        assert!(
            table.face_for(&walk, '\u{e610}', |_, _| true).is_none(),
            "§5.4 forbids installed font fallback for a private-use codepoint"
        );
        assert!(
            table.face_for(&walk, '\u{6e32}', |_, _| true).is_some(),
            "and the fallback is still used for an ordinary character"
        );
    }

    #[test]
    fn an_ordinary_character_does_use_installed_font_fallback() {
        let table = FaceTable::new(
            vec![
                family("Arial", &["arial"], &[], &[(400, FontStyle::Normal)]),
                family("YaHei", &["yahei"], &[], &[(400, FontStyle::Normal)]),
            ],
            vec![1],
        );
        let walk = table.walk(&plain("\"No Such Font\""));
        assert_eq!(covered(&walk, &table, '\u{6e32}'), Some((1, 0)));
    }

    // ---- The request value itself -------------------------------------------

    #[test]
    fn the_same_request_always_walks_the_same_families() {
        let table = FaceTable::new(
            vec![
                family("Arial", &["arial"], &[], &[(400, FontStyle::Normal)]),
                family("YaHei", &["yahei"], &[], &[(400, FontStyle::Normal)]),
            ],
            vec![1],
        );
        let first = table.walk(&plain("arial, yahei"));
        let second = table.walk(&plain("arial, yahei"));
        assert_eq!(
            first, second,
            "§5.2: the choice must not differ between two elements in the same \
             document"
        );
    }

    #[test]
    fn a_reference_text_style_carries_the_axis_into_the_measurer() {
        // The measurement request is the same value the painter receives, so a
        // `TextStyle` literal is the whole of the wiring on this side.
        let style = TextStyle {
            font_size: 32.0,
            line_height: 38.4,
            font: request_with("mono", 700, FontStyle::Oblique(14.0)),
        };
        let table = FaceTable::new(
            vec![family(
                "Mono",
                &["mono"],
                &[GenericFamily::Monospace],
                &[(400, FontStyle::Normal), (700, FontStyle::Normal)],
            )],
            Vec::new(),
        );
        let chosen = table.walk(&style.font);
        let entry = chosen.entries().first().copied().expect("a face");
        let face = table.families()[entry.family].faces[entry.face];
        assert_eq!(
            face.weight, style.font.weight,
            "the weight the request asked for is a real face of the family"
        );
        assert!(
            !entry.embolden,
            "a family that has a bold face never has one synthesized"
        );
        assert!(
            (entry.shear_degrees - 14.0).abs() < f32::EPSILON,
            "the family has no oblique face, so the request reaches the painter \
             as a shear of the requested angle rather than as upright text"
        );
    }
}
