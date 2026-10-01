//! A tolerant scanner for the parts of a WPT test file this runner reasons about.
//!
//! WPT test files are HTML, and the only things this runner needs from one are:
//!
//! * the inline `<script>` bodies, to find assertions and API usage;
//! * the attributes of every tag that declares an external resource, to check
//!   the fixture actually exists;
//! * whether a `link rel="match"` reftest reference is declared.
//!
//! This is deliberately *not* a second HTML parser. `render-html` is a real
//! HTML parser and lives in the engine; duplicating it here would mean two
//! parsers disagreeing about which tests are which, and a classifier built on
//! the wrong one produces a wrong denominator. The scanner is a scanner, and it
//! says so: [`Scan::uncertain`] is set whenever a construct was encountered
//! that the scanner cannot resolve confidently, and the census counts those
//! rather than quietly classifying them.

/// One `<script>` element found in a test file.
#[derive(Debug, Clone)]
pub struct Script {
    /// Inline source, or `None` for a `src=` script.
    pub inline: Option<String>,
    /// The `src` URL, verbatim, for a `src=` script.
    pub src: Option<String>,
    /// Byte offset of the element in the file, for locating it in a report.
    pub offset: usize,
}

/// A resource a test declares that must exist for the test to mean anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredResource {
    /// The URL exactly as written in the file, for reporting.
    pub url: String,
    /// The attribute it came from, for reporting.
    pub attribute: &'static str,
}

/// What the scanner could establish about one test file.
#[derive(Debug, Default, Clone)]
pub struct Scan {
    /// Inline script bodies, in document order.
    pub scripts: Vec<Script>,
    /// `src=` scripts, in document order.
    pub external_scripts: Vec<String>,
    /// Stylesheets, images and other subresources the test declares.
    pub resources: Vec<DeclaredResource>,
    /// The `link rel="match"` reference, if this is a reftest.
    pub reftest_reference: Option<String>,
    /// Set when the scanner met something it could not resolve confidently.
    /// A file with this set is reported as unscanned rather than classified
    /// from partial information.
    pub uncertain: Option<String>,
}

/// Scan a test file. Never fails; a file that scans badly is *reported* as
/// having scanned badly.
#[must_use]
pub fn scan(source: &str) -> Scan {
    let lower = source.to_ascii_lowercase();
    let mut out = Scan::default();
    let bytes = source.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        // Find the next '<' that is not part of a comment or a doctype.
        match lower[i..].find('<') {
            Some(rel) => i += rel,
            None => break,
        }
        if !starts_tag(&lower, i) {
            i += 1;
            continue;
        }

        let Some(tag) = read_tag_name(&lower, i) else {
            // A bare `<` or a `<` followed by a non-name character. Not worth
            // flagging: text nodes legitimately contain them.
            i += 1;
            continue;
        };

        let Some(attrs) = read_attributes(source, &lower, i, &tag) else {
            out.uncertain
                .get_or_insert_with(|| format!("unterminated <{tag}> tag"));
            i += 1;
            continue;
        };
        let (attr_map, tag_end, self_closing) = attrs;

        match tag.as_str() {
            "script" => {
                if let Some(src) = attr_map.get("src") {
                    out.external_scripts.push(src.clone());
                    out.resources.push(DeclaredResource {
                        url: src.clone(),
                        attribute: "src",
                    });
                } else if self_closing {
                    // `<script/>` is not a thing in HTML; treat it as empty
                    // rather than scanning to EOF for a close tag.
                    out.scripts.push(Script {
                        inline: Some(String::new()),
                        src: None,
                        offset: i,
                    });
                } else {
                    let close = format!("</{tag}");
                    match lower[tag_end..].find(close.as_str()) {
                        Some(rel) => {
                            let body_end = tag_end + rel;
                            out.scripts.push(Script {
                                inline: Some(source[tag_end..body_end].to_owned()),
                                src: None,
                                offset: i,
                            });
                            i = body_end;
                        }
                        None => {
                            out.uncertain
                                .get_or_insert_with(|| format!("unclosed <{tag}> element"));
                            out.scripts.push(Script {
                                inline: Some(source[tag_end..].to_owned()),
                                src: None,
                                offset: i,
                            });
                            break;
                        }
                    }
                }
            }
            "link" => {
                let rel = attr_map.get("rel").map_or(String::new(), |v| v.to_ascii_lowercase());
                let href = attr_map.get("href").cloned();
                if let Some(href) = href {
                    if rel.split_ascii_whitespace().any(|r| r == "match") {
                        out.reftest_reference = Some(href.clone());
                    }
                    if rel.split_ascii_whitespace().any(|r| r == "stylesheet") {
                        out.resources.push(DeclaredResource {
                            url: href,
                            attribute: "href",
                        });
                    }
                }
            }
            "img" | "image" | "object" | "iframe" | "frame" | "embed" | "source"
            | "track" | "audio" | "video" => {
                if let Some(src) = attr_map.get("src") {
                    out.resources.push(DeclaredResource {
                        url: src.clone(),
                        attribute: "src",
                    });
                }
            }
            _ => {}
        }

        i = tag_end.max(i + 1);
    }

    out
}

fn starts_tag(lower: &str, i: usize) -> bool {
    let rest = &lower[i..];
    rest.len() > 1
        && rest.as_bytes()[1]
            .is_ascii_alphanumeric()
        // `<!--` and `<!doctype` are not tags.
        && rest.as_bytes()[1] != b'!'
        && rest.as_bytes()[1] != b'/'
}

/// Read a lowercase tag name starting at the `<` at `i`.
fn read_tag_name(lower: &str, i: usize) -> Option<String> {
    let rest = lower.get(i + 1..)?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_owned())
}

type AttributeMap = std::collections::BTreeMap<String, String>;

/// Read attributes up to the tag's `>`. Returns `(attrs, end_offset, self_closing)`.
fn read_attributes(
    source: &str,
    lower: &str,
    start: usize,
    tag: &str,
) -> Option<(AttributeMap, usize, bool)> {
    let after_name = start + 1 + tag.len();
    let mut attrs = AttributeMap::new();
    let mut i = after_name;
    let bytes = source.as_bytes();

    loop {
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'>' {
            return Some((attrs, i + 1, false));
        }
        if bytes[i] == b'/' && lower.get(i + 1..i + 2) == Some(">") {
            return Some((attrs, i + 2, true));
        }

        // Attribute name.
        let name_start = i;
        while i < bytes.len()
            && bytes[i] != b'='
            && bytes[i] != b'>'
            && bytes[i] != b'/'
            && !(bytes[i] as char).is_ascii_whitespace()
        {
            i += 1;
        }
        if i == name_start {
            return None;
        }
        let name = source[name_start..i].to_ascii_lowercase();

        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let Some(value) = (bytes[i] == b'=').then(|| {
            i += 1;
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let v_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                let value = source[v_start..i.min(bytes.len())].to_owned();
                if i < bytes.len() {
                    i += 1;
                }
                value
            } else {
                let v_start = i;
                while i < bytes.len() && bytes[i] != b'>' && !(bytes[i] as char).is_ascii_whitespace()
                {
                    i += 1;
                }
                source[v_start..i].to_owned()
            }
        }) else {
            attrs.insert(name, String::new());
            continue;
        };
        attrs.insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::scan;

    #[test]
    fn extracts_inline_script_bodies_in_order() {
        let src = "<!doctype html><script>var a=1;</script><p><script>var b=2;</script>";
        let scan = scan(src);
        let bodies: Vec<_> = scan
            .scripts
            .iter()
            .filter_map(|s| s.inline.as_deref())
            .collect();
        assert_eq!(bodies, vec!["var a=1;", "var b=2;"]);
        assert!(scan.uncertain.is_none());
    }

    #[test]
    fn records_external_script_and_stylesheet_resources() {
        let src = "<script src=\"/resources/testharness.js\"></script>\
                   <link rel=\"stylesheet\" href=\"support/a.css\">";
        let scan = scan(src);
        assert_eq!(
            scan.external_scripts,
            vec!["/resources/testharness.js".to_owned()]
        );
        let urls: Vec<_> = scan.resources.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["/resources/testharness.js", "support/a.css"]);
    }

    #[test]
    fn detects_reftest_match_reference() {
        let scan = scan("<link rel=\"match\" href=\"ref.html\">");
        assert_eq!(scan.reftest_reference.as_deref(), Some("ref.html"));
    }

    #[test]
    fn does_not_treat_a_stylesheet_link_as_a_reftest() {
        let scan = scan("<link rel=\"stylesheet\" href=\"a.css\">");
        assert!(scan.reftest_reference.is_none());
    }

    #[test]
    fn text_containing_a_bracket_is_not_a_tag() {
        // A naive `find('<')` scan mis-attributes `a < b` in script text.
        // This is the exact class of bug that shrinks a denominator silently.
        let src = "<script>if (a<b && c>d) { f(); }</script>";
        let scan = scan(src);
        assert_eq!(scan.scripts.len(), 1);
        assert!(scan.uncertain.is_none(), "{:?}", scan.uncertain);
    }

    #[test]
    fn an_unclosed_script_is_reported_not_guessed() {
        let scan = scan("<script>var a = 1;");
        assert!(scan.uncertain.is_some());
    }

    #[test]
    fn empty_script_is_recorded_as_empty_not_dropped() {
        // An empty script is a test with no assertions. It must be visible, not
        // vanish.
        let scan = scan("<script src=\"/resources/testharness.js\"></script><script></script>");
        let inlines: Vec<_> = scan
            .scripts
            .iter()
            .filter_map(|s| s.inline.as_deref())
            .collect();
        assert_eq!(inlines, vec![""]);
    }
}
