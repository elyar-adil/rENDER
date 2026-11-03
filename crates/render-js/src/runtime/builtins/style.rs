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
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::convert::required_argument;
use crate::value::ComputedPseudo;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_css::cascade::expand_shorthand;
use render_css::computed::ComputedStyle;
use render_css::computed::PropertyRegistry;
use render_css::cssom::split_rule_list;
use render_css::pseudo::PseudoElement;
use render_css::pseudo::style_source;
use render_css::stylesheet::parse_declaration_list;
use render_dom::Dom;
use render_dom::DomError;
use render_dom::NodeId;
use std::sync::OnceLock;

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
            NativeFunction::StyleSheetInsertRule => {
                self.style_sheet_insert_rule(dom, receiver, arguments)
            }
            NativeFunction::StyleSheetDeleteRule => {
                self.style_sheet_delete_rule(dom, receiver, arguments)
            }
            NativeFunction::CssRuleListItem => self.css_rule_list_item(dom, receiver, arguments),
            NativeFunction::StyleSheetListItem => {
                self.style_sheet_list_item(dom, receiver, arguments)
            }
            other => self.dispatch_observers_native(dom, other, receiver, arguments),
        }
    }
}

/// The `CSSRule.type` constant for a top-level rule's text (CSSOM §6.3):
/// at-rules are recognised by their name, and anything else is a style rule.
pub(in crate::runtime) fn css_rule_type_code(rule: &str) -> f64 {
    match rule.trim_start().to_ascii_lowercase() {
        text if text.starts_with("@import") => 3.0,
        text if text.starts_with("@media") => 4.0,
        text if text.starts_with("@font-face") => 5.0,
        text if text.starts_with("@page") => 6.0,
        text if text.starts_with("@keyframes") || text.starts_with("@-webkit-keyframes") => 7.0,
        text if text.starts_with("@supports") => 12.0,
        text if text.starts_with("@namespace") => 10.0,
        text if text.starts_with('@') => 0.0,
        _ => 1.0,
    }
}

/// The CSSOM interface name for a top-level rule's text (CSSOM §6.3). The
/// numeric type above is the compatibility surface; the interface name is what
/// `rule instanceof CSSStyleRule` and the global constructor table need.
pub(in crate::runtime) fn css_rule_interface(rule: &str) -> &'static str {
    match css_rule_type_code(rule) as u8 {
        3 => "CSSImportRule",
        4 => "CSSMediaRule",
        5 => "CSSFontFaceRule",
        6 => "CSSPageRule",
        7 => "CSSKeyframesRule",
        10 => "CSSNamespaceRule",
        12 => "CSSSupportsRule",
        1 => "CSSStyleRule",
        _ => "CSSRule",
    }
}

/// Whether `rule` is one complete top-level rule: a qualified rule with a
/// closed block, or an at-rule statement ending in `;` (CSS Syntax §5.4).
fn is_complete_rule(rule: &str) -> bool {
    let rule = rule.trim();
    if rule.starts_with('@') {
        return rule.ends_with(';') || (rule.contains('{') && rule.ends_with('}'));
    }
    rule.contains('{') && rule.ends_with('}')
}

/// Members of the `CSSStyleDeclaration` interface that are methods rather
/// than camelCase mirrors of CSS properties; they must never be treated as
/// inline declarations.
pub(in crate::runtime) const STYLE_METHOD_PROPERTIES: [&str; 4] =
    ["getPropertyValue", "setProperty", "removeProperty", "item"];

/// Parse the second argument of `getComputedStyle`. The legacy single-colon
/// spellings are accepted for compatibility; every other selector is a
/// pseudo-element this engine does not generate, and gets an empty
/// declaration rather than the element's values.
pub(in crate::runtime) fn computed_pseudo_from_text(text: &str) -> ComputedPseudo {
    match text.trim().to_ascii_lowercase().as_str() {
        "" => ComputedPseudo::Element,
        "::before" | ":before" => ComputedPseudo::Before,
        "::after" | ":after" => ComputedPseudo::After,
        _ => ComputedPseudo::Unsupported,
    }
}

/// The longhands a computed shorthand is serialized from. A shorthand the
/// cascade expanded has no stored value of its own, so CSSOM's getter has to
/// put the parts back together. Shorthands whose serialization is not a plain
/// joining of longhands (`border`, `background`, `font`, ...) are deliberately
/// absent and return the inline declaration, or the empty string.
fn shorthand_longhands(property: &str) -> Option<&'static [&'static str]> {
    match property {
        "margin" => Some(&["margin-top", "margin-right", "margin-bottom", "margin-left"]),
        "padding" => Some(&[
            "padding-top",
            "padding-right",
            "padding-bottom",
            "padding-left",
        ]),
        "border-width" => Some(&[
            "border-top-width",
            "border-right-width",
            "border-bottom-width",
            "border-left-width",
        ]),
        "border-style" => Some(&[
            "border-top-style",
            "border-right-style",
            "border-bottom-style",
            "border-left-style",
        ]),
        "border-color" => Some(&[
            "border-top-color",
            "border-right-color",
            "border-bottom-color",
            "border-left-color",
        ]),
        "inset" => Some(&["top", "right", "bottom", "left"]),
        "overflow" => Some(&["overflow-x", "overflow-y"]),
        "gap" => Some(&["row-gap", "column-gap"]),
        "grid-gap" => Some(&["row-gap", "column-gap"]),
        "flex-flow" => Some(&["flex-direction", "flex-wrap"]),
        "flex" => Some(&["flex-grow", "flex-shrink", "flex-basis"]),
        "grid-column" => Some(&["grid-column-start", "grid-column-end"]),
        "grid-row" => Some(&["grid-row-start", "grid-row-end"]),
        "columns" => Some(&["column-width", "column-count"]),
        _ => None,
    }
}

/// Serialize longhand values as a CSS shorthand. `values` is in the order
/// [`shorthand_longhands`] returns; only properties that function names reach
/// here.
fn serialize_shorthand(property: &str, values: &[String]) -> Option<String> {
    if values.iter().any(String::is_empty) {
        return None;
    }
    match property {
        "overflow" | "gap" | "grid-gap" | "flex-flow" => {
            let [first, second] = values else {
                return None;
            };
            Some(if first == second {
                first.clone()
            } else {
                format!("{first} {second}")
            })
        }
        "flex" => Some(values.join(" ")),
        "grid-column" | "grid-row" => {
            let [start, end] = values else {
                return None;
            };
            let start = start.trim();
            let end = end.trim();
            Some(if end.eq_ignore_ascii_case("auto") {
                start.to_owned()
            } else {
                format!("{start} / {end}")
            })
        }
        "columns" => {
            let [width, count] = values else {
                return None;
            };
            if width.eq_ignore_ascii_case("auto") && count.eq_ignore_ascii_case("auto") {
                Some("auto".to_owned())
            } else if width.eq_ignore_ascii_case("auto") {
                Some(count.clone())
            } else if count.eq_ignore_ascii_case("auto") {
                Some(width.clone())
            } else {
                Some(format!("{width} {count}"))
            }
        }
        _ => {
            let [top, right, bottom, left] = values else {
                return None;
            };
            Some(if top == right && right == bottom && bottom == left {
                top.clone()
            } else if top == bottom && right == left {
                format!("{top} {right}")
            } else {
                format!("{top} {right} {bottom} {left}")
            })
        }
    }
}

/// The property registry's initial values, keyed by name. These are the values
/// `getComputedStyle` answers before a page has rendered, and for a property
/// the computed snapshot does not contain. Built once: the registry is
/// immutable and the map is the same for every realm.
fn property_initial_values() -> &'static std::collections::BTreeMap<String, String> {
    static INITIAL: OnceLock<std::collections::BTreeMap<String, String>> = OnceLock::new();
    INITIAL.get_or_init(|| {
        PropertyRegistry::standard_baseline()
            .iter()
            .map(|(name, definition)| {
                // `font-size: medium` is the user's default font size. The
                // engine's root default is 16px (`root_font_size`), and a
                // browser's computed style reports an absolute length, so the
                // pre-render fallback does too.
                let value = if name == "font-size" && definition.initial_value == "medium" {
                    "16px".to_owned()
                } else {
                    definition.initial_value.clone()
                };
                (name.clone(), value)
            })
            .collect()
    })
}

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

    /// The computed style of `node` or one of its generated pseudo-elements,
    /// or `None` when the last committed render produced no entry.
    pub(in crate::runtime) fn computed_style_for(
        &self,
        node: NodeId,
        pseudo: ComputedPseudo,
    ) -> Option<&ComputedStyle> {
        match pseudo {
            ComputedPseudo::Element => self.computed_styles.get(&node),
            ComputedPseudo::Before => self
                .computed_styles
                .get(&style_source(node, PseudoElement::Before)),
            ComputedPseudo::After => self
                .computed_styles
                .get(&style_source(node, PseudoElement::After)),
            ComputedPseudo::Unsupported => None,
        }
    }

    /// The property names a computed declaration enumerates, in the order
    /// `length`/`item` expose them.
    ///
    /// A rendered element answers every property the registry defines (the
    /// computed style's own map already includes the shared initial values),
    /// plus any custom or unregistered property the document wrote inline. A
    /// generated pseudo-element enumerates only the style the cascade actually
    /// produced. Before the first render the element's inline names and the
    /// registry's names are the whole answer.
    pub(in crate::runtime) fn computed_style_property_names(
        &self,
        dom: &Dom,
        node: NodeId,
        pseudo: ComputedPseudo,
    ) -> Vec<String> {
        if pseudo == ComputedPseudo::Unsupported {
            return Vec::new();
        }
        let style = self.computed_style_for(node, pseudo);
        if pseudo != ComputedPseudo::Element && style.is_none() {
            return Vec::new();
        }
        let mut names = match style {
            Some(style) => style
                .property_names()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
            None => property_initial_values()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
        };
        if pseudo == ComputedPseudo::Element {
            for (name, _, _) in Self::inline_declarations(dom, node) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// The value `getComputedStyle(...).getPropertyValue(property)` returns.
    ///
    /// The order is the CSSOM one: the committed cascade's computed value,
    /// then a serialization synthesized from longhands for a shorthand the
    /// cascade expanded, then the element's current inline declaration, then
    /// the registry's initial value. Percentages and `auto` stay computed
    /// values; this method never substitutes a layout used value.
    pub(in crate::runtime) fn computed_property_value(
        &self,
        dom: &Dom,
        node: NodeId,
        pseudo: ComputedPseudo,
        property: &str,
    ) -> String {
        let property = property.trim().to_ascii_lowercase();
        if property.is_empty() {
            return String::new();
        }
        let style = self.computed_style_for(node, pseudo);
        // CSSOM has no useful computed style for a pseudo-element the engine
        // did not generate: the declaration is empty rather than the element's
        // inherited values. `::before`/`::after` styles exist in the snapshot
        // exactly when the cascade produced a box for them.
        if pseudo == ComputedPseudo::Unsupported
            || (pseudo != ComputedPseudo::Element && style.is_none())
        {
            return String::new();
        }
        Self::lookup_computed_property(dom, node, pseudo, style, &property)
            .or_else(|| property_initial_values().get(&property).cloned())
            .unwrap_or_default()
    }

    fn lookup_computed_property(
        dom: &Dom,
        node: NodeId,
        pseudo: ComputedPseudo,
        style: Option<&ComputedStyle>,
        property: &str,
    ) -> Option<String> {
        if let Some(value) = style.and_then(|style| style.get(property)) {
            let text = value.css_text().trim();
            if !text.is_empty() {
                return Some(text.to_owned());
            }
        }
        if let Some(longhands) = shorthand_longhands(property) {
            let values = longhands
                .iter()
                .map(|longhand| {
                    Self::lookup_computed_property(dom, node, pseudo, style, longhand)
                        .or_else(|| property_initial_values().get(*longhand).cloned())
                })
                .collect::<Option<Vec<_>>>()?;
            if let Some(serialized) = serialize_shorthand(property, &values) {
                return Some(serialized);
            }
        }
        if pseudo == ComputedPseudo::Element {
            return Self::inline_property_value(dom, node, property);
        }
        None
    }

    /// The current inline declaration of `property`, expanding a shorthand
    /// (`margin: 8px` answers `margin-top`) and honouring `!important` within
    /// the inline block itself.
    fn inline_property_value(dom: &Dom, node: NodeId, property: &str) -> Option<String> {
        let mut selected: Option<(String, bool)> = None;
        for (name, value, important) in Self::inline_declarations(dom, node) {
            for (longhand, longhand_value) in expand_shorthand(&name, &value) {
                if longhand != property {
                    continue;
                }
                let wins = selected
                    .as_ref()
                    .is_none_or(|(_, current_important)| important || !*current_important);
                if wins {
                    selected = Some((longhand_value, important));
                }
            }
        }
        selected.map(|(value, _)| value)
    }

    pub(in crate::runtime) fn style_get_property(
        &mut self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let requested = required_argument(arguments, 0, "getPropertyValue")?.to_js_string();
        let requested = requested.to_lossy().trim().to_ascii_lowercase();
        match self.realm.host(receiver) {
            Some(ObjectHost::CssStyleDeclaration(node)) => Ok(JsValue::String(
                (Self::inline_declarations(dom, node)
                    .into_iter()
                    .find(|(name, _, _)| *name == requested)
                    .map_or_else(String::new, |(_, value, _)| value))
                .into(),
            )),
            Some(ObjectHost::ComputedStyleDeclaration { node, pseudo }) => Ok(JsValue::String(
                (self.computed_property_value(dom, node, pseudo, &requested)).into(),
            )),
            _ => Err(JsError::type_error(
                "incompatible CSSStyleDeclaration method receiver",
            )),
        }
    }

    pub(in crate::runtime) fn style_set_property(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // CSSOM exposes computed declarations as read-only: a write is a
        // silent no-op, not an error, exactly as it is in browsers.
        if matches!(
            self.realm.host(receiver),
            Some(ObjectHost::ComputedStyleDeclaration { .. })
        ) {
            return Ok(JsValue::Undefined);
        }
        let node = self.require_style_declaration(receiver)?;
        let name = required_argument(arguments, 0, "setProperty")?
            .to_js_string()
            .to_lossy()
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
            .is_some_and(|priority| priority.to_lossy().eq_ignore_ascii_case("important"));
        value = value.to_lossy().trim().into();
        let mut declarations: Vec<(String, String, bool)> = Self::inline_declarations(dom, node)
            .into_iter()
            .filter(|(existing, _, _)| *existing != name)
            .collect();
        if !value.is_empty() {
            declarations.push((name, (value).to_lossy(), important));
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
        if matches!(
            self.realm.host(receiver),
            Some(ObjectHost::ComputedStyleDeclaration { .. })
        ) {
            return Ok(JsValue::String((String::new()).into()));
        }
        let node = self.require_style_declaration(receiver)?;
        let name = required_argument(arguments, 0, "removeProperty")?
            .to_js_string()
            .to_lossy()
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
        Ok(JsValue::String((previous).into()))
    }

    pub(in crate::runtime) fn style_item(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let index = match arguments.first() {
            Some(value) => self.to_number_value(dom, value)?,
            None => return Ok(JsValue::String((String::new()).into())),
        };
        let computed = match self.realm.host(receiver) {
            Some(ObjectHost::ComputedStyleDeclaration { node, pseudo }) => {
                Some(self.computed_style_property_names(dom, node, pseudo))
            }
            _ => None,
        };
        let declarations = if let Some(names) = &computed {
            names.clone()
        } else {
            let node = self.require_style_declaration(receiver)?;
            Self::inline_declarations(dom, node)
                .into_iter()
                .map(|(name, _, _)| name)
                .collect()
        };
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "style indices are validated array positions"
        )]
        let position = index as usize;
        Ok(match declarations.get(position) {
            Some(name) => JsValue::String((name.clone()).into()),
            None => JsValue::String((String::new()).into()),
        })
    }

    /// The style element `sheet` is the `CSSStyleSheet` of, or an illegal
    /// invocation when the receiver is not one.
    pub(in crate::runtime) fn require_style_sheet(
        &self,
        object: ObjectId,
    ) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::CssStyleSheet(node)) => Ok(node),
            _ => Err(JsError::type_error(
                "incompatible CSSStyleSheet method receiver",
            )),
        }
    }

    /// Whether `node` is the owner of a sheet whose rules live in its text.
    /// `<style>` children are the CSS the cascade reads; a `<link>` sheet was
    /// loaded from a resource this realm does not keep as DOM text.
    fn style_sheet_has_text(dom: &Dom, node: NodeId) -> bool {
        matches!(
            dom.node(node).map(render_dom::Node::kind),
            Some(render_dom::NodeKind::Element(element)) if element.local_name == "style"
        )
    }

    /// The top-level rules of the sheet owned by `node`, in cascade order.
    pub(in crate::runtime) fn style_sheet_rules(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
    ) -> Result<Vec<String>, JsError> {
        if Self::style_sheet_has_text(dom, node) {
            let text = self.text_content(dom, node)?;
            return Ok(split_rule_list(&text));
        }
        Ok(self
            .realm
            .css_rule_overrides
            .get(&node)
            .cloned()
            .unwrap_or_default())
    }

    /// Replace the sheet's rules with `rules`. For a `<style>` owner this
    /// rewrites its child text, so the ordinary style recalculation sees the
    /// edit; for a `<link>` owner the CSSOM list is the only store available.
    pub(in crate::runtime) fn write_style_sheet_rules(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        rules: Vec<String>,
    ) -> Result<(), JsError> {
        if Self::style_sheet_has_text(dom, node) {
            let text = rules.join("\n");
            return self.set_text_content(dom, node, text);
        }
        self.realm.css_rule_overrides.insert(node, rules);
        Ok(())
    }

    /// `CSSStyleSheet.prototype.insertRule(rule, index)` (CSSOM §6.4.2): parse
    /// one rule, insert it at `index`, and return the index it landed on.
    fn style_sheet_insert_rule(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_sheet(receiver)?;
        let source = required_argument(arguments, 0, "insertRule")?
            .to_js_string()
            .to_lossy();
        // CSSOM §6.4.2 parses one rule from the string; anything after that
        // rule is outside what `insertRule` consumes. A string that starts
        // with no rule at all is a SyntaxError.
        let Some(rule) = split_rule_list(&source).into_iter().next() else {
            return Err(self.dom_exception(
                DomExceptionName::Syntax,
                "Failed to execute 'insertRule' on 'CSSStyleSheet': the rule text did not contain a rule",
            ));
        };
        if !is_complete_rule(&rule) {
            return Err(self.dom_exception(
                DomExceptionName::Syntax,
                "Failed to execute 'insertRule' on 'CSSStyleSheet': the rule text is not a complete rule",
            ));
        }
        let mut rules = self.style_sheet_rules(dom, node)?;
        let index = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0,
            Some(value) => {
                super::dom_collections::unsigned_long(self.to_number_value(dom, value)?) as usize
            }
        };
        if index > rules.len() {
            return Err(self.dom_exception(
                DomExceptionName::IndexSize,
                "Failed to execute 'insertRule' on 'CSSStyleSheet': the index is larger than the rule count",
            ));
        }
        rules.insert(index, rule.clone());
        self.write_style_sheet_rules(dom, node, rules)?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "CSSOM rule indices stay far below any precision boundary"
        )]
        Ok(JsValue::Number(index as f64))
    }

    /// `CSSStyleSheet.prototype.deleteRule(index)` (CSSOM §6.4.3).
    fn style_sheet_delete_rule(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_sheet(receiver)?;
        let index = self.to_number_value(dom, required_argument(arguments, 0, "deleteRule")?)?;
        let mut rules = self.style_sheet_rules(dom, node)?;
        let index = super::dom_collections::unsigned_long(index) as usize;
        if index >= rules.len() {
            return Err(self.dom_exception(
                DomExceptionName::IndexSize,
                "Failed to execute 'deleteRule' on 'CSSStyleSheet': the index is not less than the rule count",
            ));
        }
        rules.remove(index);
        self.write_style_sheet_rules(dom, node, rules)?;
        Ok(JsValue::Undefined)
    }

    /// `StyleSheetList.prototype.item(index)` (CSSOM §6.1): the sheet at
    /// `index`, or null past the end.
    fn style_sheet_list_item(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let is_style_sheets = matches!(
            self.realm.host(receiver),
            Some(ObjectHost::LiveCollection {
                filter: crate::value::CollectionFilter::StyleSheets,
                ..
            })
        );
        if !is_style_sheets {
            return Err(JsError::type_error(
                "incompatible StyleSheetList method receiver",
            ));
        }
        let number = self.to_number_value(dom, required_argument(arguments, 0, "item")?)?;
        let index = super::dom_collections::unsigned_long(number) as usize;
        let node = self
            .collection_members_of(dom, receiver)
            .and_then(|members| members.get(index).copied());
        match node {
            Some(node) => {
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.style_sheet_wrapper(node)))
            }
            None => Ok(JsValue::Null),
        }
    }

    /// `CSSRuleList.prototype.item(index)` (CSSOM §6.7).
    fn css_rule_list_item(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::CssRuleList(node)) = self.realm.host(receiver) else {
            return Err(JsError::type_error(
                "incompatible CSSRuleList method receiver",
            ));
        };
        let number = self.to_number_value(dom, required_argument(arguments, 0, "item")?)?;
        let index = super::dom_collections::unsigned_long(number) as usize;
        let rules = self.style_sheet_rules(dom, node)?;
        if index >= rules.len() {
            return Ok(JsValue::Null);
        }
        let interface = css_rule_interface(&rules[index]);
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(
            self.realm.css_rule_wrapper(node, index, interface),
        ))
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
