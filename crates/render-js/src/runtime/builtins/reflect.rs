//! IDL reflection of HTML content attributes (WHATWG HTML "Reflecting
//! content attributes in IDL attributes", "Enumerated attributes").
//!
//! An element wrapper reads and writes an IDL attribute by the rule its
//! interface declares for it. [`reflection_rule`] walks the interface chain
//! (`HTMLVideoElement` -> `HTMLMediaElement` -> `HTMLElement` -> `Element`), so a
//! row is written once for every interface that inherits it. The per-element
//! rows are generated into `reflect_table.rs`; the global rows are here.

use render_dom::Dom;
use render_dom::NodeId;
use render_dom::NodeKind;
use url::Url;

use super::dom::dom_interface_name;
use super::reflect_table::INTERFACES;
use crate::JsError;
use crate::JsValue;
use crate::runtime::JsRuntime;
use crate::runtime::convert::uint32_of_number;

/// How one IDL attribute reflects its content attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reflection {
    /// `DOMString`. An absent attribute reads as "", and a write stores the
    /// string conversion of the value.
    String,
    /// `DOMString` with `[LegacyNullToEmptyString]`: writing `null` stores "".
    NullAsEmpty,
    /// `USVString` holding a URL. A read resolves the attribute against the
    /// document base URL, and falls back to the raw value when that fails.
    Url,
    /// `boolean`. A read reports presence, and a write adds or removes the
    /// attribute.
    Boolean,
    /// An enumerated attribute limited to known values.
    Keyword(Keywords),
    /// `long`. A read yields `default` when the attribute is absent or is not
    /// a signed 32-bit integer.
    Long { default: i32 },
    /// `unsigned long`. A read yields `default` when the attribute is absent,
    /// is not a non-negative integer, or exceeds 2147483647. With `clamp`, an
    /// integer outside the range clamps into it instead.
    UnsignedLong {
        default: u32,
        clamp: Option<(u32, u32)>,
    },
    /// `translate` (HTML "The translate attribute"): a read reports whether the element is
    /// translate-enabled, and a write stores `yes` or `no`.
    Translate,
}

/// The states of an enumerated attribute (HTML "Enumerated attributes").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Keywords {
    /// The keywords, which match ASCII case-insensitively.
    pub(crate) values: &'static [&'static str],
    /// The state of a present attribute that matches no keyword. `None` is
    /// "no state", which reads as "" or, when `nullable`, as `null`.
    pub(crate) invalid: Option<&'static str>,
    /// The state of an absent attribute. `None` is "no state".
    pub(crate) missing: Option<&'static str>,
    /// Whether "no state" reads as `null`, and whether `null` written to the
    /// attribute removes it.
    pub(crate) nullable: bool,
}

/// One reflected IDL attribute of an interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reflected {
    /// The IDL attribute name.
    pub(crate) idl: &'static str,
    /// The content attribute name, lower case.
    pub(crate) attribute: &'static str,
    pub(crate) reflection: Reflection,
}

const DIR: Keywords = Keywords {
    values: &["ltr", "rtl", "auto"],
    invalid: None,
    missing: None,
    nullable: false,
};

const ENTER_KEY_HINT: Keywords = Keywords {
    values: &["enter", "done", "go", "next", "previous", "search", "send"],
    invalid: None,
    missing: None,
    nullable: false,
};

const INPUT_MODE: Keywords = Keywords {
    values: &[
        "none", "text", "tel", "url", "email", "numeric", "decimal", "search",
    ],
    invalid: None,
    missing: None,
    nullable: false,
};

/// The global attributes of `HTMLElement` (HTML "Global attributes").
const HTML_ELEMENT: &[Reflected] = &[
    Reflected {
        idl: "accessKey",
        attribute: "accesskey",
        reflection: Reflection::String,
    },
    Reflected {
        idl: "dir",
        attribute: "dir",
        reflection: Reflection::Keyword(DIR),
    },
    Reflected {
        idl: "enterKeyHint",
        attribute: "enterkeyhint",
        reflection: Reflection::Keyword(ENTER_KEY_HINT),
    },
    Reflected {
        idl: "hidden",
        attribute: "hidden",
        reflection: Reflection::Boolean,
    },
    Reflected {
        idl: "inert",
        attribute: "inert",
        reflection: Reflection::Boolean,
    },
    Reflected {
        idl: "inputMode",
        attribute: "inputmode",
        reflection: Reflection::Keyword(INPUT_MODE),
    },
    Reflected {
        idl: "lang",
        attribute: "lang",
        reflection: Reflection::String,
    },
    Reflected {
        idl: "title",
        attribute: "title",
        reflection: Reflection::String,
    },
    Reflected {
        idl: "translate",
        attribute: "translate",
        reflection: Reflection::Translate,
    },
];

/// The rows an interface itself declares, if it declares any.
fn own_rows(interface: &str) -> Option<&'static [Reflected]> {
    if interface == "HTMLElement" {
        return Some(HTML_ELEMENT);
    }
    INTERFACES
        .iter()
        .find(|(name, _)| *name == interface)
        .map(|(_, rows)| *rows)
}

/// The interface an interface inherits from, following the IDL inheritance
/// of the element interfaces.
fn parent_interface(interface: &str) -> Option<&'static str> {
    match interface {
        "Element" => None,
        "HTMLElement" => Some("Element"),
        "HTMLMediaElement" => Some("HTMLElement"),
        "HTMLVideoElement" | "HTMLAudioElement" => Some("HTMLMediaElement"),
        _ => Some("HTMLElement"),
    }
}

/// The rule that `interface` applies to the IDL attribute `idl`: the row of the
/// nearest interface in its chain that declares it.
pub(crate) fn reflection_rule(interface: &str, idl: &str) -> Option<&'static Reflected> {
    let mut current = Some(interface);
    while let Some(name) = current {
        if let Some(rule) = own_rows(name).and_then(|rows| rows.iter().find(|row| row.idl == idl)) {
            return Some(rule);
        }
        current = parent_interface(name);
    }
    None
}

/// The "rules for parsing integers" (HTML "Rules for parsing integers"): skip leading ASCII
/// whitespace, read an optional sign, then decimal digits. `None` when there
/// are no digits. Values beyond the `i64` range saturate, which is harmless
/// because every caller range-checks against 32 bits.
pub(crate) fn parse_integer(input: &str) -> Option<i64> {
    let trimmed = input.trim_start_matches(['\t', '\n', '\u{c}', '\r', ' ']);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let length = digits.bytes().take_while(u8::is_ascii_digit).count();
    if length == 0 {
        return None;
    }
    let magnitude = digits.as_bytes()[..length]
        .iter()
        .fold(0_i64, |value, digit| {
            value
                .saturating_mul(10)
                .saturating_add(i64::from(digit - b'0'))
        });
    Some(if negative { -magnitude } else { magnitude })
}

/// The read of an `unsigned long` attribute: the value of the rules for
/// parsing non-negative integers, or `None` when they fail.
fn non_negative(value: &str) -> Option<i64> {
    parse_integer(value).filter(|parsed| *parsed >= 0)
}

/// Read a keyword attribute by its states (HTML "Enumerated attributes").
fn keyword_state(keywords: &Keywords, attribute: Option<&str>) -> JsValue {
    let state = match attribute {
        None => keywords.missing,
        Some(value) => match keywords
            .values
            .iter()
            .find(|keyword| keyword.eq_ignore_ascii_case(value))
        {
            Some(keyword) => Some(*keyword),
            None => keywords.invalid,
        },
    };
    match (state, keywords.nullable) {
        (Some(state), _) => JsValue::String(state.into()),
        (None, true) => JsValue::Null,
        (None, false) => JsValue::String("".into()),
    }
}

/// The `body` attribute that a legacy colour property of `document` reflects
/// (the obsolete `document.fgColor` and its siblings).
fn body_colour_idl(property: &str) -> Option<&'static str> {
    match property {
        "fgColor" => Some("text"),
        "bgColor" => Some("bgColor"),
        "linkColor" => Some("link"),
        "vlinkColor" => Some("vLink"),
        "alinkColor" => Some("aLink"),
        _ => None,
    }
}

/// Whether `property` is one of the legacy colour properties of `document`.
pub(in crate::runtime) fn is_document_colour(property: &str) -> bool {
    body_colour_idl(property).is_some()
}

impl JsRuntime {
    /// The `body` element and the rule of the attribute a legacy colour property
    /// of `document` reflects. `None` when there is no body element.
    fn document_colour_target(
        &mut self,
        dom: &Dom,
        document: NodeId,
        property: &str,
    ) -> Result<Option<(NodeId, &'static Reflected)>, JsError> {
        let Some(rule) =
            body_colour_idl(property).and_then(|idl| reflection_rule("HTMLBodyElement", idl))
        else {
            return Ok(None);
        };
        Ok(self
            .find_element_by_tag(dom, document, "body")?
            .map(|body| (body, rule)))
    }

    /// Read a legacy colour property of `document`: the body's reflected
    /// attribute, or "" when there is no body element.
    pub(in crate::runtime) fn read_document_colour(
        &mut self,
        dom: &mut Dom,
        document: NodeId,
        property: &str,
    ) -> Result<JsValue, JsError> {
        match self.document_colour_target(dom, document, property)? {
            Some((body, rule)) => self.read_reflected(dom, body, rule),
            None => Ok(JsValue::String("".into())),
        }
    }

    /// Write a legacy colour property of `document` through to the body's
    /// reflected attribute. Without a body element the write has no effect.
    pub(in crate::runtime) fn write_document_colour(
        &mut self,
        dom: &mut Dom,
        document: NodeId,
        property: &str,
        value: &JsValue,
    ) -> Result<(), JsError> {
        match self.document_colour_target(dom, document, property)? {
            Some((body, rule)) => self.write_reflected(dom, body, rule, value),
            None => Ok(()),
        }
    }

    /// The reflection rule for property `property` of `node`, or `None` when
    /// the node is not an HTML element or its interface does not reflect it.
    pub(in crate::runtime) fn reflected_attribute(
        dom: &Dom,
        node: NodeId,
        property: &str,
    ) -> Option<&'static Reflected> {
        match dom.node(node).map(render_dom::Node::kind) {
            Some(NodeKind::Element(element))
                if element.namespace == render_dom::Namespace::Html =>
            {
                reflection_rule(dom_interface_name(dom, node), property)
            }
            _ => None,
        }
    }

    /// Read a reflected IDL attribute of `node` (HTML "Reflecting content attributes in IDL attributes").
    pub(in crate::runtime) fn read_reflected(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        rule: &Reflected,
    ) -> Result<JsValue, JsError> {
        let attribute = dom.attribute(node, rule.attribute)?.map(str::to_owned);
        Ok(match rule.reflection {
            Reflection::String | Reflection::NullAsEmpty => {
                JsValue::String(attribute.unwrap_or_default().into())
            }
            Reflection::Url => match attribute {
                None => JsValue::String("".into()),
                Some(value) => {
                    let base = self.reflection_base(dom);
                    let resolved = match base.and_then(|base| base.join(&value).ok()) {
                        Some(url) => url.to_string(),
                        None => value,
                    };
                    JsValue::String(resolved.into())
                }
            },
            Reflection::Boolean => JsValue::Boolean(attribute.is_some()),
            Reflection::Translate => JsValue::Boolean(translate_enabled(dom, node)),
            Reflection::Keyword(keywords) => keyword_state(&keywords, attribute.as_deref()),
            Reflection::Long { default } => JsValue::Number(f64::from(
                attribute
                    .as_deref()
                    .and_then(parse_integer)
                    .and_then(|parsed| i32::try_from(parsed).ok())
                    .unwrap_or(default),
            )),
            Reflection::UnsignedLong { default, clamp } => {
                let parsed = attribute.as_deref().and_then(non_negative);
                let value = match (parsed, clamp) {
                    (None, _) => default,
                    (Some(parsed), Some((min, max))) => {
                        u32::try_from(parsed.clamp(i64::from(min), i64::from(max))).unwrap_or(max)
                    }
                    (Some(parsed), None) => u32::try_from(parsed)
                        .ok()
                        .filter(|value| *value <= i32::MAX.unsigned_abs())
                        .unwrap_or(default),
                };
                JsValue::Number(f64::from(value))
            }
        })
    }

    /// Write a reflected IDL attribute of `node` (HTML "Reflecting content attributes in IDL attributes").
    pub(in crate::runtime) fn write_reflected(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        rule: &Reflected,
        value: &JsValue,
    ) -> Result<(), JsError> {
        let name = rule.attribute;
        match rule.reflection {
            Reflection::String | Reflection::Url => {
                let text = self.to_string_value(dom, value)?.to_lossy();
                dom.set_attribute(node, name, text)?;
            }
            Reflection::NullAsEmpty => {
                let text = match value {
                    JsValue::Null => String::new(),
                    value => self.to_string_value(dom, value)?.to_lossy(),
                };
                dom.set_attribute(node, name, text)?;
            }
            Reflection::Boolean => {
                if value.is_truthy() {
                    dom.set_attribute(node, name, String::new())?;
                } else {
                    dom.remove_attribute(node, name)?;
                }
            }
            Reflection::Translate => {
                let state = if value.is_truthy() { "yes" } else { "no" };
                dom.set_attribute(node, name, state.to_owned())?;
            }
            Reflection::Keyword(keywords) => {
                if keywords.nullable && matches!(value, JsValue::Null | JsValue::Undefined) {
                    dom.remove_attribute(node, name)?;
                } else {
                    let text = self.to_string_value(dom, value)?.to_lossy();
                    dom.set_attribute(node, name, text)?;
                }
            }
            Reflection::Long { .. } => {
                let number = self.to_int32_value(dom, value)?;
                dom.set_attribute(node, name, number.to_string())?;
            }
            Reflection::UnsignedLong { .. } => {
                let number = self.to_number_value(dom, value)?;
                dom.set_attribute(node, name, uint32_of_number(number).to_string())?;
            }
        }
        Ok(())
    }

    /// The document base URL that reflected URL attributes resolve against
    /// (HTML "document base URL"): the first `base` element with an `href` attribute, joined
    /// onto the document URL, else the document URL itself. The result is kept
    /// until the DOM changes, so a loop over `a.href` walks the tree once.
    pub(in crate::runtime) fn reflection_base(&mut self, dom: &Dom) -> Option<Url> {
        let revision = dom.revision();
        if let Some((cached, base)) = &self.reflection_base_cache
            && *cached == revision
        {
            return base.clone();
        }
        let document_url = self.document_base_url();
        let base = match (first_base_href(dom), document_url) {
            (Some(href), Some(document)) => document.join(&href).ok().or(Some(document)),
            (Some(href), None) => Url::parse(&href).ok(),
            (None, document) => document,
        };
        self.reflection_base_cache = Some((revision, base.clone()));
        base
    }
}

/// Whether `node` is translate-enabled (HTML "The translate attribute"): the nearest `translate`
/// attribute on the node or an ancestor decides. `yes` and the empty string
/// enable, `no` disables, and an absent or invalid value defers to the parent.
/// With no decision anywhere, the node is enabled.
fn translate_enabled(dom: &Dom, node: NodeId) -> bool {
    let mut current = Some(node);
    while let Some(element) = current {
        match dom.attribute(element, "translate").ok().flatten() {
            Some(value) if value.is_empty() || value.eq_ignore_ascii_case("yes") => return true,
            Some(value) if value.eq_ignore_ascii_case("no") => return false,
            _ => {}
        }
        current = dom.parent(element);
    }
    true
}

/// The `href` of the first `base` element in tree order that has one.
fn first_base_href(dom: &Dom) -> Option<String> {
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        if let Some(NodeKind::Element(element)) = dom.node(node).map(render_dom::Node::kind)
            && element.namespace == render_dom::Namespace::Html
            && element.local_name == "base"
            && let Ok(Some(href)) = dom.attribute(node, "href")
        {
            return Some(href.to_owned());
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev());
    }
    None
}
