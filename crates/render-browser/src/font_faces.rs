//! One document's `@font-face` faces, and the font resources they name.
//!
//! # Why the face table has to have a lifetime
//!
//! The platform table in [`crate::font_backend`] is a property of the machine:
//! it is built once at start-up from files on disk and it is the same for every
//! page. An `@font-face` rule is not. Its descriptors arrive in a stylesheet,
//! its bytes arrive from the network, and both belong to one document that stops
//! existing when the browser navigates away. §4.1 says as much: "A given set of
//! `@font-face` rules define a set of fonts available for use within the
//! documents that contain these rules", and §10.2 that "A Web Font must not be
//! accessible in any other Document from the one which either is associated with
//! the `@font-face` rule or owns the `FontFaceSet`."
//!
//! So the boundary this module draws is a **document-scoped store**: a
//! [`DocumentFonts`] holds the rules, the decoded resources, and nothing else. It
//! is owned by the browser's page state and dropped with the document, which is
//! what makes both required properties structural rather than a matter of
//! remembering to clean up:
//!
//! - **It cannot go stale.** A face is reachable only through the store the
//!   document owns. There is no process-wide collection a face can be left
//!   behind in, so there is nothing to expire.
//! - **It cannot leak.** Dropping the document drops the store, and the decoded
//!   `Font` values with it.
//!
//! # Why the resource cache is here and not process-wide
//!
//! A URL is decoded at most once per document. Two `@font-face` rules naming one
//! URL - which the corpus does, its one multi-entry `src` list is five
//! spellings of the same face - share one `Arc`, and a fetch plan rebuilt on a
//! new DOM revision finds the resource already present rather than requesting it
//! again. That is the duplication that has to be avoided, because a 10 MB CJK
//! face is worth avoiding.
//!
//! Across navigations the answer is a different layer's: §4.1 says explicitly
//! that these restrictions "do not affect caching behavior, fonts are cached the
//! same way other web resources are cached", and the engine's resource cache is
//! where that already lives. A font cache here would be a second one with its
//! own eviction policy, and a bounded cache is a policy someone has to get
//! right. Document scope has no policy to get wrong.
//!
//! # What a face looks like before it has arrived
//!
//! Nothing. §5.2: "If the font resources defined for a given face in an
//! `@font-face` rule are either not available or contain invalid font data, then
//! the face should be treated as not present in the family." This module
//! therefore records a rule whether or not its resource has arrived, and the
//! face the matcher sees is a question asked at table-build time rather than a
//! state the rule carries. That is the whole of the "not yet arrived" answer, and
//! it is consistent between measurement and painting because there is nothing
//! per-face for the two to disagree about: both read the one table.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use fontdue::{Font, FontSettings};
use render_core::font_face::{FontFaceRule, FontFaceSheet, FontSource, UnicodeRangeSet};
use render_core::layout::FontStyle;
use render_net::Url;

/// One `@font-face` rule, with every URL already resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentRule {
    /// §4.4's `font-weight` descriptor.
    pub weight: u16,
    /// §4.4's `font-style` descriptor.
    pub style: FontStyle,
    /// §4.5's `unicode-range` descriptor.
    pub range: UnicodeRangeSet,
    /// §4.3.1's items, in author order, with §4.3.2's URL resolution done.
    pub sources: Vec<ResolvedSource>,
}

/// One item of a rule's `src`, with §4.3.2's resolution applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedSource {
    /// §4.3.3.1's `local()`: a name to look for among installed faces, before
    /// any URL is fetched.
    Local(String),
    /// A URL, already absolute.
    Url(Url),
}

/// One family a document declares, with its rules in §4.5.1's check order.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentFamily {
    /// §4.2's family name, lowercased for §5.1's caseless matching.
    pub name: String,
    /// The name as the author wrote it, for a diagnostic.
    pub name_as_written: String,
    /// The family's rules, **last declared first**.
    pub rules: Vec<DocumentRule>,
}

/// Why a font resource could not be turned into a face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontLoadFailure {
    /// The body is not a font container this engine's rasteriser can read.
    ///
    /// §4.3.3's reason to skip a request in advance is a format *hint*; this is
    /// the case where there was no usable hint, or the hint was right and the
    /// body is not what the hint said. §5.2 gives both the same outcome: "If the
    /// font resources ... are either not available or contain invalid font data,
    /// then the face should be treated as not present in the family."
    Undecodable,
}

impl FontLoadFailure {
    /// A message naming the engine's limit, because "the font did not load" is
    /// not something an author can act on.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Undecodable => "the body is not a font container this engine can decode",
        }
    }
}

/// The `@font-face` faces of one document.
#[derive(Clone, Debug, Default)]
pub struct DocumentFonts {
    /// The document's families, each with its rules in §4.5.1's order.
    families: Vec<DocumentFamily>,
    /// The decoded resources, keyed by absolute URL.
    ///
    /// `Font` is not `Debug`, which is why this map is not derived.
    resources: HashMap<Url, Arc<Font>>,
}

impl DocumentFonts {
    /// An empty store, which is what a document that declares no webfont has.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one stylesheet's rules, resolving every `src` URL against `base` -
    /// the URL the stylesheet itself came from - as §4.3.2 requires.
    ///
    /// The rules are appended to their family's list, so a second stylesheet's
    /// `@font-face` rules continue the same family and §4.5.1's ordering still
    /// holds across sheets: later declarations are checked first.
    ///
    /// A rule's descriptors were validated when the stylesheet was parsed, and
    /// anything wrong with them was reported then, so this cannot fail: a rule
    /// that got this far has a family and at least one usable `src`.
    pub fn add_stylesheet(&mut self, sheet: &FontFaceSheet, base: &Url) {
        self.add_rules(sheet.rules(), base);
    }

    /// Adds rules parsed from somewhere other than a fetched stylesheet, such as
    /// an inline `<style>` element's text, whose base is the document's URL.
    pub fn add_rules(&mut self, rules: &[FontFaceRule], base: &Url) {
        // `FontFaceSheet::rules` is in ascending document order and §4.5.1 wants
        // the last declared rule checked first, so this sheet's rules are built
        // backwards.
        // Every rule goes at the *front* of its family, in source order, and the
        // source order is ascending. That single rule gives both halves of §4.5.1
        // at once: within one sheet, inserting first, second, third in that order
        // at the front leaves `third, second, first`; and across sheets, the second
        // sheet's rules go in front of the first's, which is the same relationship
        // a later declaration has to an earlier one. Appending would have got the
        // second case backwards, which is the one that matters for a page that
        // ships its slices in two files.
        let resolved: Vec<(&str, DocumentRule)> = rules
            .iter()
            .map(|rule| {
                (
                    rule.family.as_str(),
                    DocumentRule {
                        weight: rule.weight,
                        style: rule.style,
                        range: rule.unicode_range.clone(),
                        sources: resolve_sources(&rule.sources, base),
                    },
                )
            })
            .collect();
        // The spelling the author wrote is kept for a diagnostic, and it is the
        // *first* rule for a family that spells it, so a later sheet's
        // differently-cased spelling does not change which name a report quotes.
        let mut name_as_written: HashMap<&str, &str> = HashMap::new();
        for rule in rules {
            name_as_written
                .entry(rule.family.as_str())
                .or_insert(rule.family_as_written.as_str());
        }
        for (name, document_rule) in resolved {
            match self.families.iter_mut().find(|family| family.name == name) {
                Some(family) => family.rules.insert(0, document_rule),
                None => self.families.push(DocumentFamily {
                    name: name.to_owned(),
                    name_as_written: name_as_written
                        .get(name)
                        .copied()
                        .unwrap_or(name)
                        .to_owned(),
                    rules: vec![document_rule],
                }),
            }
        }
    }

    /// The document's families, in the order §4.5.1 checks their rules.
    #[must_use]
    pub fn families(&self) -> &[DocumentFamily] {
        &self.families
    }

    /// Every rule in the document, in the order they are checked.
    pub fn rules(&self) -> impl Iterator<Item = &DocumentRule> {
        self.families.iter().flat_map(|family| family.rules.iter())
    }

    /// How many `@font-face` rules the document declares, whether or not their
    /// resources have arrived.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.families.iter().map(|family| family.rules.len()).sum()
    }

    /// Whether the document declares no face at all, which is the common case
    /// and the reason a fetch plan is usually empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }

    /// Whether a resource for `url` has already been decoded for this document.
    #[must_use]
    pub fn has_resource(&self, url: &Url) -> bool {
        self.resources.contains_key(url)
    }

    /// The decoded resource for `url`, if it has arrived.
    #[must_use]
    pub fn resource(&self, url: &Url) -> Option<Arc<Font>> {
        self.resources.get(url).cloned()
    }

    /// The URLs a fetch is still needed for, in the order the rules are checked
    /// and with duplicates removed.
    ///
    /// This is every URL the document could need, not every URL it needs: §4.8.1
    /// forbids downloading a font no style rule refers to, so the caller narrows
    /// it with [`DocumentFonts::plan_fetches`] before issuing requests.
    #[must_use]
    pub fn outstanding_urls(&self) -> Vec<Url> {
        let mut seen = BTreeSet::new();
        let mut urls = Vec::new();
        for rule in self.rules() {
            for source in &rule.sources {
                if let ResolvedSource::Url(url) = source
                    && !self.resources.contains_key(url)
                    && seen.insert(url.clone())
                {
                    urls.push(url.clone());
                }
            }
        }
        urls
    }

    /// Decodes a fetched body and registers it under `url`.
    ///
    /// A body that arrives for a URL no rule names is still kept, because a rule
    /// naming it may be added by a later stylesheet and a second fetch of a 10 MB
    /// face is exactly the cost this avoids.
    ///
    /// # Errors
    ///
    /// [`FontLoadFailure::Undecodable`] when the body is not a font container this
    /// engine's rasteriser can read. Nothing is registered in that case, and §5.2
    /// then treats the face as not present in its family.
    pub fn install(&mut self, url: &Url, bytes: &[u8]) -> Result<(), FontLoadFailure> {
        if self.resources.contains_key(url) {
            return Ok(());
        }
        let settings = FontSettings {
            collection_index: 0,
            ..FontSettings::default()
        };
        let font = Font::from_bytes(bytes, settings).map_err(|_| FontLoadFailure::Undecodable)?;
        self.resources.insert(url.clone(), Arc::new(font));
        Ok(())
    }

    /// The URLs to fetch for a page that names `used_families` and contains
    /// `used_characters`, deduplicated and in check order.
    ///
    /// §4.8.1 is the requirement this answers: "user agents must only download
    /// those fonts that are referred to within the style rules applicable to a
    /// given page. User agents that download all fonts defined in `@font-face`
    /// rules without considering whether those fonts are in fact used within a
    /// page are considered non-conformant." Two things are decided here, and both
    /// are needed to make the corpus's fonts affordable:
    ///
    /// - **Which families.** A family no computed `font-family` names is not
    ///   fetched at all. §4.8.1 also permits fetching a font "if it's contained
    ///   within the computed value of `font-family` for a given text run", which
    ///   is the criterion used: `used_families` is the set of family names the
    ///   page's computed styles contain, compared caselessly per §5.1.
    /// - **Which slices.** Within a fetched family, a rule is fetched only if
    ///   §4.5's range admits a character the page actually contains. §4.5 calls
    ///   the range "a hint for user agents when deciding whether or not to
    ///   download a font resource for a given text run", and the corpus makes it
    ///   the difference between 108 requests and a handful: the corpus ships
    ///   one weight-500 CJK face as 108 disjoint `unicode-range` slices, and
    ///   a page with no Cyrillic text has no use for the Cyrillic slice.
    ///
    /// The characters are not attributed to the family that uses them, so a slice
    /// is fetched if the page contains *any* character in its range and the page
    /// names the family. That is a documented over-approximation: it can fetch a
    /// slice the family is not actually used for, and it never misses one. The
    /// alternative - attributing characters to families - needs the text fragment
    /// each character was laid out with, which this crate does not carry.
    #[must_use]
    pub fn plan_fetches<'a>(
        &self,
        used_families: impl IntoIterator<Item = &'a str>,
        used_characters: &BTreeSet<char>,
    ) -> Vec<Url> {
        let wanted: Vec<String> = used_families
            .into_iter()
            .map(|name| name.trim().trim_matches(['"', '\'']).to_lowercase())
            .collect();
        let mut seen = BTreeSet::new();
        let mut urls = Vec::new();
        for family in &self.families {
            if !wanted
                .iter()
                .any(|name| render_core::layout::caseless_match(&family.name, name))
            {
                continue;
            }
            for rule in &family.rules {
                if !used_characters
                    .iter()
                    .any(|character| rule.range.contains(*character))
                {
                    continue;
                }
                // §4.3.3.1: a `local()` item is looked for first and costs
                // nothing, so the URL after it is only needed when the local
                // lookup found nothing. The store cannot answer that - it does
                // not know the installed faces - so the URL is planned and the
                // fetch is skipped by the loader when a `local()` item wins.
                // That ordering is what stops a network request for a font
                // already on the machine.
                for source in &rule.sources {
                    if let ResolvedSource::Url(url) = source
                        && seen.insert(url.clone())
                    {
                        urls.push(url.clone());
                    }
                }
            }
        }
        urls
    }

    /// The rules whose `src` names a font this document could select, for a
    /// measurement of what was registered against what the matcher can reach.
    #[must_use]
    pub fn family_count(&self) -> usize {
        self.families.len()
    }
}

/// §4.3.2: "the URL can be relative, in which case it is resolved relative to
/// the location of the style sheet containing the @font-face rule."
fn resolve_sources(sources: &[FontSource], base: &Url) -> Vec<ResolvedSource> {
    sources
        .iter()
        .filter_map(|source| match source {
            FontSource::Local { name } => Some(ResolvedSource::Local(name.clone())),
            FontSource::Url { url, .. } => Some(ResolvedSource::Url(resolve_url(url, base)?)),
        })
        .collect()
}

/// §4.3.2: resolves a `src` URL against the stylesheet's own URL.
///
/// A protocol-relative URL is the case the corpus leans on: a `<url-token>`
/// written as `url(//host/path)` carries no scheme of its own, so resolving it
/// needs the scheme the stylesheet itself arrived under. The corpus writes this
/// form, which is what makes it worth handling rather than rejecting.
/// under.
///
/// `None` means the URL cannot be resolved, and the only way that happens is a
/// relative URL under a base with no hierarchy to resolve against - a `data:`
/// stylesheet, in practice. §4.3.2 cannot be carried out there, so the item is
/// not a source: §4.3.1's rule that an item which cannot be resolved "do[es] not
/// add it to the list of supported sources" covers the case, and the caller
/// reports it through the rule having no usable `src`.
fn resolve_url(url: &str, base: &Url) -> Option<Url> {
    if let Some(rest) = url.strip_prefix("//") {
        let scheme = base.scheme();
        return Url::parse(&format!("{scheme}://{rest}")).ok();
    }
    base.join(url).ok()
}
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use render_core::font_face::parse_font_faces;
    use render_core::layout::FontStyle;

    use super::{DocumentFonts, FontLoadFailure, ResolvedSource, Url};

    fn base() -> Url {
        Url::parse("https://cdn.test/assets/main.css").expect("a parseable base URL")
    }

    fn store_of(source: &str) -> DocumentFonts {
        let (sheet, _problems) = parse_font_faces(source);
        let mut document = DocumentFonts::new();
        document.add_stylesheet(&sheet, &base());
        document
    }

    /// Every URL a family holds, in family order.
    fn urls_of(document: &DocumentFonts, family: &str) -> Vec<String> {
        document
            .families()
            .iter()
            .find(|declared| declared.name == family)
            .map(|declared| {
                declared
                    .rules
                    .iter()
                    .flat_map(|rule| rule.sources.iter())
                    .filter_map(|source| match source {
                        ResolvedSource::Url(url) => Some(url.to_string()),
                        ResolvedSource::Local(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// §4.3.2: a `src` URL is "resolved relative to the location of the style
    /// sheet containing the `@font-face` rule", and the store is where that
    /// happens, because the store is what hands the URL to the fetcher.
    #[test]
    fn a_relative_src_url_is_resolved_against_the_stylesheet() {
        let document =
            store_of("@font-face { font-family: Rel; src: url(../fonts/a.ttf) format(truetype) }");
        assert_eq!(urls_of(&document, "rel"), ["https://cdn.test/fonts/a.ttf"]);
    }

    /// The corpus's own shape: a protocol-relative URL, which is a `<url-token>`
    /// the tokenizer hands over with no scheme of its own.
    #[test]
    fn a_protocol_relative_src_url_takes_the_stylesheets_scheme() {
        let document = store_of(
            "@font-face { font-family: Abs; src: url(//s1.hdslb.com/bfs/a.woff2) format('woff2'), \
             url(//s1.hdslb.com/bfs/b.ttf) format(truetype) }",
        );
        assert_eq!(
            urls_of(&document, "abs"),
            ["https://s1.hdslb.com/bfs/b.ttf"],
            "the woff2 is dropped by §4.3.3 before the store ever sees it, so the \
             engine does not request a format it cannot decode"
        );
    }

    /// A stylesheet served from a `data:` URL has no hierarchical base, so §4.3.2's
    /// "resolved relative to the location of the style sheet" cannot be carried out
    /// and the item is not a source.
    ///
    /// The test pins the drop rather than a fallback URL, because inventing a base
    /// for a `data:` document would mean fetching a font from a location the author
    /// never wrote. §4.3.1's rule covers it: an item whose value cannot be resolved
    /// is not added to the list of sources.
    #[test]
    fn a_relative_url_under_a_data_url_base_is_not_a_source() {
        let data = Url::parse("data:text/css,body{color:red}").expect("a parseable data URL");
        let (sheet, _problems) =
            parse_font_faces("@font-face { font-family: D; src: url(a.ttf) format(truetype) }");
        let mut document = DocumentFonts::new();
        document.add_stylesheet(&sheet, &data);
        assert_eq!(document.rule_count(), 1, "the rule is still a rule");
        assert!(
            urls_of(&document, "d").is_empty(),
            "but the URL it names cannot be resolved against a data: URL, so there \
             is nothing to fetch and the face is not present in the family"
        );
        assert!(
            document
                .plan_fetches(["D"], &BTreeSet::from(['A']))
                .is_empty(),
            "and §4.8.1 has nothing to plan, because there is no request to make"
        );
    }

    /// §4.5.1's order is a property of the store, not of the matcher: a family's
    /// rules are held last-declared-first, so the matcher walks them in the order
    /// the specification says to check and has to reverse nothing itself.
    #[test]
    fn a_familys_rules_are_held_in_the_order_the_matcher_must_check_them() {
        let document = store_of(
            "@font-face { font-family: S; src: url(first.ttf) format(truetype) } \
             @font-face { font-family: S; src: url(second.ttf) format(truetype) } \
             @font-face { font-family: S; src: url(third.ttf) format(truetype) }",
        );
        assert_eq!(document.families()[0].rules.len(), 3);
        assert_eq!(
            urls_of(&document, "s"),
            [
                "https://cdn.test/assets/third.ttf",
                "https://cdn.test/assets/second.ttf",
                "https://cdn.test/assets/first.ttf"
            ],
            "§4.5.1: the last rule defined is the first to be checked for a \
             character, so the list is held in that order"
        );
    }

    /// Two stylesheets declaring one family make one family, and the later sheet's
    /// rules still come first - §4.5.1 says "the rules", not "the rules of one
    /// stylesheet".
    #[test]
    fn two_stylesheets_declare_one_family_and_the_later_sheet_wins() {
        let (first_sheet, _problems) =
            parse_font_faces("@font-face { font-family: T; src: url(one.ttf) format(truetype) }");
        let (second_sheet, _problems) =
            parse_font_faces("@font-face { font-family: T; src: url(two.ttf) format(truetype) }");
        let mut document = DocumentFonts::new();
        document.add_stylesheet(&first_sheet, &base());
        document.add_stylesheet(&second_sheet, &base());
        assert_eq!(document.family_count(), 1, "one family, not two");
        assert_eq!(
            urls_of(&document, "t"),
            [
                "https://cdn.test/assets/two.ttf",
                "https://cdn.test/assets/one.ttf"
            ],
            "and the second stylesheet's rule is checked first"
        );
    }

    /// A `local()` item is kept, in author order, and produces no URL - which is
    /// what makes it free.
    #[test]
    fn a_local_item_is_kept_before_the_url_beside_it() {
        let document = store_of(
            "@font-face { font-family: L; src: local(Gentium), url(g.ttf) format(truetype) }",
        );
        assert_eq!(
            document.families()[0].rules[0].sources,
            vec![
                ResolvedSource::Local("Gentium".to_owned()),
                ResolvedSource::Url("https://cdn.test/assets/g.ttf".parse().expect("a URL")),
            ],
            "§4.3.3.1: local() is tried first, so a font already on the machine \
             costs no request"
        );
    }

    /// §4.8.1: a font no style rule refers to must not be downloaded, and the
    /// corpus is why - 223 rules across ten stylesheets, most of which one page
    /// never uses.
    #[test]
    fn a_family_no_page_uses_is_not_planned_for_a_fetch() {
        let document = store_of(
            "@font-face { font-family: Used; src: url(u.ttf) format(truetype) } \
             @font-face { font-family: Unused; src: url(x.ttf) format(truetype) }",
        );
        assert_eq!(
            document.plan_fetches(["Used", "monospace"], &BTreeSet::from(['A', 'Z', ' '])),
            [Url::parse("https://cdn.test/assets/u.ttf").expect("a URL")],
            "\"user agents that download all fonts defined in @font-face rules \
             without considering whether those fonts are in fact used within a \
             page are considered non-conformant\""
        );
    }

    /// §4.5's range decides which slices are fetched, and it is the whole
    /// difference between the corpus's 108 requests and a handful.
    #[test]
    fn a_slice_is_only_fetched_when_the_page_has_a_character_it_admits() {
        // The corpus's shape: one weight-500 CJK face as many disjoint slices.
        let rules: Vec<String> = (0_u32..8)
            .map(|index| {
                let start = 0x4e00 + index * 0x100;
                format!(
                    "@font-face {{ font-family: Slice; font-weight: 500; \
                     src: url(s{index}.ttf) format(truetype); \
                     unicode-range: U+{start:X}-{:X}; }}\n",
                    start + 0xff
                )
            })
            .collect();
        let document = store_of(&rules.join(""));
        assert_eq!(document.rule_count(), 8);

        assert_eq!(
            document.plan_fetches(["Slice"], &BTreeSet::from(['\u{4e2d}'])),
            [Url::parse("https://cdn.test/assets/s0.ttf").expect("a URL")],
            "§4.5 calls the range \"a hint ... when deciding whether or not to \
             download a font resource for a given text run\", and one character in \
             one slice is one request"
        );
        assert_eq!(
            document
                .plan_fetches(
                    ["Slice"],
                    &BTreeSet::from(['\u{4e2d}', '\u{4f2d}', '\u{502d}'])
                )
                .len(),
            3,
            "characters in three slices are three faces"
        );
        assert!(
            document
                .plan_fetches(["Slice"], &BTreeSet::new())
                .is_empty(),
            "and a page with no character in any slice fetches no slice, which is \
             §4.8.1's rule and not an optimisation"
        );
    }

    /// The plan deduplicates, so a family two rules share one URL for is one
    /// request.
    #[test]
    fn the_fetch_plan_lists_each_url_once() {
        let document = store_of(
            "@font-face { font-family: Dup; src: url(same.ttf) format(truetype) } \
             @font-face { font-family: Dup; src: url(same.ttf) format(truetype) } \
             @font-face { font-family: Dup; src: url(other.ttf) format(truetype) }",
        );
        assert_eq!(
            document.plan_fetches(["Dup"], &BTreeSet::from(['A'])),
            vec![
                Url::parse("https://cdn.test/assets/other.ttf").expect("a URL"),
                Url::parse("https://cdn.test/assets/same.ttf").expect("a URL"),
            ],
            "each URL once, in the order the rules are checked - and the last \
             declared rule is checked first, so `other` comes before `same`"
        );
    }

    /// The plan is not offered a family it does not have, and the comparison is
    /// §5.1's caseless one, so a family declared in one case is fetched for a
    /// `font-family` written in another.
    #[test]
    fn the_fetch_plan_matches_a_family_name_caselessly() {
        let document = store_of(
            "@font-face { font-family: HarmonyOS_Medium; src: url(h.ttf) format(truetype) }",
        );
        assert_eq!(
            document.families()[0].name,
            "harmonyos_medium",
            "the name is folded once, where the table key is built"
        );
        for spelling in [
            "\"harmonyos_medium\"",
            "\"HarmonyOS_Medium\"",
            "harmonyos_medium",
        ] {
            assert_eq!(
                document
                    .plan_fetches([spelling], &BTreeSet::from(['A']))
                    .len(),
                1,
                "{spelling} is the same family, because §5.1 matches caselessly \
                 and §2.1.1's computed value is the string"
            );
        }
        assert!(
            document
                .plan_fetches(["\"Some Other Font\""], &BTreeSet::from(['A']))
                .is_empty(),
            "and a family the page does not name is not fetched at all"
        );
    }

    /// A body that is not a font container is refused, and the refusal is named
    /// rather than being a silent drop - 218 of the corpus's 223 blocks end up
    /// refused for this reason once their bytes arrive.
    #[test]
    fn a_body_that_is_not_a_font_is_refused_with_a_reason() {
        let mut document = DocumentFonts::new();
        let url = Url::parse("https://cdn.test/assets/a.woff2").expect("a URL");
        let error = document
            .install(&url, b"wOF2\0\0\0\0 not a font at all")
            .expect_err("a body with no sfnt magic is not a face");
        assert_eq!(error, FontLoadFailure::Undecodable);
        assert!(
            error.message().contains("can decode"),
            "and the message says what the engine cannot do, because \"the font \
             did not load\" is not something an author can act on: {}",
            error.message()
        );
        assert!(
            !document.has_resource(&url),
            "so nothing registers it, and §5.2 treats the face as not present in \
             the family"
        );
    }

    /// A URL with a fragment is a different URL, because §4.3.2 says a collection
    /// fragment names "the PostScript name of the font" - so one file behind two
    /// fragments is two faces, and collapsing them would pick one at random.
    #[test]
    fn two_fragments_of_one_file_are_two_addresses() {
        let document = DocumentFonts::new();
        for fragment in ["#Regular", "#Bold"] {
            let url = Url::parse(&format!("https://cdn.test/assets/c.ttc{fragment}"))
                .expect("a parseable URL");
            assert!(
                !document.has_resource(&url),
                "{fragment} is its own address until something is installed for it"
            );
        }
    }

    /// §4.1: a downloaded face is available to the document that declared it, so
    /// the store is what the document owns and a new document starts empty.
    #[test]
    fn a_new_document_starts_with_nothing() {
        let document = DocumentFonts::new();
        assert!(document.is_empty());
        assert_eq!(document.rule_count(), 0);
        assert!(
            document
                .plan_fetches(["Anything"], &BTreeSet::from(['A']))
                .is_empty()
        );
    }

    /// A rule's descriptors survive into the store, because the matcher selects on
    /// them and a store that dropped them would be a registration nothing can
    /// select.
    #[test]
    fn a_stored_rule_carries_the_descriptors_the_matcher_selects_on() {
        let document = store_of(
            "@font-face { font-family: Sel; font-weight: 700; font-style: oblique 20deg; \
             src: url(s.ttf) format(truetype); unicode-range: U+4E00-9FFF }",
        );
        let rule = &document.families()[0].rules[0];
        assert_eq!(
            rule.weight, 700,
            "§5.2's weight search needs the descriptor"
        );
        assert_eq!(
            rule.style,
            FontStyle::Oblique(20.0),
            "and so does the style search"
        );
        assert!(
            rule.range.contains('\u{4e2d}'),
            "and §4.5's range decides which characters the face may serve"
        );
        assert!(
            !rule.range.contains('A'),
            "so the range is not §4.5's initial value of U+0-10FFFF"
        );
    }
}
