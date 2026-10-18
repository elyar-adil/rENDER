#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::if_not_else,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::needless_ifs,
    clippy::too_many_lines,
    clippy::wrong_self_convention
)]

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_collections::unsigned_long;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::convert::required_argument;
use crate::runtime::types::ElementRect;
use crate::utf16::JsString;
use crate::value::CollectionFilter;
use crate::value::DomNodeKind;
use crate::value::HTML_ELEMENT_INTERFACES;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_css::selector::MatchContext;
use render_css::selector::matches_selector_list;
use render_css::selector::parse_selector_list;
use render_css::selector::select_all;
use render_dom::Dom;
use render_dom::NodeId;
use render_dom::NodeKind;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_dom_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let result: Result<JsValue, JsError> = match function {
            NativeFunction::GetElementById => {
                // A document's own tree: a nested browsing context's document
                // searches its own elements, not the page's.
                let root = self.require_document(receiver)?;
                let id = required_argument(arguments, 0, "getElementById")?
                    .to_js_string()
                    .to_lossy();
                // The empty string names no element, even one with `id=""`.
                let found = if id.is_empty() {
                    None
                } else {
                    self.find_element_by_id_within(dom, root, &id)?
                };
                match found {
                    Some(node) => self.wrap_node(dom, node),
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::NonElementGetElementById => {
                let root = self.require_node(receiver)?;
                let id = required_argument(arguments, 0, "getElementById")?
                    .to_js_string()
                    .to_lossy();
                let found = if id.is_empty() {
                    None
                } else {
                    self.find_element_by_id_within(dom, root, &id)?
                };
                match found {
                    Some(node) => self.wrap_node(dom, node),
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::QuerySelector => {
                let root = self.query_root(receiver)?;
                let selector = required_argument(arguments, 0, "querySelector")?.to_js_string();
                let selectors = self.parse_selectors(&(selector).to_lossy(), "querySelector")?;
                match select_all(dom, root, &selectors, &MatchContext::default())
                    .into_iter()
                    .next()
                {
                    Some(node) => self.wrap_node(dom, node),
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::QuerySelectorAll => {
                let root = self.query_root(receiver)?;
                let selector = required_argument(arguments, 0, "querySelectorAll")?.to_js_string();
                let selectors = self.parse_selectors(&(selector).to_lossy(), "querySelectorAll")?;
                // A static NodeList: the matches at this moment, never updated.
                let nodes = select_all(dom, root, &selectors, &MatchContext::default());
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.static_node_list_wrapper(nodes)))
            }
            NativeFunction::GetElementsByTagName => {
                let root = self.query_root(receiver)?;
                let name = required_argument(arguments, 0, "getElementsByTagName")?
                    .to_js_string()
                    .to_lossy();
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.live_collection_wrapper(
                    root,
                    CollectionFilter::TagName(name),
                    "HTMLCollection",
                )))
            }
            NativeFunction::GetElementsByClassName => {
                let root = self.query_root(receiver)?;
                let names = required_argument(arguments, 0, "getElementsByClassName")?
                    .to_js_string()
                    .to_lossy();
                let tokens = names.split_ascii_whitespace().map(str::to_owned).collect();
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.live_collection_wrapper(
                    root,
                    CollectionFilter::ClassNames(tokens),
                    "HTMLCollection",
                )))
            }
            NativeFunction::CollectionItem => self.collection_item(dom, receiver, arguments),
            NativeFunction::CollectionLength => self.collection_length_native(dom, receiver),
            NativeFunction::CollectionNamedItem => {
                self.collection_named_item(dom, receiver, arguments)
            }
            NativeFunction::CharacterDataSubstringData
            | NativeFunction::CharacterDataAppendData
            | NativeFunction::CharacterDataInsertData
            | NativeFunction::CharacterDataDeleteData
            | NativeFunction::CharacterDataReplaceData => {
                self.character_data_native(dom, function, receiver, arguments)
            }
            NativeFunction::ImplementationHasFeature => Ok(JsValue::Boolean(true)),
            NativeFunction::ImplementationCreateDocument => {
                self.implementation_document(receiver)?;
                self.create_document_method(dom, arguments)
            }
            NativeFunction::ImplementationCreateHtmlDocument => {
                self.implementation_document(receiver)?;
                self.create_html_document(dom, arguments)
            }
            NativeFunction::DocumentImplementation => self.document_implementation(dom, receiver),
            NativeFunction::AdoptNode => self.adopt_node_method(dom, receiver, arguments),
            NativeFunction::NodeNormalize => self.node_normalize(dom, receiver),
            NativeFunction::TextSplitText => self.text_split_text(dom, receiver, arguments),
            NativeFunction::CreateElementNs => {
                let document = self.require_document(receiver)?;
                let namespace = nullable_string_argument(arguments, 0);
                let qualified = required_argument(arguments, 1, "createElementNS")?
                    .to_js_string()
                    .to_lossy();
                let (namespace, prefix, local) =
                    self.validate_and_extract(namespace.as_deref(), &qualified)?;
                if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
                    return Err(JsError::resource("DOM node creation limit exceeded"));
                }
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let node = dom.create_element_qualified(
                    render_dom::Namespace::from_uri(namespace.as_deref()),
                    prefix,
                    local,
                );
                dom.set_node_document(node, document);
                self.wrap_node(dom, node)
            }
            NativeFunction::SetAttributeNs => {
                let node = self.require_node(receiver)?;
                let namespace = nullable_string_argument(arguments, 0);
                let qualified = required_argument(arguments, 1, "setAttributeNS")?
                    .to_js_string()
                    .to_lossy();
                let value = required_argument(arguments, 2, "setAttributeNS")?
                    .to_js_string()
                    .to_lossy();
                let (namespace, prefix, local) =
                    self.validate_and_extract(namespace.as_deref(), &qualified)?;
                dom.set_attribute_ns(node, namespace.as_deref(), prefix.as_deref(), local, value)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::GetAttributeNs => {
                let node = self.require_node(receiver)?;
                let namespace = null_namespace_as_none(nullable_string_argument(arguments, 0));
                let local = required_argument(arguments, 1, "getAttributeNS")?
                    .to_js_string()
                    .to_lossy();
                Ok(dom
                    .attribute_ns(node, namespace.as_deref(), &local)?
                    .map_or(JsValue::Null, |value| {
                        JsValue::String((value.to_owned()).into())
                    }))
            }
            NativeFunction::HasAttributeNs => {
                let node = self.require_node(receiver)?;
                let namespace = null_namespace_as_none(nullable_string_argument(arguments, 0));
                let local = required_argument(arguments, 1, "hasAttributeNS")?
                    .to_js_string()
                    .to_lossy();
                Ok(JsValue::Boolean(
                    dom.attribute_ns(node, namespace.as_deref(), &local)?
                        .is_some(),
                ))
            }
            NativeFunction::RemoveAttributeNs => {
                let node = self.require_node(receiver)?;
                let namespace = null_namespace_as_none(nullable_string_argument(arguments, 0));
                let local = required_argument(arguments, 1, "removeAttributeNS")?
                    .to_js_string()
                    .to_lossy();
                dom.remove_attribute_ns(node, namespace.as_deref(), &local)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::GetElementsByTagNameNs => {
                let root = self.query_root(receiver)?;
                // DOM §4.2.6.2: `*` is the namespace wildcard, and an empty or
                // null namespace is the null namespace, matched by "".
                let namespace = match nullable_string_argument(arguments, 0) {
                    Some(uri) if uri == "*" => None,
                    Some(uri) if !uri.is_empty() => Some(uri),
                    _ => Some(String::new()),
                };
                let local = required_argument(arguments, 1, "getElementsByTagNameNS")?
                    .to_js_string()
                    .to_lossy();
                let local = (local != "*").then_some(local);
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.live_collection_wrapper(
                    root,
                    CollectionFilter::TagNameNs { namespace, local },
                    "HTMLCollection",
                )))
            }
            NativeFunction::CreateCdataSection => {
                self.require_document(receiver)?;
                // DOM §4.5: CDATA sections exist only in XML documents.
                Err(self.dom_exception(
                    DomExceptionName::NotSupported,
                    "CDATA sections are not supported in HTML documents",
                ))
            }
            NativeFunction::CreateProcessingInstruction => {
                let document = self.require_document(receiver)?;
                let target = required_argument(arguments, 0, "createProcessingInstruction")?
                    .to_js_string()
                    .to_lossy();
                let data = required_argument(arguments, 1, "createProcessingInstruction")?
                    .to_js_string()
                    .to_lossy();
                if !valid_xml_name(&target) || data.contains("?>") {
                    return Err(self.dom_exception(
                        DomExceptionName::InvalidCharacter,
                        format!("{target:?} is not a valid processing instruction target"),
                    ));
                }
                self.ensure_heap_capacity(1)?;
                let node = dom.create_processing_instruction(target, data);
                dom.set_node_document(node, document);
                self.wrap_node(dom, node)
            }
            NativeFunction::ImportNode => {
                let document = self.require_document(receiver)?;
                let node = self.value_as_node(required_argument(arguments, 0, "importNode")?)?;
                // DOM §4.5 `importNode`: a Document or a shadow root cannot be imported.
                // This is checked before the copy, so a refused node allocates nothing.
                if matches!(
                    dom.node(node).map(render_dom::Node::kind),
                    Some(NodeKind::Document | NodeKind::ShadowRoot(_))
                ) {
                    return Err(self.dom_exception(
                        DomExceptionName::NotSupported,
                        "a Document or shadow root node cannot be imported",
                    ));
                }
                let deep = arguments.get(1).is_some_and(JsValue::is_truthy);
                // DOM §4.5 `importNode`: a clone that is adopted into this document.
                let copy = self.clone_node_recursive(dom, node, deep)?;
                dom.adopt_node(document, copy)?;
                self.wrap_node(dom, copy)
            }
            NativeFunction::IsSameNode => {
                let node = self.require_node(receiver)?;
                let same = match arguments.first() {
                    Some(JsValue::Object(other)) => self.require_node(*other).ok() == Some(node),
                    _ => false,
                };
                Ok(JsValue::Boolean(same))
            }
            NativeFunction::ImplementationCreateDocumentType => {
                let document = self.implementation_document(receiver)?;
                let name = required_argument(arguments, 0, "createDocumentType")?
                    .to_js_string()
                    .to_lossy();
                let public_id = required_argument(arguments, 1, "createDocumentType")?
                    .to_js_string()
                    .to_lossy();
                let system_id = required_argument(arguments, 2, "createDocumentType")?
                    .to_js_string()
                    .to_lossy();
                // DOM §4.5: a qualified name that is not a valid XML name is an
                // `InvalidCharacterError`.
                if !valid_xml_name(&name) {
                    return Err(self.dom_exception(
                        DomExceptionName::InvalidCharacter,
                        format!("{name:?} is not a valid doctype name"),
                    ));
                }
                self.ensure_heap_capacity(1)?;
                let node = dom.create_document_type(name, public_id, system_id);
                dom.set_node_document(node, document);
                self.wrap_node(dom, node)
            }
            NativeFunction::CloneNode => {
                let node = self.require_node(receiver)?;
                let deep = arguments.first().is_some_and(JsValue::is_truthy);
                self.clone_node_value(dom, node, deep)
            }
            NativeFunction::CreateTextNode => {
                let document = self.require_document(receiver)?;
                let data = required_argument(arguments, 0, "createTextNode")?
                    .to_js_string()
                    .to_lossy();
                let node = dom.create_text(data);
                dom.set_node_document(node, document);
                self.wrap_node(dom, node)
            }
            NativeFunction::CreateComment => {
                let document = self.require_document(receiver)?;
                let data = required_argument(arguments, 0, "createComment")?
                    .to_js_string()
                    .to_lossy();
                let node = dom.create_comment(data);
                dom.set_node_document(node, document);
                self.wrap_node(dom, node)
            }
            NativeFunction::CreateDocumentFragment => {
                let document = self.require_document(receiver)?;
                let node = dom.create_document_fragment();
                dom.set_node_document(node, document);
                self.wrap_node(dom, node)
            }
            NativeFunction::CreateEvent => {
                self.require_document(receiver)?;
                // DOM §4.5 `createEvent("Event")` makes an uninitialized event:
                // its type is the empty string until `initEvent` sets it.
                let event = self.create_event_object("", false, false, false, false)?;
                Ok(JsValue::Object(event))
            }
            NativeFunction::GetComputedStyle => {
                let argument = required_argument(arguments, 0, "getComputedStyle")?;
                let element = self.value_as_node(argument)?;
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(
                    self.realm.style_declaration_wrapper(element),
                ))
            }
            NativeFunction::CompareDocumentPosition => {
                let node = self.require_node(receiver)?;
                let other = self.value_as_node(required_argument(
                    arguments,
                    0,
                    "compareDocumentPosition",
                )?)?;
                if node == other {
                    return Ok(JsValue::Number(0.0));
                }
                if dom_contains(dom, node, other) {
                    // `other` is inside `node` and comes after it.
                    return Ok(JsValue::Number(
                        DOCUMENT_POSITION_CONTAINED_BY + DOCUMENT_POSITION_FOLLOWING,
                    ));
                }
                if dom_contains(dom, other, node) {
                    return Ok(JsValue::Number(
                        DOCUMENT_POSITION_CONTAINS + DOCUMENT_POSITION_PRECEDING,
                    ));
                }
                Ok(JsValue::Number(DOCUMENT_POSITION_DISCONNECTED))
            }
            NativeFunction::CreateElement => {
                let document = self.require_document(receiver)?;
                let name = required_argument(arguments, 0, "createElement")?.to_js_string();
                let local_name = name.to_lossy();
                // DOM §4.5 `createElement`: an HTML document takes an HTML name and
                // lowercases it; any other document takes an XML Name and keeps its
                // case. The namespace is HTML for an HTML or XHTML document, and null
                // for every other.
                let html = dom.is_html_document(document);
                let valid = if html {
                    valid_html_local_name(&local_name)
                } else {
                    valid_xml_name(&local_name)
                };
                if !valid {
                    return Err(self.dom_exception(
                        DomExceptionName::InvalidCharacter,
                        format!("{name:?} is not a supported local name"),
                    ));
                }
                if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
                    return Err(JsError::resource("DOM node creation limit exceeded"));
                }
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let node = if html {
                    dom.create_element(local_name)
                } else {
                    let namespace = if dom.content_type(document) == "application/xhtml+xml" {
                        render_dom::Namespace::Html
                    } else {
                        render_dom::Namespace::from_uri(None)
                    };
                    dom.create_element_qualified(namespace, None, local_name)
                };
                dom.set_node_document(node, document);
                self.wrap_node(dom, node)
            }
            NativeFunction::GetAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "getAttribute")?.to_js_string();
                Ok(dom
                    .attribute(node, &(name).to_lossy())?
                    .map_or(JsValue::Null, |value| {
                        JsValue::String((value.to_owned()).into())
                    }))
            }
            NativeFunction::HasAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "hasAttribute")?.to_js_string();
                Ok(JsValue::Boolean(
                    dom.attribute(node, &(name).to_lossy())?.is_some(),
                ))
            }
            NativeFunction::RemoveAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "removeAttribute")?.to_js_string();
                dom.remove_attribute(node, &(name).to_lossy())?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::AppendChild => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "appendChild")?)?;
                dom.append_child(parent, child)?;
                self.wrap_node(dom, child)
            }
            NativeFunction::RemoveChild => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "removeChild")?)?;
                dom.remove_child(parent, child)?;
                self.wrap_node(dom, child)
            }
            NativeFunction::InsertBefore => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "insertBefore")?)?;
                let reference = match arguments.get(1) {
                    None | Some(JsValue::Null | JsValue::Undefined) => None,
                    Some(value) => Some(self.value_as_node(value)?),
                };
                dom.insert_before(parent, child, reference)?;
                self.wrap_node(dom, child)
            }
            NativeFunction::RemoveNode => {
                let node = self.require_node(receiver)?;
                if let Some(parent) = dom.parent(node) {
                    dom.remove_child(parent, node)?;
                }
                Ok(JsValue::Undefined)
            }
            NativeFunction::Contains => {
                let root = self.require_node(receiver)?;
                let candidate = self.value_as_node(required_argument(arguments, 0, "contains")?)?;
                Ok(JsValue::Boolean(dom_contains(dom, root, candidate)))
            }
            NativeFunction::AttachShadow => {
                let host = self.require_node(receiver)?;
                let init = required_argument(arguments, 0, "attachShadow")?.clone();
                let (mode, delegates_focus, slot_assignment) = self.shadow_root_init(dom, &init)?;
                let root = dom.attach_shadow(host, mode, delegates_focus, slot_assignment)?;
                self.wrap_node(dom, root)
            }
            NativeFunction::GetRootNode => {
                let node = self.require_node(receiver)?;
                let composed =
                    self.option_flag(dom, arguments.first(), "composed", "getRootNode")?;
                let root = dom.root_node(node, composed);
                self.wrap_node(dom, root)
            }
            NativeFunction::SlotAssign => {
                let slot = self.require_node(receiver)?;
                let mut nodes = Vec::with_capacity(arguments.len());
                for value in arguments {
                    let node = self.value_as_node(value)?;
                    // A slottable is an element or a text node (DOM §4.2.2.2).
                    if !is_element_node(dom, node) && !is_text_node(dom, node) {
                        return Err(JsError::type_error(
                            "assign arguments must be elements or text nodes",
                        ));
                    }
                    nodes.push(node);
                }
                dom.assign_slot_nodes(slot, &nodes)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::SlotAssignedNodes | NativeFunction::SlotAssignedElements => {
                let slot = self.require_node(receiver)?;
                let method = if function == NativeFunction::SlotAssignedElements {
                    "assignedElements"
                } else {
                    "assignedNodes"
                };
                let flatten = self.option_flag(dom, arguments.first(), "flatten", method)?;
                let elements_only = function == NativeFunction::SlotAssignedElements;
                let values = dom
                    .assigned_nodes(slot, flatten)
                    .into_iter()
                    .filter(|node| !elements_only || is_element_node(dom, *node))
                    .map(|node| self.wrap_node(dom, node))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(JsValue::Object(self.create_array_from_values(&values)?))
            }
            NativeFunction::Matches => {
                let node = self.require_node(receiver)?;
                let selector = required_argument(arguments, 0, "matches")?.to_js_string();
                let selectors = self.parse_selectors(&(selector).to_lossy(), "matches")?;
                Ok(JsValue::Boolean(matches_selector_list(
                    dom,
                    node,
                    &selectors,
                    &MatchContext::default(),
                )))
            }
            NativeFunction::Focus => {
                let node = self.require_node(receiver)?;
                self.focus_element(dom, node)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::Blur => {
                let node = self.require_node(receiver)?;
                self.blur_element(dom, node)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::Click => {
                self.require_node(receiver)?;
                let options = self.realm.create_ordinary_object();
                self.realm
                    .set_property(options, "bubbles".to_owned(), JsValue::Boolean(true));
                self.realm
                    .set_property(options, "cancelable".to_owned(), JsValue::Boolean(true));
                let event = self.event_constructor(&[
                    JsValue::String(("click".to_owned()).into()),
                    JsValue::Object(options),
                ])?;
                let _ = self.dispatch_event(dom, receiver, &[event])?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::ClassListAdd => self.class_list_add(dom, receiver, arguments),
            NativeFunction::ClassListRemove => self.class_list_remove(dom, receiver, arguments),
            NativeFunction::ClassListToggle => self.class_list_toggle(dom, receiver, arguments),
            NativeFunction::ClassListContains => self.class_list_contains(dom, receiver, arguments),
            NativeFunction::ClassListItem => self.class_list_item(dom, receiver, arguments),
            NativeFunction::ClassListReplace => self.class_list_replace(dom, receiver, arguments),
            NativeFunction::ClassListToString => self.class_list_to_string(dom, receiver),
            NativeFunction::NamedMapItem | NativeFunction::NamedMapGetNamedItem => {
                let Some(ObjectHost::NamedNodeMap(map_node)) = self.realm.host(receiver) else {
                    return Err(JsError::type_error("incompatible attributes receiver"));
                };
                let key = required_argument(arguments, 0, "item")?.to_js_string();
                let attribute = if function == NativeFunction::NamedMapItem {
                    let index_result = key.to_lossy().parse::<usize>();
                    dom.node(map_node)
                        .and_then(|node| match node.kind() {
                            NodeKind::Element(element) => {
                                element.attributes.get(index_result.unwrap_or(usize::MAX))
                            }
                            _ => None,
                        })
                        .map(|attribute| attribute.local_name.clone())
                } else {
                    dom.attribute(map_node, &(key).to_lossy())?
                        .map(|_value| key.to_lossy())
                };
                match attribute {
                    Some(name) => {
                        self.ensure_heap_capacity(1)?;
                        Ok(JsValue::Object(self.realm.attr_wrapper(map_node, name)))
                    }
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::AttrGetName | NativeFunction::AttrGetValue => {
                let (owner, name) = match self.realm.host(receiver) {
                    Some(ObjectHost::Attr { owner, name }) => (owner, name.clone()),
                    _ => return Err(JsError::type_error("incompatible Attr receiver")),
                };
                if function == NativeFunction::AttrGetName {
                    Ok(JsValue::String((name).into()))
                } else {
                    Ok(JsValue::String(
                        (dom.attribute(owner, &name)?.unwrap_or_default().to_owned()).into(),
                    ))
                }
            }
            other => self.dispatch_video_native(dom, other, receiver, arguments),
        };
        // A receiver mismatch on its own is not actionable; naming the entry
        // point turns the error into a usable diagnostic for real pages.
        result.map_err(|error| {
            let message = error.message();
            if message.starts_with("incompatible ")
                && message.ends_with(" method receiver")
                && !message.contains("entry point")
            {
                JsError::type_error(format!("{message} (entry point {function:?})"))
            } else {
                error
            }
        })
    }
}

pub(in crate::runtime) fn dom_contains(dom: &Dom, root: NodeId, candidate: NodeId) -> bool {
    let mut current = Some(candidate);
    while let Some(node) = current {
        if node == root {
            return true;
        }
        current = dom.parent(node);
    }
    false
}

pub(in crate::runtime) fn find_body_node(dom: &Dom, root: NodeId) -> Option<NodeId> {
    for child in dom.children(root).unwrap_or_default() {
        if matches!(
            dom.node(*child).map(render_dom::Node::kind),
            Some(NodeKind::Element(element))
                if element.namespace == render_dom::Namespace::Html
                    && element.local_name == "body"
        ) {
            return Some(*child);
        }
        if let Some(found) = find_body_node(dom, *child) {
            return Some(found);
        }
    }
    None
}

pub(in crate::runtime) fn is_valid_property_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '-')
}

/// Map a camelCase style member (`backgroundColor`) to its CSS property name.
pub(in crate::runtime) fn css_prop_from_member(property: &str) -> String {
    let mut mapped = String::with_capacity(property.len() + 4);
    for character in property.chars() {
        if character.is_ascii_uppercase() {
            mapped.push('-');
            mapped.push(character.to_ascii_lowercase());
        } else {
            mapped.push(character);
        }
    }
    mapped
}

/// Map a `dataset` member (`sentryConfig`) to its `data-*` attribute name
/// (`data-sentry-config`) using the `DOMStringMap` camel-case-to-attribute
/// rule. Empty property names have no attribute representation.
pub(in crate::runtime) fn dataset_attribute_from_member(property: &str) -> Option<String> {
    if property.is_empty() {
        return None;
    }
    let mut mapped = String::with_capacity(property.len() + 6);
    mapped.push_str("data-");
    for character in property.chars() {
        if character.is_ascii_uppercase() {
            mapped.push('-');
            mapped.push(character.to_ascii_lowercase());
        } else {
            mapped.push(character);
        }
    }
    Some(mapped)
}

/// The event handler names a `body` element forwards to its Window (HTML, the
/// window-reflecting body element event handler set), without the `on` prefix.
const WINDOW_REFLECTING_BODY_HANDLERS: &[&str] = &[
    "afterprint",
    "beforeprint",
    "beforeunload",
    "blur",
    "error",
    "focus",
    "hashchange",
    "languagechange",
    "load",
    "message",
    "messageerror",
    "offline",
    "online",
    "pagehide",
    "pageshow",
    "popstate",
    "rejectionhandled",
    "resize",
    "storage",
    "unhandledrejection",
    "unload",
];

/// Whether `property` on `node` is one of a `body` element's handlers that
/// forward to the Window. Reading or writing `body.onload` then reads or writes
/// the Window's own `onload`, whether or not the body is connected.
pub(in crate::runtime) fn window_reflecting_body_handler(
    dom: &Dom,
    node: NodeId,
    property: &str,
) -> bool {
    let Some(event_type) = property.strip_prefix("on") else {
        return false;
    };
    if !WINDOW_REFLECTING_BODY_HANDLERS.contains(&event_type) {
        return false;
    }
    matches!(
        dom.node(node).map(render_dom::Node::kind),
        Some(render_dom::NodeKind::Element(element))
            if element.namespace == render_dom::Namespace::Html && element.local_name == "body"
    )
}

/// The content attribute behind an element property that is not reflected by
/// the interface tables in `reflect.rs` (HTML "Reflecting content attributes in IDL attributes"). What remains here is the
/// `Element`-level `id`, the SVG and `MathML` names that have no interface table
/// yet, and the form-control and focus names, whose reflection belongs to those
/// areas. Anything an interface table reflects is looked up there first.
pub(in crate::runtime) fn node_attribute_property(property: &str) -> Option<&str> {
    match property {
        "id" => Some("id"),
        "className" => Some("class"),
        "value" => Some("value"),
        "name" => Some("name"),
        "href" => Some("href"),
        "src" => Some("src"),
        "width" => Some("width"),
        "height" => Some("height"),
        "alt" => Some("alt"),
        "role" => Some("role"),
        "type" => Some("type"),
        "placeholder" => Some("placeholder"),
        "action" => Some("action"),
        "method" => Some("method"),
        "target" => Some("target"),
        "tabIndex" => Some("tabindex"),
        "disabled" => Some("disabled"),
        "checked" => Some("checked"),
        "selected" => Some("selected"),
        "readOnly" => Some("readonly"),
        "required" => Some("required"),
        "multiple" => Some("multiple"),
        "autofocus" | "autoFocus" => Some("autofocus"),
        _ => None,
    }
}

pub(in crate::runtime) fn node_boolean_property(property: &str) -> bool {
    matches!(
        property,
        "disabled"
            | "checked"
            | "selected"
            | "readOnly"
            | "required"
            | "multiple"
            | "autofocus"
            | "autoFocus"
    )
}

/// The `type` IDL attribute of an HTML element whose value is not a plain
/// reflection of its content attribute.
///
/// * `button` (HTML §4.10.6.3): an ASCII-case-insensitive keyword, else
///   `submit`, which is also the value for a missing or invalid attribute.
/// * `input` (HTML §4.10.5.1.1): an ASCII-case-insensitive keyword, else `text`.
/// * `textarea`, `output` and `fieldset` (HTML §4.10.11, §4.10.13, §4.10.15):
///   fixed values that ignore the attribute, since they have no `type` content
///   attribute of their own.
///
/// `None` for every other element, whose `type` reflects its attribute.
pub(in crate::runtime) fn html_element_type(
    local_name: &str,
    attribute: Option<&str>,
) -> Option<String> {
    const INPUT_TYPES: &[&str] = &[
        "hidden",
        "text",
        "search",
        "tel",
        "url",
        "email",
        "password",
        "date",
        "month",
        "week",
        "time",
        "datetime-local",
        "number",
        "range",
        "color",
        "checkbox",
        "radio",
        "file",
        "submit",
        "image",
        "reset",
        "button",
    ];
    let keyword = |allowed: &[&str], default: &str| {
        attribute
            .map(str::to_ascii_lowercase)
            .filter(|value| allowed.contains(&value.as_str()))
            .unwrap_or_else(|| default.to_owned())
    };
    match local_name {
        "button" => Some(keyword(&["submit", "reset", "button"], "submit")),
        "input" => Some(keyword(INPUT_TYPES, "text")),
        "textarea" => Some("textarea".to_owned()),
        "output" => Some("output".to_owned()),
        "fieldset" => Some("fieldset".to_owned()),
        _ => None,
    }
}

/// `DocumentPosition` bitmask values for `compareDocumentPosition`.
pub(in crate::runtime) const DOCUMENT_POSITION_DISCONNECTED: f64 = 1.0;

pub(in crate::runtime) const DOCUMENT_POSITION_PRECEDING: f64 = 2.0;

pub(in crate::runtime) const DOCUMENT_POSITION_FOLLOWING: f64 = 4.0;

pub(in crate::runtime) const DOCUMENT_POSITION_CONTAINS: f64 = 8.0;

pub(in crate::runtime) const DOCUMENT_POSITION_CONTAINED_BY: f64 = 16.0;

/// Source shape of cloned nodes in `clone_node_recursive`.
pub(in crate::runtime) enum CloneSource {
    Element {
        local_name: String,
        attributes: Vec<(String, String)>,
    },
    Text(String),
    Comment(String),
    Fragment,
    /// A copy of a document has its content type and the interface it was made as.
    Document {
        content_type: String,
        interface: &'static str,
    },
    DocumentType {
        name: String,
        public_id: String,
        system_id: String,
    },
}

/// A nullable `DOMString` argument (`WebIDL` §3.2.12): `null` and `undefined`
/// are `None`, anything else is its string value.
pub(super) fn nullable_string_argument(arguments: &[JsValue], index: usize) -> Option<String> {
    match arguments.get(index) {
        None | Some(JsValue::Null | JsValue::Undefined) => None,
        Some(value) => Some(value.to_js_string().to_lossy()),
    }
}

/// DOM §4.9.2: an empty namespace argument means the null namespace.
fn null_namespace_as_none(namespace: Option<String>) -> Option<String> {
    namespace.filter(|uri| !uri.is_empty())
}

/// The XML namespace (Namespaces in XML 1.0 §3) and the XMLNS namespace.
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// Whether `name` is a valid XML `Name` (XML 1.0 §2.3), as the DOM requires of
/// a doctype name and of element names (DOM §4.5, §4.9). Non-ASCII characters
/// are accepted as name characters, so this is an ASCII-accurate check.
pub(in crate::runtime) fn valid_xml_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters.next().is_some_and(|character| {
        character.is_alphabetic() || matches!(character, '_' | ':') || !character.is_ascii()
    }) && characters.all(|character| {
        character.is_alphanumeric()
            || matches!(character, '_' | ':' | '-' | '.' | '\u{b7}')
            || !character.is_ascii()
    })
}

pub(in crate::runtime) fn valid_html_local_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || matches!(character, '_' | ':'))
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '-' | '.')
        })
}

impl JsRuntime {
    pub(in crate::runtime) fn image_constructor(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        self.ensure_heap_capacity(1)?;
        self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
        let image = dom.create_element("img");
        // An image made by script belongs to the document of the realm it is made in.
        if let Some(document) = self.realm_document_node() {
            dom.set_node_document(image, document);
        }
        for (index, name) in [(0, "width"), (1, "height")] {
            if let Some(value) = arguments.get(index)
                && !matches!(value, JsValue::Undefined)
            {
                let number = self.to_number_value(dom, value)?.max(0.0).floor();
                dom.set_attribute(image, name, number.to_string())?;
            }
        }
        self.wrap_node(dom, image)
    }

    pub(in crate::runtime) fn element_rect_value(&mut self, node: NodeId) -> JsValue {
        let mut rect = self
            .element_geometry
            .get(&node.as_u64())
            .copied()
            .unwrap_or(ElementRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            });
        rect.x -= self.viewport.x;
        rect.y -= self.viewport.y;
        self.rect_value(rect)
    }

    pub(in crate::runtime) fn rect_value(&mut self, rect: ElementRect) -> JsValue {
        let object = self.realm.create_ordinary_object();
        for (name, value) in [
            ("x", rect.x),
            ("y", rect.y),
            ("width", rect.width),
            ("height", rect.height),
            ("top", rect.y),
            ("right", rect.x + rect.width),
            ("bottom", rect.y + rect.height),
            ("left", rect.x),
        ] {
            self.realm
                .set_property(object, name.to_owned(), JsValue::Number(f64::from(value)));
        }
        JsValue::Object(object)
    }

    /// Parse an `innerHTML` fragment and replace `target`'s children with it.
    ///
    /// The source is parsed through the ordinary HTML parser (body context is
    /// approximated), then imported node by node into a detached scratch
    /// parent so limits apply. Only once every node copied cleanly are the
    /// target's original children replaced; a failed import leaves the DOM
    /// untouched.
    ///
    /// # Errors
    ///
    /// Returns resource-limit errors when importing exceeds the configured
    /// DOM-node budget, and DOM errors when splicing fails.
    pub(in crate::runtime) fn set_inner_html(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        source: &str,
    ) -> Result<(), JsError> {
        let scratch = render_html::parse_document(source);
        let body = find_body_node(&scratch.dom, scratch.dom.document())
            .ok_or_else(|| JsError::dom("fragment parsing produced no body"))?;

        let staging = dom.create_element("fragment");
        for child in scratch.dom.children(body).unwrap_or_default().to_vec() {
            self.import_dom_subtree(dom, &scratch.dom, child, staging, true)?;
        }

        let old_children = dom.children(target).unwrap_or_default().to_vec();
        for child in old_children {
            dom.remove_child(target, child)?;
        }
        while let Some(child) = dom.children(staging).and_then(<[NodeId]>::first).copied() {
            dom.insert_before(target, child, None)?;
        }
        Ok(())
    }

    /// Parse an `outerHTML` fragment and replace `target` itself with it.
    ///
    /// Replacing a parentless node (or the document element) is rejected the
    /// way the platform rejects it, before any mutation happens.
    pub(in crate::runtime) fn set_outer_html(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        source: &str,
    ) -> Result<(), JsError> {
        if target == dom.document() {
            return Err(self.dom_exception(
                DomExceptionName::NoModificationAllowed,
                "outerHTML cannot replace the document node",
            ));
        }
        let Some(parent) = dom.parent(target) else {
            return Err(self.dom_exception(
                DomExceptionName::NoModificationAllowed,
                "outerHTML requires a parent to splice into",
            ));
        };
        let scratch = render_html::parse_document(source);
        let body = find_body_node(&scratch.dom, scratch.dom.document())
            .ok_or_else(|| JsError::dom("fragment parsing produced no body"))?;

        let staging = dom.create_element("fragment");
        for child in scratch.dom.children(body).unwrap_or_default().to_vec() {
            self.import_dom_subtree(dom, &scratch.dom, child, staging, true)?;
        }

        let next_sibling = dom.next_sibling(target);
        dom.remove_child(parent, target)?;
        while let Some(child) = dom.children(staging).and_then(<[NodeId]>::first).copied() {
            dom.insert_before(parent, child, next_sibling)?;
        }
        Ok(())
    }

    /// `node.cloneNode(deep)`: structural copy inside the same arena.
    pub(in crate::runtime) fn clone_node_value(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        deep: bool,
    ) -> Result<JsValue, JsError> {
        let copy = self.clone_node_recursive(dom, node, deep)?;
        self.wrap_node(dom, copy)
    }

    pub(in crate::runtime) fn clone_node_recursive(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        deep: bool,
    ) -> Result<NodeId, JsError> {
        if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        let source = match dom.node(node).map(render_dom::Node::kind) {
            Some(NodeKind::Element(element)) => CloneSource::Element {
                local_name: element.local_name.clone(),
                attributes: element
                    .attributes
                    .iter()
                    .map(|attribute| (attribute.local_name.clone(), attribute.value.clone()))
                    .collect(),
            },
            Some(NodeKind::Text(data)) => CloneSource::Text(data.clone()),
            Some(NodeKind::Comment(data)) => CloneSource::Comment(data.clone()),
            Some(NodeKind::DocumentFragment) => CloneSource::Fragment,
            Some(NodeKind::Document) => CloneSource::Document {
                content_type: dom.content_type(node).to_owned(),
                interface: self
                    .created_document(node)
                    .map_or("HTMLDocument", |created| created.interface),
            },
            Some(NodeKind::DocumentType(doctype)) => CloneSource::DocumentType {
                name: doctype.name.clone(),
                public_id: doctype.public_id.clone(),
                system_id: doctype.system_id.clone(),
            },
            // Every other node kind this engine models is cloneable here, and a
            // shadow root is refused (DOM §4.5 `cloneNode`).
            _ => {
                return Err(self.dom_exception(
                    DomExceptionName::NotSupported,
                    "this node type cannot be cloned here",
                ));
            }
        };
        let is_document = matches!(source, CloneSource::Document { .. });
        let copy = match source {
            CloneSource::Element {
                local_name,
                attributes,
            } => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let copy = dom.create_element(local_name);
                for (name, value) in attributes {
                    dom.set_attribute(copy, name, value)?;
                }
                copy
            }
            CloneSource::Text(data) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_text(data)
            }
            CloneSource::Comment(data) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_comment(data)
            }
            CloneSource::Fragment => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_document_fragment()
            }
            CloneSource::Document {
                content_type,
                interface,
            } => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                self.make_document(dom, &content_type, interface)
            }
            CloneSource::DocumentType {
                name,
                public_id,
                system_id,
            } => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_document_type(name, public_id, system_id)
            }
        };
        // A copy is in the node document of the node it copies. A document copy
        // is its own node document, which `make_document` set.
        if let (false, Some(owner)) = (is_document, dom.node_document(node)) {
            dom.set_node_document(copy, owner);
        }
        let children = if deep {
            dom.children(node).unwrap_or_default().to_vec()
        } else {
            Vec::new()
        };
        for child in children {
            let child_copy = self.clone_node_recursive(dom, child, true)?;
            dom.append_child(copy, child_copy)?;
        }
        Ok(copy)
    }

    pub(in crate::runtime) fn import_dom_subtree(
        &mut self,
        target: &mut Dom,
        source: &Dom,
        node: NodeId,
        parent: NodeId,
        deep: bool,
    ) -> Result<(), JsError> {
        if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        let imported = match source.node(node).map(render_dom::Node::kind) {
            Some(NodeKind::Element(element)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let copy = target.create_element(element.local_name.clone());
                for attribute in &element.attributes {
                    target.set_attribute(
                        copy,
                        attribute.local_name.clone(),
                        attribute.value.clone(),
                    )?;
                }
                copy
            }
            Some(NodeKind::Text(data)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                target.create_text(data.clone())
            }
            Some(NodeKind::Comment(data)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                target.create_comment(data.clone())
            }
            // DocumentType / ProcessingInstruction nodes have no meaning
            // inside an element fragment.
            _ => return Ok(()),
        };
        target.append_child(parent, imported)?;
        if deep {
            for child in source.children(node).unwrap_or_default().to_vec() {
                self.import_dom_subtree(target, source, child, imported, true)?;
            }
        }
        Ok(())
    }

    /// The DOM Standard's "parse a selector" step, shared by every API that
    /// takes one.
    ///
    /// Selectors §"parse a selector" is a single step that either yields a
    /// selector list or fails, and each API that calls it says what to do on
    /// failure: `querySelector`/`querySelectorAll`/`matches`/`closest`/
    /// `getElementsByTagName` all say "throw a `SyntaxError` `DOMException`".
    /// One helper rather than five keeps them from drifting, and it is also the
    /// only way to get `e.name === "SyntaxError"` and
    /// `e instanceof DOMException` to agree: `WebIDL` §2.8.1 warns that this
    /// `SyntaxError` is "used to report parsing errors in web APIs, for example
    /// when parsing selectors, while the JavaScript `SyntaxError` is reserved for
    /// the JavaScript parser", so `instanceof SyntaxError` must be **false** here
    /// even though the name says `SyntaxError`.
    pub(in crate::runtime) fn parse_selectors(
        &mut self,
        selector: &str,
        function: &str,
    ) -> Result<render_css::selector::SelectorList, JsError> {
        parse_selector_list(selector).map_err(|error| {
            self.dom_exception(
                DomExceptionName::Syntax,
                format!("{function}: {selector:?} is not a valid selector: {error}"),
            )
        })
    }

    pub(in crate::runtime) fn query_root(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Document(document) | ObjectHost::Node(document)) => Ok(document),
            _ => Err(JsError::type_error(
                "querySelector method called on a non-Document/non-Element object",
            )),
        }
    }

    pub(in crate::runtime) fn find_element_by_tag(
        &mut self,
        dom: &Dom,
        root: NodeId,
        tag: &str,
    ) -> Result<Option<NodeId>, JsError> {
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            self.consume_step()?;
            if let Some(NodeKind::Element(data)) = dom.node(node).map(render_dom::Node::kind)
                && data.local_name.eq_ignore_ascii_case(tag)
            {
                return Ok(Some(node));
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        }
        Ok(None)
    }

    /// DOM §4.9.1 "validate and extract": the namespace (`""` is null), prefix
    /// and local name for a qualified name in a namespace. A malformed name is
    /// an `InvalidCharacterError`, and a prefix and namespace that disagree are
    /// a `NamespaceError`.
    pub(in crate::runtime) fn validate_and_extract(
        &mut self,
        namespace: Option<&str>,
        qualified_name: &str,
    ) -> Result<(Option<String>, Option<String>, String), JsError> {
        let namespace = namespace.filter(|uri| !uri.is_empty());
        let (prefix, local) = match qualified_name.split_once(':') {
            Some((prefix, local)) => (Some(prefix), local),
            None => (None, qualified_name),
        };
        let well_formed = valid_xml_name(local)
            && !local.contains(':')
            && prefix.is_none_or(|prefix| valid_xml_name(prefix) && !prefix.contains(':'));
        if !well_formed {
            return Err(self.dom_exception(
                DomExceptionName::InvalidCharacter,
                format!("{qualified_name:?} is not a valid qualified name"),
            ));
        }
        let xmlns_name = prefix == Some("xmlns") || qualified_name == "xmlns";
        let namespace_error = (prefix.is_some() && namespace.is_none())
            || (prefix == Some("xml") && namespace != Some(XML_NAMESPACE))
            || (namespace == Some(XML_NAMESPACE)
                && prefix != Some("xml")
                && qualified_name != "xml")
            || (xmlns_name != (namespace == Some(XMLNS_NAMESPACE)));
        if namespace_error {
            return Err(self.dom_exception(
                DomExceptionName::Namespace,
                format!("{qualified_name:?} does not match its namespace"),
            ));
        }
        Ok((
            namespace.map(str::to_owned),
            prefix.map(str::to_owned),
            local.to_owned(),
        ))
    }
    /// `document.title` (HTML "The title element"): the text of the first `title` element in
    /// tree order, with ASCII whitespace stripped and collapsed; "" when there is
    /// no title element.
    pub(in crate::runtime) fn document_title(
        &mut self,
        dom: &Dom,
        document: NodeId,
    ) -> Result<String, JsError> {
        Ok(match self.find_element_by_tag(dom, document, "title")? {
            Some(title) => self
                .text_content(dom, title)?
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            None => String::new(),
        })
    }

    /// Assign `document.title`: the first `title` element's text is replaced. With
    /// no title element, a new one is appended to `head`, and a document without a
    /// head is left unchanged.
    pub(in crate::runtime) fn set_document_title(
        &mut self,
        dom: &mut Dom,
        document: NodeId,
        text: String,
    ) -> Result<(), JsError> {
        let title = if let Some(title) = self.find_element_by_tag(dom, document, "title")? {
            title
        } else {
            let Some(head) = self.find_element_by_tag(dom, document, "head")? else {
                return Ok(());
            };
            let title = dom.create_element("title");
            dom.append_child(head, title)?;
            title
        };
        self.set_text_content(dom, title, text)
    }

    /// The token set of an element's `class` attribute: the ordered set of its
    /// ASCII-whitespace-separated tokens, first occurrence wins (DOM §4.7).
    pub(in crate::runtime) fn class_list_tokens(
        dom: &Dom,
        node: NodeId,
    ) -> Result<Vec<String>, JsError> {
        let value = Self::class_list_value(dom, node)?;
        let mut tokens: Vec<String> = Vec::new();
        for token in value.split_ascii_whitespace() {
            if !tokens.iter().any(|existing| existing == token) {
                tokens.push(token.to_owned());
            }
        }
        Ok(tokens)
    }

    /// The literal `class` attribute value, or the empty string when absent.
    pub(in crate::runtime) fn class_list_value(dom: &Dom, node: NodeId) -> Result<String, JsError> {
        Ok(dom.attribute(node, "class")?.unwrap_or_default().to_owned())
    }

    /// `dataset.member` read: the value of the mapped `data-*` attribute.
    ///
    /// `None` means the member is not visible (no matching attribute, or the
    /// property name has no `data-*` representation) and the ordinary
    /// prototype-chain lookup should answer instead.
    pub(in crate::runtime) fn dataset_member_attribute_value(
        dom: &Dom,
        node: NodeId,
        property: &str,
    ) -> Result<Option<String>, JsError> {
        let Some(name) = dataset_attribute_from_member(property) else {
            return Ok(None);
        };
        Ok(dom.attribute(node, &name)?.map(str::to_owned))
    }

    /// `dataset.member = value` write-through: stores a stringified copy in
    /// the mapped `data-*` attribute so `getAttribute` observes the write.
    pub(in crate::runtime) fn set_dataset_member(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        property: &str,
        value: &JsValue,
    ) -> Result<(), JsError> {
        let Some(name) = dataset_attribute_from_member(property) else {
            return Err(self.dom_exception(
                DomExceptionName::Syntax,
                format!("{property:?} is not a valid dataset property name"),
            ));
        };
        Ok(dom.set_attribute(node, &name, value.to_js_string().to_lossy())?)
    }

    /// `delete dataset.member`: removes the mapped `data-*` attribute and
    /// reports whether it existed.
    pub(in crate::runtime) fn delete_dataset_member(
        dom: &mut Dom,
        node: NodeId,
        property: &str,
    ) -> Result<bool, JsError> {
        let Some(name) = dataset_attribute_from_member(property) else {
            return Ok(false);
        };
        if dom.attribute(node, &name)?.is_some() {
            dom.remove_attribute(node, &name)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(in crate::runtime) fn require_class_list(
        &self,
        object: ObjectId,
    ) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::ClassList(node)) => Ok(node),
            _ => Err(JsError::type_error("incompatible DOMTokenList receiver")),
        }
    }

    /// The `DOMTokenList` argument check: DOM Standard §"validate and extract"
    /// throws an `InvalidCharacterError` `DOMException` "if token is the empty
    /// string or contains ASCII whitespace".
    pub(in crate::runtime) fn class_list_token(
        &mut self,
        arguments: &[JsValue],
        index: usize,
        function: &str,
    ) -> Result<String, JsError> {
        let token = required_argument(arguments, index, function)?
            .to_js_string()
            .to_lossy();
        if token.is_empty()
            || token
                .chars()
                .any(|character| character.is_ascii_whitespace())
        {
            return Err(self.dom_exception(
                DomExceptionName::InvalidCharacter,
                format!("{function} token must be non-empty and contain no ASCII whitespace"),
            ));
        }
        Ok(token)
    }

    /// DOM §4.7 "update steps": write the serialized token set back to the
    /// `class` attribute. The steps run after every change the specification
    /// makes, so the attribute is normalised even when a call changes nothing.
    fn class_list_update(dom: &mut Dom, node: NodeId, tokens: &[String]) -> Result<(), JsError> {
        if tokens.is_empty() && dom.attribute(node, "class")?.is_none() {
            return Ok(());
        }
        dom.set_attribute(node, "class", tokens.join(" "))?;
        Ok(())
    }

    pub(in crate::runtime) fn class_list_add(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let mut tokens = Self::class_list_tokens(dom, node)?;
        // Every token is validated before the attribute is written, so a
        // rejected token leaves the attribute untouched.
        for index in 0..arguments.len() {
            let token = self.class_list_token(arguments, index, "classList.add")?;
            if !tokens.contains(&token) {
                tokens.push(token);
            }
        }
        Self::class_list_update(dom, node, &tokens)?;
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn class_list_remove(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let mut tokens = Self::class_list_tokens(dom, node)?;
        for index in 0..arguments.len() {
            let token = self.class_list_token(arguments, index, "classList.remove")?;
            tokens.retain(|candidate| candidate != &token);
        }
        Self::class_list_update(dom, node, &tokens)?;
        Ok(JsValue::Undefined)
    }

    /// DOM §4.7 `toggle(token, force)`. An omitted or `undefined` `force` is
    /// "not given", and the update steps run only when the set changed.
    pub(in crate::runtime) fn class_list_toggle(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let token = self.class_list_token(arguments, 0, "classList.toggle")?;
        let mut tokens = Self::class_list_tokens(dom, node)?;
        let present = tokens.contains(&token);
        let keep = match arguments.get(1) {
            None | Some(JsValue::Undefined) => !present,
            Some(force) => force.is_truthy(),
        };
        if keep != present {
            if keep {
                tokens.push(token);
            } else {
                tokens.retain(|candidate| candidate != &token);
            }
            Self::class_list_update(dom, node, &tokens)?;
        }
        Ok(JsValue::Boolean(keep))
    }

    /// DOM §4.7 `replace(token, newToken)`: swap `token` for `newToken` at its
    /// position, keeping the set free of duplicates, and report whether
    /// `token` was present.
    pub(in crate::runtime) fn class_list_replace(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let token = self.class_list_token(arguments, 0, "classList.replace")?;
        let new_token = self.class_list_token(arguments, 1, "classList.replace")?;
        let tokens = Self::class_list_tokens(dom, node)?;
        let Some(position) = tokens.iter().position(|candidate| candidate == &token) else {
            return Ok(JsValue::Boolean(false));
        };
        let mut replaced: Vec<String> = Vec::with_capacity(tokens.len());
        for (index, candidate) in tokens.into_iter().enumerate() {
            if index == position {
                if !replaced.contains(&new_token) {
                    replaced.push(new_token.clone());
                }
            } else if candidate != token && candidate != new_token {
                replaced.push(candidate);
            }
        }
        Self::class_list_update(dom, node, &replaced)?;
        Ok(JsValue::Boolean(true))
    }

    pub(in crate::runtime) fn class_list_contains(
        &mut self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let token = self.class_list_token(arguments, 0, "classList.contains")?;
        Ok(JsValue::Boolean(
            Self::class_list_tokens(dom, node)?
                .iter()
                .any(|candidate| candidate == &token),
        ))
    }

    pub(in crate::runtime) fn class_list_item(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let index = unsigned_long(
            self.to_number_value(dom, required_argument(arguments, 0, "classList.item")?)?,
        ) as usize;
        Ok(Self::class_list_tokens(dom, node)?
            .get(index)
            .cloned()
            .map_or(JsValue::Null, |token| {
                JsValue::String(JsString::from(token))
            }))
    }

    /// The `DOMTokenList` stringifier is its `value`: the attribute's literal
    /// value, not the serialized token set (DOM §4.7).
    pub(in crate::runtime) fn class_list_to_string(
        &self,
        dom: &Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        Ok(JsValue::String((Self::class_list_value(dom, node)?).into()))
    }

    /// The first element, in tree order, among the descendants of `root` whose
    /// `id` is `id` (DOM §4.2.6.2 `getElementById`). `root` itself is excluded.
    pub(in crate::runtime) fn find_element_by_id_within(
        &mut self,
        dom: &Dom,
        root: NodeId,
        id: &str,
    ) -> Result<Option<NodeId>, JsError> {
        let mut pending: Vec<NodeId> = dom
            .children(root)
            .unwrap_or_default()
            .iter()
            .rev()
            .copied()
            .collect();
        while let Some(node) = pending.pop() {
            self.consume_step()?;
            if matches!(
                dom.node(node).map(render_dom::Node::kind),
                Some(NodeKind::Element(_))
            ) && dom.attribute(node, "id")? == Some(id)
            {
                return Ok(Some(node));
            }
            if let Some(children) = dom.children(node) {
                pending.extend(children.iter().rev().copied());
            }
        }
        Ok(None)
    }

    pub(in crate::runtime) fn text_content(
        &mut self,
        dom: &Dom,
        node: NodeId,
    ) -> Result<String, JsError> {
        let Some(root) = dom.node(node) else {
            return Err(JsError::dom("DOM wrapper refers to an unknown node"));
        };
        if let NodeKind::Text(data) | NodeKind::Comment(data) = root.kind() {
            return Ok(data.clone());
        }
        let mut result = String::new();
        let mut pending = root.children().iter().rev().copied().collect::<Vec<_>>();
        while let Some(candidate) = pending.pop() {
            self.consume_step()?;
            let Some(current) = dom.node(candidate) else {
                continue;
            };
            if let NodeKind::Text(data) = current.kind() {
                result.push_str(data);
            }
            pending.extend(current.children().iter().rev());
        }
        Ok(result)
    }

    pub(in crate::runtime) fn set_text_content(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        value: String,
    ) -> Result<(), JsError> {
        let kind = dom
            .node(node)
            .map(render_dom::Node::kind)
            .ok_or_else(|| JsError::dom("DOM wrapper refers to an unknown node"))?;
        if matches!(
            kind,
            NodeKind::Text(_) | NodeKind::Comment(_) | NodeKind::ProcessingInstruction { .. }
        ) {
            dom.set_character_data(node, value)?;
            return Ok(());
        }
        if !matches!(
            kind,
            NodeKind::Element(_) | NodeKind::DocumentFragment | NodeKind::ShadowRoot(_)
        ) {
            return Ok(());
        }
        let children = dom.children(node).unwrap_or_default().to_vec();
        for child in children {
            self.consume_step()?;
            dom.remove_child(node, child)?;
        }
        if !value.is_empty() {
            if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
                return Err(JsError::resource("DOM node creation limit exceeded"));
            }
            self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
            let text = dom.create_text(value);
            dom.append_child(node, text)?;
        }
        Ok(())
    }

    /// One boolean member of an options dictionary (`WebIDL` §3.10.23): `undefined`
    /// and `null` leave the dictionary empty, so the member is `false`; any other
    /// non-object is a conversion failure.
    pub(in crate::runtime) fn option_flag(
        &mut self,
        dom: &mut Dom,
        options: Option<&JsValue>,
        member: &str,
        method: &str,
    ) -> Result<bool, JsError> {
        match options {
            None | Some(JsValue::Null | JsValue::Undefined) => Ok(false),
            Some(JsValue::Object(object)) => {
                let object = *object;
                Ok(self.get_member(dom, object, member)?.is_truthy())
            }
            Some(_) => Err(JsError::type_error(format!(
                "{method} options must be an object"
            ))),
        }
    }

    /// The `ShadowRootInit` dictionary of `attachShadow` (DOM §4.2.2): the
    /// required `mode`, and the `delegatesFocus` and `slotAssignment` members.
    /// `clonable`, `serializable` and `customElementRegistry` are not
    /// implemented, so they are not read.
    pub(in crate::runtime) fn shadow_root_init(
        &mut self,
        dom: &mut Dom,
        init: &JsValue,
    ) -> Result<
        (
            render_dom::ShadowRootMode,
            bool,
            render_dom::SlotAssignmentMode,
        ),
        JsError,
    > {
        let object = match init {
            JsValue::Object(object) => *object,
            JsValue::Undefined | JsValue::Null => {
                return Err(JsError::type_error(
                    "attachShadow requires a ShadowRootInit with a mode",
                ));
            }
            _ => {
                return Err(JsError::type_error(
                    "attachShadow init must be a ShadowRootInit object",
                ));
            }
        };
        let mode_value = self.get_member(dom, object, "mode")?;
        let mode = match mode_value.to_js_string().to_lossy().as_str() {
            "open" => render_dom::ShadowRootMode::Open,
            "closed" => render_dom::ShadowRootMode::Closed,
            _ => {
                return Err(JsError::type_error(
                    "ShadowRootMode must be \"open\" or \"closed\"",
                ));
            }
        };
        let delegates_focus = self.get_member(dom, object, "delegatesFocus")?.is_truthy();
        let slot_value = self.get_member(dom, object, "slotAssignment")?;
        let slot_assignment = match slot_value {
            JsValue::Undefined => render_dom::SlotAssignmentMode::Named,
            value => match value.to_js_string().to_lossy().as_str() {
                "manual" => render_dom::SlotAssignmentMode::Manual,
                "named" => render_dom::SlotAssignmentMode::Named,
                _ => {
                    return Err(JsError::type_error(
                        "SlotAssignmentMode must be \"manual\" or \"named\"",
                    ));
                }
            },
        };
        Ok((mode, delegates_focus, slot_assignment))
    }

    pub(in crate::runtime) fn require_document(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Document(document)) => Ok(document),
            _ => Err(JsError::type_error("incompatible Document method receiver")),
        }
    }

    pub(in crate::runtime) fn require_node(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            // Document is a Node in the DOM spec; wrappers host it under its
            // own variant, and real pages pass `document` to Node-taking
            // APIs such as `MutationObserver.prototype.observe`.
            Some(ObjectHost::Node(node) | ObjectHost::Document(node)) => Ok(node),
            _ => Err(JsError::type_error("incompatible Node method receiver")),
        }
    }

    /// Strict "Type(value) is Object" test for the operations that genuinely
    /// reject non-objects: `Proxy`/`new Proxy` targets, callable callback
    /// arguments, `in` right-hand sides, `instanceof` right-hand sides, DOM
    /// node receivers, and the entries of a `Map` constructor iterable.
    ///
    /// Operations whose spec text starts with `ToObject` must not use this;
    /// they call [`JsRuntime::to_object`], which boxes primitives.
    pub(in crate::runtime) fn require_object(value: &JsValue) -> Result<ObjectId, JsError> {
        match value {
            JsValue::Object(object) => Ok(*object),
            JsValue::Null | JsValue::Undefined => Err(JsError::type_error(
                "cannot access a property of null or undefined",
            )),
            _ => Err(JsError::type_error("value is not an object")),
        }
    }

    pub(in crate::runtime) fn value_as_node(&self, value: &JsValue) -> Result<NodeId, JsError> {
        let JsValue::Object(object) = value else {
            return Err(JsError::type_error("argument is not a Node"));
        };
        self.require_node(*object)
    }

    /// `new Text(data)`, `new Comment(data)`, `new DocumentFragment()`.
    pub(in crate::runtime) fn construct_dom_node(
        &mut self,
        dom: &mut Dom,
        kind: DomNodeKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
        let data = arguments
            .first()
            .filter(|value| !matches!(value, JsValue::Undefined))
            .map(JsValue::to_js_string)
            .unwrap_or_default();
        let node = match kind {
            DomNodeKind::Text => dom.create_text(data.to_lossy()),
            DomNodeKind::Comment => dom.create_comment(data.to_lossy()),
            DomNodeKind::Fragment => dom.create_document_fragment(),
            DomNodeKind::Document => return self.construct_document(dom),
        };
        // DOM §4.4: a node made by a constructor belongs to the document of the
        // global object it is made in, which is the running realm's document.
        if let Some(document) = self.realm_document_node() {
            dom.set_node_document(node, document);
        }
        self.wrap_node(dom, node)
    }

    pub(in crate::runtime) fn wrap_node(
        &mut self,
        dom: &Dom,
        node: NodeId,
    ) -> Result<JsValue, JsError> {
        // A wrapper belongs to the realm of the document that owns its node, so a
        // node of a nested document gets that document's prototypes and identity.
        let owner = self.realm_of_node(dom, node);
        if owner != self.realm.current_realm() {
            return self.with_realm(owner, |runtime| runtime.wrap_node(dom, node));
        }
        if matches!(
            dom.node(node).map(render_dom::Node::kind),
            Some(NodeKind::Document)
        ) {
            if self.realm_document_node() == Some(node) {
                return Ok(JsValue::Object(self.realm.document_object()));
            }
            // Another document, made by script or a copy of one, has its own wrapper.
            self.ensure_heap_capacity(1)?;
            let interface = self
                .created_document(node)
                .map_or("HTMLDocument", |created| created.interface);
            return Ok(JsValue::Object(
                self.realm.document_wrapper(node, interface),
            ));
        }
        self.ensure_heap_capacity(1)?;
        let interface = dom_interface_name(dom, node);
        Ok(JsValue::Object(self.realm.node_wrapper(node, interface)))
    }
}

/// Whether `node` is an element, whatever its namespace.
pub(in crate::runtime) fn is_element_node(dom: &Dom, node: NodeId) -> bool {
    matches!(
        dom.node(node).map(render_dom::Node::kind),
        Some(NodeKind::Element(_))
    )
}

/// Whether `node` is a text node.
pub(in crate::runtime) fn is_text_node(dom: &Dom, node: NodeId) -> bool {
    matches!(
        dom.node(node).map(render_dom::Node::kind),
        Some(NodeKind::Text(_))
    )
}

/// The DOM interface a node's wrapper should inherit from.
pub(in crate::runtime) fn dom_interface_name(dom: &Dom, node: NodeId) -> &'static str {
    match dom.node(node).map(render_dom::Node::kind) {
        Some(NodeKind::Element(element)) => match element.namespace {
            render_dom::Namespace::Html => HTML_ELEMENT_INTERFACES
                .iter()
                .find(|(_, tags)| tags.contains(&element.local_name.as_str()))
                .map_or("HTMLElement", |(interface, _)| interface),
            render_dom::Namespace::Svg if element.local_name == "svg" => "SVGSVGElement",
            render_dom::Namespace::Svg => "SVGElement",
            _ => "Element",
        },
        Some(NodeKind::Text(_)) => "Text",
        Some(NodeKind::Comment(_)) => "Comment",
        Some(NodeKind::DocumentFragment) => "DocumentFragment",
        Some(NodeKind::ShadowRoot(_)) => "ShadowRoot",
        Some(NodeKind::DocumentType(_)) => "DocumentType",
        Some(NodeKind::ProcessingInstruction { .. }) => "ProcessingInstruction",
        _ => "Node",
    }
}
