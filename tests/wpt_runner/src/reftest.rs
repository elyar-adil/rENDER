//! The reference-test rules, with no engine in them.
//!
//! A WPT reference test passes or fails on pixels, not on an assertion. That is
//! a different claim from a testharness verdict, and it needs its own rules.
//! This module holds every rule that can be stated without rendering anything:
//! which files are scored and how, how a `link rel` reference is resolved onto a
//! suite path, how the `fuzzy` metadata is read, how two rasters are compared,
//! and how the verdicts for several references combine. [`crate::wptreftest`]
//! does the rendering and calls into here for every judgement.
//!
//! # The rules, as implemented
//!
//! * **`rel=match`** passes when the test's raster equals the reference's raster
//!   within the fuzzy tolerance in force for that reference.
//! * **`rel=mismatch`** passes when the rasters differ beyond that tolerance.
//! * **Several references** follow the WPT reftest documentation: if there are
//!   match references, at least one must match; if there are mismatch references,
//!   all must differ. The two kinds combine with *and*. See [`judge`] for the one
//!   place this differs from the reference implementation.
//! * **Fuzzy tolerance** is WPT's `<meta name=fuzzy>`: a maximum per-channel
//!   difference and a count of differing pixels, each a single value or an
//!   inclusive range, optionally keyed to one reference. The equality test is the
//!   one wptrunner applies, including its zero-minimum shortcuts. See [`within`].
//! * **Crash tests** pass when the page completes without a panic or an uncaught
//!   error. They are scored by the engine adapter, not here.
//! * **Manual, print and paged** tests need a person or a non-screen environment,
//!   and are not scored.
//!
//! A reference that is itself a reftest (a chain), a `about:` or cross-origin
//! reference, and a fuzzy key in the `test==reference` form are not implemented,
//! and are declined rather than approximated.

/// The host the pinned suite is served from. A URL under it names a file in the
/// checkout; every other URL names nothing the runner can load.
pub const SUITE_HOST: &str = "web-platform.test";

/// Decode `%XX` escapes in a URL path. A malformed escape is kept as written.
#[must_use]
pub fn percent_decode(path: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(high * 16 + low);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The viewport every reference test is rendered in, as WPT's reftest harness
/// specifies (`800x600`, scrollbars included).
pub const VIEWPORT_WIDTH: u32 = 800;
/// See [`VIEWPORT_WIDTH`].
pub const VIEWPORT_HEIGHT: u32 = 600;

/// Whether a reference must match or must differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Match,
    Mismatch,
}

/// One `<link rel=match|mismatch href=...>` declared by a test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub kind: RefKind,
    /// The `href` as written. Empty when the link has no `href` attribute, which
    /// [`resolve_reference`] refuses.
    pub href: String,
}

/// What a test file declares, read from its parsed document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Every `link` whose `rel` names `match` or `mismatch`, in document order.
    pub references: Vec<Reference>,
    /// The `content` of every `<meta name=fuzzy>`, in document order.
    pub fuzzy: Vec<String>,
    /// The whitespace-separated tokens of every `<meta name=flags>`, lowercased.
    pub flags: Vec<String>,
    /// Whether the document loads WPT's `testharness.js`.
    pub has_testharness: bool,
}

/// How a file is treated by the runner, decided before anything is rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Under a `manual/` directory, or flagged `manual` or `interact`. Not
    /// automated, so not scored.
    Manual,
    /// Flagged `print` or `paged`. It needs a non-screen environment.
    NonScreen,
    /// A reference test: scored by pixel comparison.
    Reftest,
    /// A crash test: scored by completing without a panic or an uncaught error.
    Crashtest,
    /// Anything else. The testharness path decides it.
    NotScored,
}

/// Decide how a file is scored.
///
/// The order is deliberate. A manual test is excluded even when it carries
/// references, because a person has to run it. A reftest outranks a crash test
/// because a crash test with references is still a comparison.
#[must_use]
pub fn scope(test_path: &str, meta: &Metadata) -> Scope {
    let segments: Vec<&str> = test_path.split('/').collect();
    let directories = &segments[..segments.len().saturating_sub(1)];
    let flagged = |name: &str| meta.flags.iter().any(|flag| flag == name);
    if directories.contains(&"manual") || flagged("manual") || flagged("interact") {
        return Scope::Manual;
    }
    if flagged("print") || flagged("paged") {
        return Scope::NonScreen;
    }
    if !meta.references.is_empty() {
        return Scope::Reftest;
    }
    if directories.contains(&"crashtests") && !meta.has_testharness {
        return Scope::Crashtest;
    }
    Scope::NotScored
}

/// Resolve a reference's `href` onto a suite-relative path.
///
/// Relative hrefs resolve against the test's directory, rooted hrefs (`/x`) against
/// the suite root, and an absolute `http(s)://web-platform.test` URL is the same
/// as its path. Anything else is refused: an `about:` reference, a cross-origin
/// URL, or a query or fragment, none of which name a file in the checkout. A path
/// that climbs above the suite root is refused too.
///
/// # Errors
///
/// A human-readable reason, which the caller reports as the declined reason.
pub fn resolve_reference(test_path: &str, href: &str) -> Result<String, String> {
    let href = href.trim();
    if href.is_empty() {
        return Err("a reference link has no href".to_owned());
    }
    if href.contains('?') || href.contains('#') {
        return Err(format!(
            "reference {href:?} has a query or fragment, which names no file"
        ));
    }
    let raw = if let Some(rest) = strip_suite_origin(href) {
        rest.to_owned()
    } else if href.contains("://") || href.starts_with("//") || href.starts_with("about:") {
        return Err(format!("reference {href:?} is not a file in the suite"));
    } else if let Some(rooted) = href.strip_prefix('/') {
        rooted.to_owned()
    } else {
        let directory = test_path.rsplit_once('/').map_or("", |(dir, _)| dir);
        if directory.is_empty() {
            href.to_owned()
        } else {
            format!("{directory}/{href}")
        }
    };
    normalize(&percent_decode(&raw))
        .ok_or_else(|| format!("reference {href:?} climbs above the suite root"))
}

/// The path after `http(s)://web-platform.test[:port]/`, when the URL names the
/// suite's own host, else `None`.
fn strip_suite_origin(href: &str) -> Option<&str> {
    let lower = href.to_ascii_lowercase();
    let host_start = ["http://", "https://"]
        .into_iter()
        .find(|scheme| lower.starts_with(scheme))?
        .len();
    let after_scheme = &href[host_start..];
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let (host, path) = after_scheme.split_at(host_end);
    let host = host.split(':').next().unwrap_or_default();
    if !host.eq_ignore_ascii_case(SUITE_HOST) {
        return None;
    }
    Some(path.strip_prefix('/').unwrap_or(path))
}

/// Collapse `.` and `..` and empty segments. `None` when the path climbs above
/// the root.
fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// An inclusive range, `(minimum, maximum)`.
pub type Range = (u32, u32);

/// The tolerance a comparison allows, from a `<meta name=fuzzy>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fuzziness {
    /// The largest per-channel difference any pixel may show.
    pub max_difference: Range,
    /// How many pixels may differ.
    pub total_pixels: Range,
}

impl Fuzziness {
    const EXACT: Self = Self {
        max_difference: (0, 0),
        total_pixels: (0, 0),
    };
}

/// One parsed `fuzzy` annotation: its optional reference key and its tolerance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzyRule {
    /// The reference this applies to, as written before the `:`, or `None` when
    /// it applies to every reference without a more specific rule.
    pub key: Option<String>,
    pub fuzziness: Fuzziness,
}

/// Parse one `<meta name=fuzzy>` value.
///
/// The form is `[reference:]<maxDifference>;<totalPixels>`, where each part is
/// `[name=]N` or `[name=]N-M`. An unnamed part takes the property not yet named,
/// in order. A value out of range, or a key in the `test==reference` form, is an
/// error rather than a guess.
///
/// # Errors
///
/// A reason the annotation cannot be interpreted.
pub fn parse_fuzzy(content: &str) -> Result<FuzzyRule, String> {
    let content = content.trim();
    let (key, values) = match content.rsplit_once(':') {
        Some((key, values)) => (Some(key.trim().to_owned()), values),
        None => (None, content),
    };
    if let Some(key) = &key {
        if key.is_empty() {
            return Err(format!(
                "fuzzy value {content:?} has an empty reference key"
            ));
        }
        if key.contains("==") || key.contains("!=") {
            return Err(format!(
                "fuzzy key {key:?} uses the test==reference form, which is not implemented"
            ));
        }
    }
    let parts: Vec<&str> = values.split(';').collect();
    if parts.len() != 2 {
        return Err(format!("fuzzy value {content:?} does not have two parts"));
    }
    let mut max_difference = None;
    let mut total_pixels = None;
    let mut unnamed = Vec::new();
    for part in parts {
        let (name, range) = match part.split_once('=') {
            Some((name, range)) => (Some(name.trim()), range),
            None => (None, part),
        };
        let range = parse_range(range)?;
        match name {
            Some("maxDifference") => {
                if max_difference.replace(range).is_some() {
                    return Err(format!("fuzzy value {content:?} names maxDifference twice"));
                }
            }
            Some("totalPixels") => {
                if total_pixels.replace(range).is_some() {
                    return Err(format!("fuzzy value {content:?} names totalPixels twice"));
                }
            }
            Some(other) => return Err(format!("{other:?} is not a fuzzy property")),
            None => unnamed.push(range),
        }
    }
    for range in unnamed {
        if max_difference.is_none() {
            max_difference = Some(range);
        } else if total_pixels.is_none() {
            total_pixels = Some(range);
        }
    }
    match (max_difference, total_pixels) {
        (Some(max_difference), Some(total_pixels)) => Ok(FuzzyRule {
            key,
            fuzziness: Fuzziness {
                max_difference,
                total_pixels,
            },
        }),
        _ => Err(format!(
            "fuzzy value {content:?} does not give both properties"
        )),
    }
}

fn parse_range(text: &str) -> Result<Range, String> {
    let text = text.trim();
    let (low, high) = match text.split_once('-') {
        Some((low, high)) => (low.trim(), high.trim()),
        None => (text, text),
    };
    let number = |digits: &str| {
        digits
            .parse::<u32>()
            .map_err(|_| format!("{text:?} is not a number or a range of numbers"))
    };
    let (low, high) = (number(low)?, number(high)?);
    if low > high {
        return Err(format!("range {text:?} runs backwards"));
    }
    Ok((low, high))
}

/// Parse every `fuzzy` annotation a test declares.
///
/// # Errors
///
/// The first annotation that cannot be read, or a second unkeyed annotation,
/// which would leave two answers for the same comparison.
pub fn fuzzy_rules(contents: &[String]) -> Result<Vec<FuzzyRule>, String> {
    let rules = contents
        .iter()
        .map(|content| parse_fuzzy(content))
        .collect::<Result<Vec<_>, _>>()?;
    if rules.iter().filter(|rule| rule.key.is_none()).count() > 1 {
        return Err("a test declares two fuzzy annotations without a reference key".to_owned());
    }
    Ok(rules)
}

/// The tolerance that applies to the comparison with `href`.
///
/// A rule keyed to that reference wins over the unkeyed rule. A key matches when
/// it is the `href` as written or resolves to the same suite path.
#[must_use]
pub fn fuzziness_for(rules: &[FuzzyRule], test_path: &str, href: &str) -> Option<Fuzziness> {
    let target = resolve_reference(test_path, href).ok();
    let keyed = rules.iter().find(|rule| {
        rule.key.as_deref().is_some_and(|key| {
            key == href.trim()
                || target.as_ref().is_some_and(|target| {
                    resolve_reference(test_path, key).ok().as_ref() == Some(target)
                })
        })
    });
    keyed
        .or_else(|| rules.iter().find(|rule| rule.key.is_none()))
        .map(|rule| rule.fuzziness)
}

/// How two rasters differ: the largest per-channel difference over every pixel,
/// and how many pixels differ in any channel. This is WPT's definition, which
/// compares the colour channels and ignores alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Difference {
    pub max_per_channel: u32,
    pub pixels_different: u32,
}

/// Compare two rasters of the same size, as `[red, green, blue]` per pixel.
///
/// The slices must be the same length; the caller establishes that by comparing
/// the surface dimensions first.
#[must_use]
pub fn difference(actual: &[[u8; 3]], expected: &[[u8; 3]]) -> Difference {
    let mut max_per_channel = 0_u32;
    let mut pixels_different = 0_u32;
    for (a, e) in actual.iter().zip(expected) {
        let mut differs = false;
        for channel in 0..3 {
            let delta = u32::from(a[channel].abs_diff(e[channel]));
            if delta > 0 {
                differs = true;
                max_per_channel = max_per_channel.max(delta);
            }
        }
        if differs {
            pixels_different += 1;
        }
    }
    Difference {
        max_per_channel,
        pixels_different,
    }
}

/// Whether a comparison counts as equal under a tolerance.
///
/// `difference` is `None` when the two surfaces differ in size. Such a pair is
/// never equal, because no per-pixel tolerance applies to it.
///
/// This is wptrunner's rule, reproduced exactly. With no tolerance, or the zero
/// tolerance, the rasters must be identical. Otherwise they are equal when either
/// the pixel count or the channel difference is exactly zero against a minimum of
/// zero, or both measures fall inside their ranges.
#[must_use]
pub fn within(difference: Option<Difference>, fuzziness: Option<Fuzziness>) -> bool {
    let Some(difference) = difference else {
        return false;
    };
    let fuzziness = fuzziness.unwrap_or(Fuzziness::EXACT);
    if fuzziness == Fuzziness::EXACT {
        return difference.pixels_different == 0 && difference.max_per_channel == 0;
    }
    let Difference {
        max_per_channel,
        pixels_different,
    } = difference;
    (pixels_different == 0 && fuzziness.total_pixels.0 == 0)
        || (max_per_channel == 0 && fuzziness.max_difference.0 == 0)
        || (fuzziness.max_difference.0 <= max_per_channel
            && max_per_channel <= fuzziness.max_difference.1
            && fuzziness.total_pixels.0 <= pixels_different
            && pixels_different <= fuzziness.total_pixels.1)
}

/// Why a test's references did not agree with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// There were match references and none of them matched.
    MatchDiffers,
    /// The mismatch reference at `index` was equal to the test.
    MismatchMatches { index: usize },
}

/// Combine the comparison results for a test's references.
///
/// `comparisons` pairs each reference's kind with whether it was equal to the
/// test. The rule is the one the WPT reftest documentation states: with match
/// references, at least one must be equal; with mismatch references, every one
/// must be unequal; and the two conditions hold together.
///
/// wptrunner's `check_pass` walks the references in a different order and lets a
/// single passing mismatch end the search, so it accepts some combinations this
/// rule refuses. This runner takes the documented reading, because a fail here
/// is the cautious direction. The difference is reported, not hidden.
///
/// # Errors
///
/// The first condition that does not hold.
pub fn judge(comparisons: &[(RefKind, bool)]) -> Result<(), Failure> {
    let mut any_match = false;
    let mut has_match = false;
    for (kind, equal) in comparisons {
        if *kind == RefKind::Match {
            has_match = true;
            any_match |= *equal;
        }
    }
    if has_match && !any_match {
        return Err(Failure::MatchDiffers);
    }
    if let Some(index) = comparisons
        .iter()
        .position(|(kind, equal)| *kind == RefKind::Mismatch && *equal)
    {
        return Err(Failure::MismatchMatches { index });
    }
    Ok(())
}

/// The assertion text a reftest failure starts with. [`mechanism_for_assertion`]
/// keys on these, so the report can rank failures by cause.
pub const DIFFER_PREFIX: &str = "reftest: the rendering differs from its reference";
/// See [`DIFFER_PREFIX`].
pub const MATCH_PREFIX: &str = "reftest: the rendering matches a reference it must differ from";
/// See [`DIFFER_PREFIX`].
pub const REFTEST_UNCAUGHT_PREFIX: &str = "reftest: an uncaught error";
/// See [`DIFFER_PREFIX`].
pub const CRASHTEST_UNCAUGHT_PREFIX: &str = "crashtest: an uncaught error";
/// See [`DIFFER_PREFIX`].
pub const CRASHTEST_PANIC_PREFIX: &str = "crashtest: the engine panicked";

/// The mechanism key for a reftest or crash test failure whose cause is named by its
/// assertion text. An uncaught error is not named here: it is classified by the exception
/// it carries, through [`uncaught_error_message`], as a testharness failure is.
#[must_use]
pub fn mechanism_for_assertion(assertion: &str) -> Option<&'static str> {
    [
        (DIFFER_PREFIX, "reftest-pixels-differ"),
        (MATCH_PREFIX, "reftest-pixels-match"),
        (CRASHTEST_PANIC_PREFIX, "crashtest-engine-panic"),
    ]
    .into_iter()
    .find(|(prefix, _)| assertion.starts_with(prefix))
    .map(|(_, key)| key)
}

/// The exception message inside an uncaught-error assertion, which the runner classifies
/// by what the exception says: a call to a missing method names the missing API.
#[must_use]
pub fn uncaught_error_message(assertion: &str) -> Option<&str> {
    [REFTEST_UNCAUGHT_PREFIX, CRASHTEST_UNCAUGHT_PREFIX]
        .into_iter()
        .find_map(|prefix| assertion.strip_prefix(prefix))
        .map(|rest| rest.trim_start_matches(':').trim())
}

#[cfg(test)]
mod tests {
    use super::{
        Difference, Failure, Fuzziness, Metadata, REFTEST_UNCAUGHT_PREFIX, RefKind, Reference,
        Scope, difference, fuzziness_for, fuzzy_rules, judge, mechanism_for_assertion, parse_fuzzy,
        resolve_reference, scope, uncaught_error_message, within,
    };

    fn meta(references: &[(RefKind, &str)], flags: &[&str], testharness: bool) -> Metadata {
        Metadata {
            references: references
                .iter()
                .map(|(kind, href)| Reference {
                    kind: *kind,
                    href: (*href).to_owned(),
                })
                .collect(),
            fuzzy: Vec::new(),
            flags: flags.iter().map(|f| (*f).to_owned()).collect(),
            has_testharness: testharness,
        }
    }

    #[test]
    fn a_match_link_makes_a_reftest_and_a_manual_directory_outranks_it() {
        let linked = meta(&[(RefKind::Match, "ref.html")], &[], false);
        assert_eq!(scope("dom/a/test.html", &linked), Scope::Reftest);
        assert_eq!(scope("dom/a/manual/test.html", &linked), Scope::Manual);
    }

    #[test]
    fn interaction_and_manual_flags_are_manual_and_print_is_a_non_screen_environment() {
        let plain = Metadata::default();
        assert_eq!(
            scope("a/b.html", &meta(&[], &["manual"], false)),
            Scope::Manual
        );
        assert_eq!(
            scope("a/b.html", &meta(&[], &["interact"], false)),
            Scope::Manual
        );
        assert_eq!(
            scope("a/b.html", &meta(&[], &["print"], false)),
            Scope::NonScreen
        );
        assert_eq!(
            scope("a/b.html", &meta(&[], &["paged"], false)),
            Scope::NonScreen
        );
        assert_eq!(scope("a/b.html", &plain), Scope::NotScored);
    }

    #[test]
    fn a_crash_test_is_scored_only_when_it_is_not_a_testharness_file() {
        let crash = Metadata::default();
        assert_eq!(scope("dom/x/crashtests/a.html", &crash), Scope::Crashtest);
        assert_eq!(
            scope("dom/x/crashtests/a.html", &meta(&[], &[], true)),
            Scope::NotScored,
            "a crash test that loads testharness.js is the testharness path's"
        );
    }

    #[test]
    fn references_resolve_against_the_test_and_the_suite_root() {
        assert_eq!(
            resolve_reference("dom/a/test.html", "ref.html").as_deref(),
            Ok("dom/a/ref.html")
        );
        assert_eq!(
            resolve_reference("dom/a/test.html", "../ref/x.html").as_deref(),
            Ok("dom/ref/x.html")
        );
        assert_eq!(
            resolve_reference("dom/a/test.html", "/css/reference/square.html").as_deref(),
            Ok("css/reference/square.html")
        );
        assert_eq!(
            resolve_reference("dom/a/test.html", "http://web-platform.test:8000/x/y.html")
                .as_deref(),
            Ok("x/y.html")
        );
        assert_eq!(
            resolve_reference("dom/a/test.html", "ref%20one.html").as_deref(),
            Ok("dom/a/ref one.html")
        );
    }

    #[test]
    fn references_that_name_no_suite_file_are_refused_not_guessed() {
        for href in [
            "about:blank",
            "https://example.org/ref.html",
            "//example.org/ref.html",
            "ref.html?x=1",
            "ref.html#frag",
            "../../../../escape.html",
            "",
        ] {
            assert!(
                resolve_reference("dom/a/test.html", href).is_err(),
                "{href:?} should be refused"
            );
        }
    }

    #[test]
    fn a_single_fuzzy_value_is_an_exact_bound_and_a_range_is_inclusive() {
        let exact = parse_fuzzy("15;300").expect("unnamed pair parses");
        assert_eq!(exact.fuzziness.max_difference, (15, 15));
        assert_eq!(exact.fuzziness.total_pixels, (300, 300));
        let ranged = parse_fuzzy("maxDifference=10-15;totalPixels=200-300").expect("named ranges");
        assert_eq!(ranged.fuzziness.max_difference, (10, 15));
        assert_eq!(ranged.fuzziness.total_pixels, (200, 300));
        assert_eq!(ranged.key, None);
    }

    #[test]
    fn a_keyed_fuzzy_value_names_its_reference_and_a_malformed_one_is_refused() {
        let keyed = parse_fuzzy("option1-ref.html:10-15;200-300").expect("keyed");
        assert_eq!(keyed.key.as_deref(), Some("option1-ref.html"));
        assert!(parse_fuzzy("15").is_err(), "one part is not a fuzzy value");
        assert!(
            parse_fuzzy("maxDifference=5-2;3").is_err(),
            "a backwards range"
        );
        assert!(parse_fuzzy("colour=1;2").is_err(), "an unknown property");
        assert!(
            parse_fuzzy("a==b.html:1;2").is_err(),
            "test==reference keys are not implemented"
        );
    }

    #[test]
    fn the_keyed_rule_wins_for_its_reference_and_the_unkeyed_rule_covers_the_rest() {
        let rules =
            fuzzy_rules(&["ref-b.html:1;1".to_owned(), "5;5".to_owned()]).expect("rules parse");
        let for_b = fuzziness_for(&rules, "dom/a/test.html", "ref-b.html").expect("keyed");
        assert_eq!(for_b.max_difference, (1, 1));
        let for_a = fuzziness_for(&rules, "dom/a/test.html", "ref-a.html").expect("unkeyed");
        assert_eq!(for_a.max_difference, (5, 5));
        assert!(fuzzy_rules(&["1;1".to_owned(), "2;2".to_owned()]).is_err());
    }

    #[test]
    fn the_difference_is_the_largest_channel_delta_and_the_count_of_changed_pixels() {
        let a = [[0, 0, 0], [10, 20, 30], [255, 255, 255]];
        let b = [[0, 0, 0], [12, 20, 25], [255, 255, 255]];
        assert_eq!(
            difference(&a, &b),
            Difference {
                max_per_channel: 5,
                pixels_different: 1,
            }
        );
        assert_eq!(difference(&a, &a).pixels_different, 0);
    }

    #[test]
    fn exact_comparison_admits_only_identical_rasters_and_a_size_change_is_never_equal() {
        let same = Difference {
            max_per_channel: 0,
            pixels_different: 0,
        };
        let one_off = Difference {
            max_per_channel: 1,
            pixels_different: 1,
        };
        assert!(within(Some(same), None));
        assert!(!within(Some(one_off), None));
        assert!(!within(None, None), "different sizes are never equal");
        assert!(!within(
            None,
            Some(Fuzziness {
                max_difference: (0, 255),
                total_pixels: (0, 999_999)
            })
        ));
    }

    #[test]
    fn fuzzy_equality_applies_the_ranges_and_wptrunners_zero_shortcuts() {
        let fuzz = Fuzziness {
            max_difference: (10, 15),
            total_pixels: (200, 300),
        };
        let inside = Difference {
            max_per_channel: 12,
            pixels_different: 250,
        };
        assert!(within(Some(inside), Some(fuzz)));
        let too_big = Difference {
            max_per_channel: 16,
            pixels_different: 250,
        };
        assert!(!within(Some(too_big), Some(fuzz)));
        // Zero differing pixels pass when the total's minimum is zero, whatever the channel difference.
        let no_pixels = Difference {
            max_per_channel: 99,
            pixels_different: 0,
        };
        assert!(within(
            Some(no_pixels),
            Some(Fuzziness {
                max_difference: (0, 0),
                total_pixels: (0, 5),
            })
        ));
        // No channel difference passes when the maximum's minimum is zero.
        let no_channel = Difference {
            max_per_channel: 0,
            pixels_different: 400,
        };
        assert!(within(
            Some(no_channel),
            Some(Fuzziness {
                max_difference: (0, 3),
                total_pixels: (1, 2),
            })
        ));
    }

    #[test]
    fn several_references_need_one_match_and_every_mismatch_to_differ() {
        assert_eq!(judge(&[(RefKind::Match, true)]), Ok(()));
        assert_eq!(
            judge(&[(RefKind::Match, false)]),
            Err(Failure::MatchDiffers)
        );
        assert_eq!(
            judge(&[(RefKind::Match, false), (RefKind::Match, true)]),
            Ok(()),
            "one matching reference is enough among match references"
        );
        assert_eq!(judge(&[(RefKind::Mismatch, false)]), Ok(()));
        assert_eq!(
            judge(&[(RefKind::Mismatch, false), (RefKind::Mismatch, true)]),
            Err(Failure::MismatchMatches { index: 1 })
        );
        assert_eq!(
            judge(&[(RefKind::Match, true), (RefKind::Mismatch, true)]),
            Err(Failure::MismatchMatches { index: 1 }),
            "the two kinds combine with and"
        );
    }

    #[test]
    fn failure_texts_map_to_their_mechanisms() {
        assert_eq!(
            mechanism_for_assertion(&format!("{}: 12 pixels differ", super::DIFFER_PREFIX)),
            Some("reftest-pixels-differ")
        );
        assert_eq!(
            mechanism_for_assertion("ReferenceError: x is not defined"),
            None
        );
    }

    #[test]
    fn an_uncaught_error_is_classified_by_the_exception_it_carries() {
        let text =
            format!("{REFTEST_UNCAUGHT_PREFIX}: Type: value of callee.moveBefore is undefined");
        assert_eq!(
            uncaught_error_message(&text),
            Some("Type: value of callee.moveBefore is undefined")
        );
        assert_eq!(
            uncaught_error_message("reftest: the rendering differs"),
            None
        );
        assert_eq!(
            mechanism_for_assertion(&text),
            None,
            "the message names the mechanism, not the prefix"
        );
    }
}
