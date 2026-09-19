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
use crate::runtime::builtins::dom::is_valid_property_name;
use crate::runtime::convert::required_argument;
use crate::runtime::convert::to_number;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_css::stylesheet::parse_declaration_list;
use render_dom::Dom;
use render_dom::DomError;
use render_dom::NodeId;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_style_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::StyleGetProperty => self.style_get_property(dom, receiver, arguments),
            NativeFunction::StyleSetProperty => self.style_set_property(dom, receiver, arguments),
            NativeFunction::StyleRemoveProperty => {
                self.style_remove_property(dom, receiver, arguments)
            }
            NativeFunction::StyleItem => self.style_item(dom, receiver, arguments),
            other => self.dispatch_observers_native(dom, other, receiver, arguments),
        }
    }
}

/// Members of the `CSSStyleDeclaration` interface that are methods rather
/// than camelCase mirrors of CSS properties; they must never be treated as
/// inline declarations.
pub(in crate::runtime) const STYLE_METHOD_PROPERTIES: [&str; 4] =
    ["getPropertyValue", "setProperty", "removeProperty", "item"];

impl JsRuntime {
    /// Declarations of the element's inline `style` attribute, in source order.
    pub(in crate::runtime) fn inline_declarations(
        dom: &Dom,
        node: NodeId,
    ) -> Vec<(String, String, bool)> {
        let Ok(Some(source)) = dom.attribute(node, "style") else {
            return Vec::new();
        };
        parse_declaration_list(source)
            .0
            .into_iter()
            .map(|declaration| (declaration.name, declaration.value, declaration.important))
            .collect()
    }

    /// Serialize the element's inline declarations back into attribute text.
    pub(in crate::runtime) fn write_inline_declarations(
        dom: &mut Dom,
        node: NodeId,
        declarations: &[(String, String, bool)],
    ) -> Result<(), DomError> {
        if declarations.is_empty() {
            dom.remove_attribute(node, "style")
        } else {
            let source = declarations
                .iter()
                .map(|(name, value, important)| {
                    if *important {
                        format!("{name}: {value} !important;")
                    } else {
                        format!("{name}: {value};")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            dom.set_attribute(node, "style", &source)
        }
    }

    pub(in crate::runtime) fn style_get_property(
        &mut self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_declaration(receiver)?;
        let requested = required_argument(arguments, 0, "getPropertyValue")?.to_js_string();
        let requested = requested.trim().to_ascii_lowercase();
        Ok(JsValue::String(
            Self::inline_declarations(dom, node)
                .into_iter()
                .find(|(name, _, _)| *name == requested)
                .map_or_else(String::new, |(_, value, _)| value),
        ))
    }

    pub(in crate::runtime) fn style_set_property(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_declaration(receiver)?;
        let name = required_argument(arguments, 0, "setProperty")?
            .to_js_string()
            .trim()
            .to_ascii_lowercase();
        if !is_valid_property_name(&name) {
            return Err(JsError::dom(format!("invalid CSS property name {name:?}")));
        }
        let mut value = arguments
            .get(1)
            .unwrap_or(&JsValue::Undefined)
            .to_js_string();
        let important = arguments
            .get(2)
            .map(JsValue::to_js_string)
            .is_some_and(|priority| priority.eq_ignore_ascii_case("important"));
        value = value.trim().into();
        let mut declarations: Vec<(String, String, bool)> = Self::inline_declarations(dom, node)
            .into_iter()
            .filter(|(existing, _, _)| *existing != name)
            .collect();
        if !value.is_empty() {
            declarations.push((name, value, important));
        }
        Self::write_inline_declarations(dom, node, &declarations)?;
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn style_remove_property(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_declaration(receiver)?;
        let name = required_argument(arguments, 0, "removeProperty")?
            .to_js_string()
            .trim()
            .to_ascii_lowercase();
        let previous = Self::inline_declarations(dom, node)
            .into_iter()
            .find(|(existing, _, _)| *existing == name)
            .map_or_else(String::new, |(_, value, _)| value);
        let declarations: Vec<(String, String, bool)> = Self::inline_declarations(dom, node)
            .into_iter()
            .filter(|(existing, _, _)| *existing != name)
            .collect();
        Self::write_inline_declarations(dom, node, &declarations)?;
        Ok(JsValue::String(previous))
    }

    pub(in crate::runtime) fn style_item(
        &mut self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_declaration(receiver)?;
        let index = match arguments.first() {
            Some(value) => to_number(value)?,
            None => return Ok(JsValue::String(String::new())),
        };
        let declarations = Self::inline_declarations(dom, node);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "style indices are validated array positions"
        )]
        let position = index as usize;
        Ok(match declarations.get(position) {
            Some((name, _, _)) => JsValue::String(name.clone()),
            None => JsValue::String(String::new()),
        })
    }

    pub(in crate::runtime) fn require_style_declaration(
        &self,
        object: ObjectId,
    ) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::CssStyleDeclaration(node)) => Ok(node),
            _ => Err(JsError::type_error(
                "incompatible CSSStyleDeclaration method receiver",
            )),
        }
    }
}
