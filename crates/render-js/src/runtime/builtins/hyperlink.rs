//! The URL decomposition attributes of `a` and `area` (HTML "Interface
//! `HTMLHyperlinkElementUtils`"). Each read parses the `href` content attribute
//! against the document base URL; each write changes the parsed URL and stores
//! its serialization back in `href`. An `href` that does not parse leaves every
//! component reading as "" and every component write without effect.

use render_dom::Dom;
use render_dom::NodeId;
use url::Url;

use super::dom::dom_interface_name;
use crate::JsError;
use crate::JsValue;
use crate::runtime::JsRuntime;

/// The components of a URL that the hyperlink interface exposes.
const COMPONENTS: &[&str] = &[
    "hash", "host", "hostname", "origin", "password", "pathname", "port", "protocol", "search",
    "username",
];

/// Whether `property` is a URL component of `node` (an `a` or `area` element), or
/// the `text` of an `a` element, which is its `textContent` (HTML "The a element").
pub(in crate::runtime) fn is_hyperlink_component(dom: &Dom, node: NodeId, property: &str) -> bool {
    match dom_interface_name(dom, node) {
        "HTMLAnchorElement" => property == "text" || COMPONENTS.contains(&property),
        "HTMLAreaElement" => COMPONENTS.contains(&property),
        _ => false,
    }
}

/// The `host` component text: the host, then `:` and the port when there is one.
fn host_text(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    }
}

impl JsRuntime {
    /// The URL of an `a` or `area` element's `href`, or `None` when the attribute
    /// is absent or does not parse (HTML "Interface `HTMLHyperlinkElementUtils`").
    fn hyperlink_url(&mut self, dom: &Dom, node: NodeId) -> Result<Option<Url>, JsError> {
        let Some(href) = dom.attribute(node, "href")?.map(str::to_owned) else {
            return Ok(None);
        };
        Ok(match self.reflection_base(dom) {
            Some(base) => base.join(&href).ok(),
            None => Url::parse(&href).ok(),
        })
    }

    /// Read a URL component of an `a` or `area` element.
    pub(in crate::runtime) fn read_hyperlink_component(
        &mut self,
        dom: &Dom,
        node: NodeId,
        property: &str,
    ) -> Result<JsValue, JsError> {
        if property == "text" {
            let text = self.text_content(dom, node)?;
            return Ok(JsValue::String(text.into()));
        }
        let text = match self.hyperlink_url(dom, node)? {
            None => String::new(),
            Some(url) => match property {
                "protocol" => format!("{}:", url.scheme()),
                "username" => url.username().to_owned(),
                "password" => url.password().unwrap_or_default().to_owned(),
                "host" => host_text(&url),
                "hostname" => url.host_str().unwrap_or_default().to_owned(),
                "port" => url.port().map(|port| port.to_string()).unwrap_or_default(),
                "pathname" => url.path().to_owned(),
                "search" => url
                    .query()
                    .filter(|query| !query.is_empty())
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default(),
                "hash" => url
                    .fragment()
                    .filter(|fragment| !fragment.is_empty())
                    .map(|fragment| format!("#{fragment}"))
                    .unwrap_or_default(),
                "origin" => url.origin().ascii_serialization(),
                _ => String::new(),
            },
        };
        Ok(JsValue::String(text.into()))
    }

    /// Write a URL component of an `a` or `area` element. The setters follow the
    /// HTML "Interface `HTMLHyperlinkElementUtils`" steps in outline: a value that fails to parse for its component
    /// leaves the URL unchanged, and a successful change is stored in `href`.
    pub(in crate::runtime) fn write_hyperlink_component(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        property: &str,
        value: &str,
    ) -> Result<(), JsError> {
        if property == "text" {
            return self.set_text_content(dom, node, value.to_owned());
        }
        let Some(mut url) = self.hyperlink_url(dom, node)? else {
            return Ok(());
        };
        let changed = match property {
            "protocol" => url
                .set_scheme(value.split(':').next().unwrap_or_default())
                .is_ok(),
            "username" => url.set_username(value).is_ok(),
            "password" => url
                .set_password(Some(value).filter(|password| !password.is_empty()))
                .is_ok(),
            "host" => url.set_host(Some(value)).is_ok(),
            "hostname" => url
                .set_host(Some(value.split(':').next().unwrap_or_default()))
                .is_ok(),
            "port" => {
                let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
                if value.is_empty() {
                    url.set_port(None).is_ok()
                } else {
                    digits
                        .parse::<u16>()
                        .is_ok_and(|port| url.set_port(Some(port)).is_ok())
                }
            }
            "pathname" => {
                url.set_path(value);
                true
            }
            "search" => {
                url.set_query(
                    Some(value.strip_prefix('?').unwrap_or(value)).filter(|q| !q.is_empty()),
                );
                true
            }
            "hash" => {
                url.set_fragment(
                    Some(value.strip_prefix('#').unwrap_or(value)).filter(|f| !f.is_empty()),
                );
                true
            }
            _ => false,
        };
        if changed {
            dom.set_attribute(node, "href", url.to_string())?;
        }
        Ok(())
    }
}
