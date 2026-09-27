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
use crate::JsObject;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::PropertyDescriptor;
use crate::runtime::JsRuntime;
use crate::runtime::convert::format_number_precision;
use crate::runtime::convert::required_argument;
use crate::runtime::convert::to_number;
use crate::runtime::types::ObjectEntryKind;
use crate::value::ErrorKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_object_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::ObjectGetOwnPropertySymbols => {
                let object = Self::require_object(required_argument(
                    arguments,
                    0,
                    "Object.getOwnPropertySymbols",
                )?)?;
                let symbols = self
                    .realm
                    .own_symbols(object)
                    .unwrap_or_default()
                    .into_iter()
                    .map(JsValue::Symbol)
                    .collect::<Vec<_>>();
                Ok(JsValue::Object(self.create_array_from_values(&symbols)?))
            }
            NativeFunction::ObjectAssign => self.object_assign(arguments),
            NativeFunction::ObjectKeys => {
                self.object_entries(dom, arguments, ObjectEntryKind::Keys)
            }
            NativeFunction::ObjectValues => {
                self.object_entries(dom, arguments, ObjectEntryKind::Values)
            }
            NativeFunction::ObjectEntries => {
                self.object_entries(dom, arguments, ObjectEntryKind::Entries)
            }
            NativeFunction::ObjectCreate => self.object_create(arguments),
            NativeFunction::ObjectDefineProperty => self.object_define_property(arguments),
            NativeFunction::ObjectDefineProperties => self.object_define_properties(arguments),
            NativeFunction::ObjectGetOwnPropertyDescriptor => {
                self.object_get_own_property_descriptor(arguments)
            }
            NativeFunction::ObjectGetOwnPropertyDescriptors => {
                self.object_get_own_property_descriptors(arguments)
            }
            NativeFunction::ObjectGetOwnPropertyNames => {
                self.object_get_own_property_names(arguments)
            }
            NativeFunction::ObjectGetPrototypeOf => self.object_get_prototype_of(arguments),
            NativeFunction::ObjectSetPrototypeOf => self.object_set_prototype_of(arguments),
            NativeFunction::ObjectHasOwn => self.object_has_own(arguments),
            NativeFunction::ObjectPrototypeHasOwnProperty => {
                self.object_prototype_has_own_property(receiver, arguments)
            }
            NativeFunction::ObjectPrototypeIsPrototypeOf => {
                Ok(self.object_prototype_is_prototype_of(receiver, arguments))
            }
            NativeFunction::SymbolToString => match self.realm.host(receiver) {
                Some(ObjectHost::SymbolInstance(symbol)) => {
                    Ok(JsValue::String(symbol.to_display()))
                }
                _ => Err(JsError::type_error(
                    "Symbol.prototype.toString requires that 'this' be a Symbol",
                )),
            },
            NativeFunction::SymbolDescription => match self.realm.host(receiver) {
                Some(ObjectHost::SymbolInstance(symbol)) => Ok(symbol
                    .description()
                    .map_or(JsValue::Undefined, |text| JsValue::String(text.to_owned()))),
                _ => Err(JsError::type_error(
                    "Symbol.prototype.description requires that 'this' be a Symbol",
                )),
            },
            NativeFunction::SymbolValueOf => match self.realm.host(receiver) {
                Some(ObjectHost::SymbolInstance(symbol)) => Ok(JsValue::Symbol(symbol.clone())),
                _ => Err(JsError::type_error(
                    "Symbol.prototype.valueOf requires that 'this' be a Symbol",
                )),
            },
            NativeFunction::NumValueOf => Ok(JsValue::Object(receiver)),
            NativeFunction::NumToFixed => {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "toFixed digits are validated small integers"
                )]
                let digits = match arguments.first() {
                    Some(JsValue::Number(n)) => *n as usize,
                    _ => 0,
                };
                match self.realm.host(receiver) {
                    Some(ObjectHost::NumberPrimitive(value)) => {
                        Ok(JsValue::String(format!("{value:.digits$}")))
                    }
                    _ => Err(JsError::type_error("incompatible Number method receiver")),
                }
            }
            NativeFunction::NumToPrecision => {
                let value = match self.realm.host(receiver) {
                    Some(ObjectHost::NumberPrimitive(value)) => value,
                    _ => {
                        return Err(JsError::type_error("incompatible Number method receiver"));
                    }
                };
                let Some(argument) = arguments.first() else {
                    return Ok(JsValue::String(crate::value::number_to_string(value)));
                };
                if matches!(argument, JsValue::Undefined) {
                    return Ok(JsValue::String(crate::value::number_to_string(value)));
                }
                let precision = to_number(argument)?;
                if !precision.is_finite()
                    || precision.fract() != 0.0
                    || !(1.0..=100.0).contains(&precision)
                {
                    return Err(JsError::type_error("invalid toPrecision precision"));
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "toPrecision precision is validated in the 1..=100 range"
                )]
                let precision = precision as usize;
                Ok(JsValue::String(format_number_precision(value, precision)))
            }
            NativeFunction::NumToString => match self.realm.host(receiver) {
                Some(ObjectHost::NumberPrimitive(value)) => {
                    Ok(JsValue::String(crate::value::number_to_string(value)))
                }
                _ => Err(JsError::type_error("incompatible Number method receiver")),
            },
            NativeFunction::BoolToString | NativeFunction::BoolValueOf => {
                match self.realm.host(receiver) {
                    Some(ObjectHost::BooleanPrimitive(value)) => Ok(JsValue::String(
                        if value { "true" } else { "false" }.to_owned(),
                    )),
                    _ => Err(JsError::type_error("incompatible Boolean method receiver")),
                }
            }
            NativeFunction::ObjectPrototypePropertyIsEnumerable => {
                self.object_prototype_property_is_enumerable(receiver, arguments)
            }
            NativeFunction::ObjectPrototypeToString => Ok(JsValue::String(
                self.object_to_string_tag_for_object(receiver),
            )),
            NativeFunction::ObjectPrototypeValueOf => Ok(JsValue::Object(receiver)),
            NativeFunction::ObjectDefineGetter => {
                self.object_define_accessor(receiver, arguments, true)
            }
            NativeFunction::ObjectPreventExtensions => {
                let object = self.integrity_target(arguments, "preventExtensions")?;
                self.realm.prevent_extensions(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectSeal => {
                let object = self.integrity_target(arguments, "seal")?;
                self.realm.seal_object(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectFreeze => {
                let object = self.integrity_target(arguments, "freeze")?;
                self.realm.freeze_object(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectIsExtensible => {
                let object = self.integrity_target(arguments, "isExtensible")?;
                Ok(JsValue::Boolean(self.realm.is_extensible(object)))
            }
            NativeFunction::ObjectIsSealed => {
                let object = self.integrity_target(arguments, "isSealed")?;
                Ok(JsValue::Boolean(self.realm.is_sealed(object)))
            }
            NativeFunction::ObjectIsFrozen => {
                let object = self.integrity_target(arguments, "isFrozen")?;
                Ok(JsValue::Boolean(self.realm.is_frozen(object)))
            }
            NativeFunction::ObjectDefineSetter => {
                self.object_define_accessor(receiver, arguments, false)
            }
            NativeFunction::ObjectLookupGetter => {
                self.object_lookup_accessor(receiver, arguments, true)
            }
            NativeFunction::ObjectLookupSetter => {
                self.object_lookup_accessor(receiver, arguments, false)
            }
            NativeFunction::ErrorPrototypeToString => Ok(self.error_to_string(receiver)),
            other => self.dispatch_math_native(dom, other, receiver, arguments),
        }
    }

    /// Annex-B `__defineGetter__`/`__defineSetter__`: install or extend an
    /// own accessor descriptor, keeping the opposite slot when present.
    pub(in crate::runtime) fn object_define_accessor(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
        getter: bool,
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "__defineAccessor__")?.to_js_string();
        let function = Self::require_callable_object(
            required_argument(arguments, 1, "__defineAccessor__")?,
            &self.realm,
        )?;
        let existing = self.realm.own_property(receiver, &key);
        let (getter_slot, setter_slot) = match (getter, existing) {
            (true, Some(existing)) => (Some(function), existing.setter),
            (false, Some(existing)) => (existing.getter, Some(function)),
            (true, None) => (Some(function), None),
            (false, None) => (None, Some(function)),
        };
        if !self.realm.define_property(
            receiver,
            key,
            PropertyDescriptor {
                value: JsValue::Undefined,
                writable: false,
                getter: getter_slot,
                setter: setter_slot,
                enumerable: true,
                configurable: true,
            },
        ) {
            return Err(JsError::type_error(
                "cannot redefine non-configurable property",
            ));
        }
        Ok(JsValue::Undefined)
    }

    /// Annex-B `__lookupGetter__`/`__lookupSetter__`: own accessor slots
    /// only, `undefined` when absent.
    pub(in crate::runtime) fn object_lookup_accessor(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
        getter: bool,
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "__lookupAccessor__")?.to_js_string();
        let slot = match self.realm.own_property(receiver, &key) {
            Some(descriptor) if descriptor.is_accessor() => {
                if getter {
                    descriptor.getter
                } else {
                    descriptor.setter
                }
            }
            _ => None,
        };
        Ok(slot.map_or(JsValue::Undefined, JsValue::Object))
    }
    /// Normalized first argument of the object-integrity builtins:
    /// primitives coerce to their wrapper, mirroring `Object(...)`.
    pub(in crate::runtime) fn integrity_target(
        &mut self,
        arguments: &[JsValue],
        name: &str,
    ) -> Result<ObjectId, JsError> {
        let value = required_argument(arguments, 0, name)?;
        match value {
            JsValue::Object(object) => Ok(*object),
            JsValue::Null | JsValue::Undefined => Ok(self.realm.global_object()),
            other => self.coerce_member_base(other, name),
        }
    }
}

impl JsRuntime {
    pub(in crate::runtime) fn object_constructor(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // ECMA-262 Object(value) performs ToObject: primitives box into
        // their wrapper hosts so brand checks like
        // `Object(symbol) instanceof Symbol` behave as the spec requires
        // (core-js gates its whole feature table on this).
        match arguments.first() {
            Some(JsValue::Object(object)) => return Ok(JsValue::Object(*object)),
            Some(JsValue::Symbol(symbol)) => {
                self.ensure_heap_capacity(1)?;
                return Ok(JsValue::Object(
                    self.realm.symbol_instance_wrapper(symbol.clone()),
                ));
            }
            Some(JsValue::String(text)) => {
                self.ensure_heap_capacity(1)?;
                return Ok(JsValue::Object(self.realm.string_wrapper(text.clone())));
            }
            Some(JsValue::Number(number)) => {
                self.ensure_heap_capacity(1)?;
                return Ok(JsValue::Object(
                    self.realm.number_primitive_wrapper(*number),
                ));
            }
            Some(JsValue::Boolean(flag)) => {
                self.ensure_heap_capacity(1)?;
                return Ok(JsValue::Object(self.realm.boolean_primitive_wrapper(*flag)));
            }
            _ => {}
        }
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.create_ordinary_object()))
    }

    pub(in crate::runtime) fn error_constructor(
        &mut self,
        constructor: ObjectId,
        _kind: ErrorKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(prototype) => Some(prototype),
                _ => None,
            })
            .ok_or_else(|| JsError::type_error("Error constructor prototype is not an object"))?;
        let message = arguments
            .first()
            .and_then(|value| (!matches!(value, JsValue::Undefined)).then(|| value.to_js_string()));
        let object = self.realm.create_error(prototype, message.clone());
        // `Error.prototype.stack`: engine-captured at construction, shaped
        // like a real engine ("TypeError: msg" header + `    at` frames).
        let stack = {
            let name = self
                .realm
                .get_property(object, "name")
                .unwrap_or_else(|| JsValue::String("Error".to_owned()))
                .to_js_string();
            let header = match &message {
                Some(text) if !text.is_empty() => format!("{name}: {text}"),
                _ => name,
            };
            format!("{header}{}", self.stack_frame_lines())
        };
        self.realm.define_property(
            object,
            "stack",
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::String(stack),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        Ok(JsValue::Object(object))
    }

    pub(in crate::runtime) fn error_to_string(&self, receiver: ObjectId) -> JsValue {
        let name = self
            .realm
            .get_property(receiver, "name")
            .unwrap_or_else(|| JsValue::String("Error".to_owned()))
            .to_js_string();
        let message = self
            .realm
            .get_property(receiver, "message")
            .unwrap_or_else(|| JsValue::String(String::new()))
            .to_js_string();
        let result = match (name.is_empty(), message.is_empty()) {
            (true, _) => message,
            (_, true) => name,
            (false, false) => format!("{name}: {message}"),
        };
        JsValue::String(result)
    }

    pub(in crate::runtime) fn object_assign(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target_value = required_argument(arguments, 0, "Object.assign")?;
        let target = if matches!(target_value, JsValue::Null | JsValue::Undefined) {
            self.realm.create_ordinary_object()
        } else {
            Self::require_object(target_value)?
        };
        for source in &arguments[1..] {
            match source {
                JsValue::Symbol(_) => {}
                JsValue::Object(source) => {
                    let properties = self
                        .realm
                        .enumerable_own_properties(*source)
                        .ok_or_else(|| JsError::type_error("Object.assign source is invalid"))?;
                    for (key, value) in properties {
                        if !self.realm.set_property(target, key, value) {
                            return Err(JsError::type_error(
                                "Object.assign could not write target property",
                            ));
                        }
                    }
                }
                JsValue::String(source) => {
                    for (index, character) in source.chars().enumerate() {
                        if !self.realm.set_property(
                            target,
                            index.to_string(),
                            JsValue::String(character.to_string()),
                        ) {
                            return Err(JsError::type_error(
                                "Object.assign could not write target property",
                            ));
                        }
                    }
                }
                JsValue::Undefined | JsValue::Null | JsValue::Boolean(_) | JsValue::Number(_) => {}
            }
        }
        Ok(JsValue::Object(target))
    }

    pub(in crate::runtime) fn object_entries(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        kind: ObjectEntryKind,
    ) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, kind.function_name())?;
        let properties = match value {
            JsValue::Undefined | JsValue::Null | JsValue::Symbol(_) => Vec::new(),
            JsValue::Object(object)
                if matches!(self.realm.host(*object), Some(ObjectHost::Proxy { .. })) =>
            {
                let keys = self.proxy_own_keys(dom, *object)?;
                let mut properties = Vec::new();
                for key in keys {
                    if self
                        .proxy_get_own_property_descriptor(dom, *object, &key)?
                        .is_some_and(|descriptor| descriptor.enumerable)
                    {
                        let value = if kind == ObjectEntryKind::Keys {
                            JsValue::Undefined
                        } else {
                            self.get_member(dom, *object, &key)?
                        };
                        properties.push((key, value));
                    }
                }
                properties
            }
            JsValue::Object(object) => self
                .realm
                .enumerable_own_properties(*object)
                .ok_or_else(|| JsError::type_error("object is invalid"))?,
            JsValue::String(value) => value
                .chars()
                .enumerate()
                .map(|(index, character)| {
                    (index.to_string(), JsValue::String(character.to_string()))
                })
                .collect(),
            JsValue::Boolean(_) | JsValue::Number(_) => Vec::new(),
        };
        let mut output = Vec::with_capacity(properties.len());
        for (key, value) in properties {
            output.push(match kind {
                ObjectEntryKind::Keys => JsValue::String(key),
                ObjectEntryKind::Values => value,
                ObjectEntryKind::Entries => {
                    JsValue::Object(self.create_array_from_values(&[JsValue::String(key), value])?)
                }
            });
        }
        Ok(JsValue::Object(self.create_array_from_values(&output)?))
    }

    pub(in crate::runtime) fn object_create(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let prototype = match required_argument(arguments, 0, "Object.create")? {
            JsValue::Null => None,
            JsValue::Object(object) => Some(*object),
            // A missing optional base class is represented by `undefined` in
            // transpiled bundles.  Treat it as a null prototype so helper
            // setup can continue and the eventual subclass remains usable.
            _ => None,
        };
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.create_object(prototype)))
    }

    pub(in crate::runtime) fn object_define_property(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = required_argument(arguments, 0, "Object.defineProperty")?;
        if matches!(target, JsValue::Null | JsValue::Undefined) {
            // Keep descriptor-heavy compatibility shims from aborting an
            // entire page when an optional host object is absent.
            return Ok(target.clone());
        }
        let object = Self::require_object(target)?;
        let key_argument = required_argument(arguments, 1, "Object.defineProperty")?;
        // `ToPropertyKey` keeps symbols as symbols: a symbol key must address
        // the object's symbol slots, never the string `Symbol(desc)` form.
        // core-js (bilibili log-reporter) installs `Symbol.unscopables` on
        // `Array.prototype` through this path.
        let symbol_key = match &key_argument {
            JsValue::Symbol(symbol) => Some(symbol.clone()),
            _ => None,
        };
        let descriptor_value = required_argument(arguments, 2, "Object.defineProperty")?;
        if matches!(descriptor_value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(object));
        }
        let descriptor = match descriptor_value {
            JsValue::Object(descriptor) => *descriptor,
            // Descriptor objects are often assembled by feature-detection
            // shims.  A primitive here has no descriptor fields; accepting it
            // as an empty descriptor keeps the target usable like browsers do
            // for permissive host objects.
            JsValue::String(_) | JsValue::Number(_) | JsValue::Boolean(_) | JsValue::Symbol(_) => {
                return Ok(JsValue::Object(object));
            }
            JsValue::Null | JsValue::Undefined => unreachable!(),
        };
        let existing = match &symbol_key {
            Some(symbol) => self.realm.own_symbol_property(object, symbol),
            None => self
                .realm
                .own_property(object, &key_argument.to_js_string()),
        };
        // Field presence uses own-property checks: per spec, `{get:
        // undefined}` means "accessor with no getter", not "field absent".
        let get_field = self
            .realm
            .own_property(descriptor, "get")
            .map(|field| field.value);
        let set_field = self
            .realm
            .own_property(descriptor, "set")
            .map(|field| field.value);
        let has_value_field = self.realm.own_property(descriptor, "value").is_some();
        let has_writable_field = self.realm.own_property(descriptor, "writable").is_some();
        if (get_field.is_some() || set_field.is_some()) && (has_value_field || has_writable_field) {
            return Err(JsError::type_error(
                "Invalid property descriptor: cannot specify accessors together with value or writable",
            ));
        }
        let callable_slot =
            |field: Option<JsValue>, name: &str| -> Result<Option<ObjectId>, JsError> {
                match field {
                    None => Ok(None),
                    Some(JsValue::Object(function))
                        if JsRuntime::is_callable_object(function, &self.realm) =>
                    {
                        Ok(Some(function))
                    }
                    Some(_) => Err(JsError::type_error(format!(
                        "Property accessor {name:?} must be a function"
                    ))),
                }
            };
        let getter = callable_slot(get_field, "get")?;
        let setter = callable_slot(set_field, "set")?;
        let enumerable = self
            .realm
            .get_property(descriptor, "enumerable")
            .map_or_else(
                || {
                    existing
                        .as_ref()
                        .is_some_and(|property| property.enumerable)
                },
                |value| value.is_truthy(),
            );
        let configurable = self
            .realm
            .get_property(descriptor, "configurable")
            .map_or_else(
                || {
                    existing
                        .as_ref()
                        .is_some_and(|property| property.configurable)
                },
                |value| value.is_truthy(),
            );
        let descriptor = if getter.is_some() || setter.is_some() {
            PropertyDescriptor {
                value: JsValue::Undefined,
                writable: false,
                getter,
                setter,
                enumerable,
                configurable,
            }
        } else {
            PropertyDescriptor {
                value: self
                    .realm
                    .get_property(descriptor, "value")
                    .or_else(|| existing.as_ref().map(|property| property.value.clone()))
                    .unwrap_or(JsValue::Undefined),
                writable: self.realm.get_property(descriptor, "writable").map_or_else(
                    || existing.as_ref().is_some_and(|property| property.writable),
                    |value| value.is_truthy(),
                ),
                getter: None,
                setter: None,
                enumerable,
                configurable,
            }
        };
        let descriptor_value = descriptor.value.clone();
        if let Some(symbol) = &symbol_key {
            if !self
                .realm
                .define_symbol_property(object, symbol, descriptor)
            {
                // Mirror the string-key fallback for polyfills that re-run
                // their descriptor installer: applying the value is the
                // observable part callers rely on when the existing property
                // is writable.
                let applied = match self.realm.own_symbol_property(object, symbol) {
                    Some(current) if current.writable => self.realm.define_symbol_property(
                        object,
                        symbol,
                        PropertyDescriptor {
                            value: descriptor_value,
                            ..current
                        },
                    ),
                    _ => false,
                };
                if !applied {
                    return Err(JsError::type_error("cannot redefine object property"));
                }
            }
            return Ok(JsValue::Object(object));
        }
        let key = key_argument.to_js_string();
        if !self.realm.define_property(object, key.clone(), descriptor) {
            // Browser polyfills frequently re-run their descriptor installer
            // after a partial initialization. If the existing property is
            // writable, applying its value is the observable part callers
            // rely on; keep the descriptor's non-configurable guard for
            // genuinely read-only properties.
            if !self.realm.set_property(object, key, descriptor_value) {
                return Err(JsError::type_error("cannot redefine object property"));
            }
        }
        Ok(JsValue::Object(object))
    }

    pub(in crate::runtime) fn object_define_properties(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target_value = required_argument(arguments, 0, "Object.defineProperties")?;
        if matches!(target_value, JsValue::Null | JsValue::Undefined) {
            return Ok(target_value.clone());
        }
        let target = Self::require_object(target_value)?;
        let descriptors_value = required_argument(arguments, 1, "Object.defineProperties")?;
        if matches!(descriptors_value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(target));
        }
        let descriptors = Self::require_object(descriptors_value)?;
        let properties = self
            .realm
            .enumerable_own_properties(descriptors)
            .unwrap_or_default();
        for (key, descriptor) in properties {
            self.object_define_property(&[
                JsValue::Object(target),
                JsValue::String(key),
                descriptor,
            ])?;
        }
        Ok(JsValue::Object(target))
    }

    pub(in crate::runtime) fn object_get_own_property_descriptor(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let object = Self::require_object(required_argument(
            arguments,
            0,
            "Object.getOwnPropertyDescriptor",
        )?)?;
        let key_argument = required_argument(arguments, 1, "Object.getOwnPropertyDescriptor")?;
        let descriptor = match &key_argument {
            JsValue::Symbol(symbol) => self.realm.own_symbol_property(object, symbol),
            key => self.realm.own_property(object, &key.to_js_string()),
        };
        let Some(descriptor) = descriptor else {
            return Ok(JsValue::Undefined);
        };
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        if descriptor.is_accessor() {
            for (name, slot) in [("get", descriptor.getter), ("set", descriptor.setter)] {
                let value = slot.map_or(JsValue::Undefined, JsValue::Object);
                self.realm.set_property(result, name.to_owned(), value);
            }
        } else {
            self.realm
                .set_property(result, "value".to_owned(), descriptor.value);
            self.realm.set_property(
                result,
                "writable".to_owned(),
                JsValue::Boolean(descriptor.writable),
            );
        }
        self.realm.set_property(
            result,
            "enumerable".to_owned(),
            JsValue::Boolean(descriptor.enumerable),
        );
        self.realm.set_property(
            result,
            "configurable".to_owned(),
            JsValue::Boolean(descriptor.configurable),
        );
        Ok(JsValue::Object(result))
    }

    pub(in crate::runtime) fn object_get_own_property_descriptors(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, "Object.getOwnPropertyDescriptors")?;
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(self.realm.create_ordinary_object()));
        }
        let object = Self::require_object(value)?;
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        for key in self.realm.own_property_names(object).unwrap_or_default() {
            let Some(descriptor) = self.realm.own_property(object, &key) else {
                continue;
            };
            let descriptor_object = self.realm.create_ordinary_object();
            if descriptor.is_accessor() {
                for (name, slot) in [("get", descriptor.getter), ("set", descriptor.setter)] {
                    let value = slot.map_or(JsValue::Undefined, JsValue::Object);
                    self.realm
                        .set_property(descriptor_object, name.to_owned(), value);
                }
            } else {
                self.realm
                    .set_property(descriptor_object, "value".to_owned(), descriptor.value);
                self.realm.set_property(
                    descriptor_object,
                    "writable".to_owned(),
                    JsValue::Boolean(descriptor.writable),
                );
            }
            for (name, value) in [
                ("enumerable", JsValue::Boolean(descriptor.enumerable)),
                ("configurable", JsValue::Boolean(descriptor.configurable)),
            ] {
                self.realm
                    .set_property(descriptor_object, name.to_owned(), value);
            }
            self.realm
                .set_property(result, key, JsValue::Object(descriptor_object));
        }
        Ok(JsValue::Object(result))
    }

    pub(in crate::runtime) fn object_get_prototype_of(
        &self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, "Object.getPrototypeOf")?;
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Null);
        }
        let object = Self::require_object(value)?;
        Ok(self
            .realm
            .object(object)
            .and_then(JsObject::prototype)
            .map_or(JsValue::Null, JsValue::Object))
    }

    pub(in crate::runtime) fn object_set_prototype_of(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = required_argument(arguments, 0, "Object.setPrototypeOf")?.clone();
        if matches!(target, JsValue::Null | JsValue::Undefined) {
            return Err(JsError::type_error(
                "Object.setPrototypeOf target is nullish",
            ));
        }
        let prototype = match required_argument(arguments, 1, "Object.setPrototypeOf")? {
            JsValue::Object(object) => Some(*object),
            JsValue::Null => None,
            _ => return Err(JsError::type_error("prototype must be an object or null")),
        };
        let JsValue::Object(object) = target else {
            return Ok(target);
        };
        if self.realm.set_prototype(object, prototype) {
            Ok(JsValue::Object(object))
        } else {
            Err(JsError::type_error("cannot set prototype"))
        }
    }

    pub(in crate::runtime) fn object_get_own_property_names(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, "Object.getOwnPropertyNames")?;
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(self.create_array_from_values(&[])?));
        }
        let object = Self::require_object(value)?;
        let names = self
            .realm
            .own_property_names(object)
            .ok_or_else(|| JsError::type_error("object is invalid"))?
            .into_iter()
            .map(JsValue::String)
            .collect::<Vec<_>>();
        Ok(JsValue::Object(self.create_array_from_values(&names)?))
    }

    pub(in crate::runtime) fn object_has_own(
        &self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, "Object.hasOwn")?;
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Boolean(false));
        }
        let object = Self::require_object(value)?;
        let key_argument = required_argument(arguments, 1, "Object.hasOwn")?;
        let owned = match &key_argument {
            JsValue::Symbol(symbol) => self.realm.own_symbol_property(object, symbol).is_some(),
            key => self
                .realm
                .own_property(object, &key.to_js_string())
                .is_some(),
        };
        Ok(JsValue::Boolean(owned))
    }

    pub(in crate::runtime) fn object_prototype_has_own_property(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key_argument = required_argument(arguments, 0, "Object.prototype.hasOwnProperty")?;
        let owned = match &key_argument {
            JsValue::Symbol(symbol) => self.realm.own_symbol_property(receiver, symbol).is_some(),
            key => self
                .realm
                .own_property(receiver, &key.to_js_string())
                .is_some(),
        };
        Ok(JsValue::Boolean(owned))
    }

    pub(in crate::runtime) fn object_prototype_is_prototype_of(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> JsValue {
        let Some(&JsValue::Object(mut candidate)) = arguments.first() else {
            return JsValue::Boolean(false);
        };
        for _ in 0..self.realm.object_count() {
            let Some(object) = self.realm.object(candidate) else {
                return JsValue::Boolean(false);
            };
            let Some(prototype) = object.prototype() else {
                return JsValue::Boolean(false);
            };
            if prototype == receiver {
                return JsValue::Boolean(true);
            }
            candidate = prototype;
        }
        JsValue::Boolean(false)
    }

    pub(in crate::runtime) fn object_prototype_property_is_enumerable(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "Object.prototype.propertyIsEnumerable")?
            .to_js_string();
        Ok(JsValue::Boolean(
            self.realm
                .own_property(receiver, &key)
                .is_some_and(|descriptor| descriptor.enumerable),
        ))
    }

    /// `Object.prototype.toString` tag for any value (primitives included).
    pub(in crate::runtime) fn object_to_string_tag(&self, value: &JsValue) -> String {
        let builtin = match value {
            JsValue::Undefined => return "[object Undefined]".to_owned(),
            JsValue::Null => return "[object Null]".to_owned(),
            JsValue::Boolean(_) => "Boolean",
            JsValue::Number(_) => "Number",
            JsValue::String(_) => "String",
            JsValue::Symbol(_) => "Symbol",
            JsValue::Object(object) => {
                return self.object_to_string_tag_for_object(*object);
            }
        };
        format!("[object {builtin}]")
    }

    pub(in crate::runtime) fn object_to_string_tag_for_object(&self, object: ObjectId) -> String {
        // ECMA-262 Object.prototype.toString step 7: a string-valued
        // `Symbol.toStringTag` overrides the builtin tag. Real-world
        // polyfills (core-js) probe this before selecting their fast paths.
        let to_string_tag = JsSymbol::well_known("@@toStringTag");
        if let Some(descriptor) = self.realm.get_symbol_descriptor(object, &to_string_tag)
            && let JsValue::String(tag) = descriptor.value
        {
            return format!("[object {tag}]");
        }
        let host_tag = match self.realm.host(object) {
            Some(ObjectHost::Array) => "Array",
            Some(ObjectHost::RegExp(_)) => "RegExp",
            Some(ObjectHost::StringPrimitive(_)) => "String",
            Some(ObjectHost::Document(_)) => "HTMLDocument",
            Some(ObjectHost::TypedArray { kind, .. }) => kind.name(),
            _ if Self::is_callable_object(object, &self.realm) => "Function",
            _ => "Object",
        };
        format!("[object {host_tag}]")
    }
}
