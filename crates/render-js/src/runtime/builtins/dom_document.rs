//! `DOMImplementation` (DOM §4.5), the `Document` constructor and the documents
//! a script makes with them.
//!
//! A document made by script is a document node of the page's arena with no
//! browsing context. It is its own node document, it has no window, and its
//! wrapper inherits from the interface it was made as: `Document` for
//! `new Document()`, `XMLDocument` for `createDocument`, and `HTMLDocument` for
//! `createHTMLDocument`. The realm that made it owns the wrapper.

use render_dom::{Dom, Namespace, Node, NodeId, NodeKind};

use super::dom::nullable_string_argument;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::value::ObjectHost;
use crate::{JsError, JsValue, ObjectId, RealmId};

const HTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

/// A document a script made, or a copy of one (DOM §4.5 `createHTMLDocument`,
/// `createDocument`, `cloneNode` and the `Document` constructor).
#[derive(Clone, Copy, Debug)]
pub(in crate::runtime) struct CreatedDocument {
    /// The realm that made the document, which owns its wrapper.
    pub(in crate::runtime) realm: RealmId,
    /// The interface its wrapper inherits from: `Document`, `XMLDocument` or
    /// `HTMLDocument`.
    pub(in crate::runtime) interface: &'static str,
}

impl JsRuntime {
    /// The record of `document`, if a script made it or made a copy of it.
    pub(in crate::runtime) fn created_document(&self, document: NodeId) -> Option<CreatedDocument> {
        self.created_documents.get(&document).copied()
    }

    /// The document of the realm that is running, the one `window.document` names.
    pub(in crate::runtime) fn realm_document_node(&self) -> Option<NodeId> {
        match self.realm.host(self.realm.document_object()) {
            Some(ObjectHost::Document(node)) => Some(node),
            _ => None,
        }
    }

    /// Makes an empty document of `content_type`, owned by the running realm,
    /// whose wrapper inherits from `interface`.
    pub(in crate::runtime) fn make_document(
        &mut self,
        dom: &mut Dom,
        content_type: &str,
        interface: &'static str,
    ) -> NodeId {
        let document = dom.create_document(content_type);
        let realm = self.current_realm();
        self.created_documents
            .insert(document, CreatedDocument { realm, interface });
        document
    }

    /// Charges `count` new DOM nodes against the page's node budget, the same
    /// limit the element and text creation methods observe.
    fn charge_dom_nodes(&mut self, count: usize) -> Result<(), JsError> {
        if self.dom_nodes_created.saturating_add(count) > self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        self.dom_nodes_created = self.dom_nodes_created.saturating_add(count);
        self.ensure_heap_capacity(count)
    }

    /// The `DOMImplementation` method receiver: the document it belongs to.
    pub(in crate::runtime) fn implementation_document(
        &self,
        receiver: ObjectId,
    ) -> Result<NodeId, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::DomImplementation(document)) => Ok(document),
            _ => Err(JsError::type_error(
                "incompatible DOMImplementation method receiver",
            )),
        }
    }

    /// `document.implementation` (DOM §4.5): the document's own
    /// `DOMImplementation`, made on first read. A document keeps it even when it
    /// has no browsing context.
    pub(in crate::runtime) fn document_implementation(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let document = self.require_document(receiver)?;
        let realm = self.realm_of_node(dom, document);
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.with_realm(realm, |runtime| {
            runtime.realm.implementation_wrapper(document)
        })))
    }

    /// `createHTMLDocument(title)` (DOM §4.5): an HTML document with a doctype
    /// `html`, a `html` element holding `head` (with a `title` when a title is
    /// given, even an empty one) and an empty `body`. The URL is `about:blank`.
    pub(in crate::runtime) fn create_html_document(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // The title is optional, and `undefined` is a missing argument.
        let title = match arguments.first() {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(value.to_js_string().to_lossy()),
        };
        // The document, its doctype, `html`, `head` and `body`, and given a
        // title, the `title` element and its text.
        self.charge_dom_nodes(if title.is_some() { 7 } else { 5 })?;
        let document = self.make_document(dom, "text/html", "HTMLDocument");
        let doctype = dom.create_document_type("html", "", "");
        dom.set_node_document(doctype, document);
        dom.append_child(document, doctype)?;
        let html = owned_element(dom, document, Namespace::Html, "html");
        dom.append_child(document, html)?;
        let head = owned_element(dom, document, Namespace::Html, "head");
        dom.append_child(html, head)?;
        if let Some(title) = title {
            let element = owned_element(dom, document, Namespace::Html, "title");
            let text = dom.create_text(title);
            dom.set_node_document(text, document);
            dom.append_child(element, text)?;
            dom.append_child(head, element)?;
        }
        let body = owned_element(dom, document, Namespace::Html, "body");
        dom.append_child(html, body)?;
        self.wrap_node(dom, document)
    }

    /// `createDocument(namespace, qualifiedName, doctype)` (DOM §4.5): an XML
    /// document, typed by `namespace` (XHTML, SVG, or `application/xml`), with
    /// the doctype and, for a non-empty qualified name, a root element.
    pub(in crate::runtime) fn create_document_method(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // `namespace` is a nullable DOMString, `qualifiedName` a DOMString
        // where null is the empty string, and `doctype` a DocumentType or null.
        let namespace = nullable_string_argument(arguments, 0);
        let qualified_name = match arguments.get(1) {
            None => {
                return Err(JsError::type_error(
                    "createDocument requires a qualifiedName argument",
                ));
            }
            Some(JsValue::Null) => String::new(),
            Some(value) => value.to_js_string().to_lossy(),
        };
        let doctype = match arguments.get(2) {
            None | Some(JsValue::Undefined | JsValue::Null) => None,
            Some(JsValue::Object(object)) => match self.realm.host(*object) {
                Some(ObjectHost::Node(node))
                    if matches!(
                        dom.node(node).map(Node::kind),
                        Some(NodeKind::DocumentType(_))
                    ) =>
                {
                    Some(node)
                }
                _ => {
                    return Err(JsError::type_error(
                        "createDocument: the doctype argument is not a DocumentType",
                    ));
                }
            },
            Some(_) => {
                return Err(JsError::type_error(
                    "createDocument: the doctype argument is not a DocumentType",
                ));
            }
        };
        // The root element is created as `createElementNS` would create it, so
        // a bad qualified name fails before any document exists.
        let root = if qualified_name.is_empty() {
            None
        } else {
            Some(self.validate_and_extract(namespace.as_deref(), &qualified_name)?)
        };
        let content_type = match namespace.as_deref() {
            Some(HTML_NAMESPACE) => "application/xhtml+xml",
            Some(SVG_NAMESPACE) => "image/svg+xml",
            _ => "application/xml",
        };
        self.charge_dom_nodes(usize::from(root.is_some()) + 1)?;
        let document = self.make_document(dom, content_type, "XMLDocument");
        if let Some(doctype) = doctype {
            dom.append_child(document, doctype)?;
        }
        if let Some((namespace, prefix, local)) = root {
            let element = dom.create_element_qualified(
                Namespace::from_uri(namespace.as_deref()),
                prefix,
                local,
            );
            dom.set_node_document(element, document);
            dom.append_child(document, element)?;
        }
        self.wrap_node(dom, document)
    }

    /// `new Document()` (DOM): an empty XML document with no browsing context,
    /// whose wrapper is a plain `Document`.
    pub(in crate::runtime) fn construct_document(
        &mut self,
        dom: &mut Dom,
    ) -> Result<JsValue, JsError> {
        let document = self.make_document(dom, "application/xml", "Document");
        self.wrap_node(dom, document)
    }

    /// `document.adoptNode(node)` (DOM): the node leaves its parent and becomes
    /// part of this document.
    pub(in crate::runtime) fn adopt_node_method(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let document = self.require_document(receiver)?;
        let node = self.value_as_node(required_argument(arguments, 0, "adoptNode")?)?;
        dom.adopt_node(document, node)?;
        self.wrap_node(dom, node)
    }

    /// The value of a property that a document made by script answers itself.
    /// `None` means the property reads as it does for a page's document.
    pub(in crate::runtime) fn created_document_property(
        &self,
        dom: &Dom,
        document: NodeId,
        property: &str,
    ) -> Option<JsValue> {
        self.created_document(document)?;
        Some(match property {
            // A document with no browsing context has the URL about:blank, no
            // location and no window, and it is never visible.
            "URL" | "documentURI" => JsValue::String("about:blank".to_owned().into()),
            "contentType" => JsValue::String(dom.content_type(document).to_owned().into()),
            "compatMode" => JsValue::String("CSS1Compat".to_owned().into()),
            "readyState" => JsValue::String("complete".to_owned().into()),
            "location" | "defaultView" => JsValue::Null,
            "hidden" => JsValue::Boolean(true),
            "visibilityState" => JsValue::String("hidden".to_owned().into()),
            _ => return None,
        })
    }
}

/// An element in the namespace of `document`, made in `document` (DOM §4.4).
fn owned_element(
    dom: &mut Dom,
    document: NodeId,
    namespace: Namespace,
    local_name: &str,
) -> NodeId {
    let element = dom.create_element_qualified(namespace, None, local_name);
    dom.set_node_document(element, document);
    element
}
