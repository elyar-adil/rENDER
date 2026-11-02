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
//! # What a face looks like before it has arrived, and for how long
//!
//! Nothing. §5.2: "If the font resources defined for a given face in an
//! `@font-face` rule are either not available or contain invalid font data, then
//! the face should be treated as not present in the family." This module
//! therefore records a rule whether or not its resource has arrived, and the
//! face the matcher sees is a question asked at table-build time rather than a
//! state the rule carries. That is the whole of the "not yet arrived" answer, and
//! it is consistent between measurement and painting because there is nothing
//! per-face for the two to disagree about: both read the one table.
//!
//! How long "not yet" can last is §3.2's font download timeline, and this module
//! is its clock. [`DocumentFontLoader::begin_document`] starts a monotonic
//! timeline for each navigation; each URL records when its stylesheet first
//! entered this document's render (the first full render that installs it); and
//! `DocumentFonts::apply_display_policy` compares the arrival time recorded by
//! [`DocumentFontLoader::install_fetched`] against the block+swap boundary
//! [`render_core::font_face::FontDisplay::timing`] gives. A `block` or `swap`
//! face (and `auto`, which this engine maps to `block`) has an infinite swap
//! period and may swap in whenever it arrives. A `fallback` face may swap for
//! the first 3.1s after it is needed; an `optional` face only for the first
//! 100ms. After the failure period starts the URL is marked expired on the rule:
//! the bytes stay cached for another rule that can still use them, but this rule
//! is permanently on §5.2's fallback path. Nothing sleeps - a render asks the
//! clock where it is, and an arriving body is what schedules the next render
//! (the browser already re-renders when `finish_font_fetches` installs bytes).
//!
//! During the block period the engine paints the fallback visibly and measures
//! it, rather than §3.2's invisible fallback. That is the UA policy documented on
//! `render_core::font_face`: a single measure/paint pass cannot hide ink without
//! a second face table, and §4.8.1 requires unavailable fonts to leave text
//! visible. The periods still do their real work, which is deciding how long a
//! late face may still replace that fallback.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::fs;
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use fontdue::{Font, FontSettings};
use render_core::document::FontFaceInstaller;
use render_core::dom::{Dom, NodeKind};
use render_core::font_face::{
    FontDisplay, FontFaceRule, FontFaceSheet, FontFaceSource, FontSource, UnicodeRangeSet,
};
use render_core::layout::FontStyle;
use render_net::{FetchRequest, FetchResult, Url};

use crate::font_backend::SystemFontBackend;

/// One `@font-face` rule, with every URL already resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentRule {
    /// §4.4's `font-weight` descriptor.
    pub weight: u16,
    /// §4.4's `font-style` descriptor.
    pub style: FontStyle,
    /// §4.5's `unicode-range` descriptor.
    pub range: UnicodeRangeSet,
    /// §4.9's `font-display`, so the loader can apply §3.2's periods.
    pub display: FontDisplay,
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
    Url {
        url: Url,
        /// §3.2's failure period has started and this face was not loaded, so
        /// the URL may never supply this rule again. The bytes stay cached (a
        /// second rule with `swap` may still use them), but this rule's table
        /// entry is permanently absent and §5.2 falls through to its family
        /// fallback.
        expired: bool,
    },
}

impl ResolvedSource {
    /// The absolute URL this item names, when it is a URL item.
    #[must_use]
    pub fn url(&self) -> Option<&Url> {
        match self {
            Self::Local(_) => None,
            Self::Url { url, .. } => Some(url),
        }
    }

    /// Whether the failure period has made this URL permanently unusable for
    /// its rule.
    #[must_use]
    pub const fn is_expired(&self) -> bool {
        match self {
            Self::Local(_) => false,
            Self::Url { expired, .. } => *expired,
        }
    }
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

/// The `Accept` value for a font request. It names every container this engine
/// can decode and leaves a server that serves an unlisted type a last-resort
/// `*/*` so a wrong hint is still a decodable body rather than a refused
/// request.
pub const FONT_ACCEPT: &str = "font/ttf,font/otf,font/woff2,font/woff,application/font-woff,application/octet-stream,*/*;q=0.1";

/// The network work one render still needs for its webfonts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFetchPlan {
    pub urls: Vec<Url>,
}

impl FontFetchPlan {
    #[must_use]
    pub fn new(urls: Vec<Url>) -> Self {
        let mut seen = BTreeSet::new();
        Self {
            urls: urls
                .into_iter()
                .filter(|url| seen.insert(url.clone()))
                .collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }

    #[must_use]
    pub fn requests(&self) -> Vec<FetchRequest> {
        self.urls
            .iter()
            .map(|url| FetchRequest::get(url.clone()).with_accept(FONT_ACCEPT))
            .collect()
    }
}

/// A fetch plan's successful bodies, plus the URLs that did not produce one.
#[derive(Clone, Debug, Default)]
pub struct FontResourceBatch {
    pub resources: Vec<(Url, Vec<u8>)>,
    pub failures: Vec<FontResourceFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontResourceFailure {
    pub url: Url,
    pub message: String,
}

/// Pairs a plan's URLs with the batch results, preserving request order.
///
/// A successful body is kept whatever its content type: a server that labels a
/// font `application/octet-stream` (the common case) is still serving bytes the
/// decoder can identify from the magic number. The decoder, not the header,
/// decides whether the body is a face.
#[must_use]
pub fn apply_font_batch(plan: &FontFetchPlan, results: Vec<FetchResult>) -> FontResourceBatch {
    let mut batch = FontResourceBatch::default();
    let mut results = results.into_iter();
    for url in &plan.urls {
        match results.next() {
            Some(Ok(response)) if response.status.is_success() => {
                batch.resources.push((url.clone(), response.body));
            }
            Some(Ok(response)) => batch.failures.push(FontResourceFailure {
                url: url.clone(),
                message: format!(
                    "font server returned HTTP status {}",
                    response.status.as_u16()
                ),
            }),
            Some(Err(error)) => batch.failures.push(FontResourceFailure {
                url: url.clone(),
                message: format!("font transfer failed: {error}"),
            }),
            None => batch.failures.push(FontResourceFailure {
                url: url.clone(),
                message: "font batch ended without a result".to_owned(),
            }),
        }
    }
    batch
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
        self.add_rules_at(rules, Some(base));
    }

    /// Adds rules whose relative `src` URLs resolve against `base`.
    ///
    /// `None` still accepts absolute URLs, but drops relative ones: §4.3.2
    /// resolves against "the location of the style sheet", and a document with
    /// no URL has no such location. Inventing one would request a font from a
    /// place the author never named.
    pub fn add_rules_at(&mut self, rules: &[FontFaceRule], base: Option<&Url>) {
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
                        display: rule.display,
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
                if let Some(url) = source.url()
                    && !source.is_expired()
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
        let font = load_font_bytes(bytes)?;
        self.resources.insert(url.clone(), font);
        Ok(())
    }

    /// Registers an already-decoded face under `url`.
    ///
    /// A re-render rebuilds the rule table from the current stylesheets but
    /// must not decode the same file again; this carries a previously decoded
    /// `Arc<Font>` across that rebuild.
    pub fn insert_resource(&mut self, url: Url, font: Arc<Font>) {
        self.resources.insert(url, font);
    }

    /// Applies §3.2's font download timeline to every URL source.
    ///
    /// `started_at` is the document's timeline origin, `url_started` the first
    /// instant each URL was needed, and `arrivals` the instant each resource was
    /// decoded. A rule with a finite swap period is permanently expired when
    /// its failure period begins without the resource having arrived; an
    /// infinite swap period (`block`, `swap`, `auto`) never expires. A resource
    /// decoded before the document started counts as available immediately,
    /// which is what makes the cross-document decoded-resource cache safe.
    ///
    /// Returns the earliest future failure-period boundary among rules that are
    /// still waiting, so the loader can tell whether a rebuild is necessary
    /// without rebuilding the table on every scroll frame.
    fn apply_display_policy(
        &mut self,
        now: Duration,
        started_at: Duration,
        url_started: &HashMap<Url, Duration>,
        arrivals: &HashMap<Url, Duration>,
    ) -> Option<Duration> {
        let mut next_expiry: Option<Duration> = None;
        for family in &mut self.families {
            for rule in &mut family.rules {
                for source in &mut rule.sources {
                    let ResolvedSource::Url { url, expired } = source else {
                        continue;
                    };
                    let Some(failure) = rule.display.timing().failure_period_start() else {
                        // An infinite swap period: a late arrival is always
                        // allowed, so there is no boundary to schedule.
                        continue;
                    };
                    let start = url_started.get(url).copied().unwrap_or(started_at);
                    let deadline = start + failure;
                    let arrived_in_time =
                        arrivals.get(url).is_some_and(|arrival| *arrival < deadline)
                            || (self.resources.contains_key(url) && !arrivals.contains_key(url));
                    if arrived_in_time {
                        continue;
                    }
                    if now >= deadline {
                        *expired = true;
                    } else {
                        next_expiry = Some(next_expiry.map_or(deadline, |next| next.min(deadline)));
                    }
                }
            }
        }
        next_expiry
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
                    if let Some(url) = source.url()
                        && !source.is_expired()
                        && !self.resources.contains_key(url)
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
fn resolve_sources(sources: &[FontSource], base: Option<&Url>) -> Vec<ResolvedSource> {
    sources
        .iter()
        .filter_map(|source| match source {
            FontSource::Local { name } => Some(ResolvedSource::Local(name.clone())),
            FontSource::Url { url, .. } => Some(ResolvedSource::Url {
                url: resolve_url(url, base)?,
                expired: false,
            }),
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
fn resolve_url(url: &str, base: Option<&Url>) -> Option<Url> {
    if let Some(rest) = url.strip_prefix("//") {
        let scheme = base?.scheme();
        return Url::parse(&format!("{scheme}://{rest}")).ok();
    }
    if let Ok(absolute) = Url::parse(url) {
        return Some(absolute);
    }
    base?.join(url).ok()
}

#[derive(Debug, Default)]
struct DocumentFontState {
    document: DocumentFonts,
    sources: Vec<FontFaceSource>,
    /// Decoded faces by URL, independent of the current rule set. A fetch can
    /// arrive before the stylesheet that names it is re-rendered, and a rule
    /// rebuild must not decode a 10 MB face twice.
    resources: HashMap<Url, Arc<Font>>,
    /// When each decoded resource arrived on the loader's clock. Absolute rather
    /// than document-relative because the resource cache outlives a document:
    /// an arrival before the current document's origin is "available
    /// immediately".
    resource_arrivals: HashMap<Url, Duration>,
    /// The navigation this timeline belongs to. `None` starts the timeline at
    /// the first install, which is the useful fallback for a direct caller.
    document_key: Option<(u64, u64)>,
    /// The document timeline's zero, on the loader's clock.
    started_at: Duration,
    /// When each URL first entered the current document's face set, which is
    /// the first full render that installs its stylesheet. A stylesheet added
    /// later starts its faces' timers then, not at document load.
    url_started: HashMap<Url, Duration>,
    /// The earliest failure-period boundary still pending, so an unchanged rule
    /// set does not rebuild the backend table until a `fallback`/`optional`
    /// face actually expires.
    next_expiry: Option<Duration>,
    /// Set when `install_fetched` added a resource, so the next `install`
    /// rebuilds the backend table even though the rule set itself did not
    /// change.
    dirty: bool,
    /// The document characters for `character_revision`, so the §4.8.1
    /// `unicode-range` hint does not walk the DOM on every frame.
    character_revision: Option<(u64, u64, u64)>,
    characters: Arc<BTreeSet<char>>,
}

/// A monotonic clock for §3.2's font download timer.
///
/// The loader never sleeps and never blocks a render thread waiting for a
/// period to pass: fonts arrive when they arrive, and a render asks the clock
/// where on the timeline it is. Tests substitute a manual clock to pin the
/// block/swap/failure boundaries exactly.
pub trait FontClock: Send + Sync + fmt::Debug {
    /// Time elapsed since an arbitrary fixed origin. Only differences are used.
    fn now(&self) -> Duration;
}

/// The production clock: a process-lifetime monotonic instant.
#[derive(Debug)]
pub struct MonotonicFontClock {
    start: Instant,
}

impl MonotonicFontClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for MonotonicFontClock {
    fn default() -> Self {
        Self::new()
    }
}

impl FontClock for MonotonicFontClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Loads each document's `@font-face` resources and publishes the resulting
/// family table to a [`SystemFontBackend`].
///
/// The loader owns the previous document's [`DocumentFonts`] so a re-render can
/// rebuild the rule table from the current DOM while reusing decoded `Arc<Font>`
/// values. That is the document-scoped cache described in this module's header:
/// the rules are replaced whenever the document changes, but a file that is
/// still named is not decoded twice. An unchanged rule set does not rebuild the
/// backend table at all, which keeps the font-instance generation stable across
/// scroll and animation frames.
#[derive(Debug)]
pub struct DocumentFontLoader {
    state: Mutex<DocumentFontState>,
    clock: Arc<dyn FontClock>,
}

impl Default for DocumentFontLoader {
    fn default() -> Self {
        Self {
            state: Mutex::new(DocumentFontState::default()),
            clock: Arc::new(MonotonicFontClock::new()),
        }
    }
}

impl DocumentFontLoader {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A loader whose timeline is read from `clock`.
    ///
    /// The clock is the document's font download timer. Production passes
    /// [`MonotonicFontClock`]; tests pass a manual clock and call
    /// [`Self::install`] at chosen instants, which is how the §3.2 boundaries are
    /// pinned without sleeping.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn FontClock>) -> Self {
        Self {
            state: Mutex::new(DocumentFontState::default()),
            clock,
        }
    }

    /// Starts a new document timeline at the current clock reading.
    ///
    /// The browser calls this for every full render with `(tab_id,
    /// document_serial)`, so a navigation starts its webfont timers at zero
    /// rather than inheriting the previous document's elapsed time. Repeating
    /// the same key is a no-op, which keeps a re-render inside one document on
    /// one timeline. The decoded-resource cache is deliberately not cleared:
    /// §4.1 says font caching behaves as for any other web resource, and a
    /// resource that predates the document counts as available immediately.
    pub fn begin_document(&self, key: (u64, u64)) {
        let now = self.clock.now();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.document_key == Some(key) {
            return;
        }
        state.document_key = Some(key);
        state.started_at = now;
        state.url_started.clear();
        state.next_expiry = None;
        // Force the next `install` to rebuild even when the two documents name
        // exactly the same sources: their timelines differ by construction.
        state.sources.clear();
        state.document = DocumentFonts::new();
        state.character_revision = None;
    }

    /// Installs `sources` into `backend`, loading `file:` and base64 `data:`
    /// resources synchronously. Network resources are left outstanding: the
    /// browser fetches them and calls [`Self::install_fetched`] before the next
    /// render, so §5.2's "not present" rule applies only until the bytes arrive,
    /// and §3.2's periods decide whether there is still time to swap.
    pub fn install(&self, backend: &SystemFontBackend, sources: &[FontFaceSource]) {
        let now = self.clock.now();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.document_key.is_none() {
            // A direct caller that never called `begin_document`: the first
            // install is the moment the document first attempts to use a face,
            // which is exactly where §3.2 starts the timer.
            state.started_at = now;
            state.document_key = Some((0, 0));
        }
        let local_resource_pending = state
            .document
            .outstanding_urls()
            .iter()
            .any(|url| matches!(url.scheme(), "file" | "data"));
        let before_expiry = state.next_expiry.is_some_and(|boundary| now >= boundary);
        if state.sources == sources && !local_resource_pending && !state.dirty && !before_expiry {
            return;
        }
        let mut next = DocumentFonts::new();
        for source in sources {
            for rule in &source.rules {
                for item in &rule.sources {
                    if let FontSource::Url { url, .. } = item
                        && let Some(resolved) = resolve_url(url, source.base_url.as_ref())
                    {
                        state.url_started.entry(resolved).or_insert(now);
                    }
                }
            }
            next.add_rules_at(&source.rules, source.base_url.as_ref());
        }
        let mut decoded = Vec::new();
        for url in next.outstanding_urls() {
            let font = state
                .resources
                .get(&url)
                .cloned()
                .or_else(|| state.document.resource(&url))
                .or_else(|| {
                    local_resource_bytes(&url).and_then(|bytes| load_font_bytes(&bytes).ok())
                });
            if let Some(font) = font {
                next.insert_resource(url.clone(), Arc::clone(&font));
                state.resource_arrivals.entry(url.clone()).or_insert(now);
                decoded.push((url, font));
            }
        }
        for (url, font) in decoded {
            state.resources.insert(url, font);
        }
        state.next_expiry = next.apply_display_policy(
            now,
            state.started_at,
            &state.url_started,
            &state.resource_arrivals,
        );
        backend.set_document_faces(&next);
        state.sources = sources.to_vec();
        state.document = next;
        state.dirty = false;
    }

    /// Registers bytes the browser fetched for `url`.
    ///
    /// WOFF and WOFF2 are decoded here, so the same path serves a network face
    /// and a local one. A body that cannot be decoded is skipped; §5.2 then
    /// treats the face as not present, which is the same result as a failed
    /// transfer.
    pub fn install_fetched(&self, resources: &[(Url, Vec<u8>)]) {
        if resources.is_empty() {
            return;
        }
        let now = self.clock.now();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for (url, bytes) in resources {
            if state.resources.contains_key(url) {
                continue;
            }
            if let Ok(font) = load_font_bytes(bytes) {
                state.resources.insert(url.clone(), font);
                state.resource_arrivals.entry(url.clone()).or_insert(now);
                state.dirty = true;
            }
        }
    }

    /// [`Self::install_fetched`] for the render worker's byte map. Returns the
    /// URLs that were newly decoded, so the app can drop only the raw bodies
    /// the worker actually consumed.
    pub fn install_fetched_map(&self, resources: &HashMap<Url, Arc<[u8]>>) -> Vec<Url> {
        let mut installed = Vec::new();
        if resources.is_empty() {
            return installed;
        }
        let now = self.clock.now();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for (url, bytes) in resources {
            if state.resources.contains_key(url) {
                continue;
            }
            if let Ok(font) = load_font_bytes(bytes) {
                state.resources.insert(url.clone(), font);
                state.resource_arrivals.entry(url.clone()).or_insert(now);
                state.dirty = true;
                installed.push(url.clone());
            }
        }
        installed
    }

    /// The HTTP(S) URLs a render still needs, narrowed by §4.8.1 to families
    /// the page's computed styles name and characters the page contains.
    ///
    /// `file:` and `data:` resources were already resolved by [`Self::install`]
    /// and are not network work; URLs whose bytes are cached are skipped.
    #[must_use]
    pub fn plan_fetches<'a>(
        &self,
        used_families: impl IntoIterator<Item = &'a str>,
        used_characters: &BTreeSet<char>,
    ) -> Vec<Url> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .document
            .plan_fetches(used_families, used_characters)
            .into_iter()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .filter(|url| !state.resources.contains_key(url))
            .collect()
    }

    /// Whether the last render declared any `@font-face` rule, so the caller
    /// can skip walking the DOM for characters on the common font-free page.
    #[must_use]
    pub fn has_rules(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.document.rule_count() > 0 || !state.sources.is_empty()
    }

    /// Whether any HTTP(S) URL the last render declared still lacks bytes.
    ///
    /// This is the cheap guard before walking the DOM for characters: once
    /// every face has arrived, the plan is empty and the walk would be wasted.
    #[must_use]
    pub fn has_pending_network(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.document.outstanding_urls().iter().any(|url| {
            matches!(url.scheme(), "http" | "https") && !state.resources.contains_key(url)
        })
    }

    /// Every character the document can render, cached by DOM revision.
    ///
    /// §4.5 uses `unicode-range` as a download hint per character, so a
    /// multi-slice CJK face needs the page's character set. Walking every text
    /// node each frame would be wasted work; the revision is the same key the
    /// rest of the browser uses for a cached document snapshot.
    #[must_use]
    pub fn document_characters(&self, document: (u64, u64), dom: &Dom) -> Arc<BTreeSet<char>> {
        let key = (document.0, document.1, dom.revision().as_u64());
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.character_revision == Some(key) {
            return Arc::clone(&state.characters);
        }
        let mut characters = BTreeSet::new();
        let mut stack = vec![dom.document()];
        while let Some(node_id) = stack.pop() {
            let Some(node) = dom.node(node_id) else {
                continue;
            };
            if let NodeKind::Text(text) = node.kind() {
                characters.extend(text.chars());
            }
            stack.extend(node.children().iter().rev().copied());
            if let Some(root) = dom.shadow_root(node_id) {
                stack.push(root);
            }
        }
        state.character_revision = Some(key);
        state.characters = Arc::new(characters);
        Arc::clone(&state.characters)
    }
}

/// The adapter the render path hands to [`render_core::document::DocumentBackends`].
pub struct SystemFontFaceInstaller<'a> {
    pub backend: &'a SystemFontBackend,
    pub loader: &'a DocumentFontLoader,
}

impl FontFaceInstaller for SystemFontFaceInstaller<'_> {
    fn install_font_faces(&self, sources: &[FontFaceSource]) {
        self.loader.install(self.backend, sources);
    }
}

/// Decodes one font body and builds the rasteriser's face.
///
/// Raw sfnt is passed through; WOFF is inflated into sfnt here, and WOFF2 goes
/// through the `woff2` crate's Brotli decoder. The rasteriser itself only reads
/// sfnt, so this is the single seam that turns every webfont container into the
/// bytes it can consume.
fn load_font_bytes(bytes: &[u8]) -> Result<Arc<Font>, FontLoadFailure> {
    let decoded = decode_font_container(bytes).ok_or(FontLoadFailure::Undecodable)?;
    let settings = FontSettings {
        collection_index: 0,
        // The engine rasterizes one scalar at a time and never asks fontdue
        // for GSUB-indexed glyphs, so eagerly loading every substitution
        // outline only costs startup time and resident memory. A large CJK
        // collection makes that cost hundreds of megabytes.
        load_substitutions: false,
        ..FontSettings::default()
    };
    Font::from_bytes(decoded, settings)
        .map(Arc::new)
        .map_err(|_| FontLoadFailure::Undecodable)
}

/// Turns a raw sfnt, WOFF or WOFF2 body into the sfnt the rasteriser reads.
///
/// A body that is already sfnt is returned as-is. Anything else is rejected, so
/// a WOFF2 that the decoder does not support (for example one using the
/// unimplemented `hmtx` transform) has the same §5.2 outcome as a corrupt file:
/// the face is not present in its family.
fn decode_font_container(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.starts_with(b"wOF2") {
        return woff2::decode::convert_woff2_to_ttf(&mut std::io::Cursor::new(bytes)).ok();
    }
    if bytes.starts_with(b"wOFF") {
        return decode_woff1(bytes);
    }
    if bytes.starts_with(&[0, 1, 0, 0])
        || bytes.starts_with(b"OTTO")
        || bytes.starts_with(b"true")
        || bytes.starts_with(b"typ1")
    {
        return Some(bytes.to_vec());
    }
    None
}

/// WOFF 1.0: the header and table directory are uncompressed, each table is
/// either zlib-deflated or stored raw, and the output is the ordinary sfnt the
/// entries describe.
///
/// WOFF 1.0 is a container, not a different font format: a compliant decoder
/// reconstructs the original table bytes and the sfnt header. Returning the
/// reconstructed sfnt is what lets one `decode_font_container` feed both the
/// direct-download and the stored-local cases.
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "WOFF's directory bounds tables and counts to u16/u32; the output size is checked against those fields"
)]
fn decode_woff1(bytes: &[u8]) -> Option<Vec<u8>> {
    const HEADER: usize = 44;
    if bytes.len() < HEADER || !bytes.starts_with(b"wOFF") {
        return None;
    }
    let u16_at = |offset: usize| u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
    let u32_at = |offset: usize| {
        u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize
    };
    let flavor = &bytes[4..8];
    let num_tables = u16_at(12);
    if num_tables == 0 || HEADER + num_tables.saturating_mul(20) > bytes.len() {
        return None;
    }

    // tag, original checksum, table bytes. `tag` is kept as the four wire bytes
    // so sorting is the byte order the sfnt directory requires.
    let mut tables: Vec<([u8; 4], u32, Vec<u8>)> = Vec::with_capacity(num_tables);
    for index in 0..num_tables {
        let entry = HEADER + index * 20;
        let tag = [
            bytes[entry],
            bytes[entry + 1],
            bytes[entry + 2],
            bytes[entry + 3],
        ];
        let offset = u32_at(entry + 4);
        let compressed_len = u32_at(entry + 8);
        let original_len = u32_at(entry + 12);
        let checksum = u32::from_be_bytes([
            bytes[entry + 16],
            bytes[entry + 17],
            bytes[entry + 18],
            bytes[entry + 19],
        ]);
        if compressed_len > original_len
            || offset
                .checked_add(compressed_len)
                .is_none_or(|end| end > bytes.len())
        {
            return None;
        }
        let body = &bytes[offset..offset + compressed_len];
        let table = if compressed_len < original_len {
            let mut decoded = Vec::with_capacity(original_len);
            flate2::read::ZlibDecoder::new(body)
                .read_to_end(&mut decoded)
                .ok()?;
            if decoded.len() != original_len {
                return None;
            }
            decoded
        } else {
            body.to_vec()
        };
        tables.push((tag, checksum, table));
    }
    tables.sort_by_key(|(tag, _, _)| *tag);

    let entry_selector = (usize::BITS - 1 - num_tables.leading_zeros()) as usize;
    let search_range = (1_usize << entry_selector) * 16;
    let range_shift = num_tables * 16 - search_range;
    let table_bytes = tables
        .iter()
        .map(|(_, _, table)| table.len().saturating_add(3) & !3)
        .sum::<usize>();
    let total = 12 + num_tables * 16 + table_bytes;
    let mut output = Vec::with_capacity(total);
    output.extend_from_slice(flavor);
    output.extend_from_slice(&(num_tables as u16).to_be_bytes());
    output.extend_from_slice(&(search_range as u16).to_be_bytes());
    output.extend_from_slice(&(entry_selector as u16).to_be_bytes());
    output.extend_from_slice(&(range_shift as u16).to_be_bytes());

    let mut offset = 12 + num_tables * 16;
    let mut head_offset = None;
    for (tag, checksum, table) in &tables {
        output.extend_from_slice(tag);
        output.extend_from_slice(&checksum.to_be_bytes());
        output.extend_from_slice(&(offset as u32).to_be_bytes());
        output.extend_from_slice(&(table.len() as u32).to_be_bytes());
        if tag == b"head" {
            head_offset = Some(offset);
        }
        offset += table.len().saturating_add(3) & !3;
    }
    for (_, _, table) in &tables {
        output.extend_from_slice(table);
        while output.len() % 4 != 0 {
            output.push(0);
        }
    }

    // sfnt's `head.checkSumAdjustment` is the value that makes the whole file
    // sum to 0xB1B0AFBA. WOFF leaves the field zero, so reconstruct it after the
    // tables are in place.
    if let Some(head) = head_offset
        && head + 12 <= output.len()
    {
        output[head + 8..head + 12].fill(0);
        let mut sum = 0_u32;
        for chunk in output.chunks(4) {
            let mut word = [0_u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            sum = sum.wrapping_add(u32::from_be_bytes(word));
        }
        let adjustment = 0xB1B0_AFBA_u32.wrapping_sub(sum);
        output[head + 8..head + 12].copy_from_slice(&adjustment.to_be_bytes());
    }
    Some(output)
}

/// Reads a font resource this process can obtain without a network round trip.
///
/// `file:` is the scheme a local page and the layout tests use. `data:` covers
/// inline faces; base64 is the only encoding accepted because the percent-
/// encoded form would need a decoder for a case that is vanishingly rare in
/// real webfont CSS.
fn local_resource_bytes(url: &Url) -> Option<Vec<u8>> {
    match url.scheme() {
        "file" => fs::read(url.to_file_path().ok()?).ok(),
        "data" => {
            let rest = url.as_str().strip_prefix("data:")?;
            let (metadata, payload) = rest.split_once(',')?;
            if !metadata
                .split(';')
                .any(|part| part.eq_ignore_ascii_case("base64"))
            {
                return None;
            }
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        reason = "fixture advances are exact integer font units scaled by the font size"
    )]

    use std::collections::BTreeSet;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use render_core::font_face::{FontDisplay, FontFaceSource, parse_font_faces};
    use render_core::layout::{FontRequest, FontStyle, TextMeasurer, TextStyle};
    use render_net::{BatchOptions, CancelToken, FetchConfig, HttpTransport};

    use super::{
        DocumentFontLoader, DocumentFonts, FontClock, FontFetchPlan, FontLoadFailure,
        ResolvedSource, Url, apply_font_batch, decode_font_container,
    };
    use crate::font_backend::SystemFontBackend;

    fn base() -> Url {
        Url::parse("https://cdn.test/assets/main.css").expect("a parseable base URL")
    }

    fn store_of(source: &str) -> DocumentFonts {
        let (sheet, _problems) = parse_font_faces(source);
        let mut document = DocumentFonts::new();
        document.add_stylesheet(&sheet, &base());
        document
    }

    /// A [`FontClock`] whose reading a test sets directly, so §3.2's period
    /// boundaries can be pinned without sleeping on the render thread.
    #[derive(Debug, Default)]
    struct ManualClock {
        millis: AtomicU64,
    }

    impl ManualClock {
        fn set(&self, millis: u64) {
            self.millis.store(millis, Ordering::SeqCst);
        }
    }

    impl FontClock for ManualClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.millis.load(Ordering::SeqCst))
        }
    }

    const TIMED_FAMILY: &str = "Render Test Webfont";

    /// One document face on a loopback URL, with `font-display` chosen by the
    /// test. The fixture's `H` is one em, so `HHH` is 48px at 16px when the face
    /// is in the table and the platform fallback's own advance otherwise.
    fn timed_sources(display: FontDisplay, url: &Url) -> Vec<FontFaceSource> {
        let source = format!(
            "@font-face {{ font-family: '{TIMED_FAMILY}'; src: url({url}) format(woff2); \
             font-display: {} }}",
            display.keyword()
        );
        let (sheet, problems) = parse_font_faces(&source);
        assert!(problems.is_empty(), "{problems:?}");
        vec![FontFaceSource::new(Some(base()), sheet.rules().to_vec())]
    }

    fn timed_advance(backend: &SystemFontBackend) -> f32 {
        backend
            .measure(
                "HHH",
                TextStyle {
                    font_size: 16.0,
                    line_height: 19.2,
                    font: FontRequest::of_family(TIMED_FAMILY),
                },
            )
            .advance
    }

    fn timed_url() -> Url {
        Url::parse("https://cdn.test/assets/timed.woff2").expect("a parseable font URL")
    }

    /// Runs one face through a loader with a manual clock and returns
    /// `(fallback_advance, advance after the bytes arrive at
    /// arrival_millis)`. The fixture's face is one em, so an accepted arrival
    /// measures 48px; a rejected one keeps the platform fallback's advance.
    fn measure_at_arrival(display: FontDisplay, arrival_millis: u64) -> (f32, f32) {
        let backend = SystemFontBackend::load().expect("the platform table loads");
        // Before the document face exists the request falls through to the
        // platform fallback, which is the advance a missing family keeps.
        let fallback = timed_advance(&backend);
        let clock = Arc::new(ManualClock::default());
        let loader = DocumentFontLoader::with_clock(clock.clone());
        loader.begin_document((7, 1));
        let url = timed_url();
        let sources = timed_sources(display, &url);
        loader.install(&backend, &sources);
        assert_eq!(
            timed_advance(&backend),
            fallback,
            "before the bytes arrive the fallback face is measured"
        );
        clock.set(arrival_millis);
        loader.install_fetched(&[(url, fixture_bytes("woff2"))]);
        loader.install(&backend, &sources);
        (fallback, timed_advance(&backend))
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
                    .filter_map(|source| source.url().map(Url::to_string))
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
            [
                "https://s1.hdslb.com/bfs/a.woff2",
                "https://s1.hdslb.com/bfs/b.ttf"
            ],
            "both WOFF2 and raw sfnt are containers this engine decodes, so \
             both items are sources"
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
                ResolvedSource::Url {
                    url: "https://cdn.test/assets/g.ttf".parse().expect("a URL"),
                    expired: false,
                },
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
             src: url(s.ttf) format(truetype); unicode-range: U+4E00-9FFF; \
             font-display: fallback }",
        );
        let rule = &document.families()[0].rules[0];
        assert_eq!(
            rule.display,
            FontDisplay::Fallback,
            "§4.9's value reaches the loader that runs its §3.2 periods"
        );
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

    /// The production loader's local-file path, through the real platform
    /// backend: a rule whose `src` is a relative `file:` URL changes the
    /// advance the measurer returns.
    #[test]
    fn a_local_file_webfont_changes_the_platform_measurers_advance() {
        let backend = SystemFontBackend::load().expect("the platform table loads");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../render-core/tests/fixtures/index.html");
        let base = Url::from_file_path(fixture).expect("the fixture has a file URL");
        let (sheet, problems) = parse_font_faces(
            "@font-face { font-family: 'Render Test Webfont'; \
             src: url(render-test-webfont.ttf) format(truetype) }",
        );
        assert!(problems.is_empty(), "{problems:?}");
        let sources = vec![FontFaceSource::new(Some(base), sheet.rules().to_vec())];

        let request = FontRequest::of_family("Render Test Webfont");
        let style = TextStyle {
            font_size: 16.0,
            line_height: 19.2,
            font: request,
        };
        let before = backend.measure("HHH", style).advance;
        assert_ne!(before, 48.0, "no document face has been installed yet");

        let loader = DocumentFontLoader::new();
        loader.install(&backend, &sources);

        let after = backend.measure("HHH", style).advance;
        assert_eq!(
            after, 48.0,
            "the fixture's H advances one em, so three at 16px are 48px"
        );
        assert_ne!(before, after, "the local face really replaced the fallback");
    }

    fn fixture_bytes(extension: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../render-core/tests/fixtures")
            .join(format!("render-test-webfont.{extension}"));
        std::fs::read(path).expect("the fixture is readable")
    }

    /// The three containers are the same face: a raw sfnt, WOFF 1.0 and WOFF 2.0
    /// all decode to an sfnt whose `H` still advances one em.
    #[test]
    fn woff_and_woff2_decode_to_the_fixture_face() {
        for extension in ["ttf", "woff", "woff2"] {
            let bytes = fixture_bytes(extension);
            let decoded =
                decode_font_container(&bytes).unwrap_or_else(|| panic!("{extension} decodes"));
            assert!(
                decoded.starts_with(&[0, 1, 0, 0]) || decoded.starts_with(b"OTTO"),
                "{extension} decodes to an sfnt container"
            );
            let font = fontdue::Font::from_bytes(decoded, fontdue::FontSettings::default())
                .unwrap_or_else(|_| panic!("{extension} decodes to a readable face"));
            assert_eq!(
                font.metrics('H', 16.0).advance_width,
                16.0,
                "{extension} preserves the fixture's one-em advance"
            );
        }
    }

    /// A network body follows the same path as a local file: the bytes arrive
    /// as WOFF2, decode once, and the platform measurer reports the face's own
    /// advance before the next layout.
    #[test]
    fn a_fetched_woff2_body_changes_the_platform_measurers_advance() {
        let backend = SystemFontBackend::load().expect("the platform table loads");
        let base = Url::parse("https://example.test/page.css").expect("a base URL");
        let url =
            Url::parse("https://example.test/fonts/render-test-webfont.woff2").expect("a font URL");
        let (sheet, problems) = parse_font_faces(
            "@font-face { font-family: 'Render Test Webfont'; \
             src: url(fonts/render-test-webfont.woff2) format(woff2) }",
        );
        assert!(problems.is_empty(), "{problems:?}");
        let sources = vec![FontFaceSource::new(Some(base), sheet.rules().to_vec())];

        let loader = DocumentFontLoader::new();
        assert_eq!(
            loader.plan_fetches(["Render Test Webfont"], &BTreeSet::from(['H'])),
            Vec::<Url>::new(),
            "before the rules are installed there is nothing to fetch"
        );
        loader.install(&backend, &sources);
        assert_eq!(
            loader.plan_fetches(["Render Test Webfont"], &BTreeSet::from(['H'])),
            vec![url.clone()],
            "the installed rule names the HTTP URL a fetch must request"
        );

        loader.install_fetched(&[(url, fixture_bytes("woff2"))]);
        // Force the rules to be published again now that the decoded face is
        // cached.
        loader.install(&backend, &sources);

        let style = TextStyle {
            font_size: 16.0,
            line_height: 19.2,
            font: FontRequest::of_family("Render Test Webfont"),
        };
        assert_eq!(
            backend.measure("HHH", style).advance,
            48.0,
            "the fetched WOFF2's one-em H is the advance layout measures"
        );
    }

    /// The whole network slice: plan an HTTP font, submit the batch, apply the
    /// response, and measure the face that came back.
    #[test]
    fn an_http_fetched_woff2_changes_the_measured_advance() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let address = listener.local_addr().expect("a bound address");
        let body = fixture_bytes("woff2");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("one client");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("a read timeout");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: font/woff2\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            )
            .expect("response headers");
            stream.write_all(&body).expect("response body");
        });

        let base = Url::parse(&format!("http://{address}/page.css")).expect("a base URL");
        let url = Url::parse(&format!("http://{address}/font.woff2")).expect("a font URL");
        let (sheet, problems) = parse_font_faces(
            "@font-face { font-family: 'Render Test Webfont'; \
             src: url(font.woff2) format(woff2) }",
        );
        assert!(problems.is_empty(), "{problems:?}");
        let sources = vec![FontFaceSource::new(Some(base), sheet.rules().to_vec())];
        let backend = SystemFontBackend::load().expect("the platform table loads");
        let loader = DocumentFontLoader::new();
        loader.install(&backend, &sources);

        let plan = FontFetchPlan::new(
            loader.plan_fetches(["Render Test Webfont"], &BTreeSet::from(['H'])),
        );
        assert_eq!(plan.urls, vec![url.clone()]);
        assert_eq!(
            plan.requests()[0].accept.as_deref(),
            Some(super::FONT_ACCEPT)
        );

        let transport = HttpTransport::new(FetchConfig::default());
        let results = transport.fetch_batch(
            plan.requests(),
            &BatchOptions::default(),
            &CancelToken::default(),
        );
        let batch = apply_font_batch(&plan, results);
        assert!(batch.failures.is_empty(), "{:?}", batch.failures);
        assert_eq!(batch.resources.len(), 1);

        loader.install_fetched(&batch.resources);
        loader.install(&backend, &sources);
        let style = TextStyle {
            font_size: 16.0,
            line_height: 19.2,
            font: FontRequest::of_family("Render Test Webfont"),
        };
        assert_eq!(
            backend.measure("HHH", style).advance,
            48.0,
            "the HTTP-fetched WOFF2 is the face layout measures"
        );
        server.join().expect("the test server finishes");
    }

    // ---- §3.2 / §4.9: the font display timeline ---------------------------

    /// `block`: the face gets a 3s block period and then an infinite swap
    /// period. This engine paints fallback metrics during the block period, so
    /// the measurement before the bytes arrive is the reference fallback, and
    /// an arrival after the block period still swaps in because the swap period
    /// never ends.
    #[test]
    fn font_display_block_shows_fallback_then_swaps_even_after_the_block_period() {
        assert_eq!(
            measure_at_arrival(FontDisplay::Block, 1_000).1,
            48.0,
            "an arrival during the block period swaps in"
        );
        assert_eq!(
            measure_at_arrival(FontDisplay::Block, 60_000).1,
            48.0,
            "an infinite swap period means even a very late face still swaps"
        );
    }

    /// `swap`: no block period worth measuring and an infinite swap period, so
    /// the existing "fallback immediately, swap when it arrives" behaviour is
    /// preserved - including for the corpus's dominant value.
    #[test]
    fn font_display_swap_accepts_a_late_face() {
        assert_eq!(measure_at_arrival(FontDisplay::Swap, 4_000).1, 48.0);
    }

    /// `auto` is user-agent-defined; this engine chooses the `block` policy, so
    /// a late face still swaps.
    #[test]
    fn font_display_auto_uses_the_block_policy() {
        assert_eq!(measure_at_arrival(FontDisplay::Auto, 4_000).1, 48.0);
    }

    /// `fallback`: a 100ms block period plus a 3s swap period. An arrival one
    /// millisecond before the failure period swaps; an arrival exactly at the
    /// boundary does not, because §3.2 starts the failure period at that
    /// instant.
    #[test]
    fn font_display_fallback_swaps_inside_the_swap_period_and_not_at_its_end() {
        assert_eq!(
            measure_at_arrival(FontDisplay::Fallback, 3_099).1,
            48.0,
            "inside the swap period the face replaces the fallback"
        );
        let (fallback, at_boundary) = measure_at_arrival(FontDisplay::Fallback, 3_100);
        assert_eq!(
            at_boundary, fallback,
            "at the start of the failure period the fallback is permanent"
        );
        let (fallback, later) = measure_at_arrival(FontDisplay::Fallback, 10_000);
        assert_eq!(
            later, fallback,
            "and it stays permanent for the rest of the document"
        );
    }

    /// `optional`: a zero-length swap period. The face must be available by the
    /// end of the 100ms block period or it is never used, so it can never cause
    /// a late layout jump.
    #[test]
    fn font_display_optional_only_uses_a_face_from_its_initial_period() {
        assert_eq!(
            measure_at_arrival(FontDisplay::Optional, 99).1,
            48.0,
            "available within the initial 100ms period"
        );
        let (fallback, at_boundary) = measure_at_arrival(FontDisplay::Optional, 100);
        assert_eq!(
            at_boundary, fallback,
            "the zero swap period ends at the block-period boundary"
        );
        let (fallback, later) = measure_at_arrival(FontDisplay::Optional, 5_000);
        assert_eq!(later, fallback, "and an optional face never swaps in later");
    }

    /// Once a finite swap period has elapsed, the loader stops planning the
    /// fetch: the bytes are no longer useful for this rule, so §3.2's failure
    /// state is also a scheduling fact and not only a rendering one.
    #[test]
    fn an_expired_pending_face_is_no_longer_planned_for_a_fetch() {
        let backend = SystemFontBackend::load().expect("the platform table loads");
        let fallback = timed_advance(&backend);
        let clock = Arc::new(ManualClock::default());
        let loader = DocumentFontLoader::with_clock(clock.clone());
        loader.begin_document((7, 2));
        let url = timed_url();
        let sources = timed_sources(FontDisplay::Fallback, &url);
        loader.install(&backend, &sources);
        assert_eq!(
            loader.plan_fetches([TIMED_FAMILY], &BTreeSet::from(['H'])),
            vec![url.clone()],
            "before the failure period the face is worth fetching"
        );

        clock.set(3_100);
        loader.install(&backend, &sources);
        assert!(
            loader
                .plan_fetches([TIMED_FAMILY], &BTreeSet::from(['H']))
                .is_empty(),
            "the failure period has started, so the URL is no longer planned"
        );
        assert!(!loader.has_pending_network());
        assert_eq!(
            timed_advance(&backend),
            fallback,
            "the family is still present as a missing family, so §5.2 does not \
             borrow an installed family of the same name"
        );
    }

    /// The HTTP path proves the same timeline end to end: the same late body
    /// swaps under `swap` and is refused under `fallback` once its swap period
    /// has expired. The clock is manual, so this is not a sleep.
    #[test]
    fn a_late_http_face_swaps_under_swap_but_not_after_the_fallback_period() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let address = listener.local_addr().expect("a bound address");
        let body = fixture_bytes("woff2");
        let url = Url::parse(&format!("http://{address}/font.woff2")).expect("a font URL");
        let swap_sources = timed_sources(FontDisplay::Swap, &url);
        let fallback_sources = timed_sources(FontDisplay::Fallback, &url);
        let backend = SystemFontBackend::load().expect("the platform table loads");
        let fallback = timed_advance(&backend);
        let swap_clock = Arc::new(ManualClock::default());
        let fallback_clock = Arc::new(ManualClock::default());
        let swap_loader = DocumentFontLoader::with_clock(swap_clock.clone());
        let fallback_loader = DocumentFontLoader::with_clock(fallback_clock.clone());
        swap_loader.begin_document((8, 1));
        fallback_loader.begin_document((8, 2));
        swap_loader.install(&backend, &swap_sources);
        fallback_loader.install(&backend, &fallback_sources);

        // Start serving only after the (slow) platform table load, so the
        // bounded lifetime below covers the transfers and not the setup.
        let server = std::thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("a non-blocking listener");
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut served = 0;
            while served < 2 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("a blocking client stream");
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .expect("a read timeout");
                        let mut request = [0_u8; 1024];
                        let _ = stream.read(&mut request);
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: font/woff2\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .expect("response headers");
                        stream.write_all(&body).expect("response body");
                        served += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("the test server stopped: {error}"),
                }
            }
        });

        let transport = HttpTransport::new(FetchConfig::default());
        let swap_plan =
            FontFetchPlan::new(swap_loader.plan_fetches([TIMED_FAMILY], &BTreeSet::from(['H'])));
        let fallback_plan = FontFetchPlan::new(
            fallback_loader.plan_fetches([TIMED_FAMILY], &BTreeSet::from(['H'])),
        );
        assert_eq!(swap_plan.urls, vec![url.clone()]);
        assert_eq!(fallback_plan.urls, vec![url.clone()]);

        let swap_batch = apply_font_batch(
            &swap_plan,
            transport.fetch_batch(
                swap_plan.requests(),
                &BatchOptions::default(),
                &CancelToken::default(),
            ),
        );
        let fallback_batch = apply_font_batch(
            &fallback_plan,
            transport.fetch_batch(
                fallback_plan.requests(),
                &BatchOptions::default(),
                &CancelToken::default(),
            ),
        );
        assert!(swap_batch.failures.is_empty(), "{:?}", swap_batch.failures);
        assert!(
            fallback_batch.failures.is_empty(),
            "{:?}",
            fallback_batch.failures
        );

        // Both bodies really arrived over TCP; only the timeline sees them at
        // 4s, which is two seconds past `fallback`'s swap period.
        swap_clock.set(4_000);
        swap_loader.install_fetched(&swap_batch.resources);
        swap_loader.install(&backend, &swap_sources);
        assert_eq!(
            timed_advance(&backend),
            48.0,
            "§4.9 `swap` has an infinite swap period, so the late face is used"
        );

        fallback_clock.set(4_000);
        fallback_loader.install_fetched(&fallback_batch.resources);
        fallback_loader.install(&backend, &fallback_sources);
        assert_eq!(
            timed_advance(&backend),
            fallback,
            "§4.9 `fallback` expired at 3.1s, so the body that arrived at 4s is \
             not used and the fallback is permanent"
        );

        server.join().expect("the test server finishes");
    }
}
