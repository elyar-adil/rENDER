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

use crate::css::selector::MatchContext;
use crate::css::selector::matches_selector_list;
use crate::css::selector::parse_selector_list;
use crate::css::selector::select_all;
use crate::dom::Dom;
use crate::dom::NodeId;
use crate::dom::NodeKind;
use crate::js::JsError;
use crate::js::JsValue;
use crate::js::ObjectId;
use crate::js::runtime::JsRuntime;
use crate::js::runtime::convert::required_argument;
use crate::js::runtime::convert::to_number;
use crate::js::runtime::types::ElementRect;
use crate::js::value::NativeFunction;
use crate::js::value::ObjectHost;

impl JsRuntime {
    pub(in crate::js::runtime) fn dispatch_dom_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let result: Result<JsValue, JsError> = match function {
            NativeFunction::GetElementById => {
                self.require_document(receiver)?;
                let id = required_argument(arguments, 0, "getElementById")?.to_js_string();
                match self.find_element_by_id(dom, &id)? {
                    Some(node) => self.wrap_node(node),
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::QuerySelector => {
                let root = self.query_root(receiver)?;
                let selector = required_argument(arguments, 0, "querySelector")?.to_js_string();
                let selectors = parse_selector_list(&selector)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                match select_all(dom, root, &selectors, &MatchContext::default())
                    .into_iter()
                    .next()
                {
                    Some(node) => self.wrap_node(node),
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::QuerySelectorAll => {
                let root = self.query_root(receiver)?;
                let selector = required_argument(arguments, 0, "querySelectorAll")?.to_js_string();
                let selectors = parse_selector_list(&selector)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                let nodes = select_all(dom, root, &selectors, &MatchContext::default())
                    .into_iter()
                    .map(|node| self.wrap_node(node))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(JsValue::Object(self.create_array_from_values(&nodes)?))
            }
            NativeFunction::GetElementsByTagName => {
                let root = self.query_root(receiver)?;
                let tag = required_argument(arguments, 0, "getElementsByTagName")?.to_js_string();
                // Type selectors match case-insensitively for HTML elements,
                // which is exactly the legacy API contract.
                let selectors = parse_selector_list(&tag)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                let nodes = select_all(dom, root, &selectors, &MatchContext::default())
                    .into_iter()
                    .map(|node| self.wrap_node(node))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(JsValue::Object(self.create_array_from_values(&nodes)?))
            }
            NativeFunction::GetElementsByClassName => {
                let root = self.query_root(receiver)?;
                let names =
                    required_argument(arguments, 0, "getElementsByClassName")?.to_js_string();
                let selector: String = names
                    .split_ascii_whitespace()
                    .map(|name| format!(".{name}"))
                    .fold(String::new(), |mut acc, part| {
                        acc.push_str(&part);
                        acc
                    });
                if selector.is_empty() {
                    return Ok(JsValue::Object(self.create_array_from_values(&[])?));
                }
                let selectors = parse_selector_list(&selector)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                let nodes = select_all(dom, root, &selectors, &MatchContext::default())
                    .into_iter()
                    .map(|node| self.wrap_node(node))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(JsValue::Object(self.create_array_from_values(&nodes)?))
            }
            NativeFunction::CloneNode => {
                let node = self.require_node(receiver)?;
                let deep = arguments.first().is_some_and(JsValue::is_truthy);
                self.clone_node_value(dom, node, deep)
            }
            NativeFunction::CreateTextNode => {
                self.require_document(receiver)?;
                let data = required_argument(arguments, 0, "createTextNode")?.to_js_string();
                let node = dom.create_text(data);
                self.wrap_node(node)
            }
            NativeFunction::CreateDocumentFragment => {
                self.require_document(receiver)?;
                let node = dom.create_document_fragment();
                self.wrap_node(node)
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
                self.require_document(receiver)?;
                let name = required_argument(arguments, 0, "createElement")?.to_js_string();
                if !valid_html_local_name(&name) {
                    return Err(JsError::dom(format!(
                        "{name:?} is not a supported HTML local name"
                    )));
                }
                if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
                    return Err(JsError::resource("DOM node creation limit exceeded"));
                }
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let node = dom.create_element(name);
                Ok(JsValue::Object(self.realm.node_wrapper(node)))
            }
            NativeFunction::GetAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "getAttribute")?.to_js_string();
                Ok(dom
                    .attribute(node, &name)?
                    .map_or(JsValue::Null, |value| JsValue::String(value.to_owned())))
            }
            NativeFunction::HasAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "hasAttribute")?.to_js_string();
                Ok(JsValue::Boolean(dom.attribute(node, &name)?.is_some()))
            }
            NativeFunction::RemoveAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "removeAttribute")?.to_js_string();
                dom.remove_attribute(node, &name)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::AppendChild => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "appendChild")?)?;
                dom.append_child(parent, child)?;
                self.wrap_node(child)
            }
            NativeFunction::RemoveChild => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "removeChild")?)?;
                dom.remove_child(parent, child)?;
                self.wrap_node(child)
            }
            NativeFunction::InsertBefore => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "insertBefore")?)?;
                let reference = match arguments.get(1) {
                    None | Some(JsValue::Null | JsValue::Undefined) => None,
                    Some(value) => Some(self.value_as_node(value)?),
                };
                dom.insert_before(parent, child, reference)?;
                self.wrap_node(child)
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
            NativeFunction::Matches => {
                let node = self.require_node(receiver)?;
                let selector = required_argument(arguments, 0, "matches")?.to_js_string();
                let selectors = parse_selector_list(&selector)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                Ok(JsValue::Boolean(matches_selector_list(
                    dom,
                    node,
                    &selectors,
                    &MatchContext::default(),
                )))
            }
            NativeFunction::Click => {
                self.require_node(receiver)?;
                let options = self.realm.create_ordinary_object();
                self.realm
                    .set_property(options, "bubbles".to_owned(), JsValue::Boolean(true));
                self.realm
                    .set_property(options, "cancelable".to_owned(), JsValue::Boolean(true));
                let event = self.event_constructor(&[
                    JsValue::String("click".to_owned()),
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
            NativeFunction::ClassListToString => self.class_list_to_string(dom, receiver),
            NativeFunction::NamedMapItem | NativeFunction::NamedMapGetNamedItem => {
                let Some(ObjectHost::NamedNodeMap(map_node)) = self.realm.host(receiver) else {
                    return Err(JsError::type_error("incompatible attributes receiver"));
                };
                let key = required_argument(arguments, 0, "item")?.to_js_string();
                let attribute = if function == NativeFunction::NamedMapItem {
                    let index_result = key.parse::<usize>();
                    dom.node(map_node)
                        .and_then(|node| match node.kind() {
                            NodeKind::Element(element) => {
                                element.attributes.get(index_result.unwrap_or(usize::MAX))
                            }
                            _ => None,
                        })
                        .map(|attribute| attribute.local_name.clone())
                } else {
                    dom.attribute(map_node, &key)?.map(|_value| key.clone())
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
                    Ok(JsValue::String(name))
                } else {
                    Ok(JsValue::String(
                        dom.attribute(owner, &name)?.unwrap_or_default().to_owned(),
                    ))
                }
            }
            other => self.dispatch_events_native(dom, other, receiver, arguments),
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

pub(in crate::js::runtime) fn dom_contains(dom: &Dom, root: NodeId, candidate: NodeId) -> bool {
    let mut current = Some(candidate);
    while let Some(node) = current {
        if node == root {
            return true;
        }
        current = dom.parent(node);
    }
    false
}

pub(in crate::js::runtime) fn find_body_node(dom: &Dom, root: NodeId) -> Option<NodeId> {
    for child in dom.children(root).unwrap_or_default() {
        if matches!(
            dom.node(*child).map(crate::dom::Node::kind),
            Some(NodeKind::Element(element))
                if element.namespace == crate::dom::Namespace::Html
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

pub(in crate::js::runtime) fn is_valid_property_name(name: &str) -> bool {
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
pub(in crate::js::runtime) fn css_prop_from_member(property: &str) -> String {
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

pub(in crate::js::runtime) fn node_attribute_property(property: &str) -> Option<&str> {
    match property {
        "id" => Some("id"),
        "className" => Some("class"),
        "value" => Some("value"),
        "name" => Some("name"),
        "title" => Some("title"),
        "href" => Some("href"),
        "src" => Some("src"),
        "srcset" | "srcSet" => Some("srcset"),
        "sizes" => Some("sizes"),
        "poster" => Some("poster"),
        "loading" => Some("loading"),
        "width" => Some("width"),
        "height" => Some("height"),
        "alt" => Some("alt"),
        "role" => Some("role"),
        "type" => Some("type"),
        "placeholder" => Some("placeholder"),
        "action" => Some("action"),
        "method" => Some("method"),
        "target" => Some("target"),
        "rel" => Some("rel"),
        "tabIndex" => Some("tabindex"),
        "disabled" => Some("disabled"),
        "checked" => Some("checked"),
        "selected" => Some("selected"),
        "hidden" => Some("hidden"),
        "readOnly" => Some("readonly"),
        "required" => Some("required"),
        "multiple" => Some("multiple"),
        "autofocus" | "autoFocus" => Some("autofocus"),
        _ => None,
    }
}

pub(in crate::js::runtime) fn node_boolean_property(property: &str) -> bool {
    matches!(
        property,
        "disabled"
            | "checked"
            | "selected"
            | "hidden"
            | "readOnly"
            | "required"
            | "multiple"
            | "autofocus"
            | "autoFocus"
    )
}

/// `DocumentPosition` bitmask values for `compareDocumentPosition`.
pub(in crate::js::runtime) const DOCUMENT_POSITION_DISCONNECTED: f64 = 1.0;

pub(in crate::js::runtime) const DOCUMENT_POSITION_PRECEDING: f64 = 2.0;

pub(in crate::js::runtime) const DOCUMENT_POSITION_FOLLOWING: f64 = 4.0;

pub(in crate::js::runtime) const DOCUMENT_POSITION_CONTAINS: f64 = 8.0;

pub(in crate::js::runtime) const DOCUMENT_POSITION_CONTAINED_BY: f64 = 16.0;

/// Source shape of cloned nodes in `clone_node_recursive`.
pub(in crate::js::runtime) enum CloneSource {
    Element {
        local_name: String,
        attributes: Vec<(String, String)>,
    },
    Text(String),
    Comment(String),
    Fragment,
}

pub(in crate::js::runtime) fn valid_html_local_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || matches!(character, '_' | ':'))
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '-' | '.')
        })
}

impl JsRuntime {
    pub(in crate::js::runtime) fn image_constructor(
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
        for (index, name) in [(0, "width"), (1, "height")] {
            if let Some(value) = arguments.get(index)
                && !matches!(value, JsValue::Undefined)
            {
                let number = to_number(value)?.max(0.0).floor();
                dom.set_attribute(image, name, number.to_string())?;
            }
        }
        self.wrap_node(image)
    }

    pub(in crate::js::runtime) fn element_rect_value(&mut self, node: NodeId) -> JsValue {
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

    pub(in crate::js::runtime) fn rect_value(&mut self, rect: ElementRect) -> JsValue {
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
    pub(in crate::js::runtime) fn set_inner_html(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        source: &str,
    ) -> Result<(), JsError> {
        let scratch = crate::html::parse_document(source);
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
    pub(in crate::js::runtime) fn set_outer_html(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        source: &str,
    ) -> Result<(), JsError> {
        if target == dom.document() {
            return Err(JsError::dom("outerHTML cannot replace the document node"));
        }
        let Some(parent) = dom.parent(target) else {
            return Err(JsError::dom("outerHTML requires a parent to splice into"));
        };
        let scratch = crate::html::parse_document(source);
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
    pub(in crate::js::runtime) fn clone_node_value(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        deep: bool,
    ) -> Result<JsValue, JsError> {
        let copy = self.clone_node_recursive(dom, node, deep)?;
        self.wrap_node(copy)
    }

    pub(in crate::js::runtime) fn clone_node_recursive(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        deep: bool,
    ) -> Result<NodeId, JsError> {
        if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        let source = match dom.node(node).map(crate::dom::Node::kind) {
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
            _ => return Err(JsError::dom("this node type cannot be cloned here")),
        };
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
        };
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

    pub(in crate::js::runtime) fn import_dom_subtree(
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
        let imported = match source.node(node).map(crate::dom::Node::kind) {
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

    pub(in crate::js::runtime) fn query_root(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Document(document) | ObjectHost::Node(document)) => Ok(document),
            _ => Err(JsError::type_error(
                "querySelector method called on a non-Document/non-Element object",
            )),
        }
    }

    pub(in crate::js::runtime) fn find_element_by_tag(
        &mut self,
        dom: &Dom,
        root: NodeId,
        tag: &str,
    ) -> Result<Option<NodeId>, JsError> {
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            self.consume_step()?;
            if let Some(NodeKind::Element(data)) = dom.node(node).map(crate::dom::Node::kind)
                && data.local_name.eq_ignore_ascii_case(tag)
            {
                return Ok(Some(node));
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        }
        Ok(None)
    }

    pub(in crate::js::runtime) fn class_list_tokens(
        dom: &Dom,
        node: NodeId,
    ) -> Result<Vec<String>, JsError> {
        let value = dom.attribute(node, "class")?.unwrap_or_default();
        Ok(value.split_ascii_whitespace().map(str::to_owned).collect())
    }

    pub(in crate::js::runtime) fn require_class_list(
        &self,
        object: ObjectId,
    ) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::ClassList(node)) => Ok(node),
            _ => Err(JsError::type_error("incompatible DOMTokenList receiver")),
        }
    }

    pub(in crate::js::runtime) fn class_list_token(
        arguments: &[JsValue],
        index: usize,
        function: &str,
    ) -> Result<String, JsError> {
        let token = required_argument(arguments, index, function)?.to_js_string();
        if token.is_empty()
            || token
                .chars()
                .any(|character| character.is_ascii_whitespace())
        {
            return Err(JsError::dom(format!(
                "{function} token must be non-empty and contain no ASCII whitespace"
            )));
        }
        Ok(token)
    }

    pub(in crate::js::runtime) fn class_list_add(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let mut tokens = Self::class_list_tokens(dom, node)?;
        let mut changed = false;
        for index in 0..arguments.len() {
            let token = Self::class_list_token(arguments, index, "classList.add")?;
            if !tokens.contains(&token) {
                tokens.push(token);
                changed = true;
            }
        }
        if changed {
            dom.set_attribute(node, "class", tokens.join(" "))?;
        }
        Ok(JsValue::Undefined)
    }

    pub(in crate::js::runtime) fn class_list_remove(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let mut tokens = Self::class_list_tokens(dom, node)?;
        let original_len = tokens.len();
        for index in 0..arguments.len() {
            let token = Self::class_list_token(arguments, index, "classList.remove")?;
            tokens.retain(|candidate| candidate != &token);
        }
        if tokens.len() != original_len {
            if tokens.is_empty() {
                dom.remove_attribute(node, "class")?;
            } else {
                dom.set_attribute(node, "class", tokens.join(" "))?;
            }
        }
        Ok(JsValue::Undefined)
    }

    pub(in crate::js::runtime) fn class_list_toggle(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let token = Self::class_list_token(arguments, 0, "classList.toggle")?;
        let mut tokens = Self::class_list_tokens(dom, node)?;
        let present = tokens.iter().any(|candidate| candidate == &token);
        let next = match arguments.get(1) {
            Some(force) => force.is_truthy(),
            None => !present,
        };
        if next && !present {
            tokens.push(token);
            dom.set_attribute(node, "class", tokens.join(" "))?;
        } else if !next && present {
            tokens.retain(|candidate| candidate != &token);
            if tokens.is_empty() {
                dom.remove_attribute(node, "class")?;
            } else {
                dom.set_attribute(node, "class", tokens.join(" "))?;
            }
        }
        Ok(JsValue::Boolean(next))
    }

    pub(in crate::js::runtime) fn class_list_contains(
        &self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let token = Self::class_list_token(arguments, 0, "classList.contains")?;
        Ok(JsValue::Boolean(
            Self::class_list_tokens(dom, node)?
                .iter()
                .any(|candidate| candidate == &token),
        ))
    }

    pub(in crate::js::runtime) fn class_list_item(
        &self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let index = to_number(required_argument(arguments, 0, "classList.item")?)?;
        if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
            return Ok(JsValue::Null);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "index was validated as a finite non-negative integer"
        )]
        let index = index as usize;
        Ok(Self::class_list_tokens(dom, node)?
            .get(index)
            .cloned()
            .map_or(JsValue::Null, JsValue::String))
    }

    pub(in crate::js::runtime) fn class_list_to_string(
        &self,
        dom: &Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        Ok(JsValue::String(
            Self::class_list_tokens(dom, node)?.join(" "),
        ))
    }

    pub(in crate::js::runtime) fn find_element_by_id(
        &mut self,
        dom: &Dom,
        id: &str,
    ) -> Result<Option<NodeId>, JsError> {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            self.consume_step()?;
            if matches!(
                dom.node(node).map(crate::dom::Node::kind),
                Some(NodeKind::Element(_))
            ) && dom.attribute(node, "id")? == Some(id)
            {
                return Ok(Some(node));
            }
            if let Some(children) = dom.children(node) {
                pending.extend(children.iter().rev());
            }
        }
        Ok(None)
    }

    pub(in crate::js::runtime) fn text_content(
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

    pub(in crate::js::runtime) fn set_text_content(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        value: String,
    ) -> Result<(), JsError> {
        let kind = dom
            .node(node)
            .map(crate::dom::Node::kind)
            .ok_or_else(|| JsError::dom("DOM wrapper refers to an unknown node"))?;
        if matches!(
            kind,
            NodeKind::Text(_) | NodeKind::Comment(_) | NodeKind::ProcessingInstruction { .. }
        ) {
            dom.set_character_data(node, value)?;
            return Ok(());
        }
        if !matches!(kind, NodeKind::Element(_) | NodeKind::DocumentFragment) {
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

    pub(in crate::js::runtime) fn require_document(
        &self,
        object: ObjectId,
    ) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Document(document)) => Ok(document),
            _ => Err(JsError::type_error("incompatible Document method receiver")),
        }
    }

    pub(in crate::js::runtime) fn require_node(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            // Document is a Node in the DOM spec; wrappers host it under its
            // own variant, and real pages pass `document` to Node-taking
            // APIs such as `MutationObserver.prototype.observe`.
            Some(ObjectHost::Node(node) | ObjectHost::Document(node)) => Ok(node),
            _ => Err(JsError::type_error("incompatible Node method receiver")),
        }
    }

    pub(in crate::js::runtime) fn require_object(value: &JsValue) -> Result<ObjectId, JsError> {
        match value {
            JsValue::Object(object) => Ok(*object),
            JsValue::Null | JsValue::Undefined => Err(JsError::type_error(
                "cannot access a property of null or undefined",
            )),
            _ => Err(JsError::type_error(
                "primitive object coercion is not implemented in this runtime slice",
            )),
        }
    }

    pub(in crate::js::runtime) fn value_as_node(&self, value: &JsValue) -> Result<NodeId, JsError> {
        let JsValue::Object(object) = value else {
            return Err(JsError::type_error("argument is not a Node"));
        };
        self.require_node(*object)
    }

    pub(in crate::js::runtime) fn wrap_node(&mut self, node: NodeId) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.node_wrapper(node)))
    }
}
