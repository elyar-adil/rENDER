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
use crate::runtime::eval::PrimitiveHint;
use crate::runtime::types::ObjectEntryKind;
use crate::value::ErrorKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

/// A property key after `ToPropertyKey` (ECMA-262 7.1.19): a string name, or a
/// symbol that addresses the object's symbol slots and never its string form.
#[derive(Clone)]
pub(in crate::runtime) enum PropertyName {
    String(String),
    Symbol(JsSymbol),
}

/// A `ToPropertyDescriptor` result (ECMA-262 6.2.6.5). Each field is `None` when
/// the descriptor object does not have it, so an absent field is told apart
/// from one that is explicitly `undefined`.
#[derive(Default)]
struct PartialDescriptor {
    value: Option<JsValue>,
    writable: Option<bool>,
    get: AccessorField,
    set: AccessorField,
    enumerable: Option<bool>,
    configurable: Option<bool>,
}

/// A `get` or `set` field of a `ToPropertyDescriptor` result: absent, or present
/// with a function or with `undefined`.
#[derive(Clone, Copy, Default)]
enum AccessorField {
    #[default]
    Absent,
    Present(Option<ObjectId>),
}

impl AccessorField {
    /// The function this field sets, or the current one when the field is absent.
    const fn or_current(self, current: Option<ObjectId>) -> Option<ObjectId> {
        match self {
            Self::Absent => current,
            Self::Present(function) => function,
        }
    }
}

/// Fills a partial descriptor's absent fields from the current own property
/// (ECMA-262 10.1.6.3 defaults). A new property starts with every attribute
/// false, and a generic descriptor (neither a value nor an accessor field) keeps
/// an existing accessor an accessor.
fn merge_partial_descriptor(
    current: Option<&PropertyDescriptor>,
    partial: PartialDescriptor,
) -> PropertyDescriptor {
    let enumerable = partial
        .enumerable
        .unwrap_or_else(|| current.is_some_and(|property| property.enumerable));
    let configurable = partial
        .configurable
        .unwrap_or_else(|| current.is_some_and(|property| property.configurable));
    let data_requested = partial.value.is_some() || partial.writable.is_some();
    let accessor_requested = !matches!(partial.get, AccessorField::Absent)
        || !matches!(partial.set, AccessorField::Absent);
    let current_accessor = current.filter(|property| property.is_accessor());
    if accessor_requested || (!data_requested && current_accessor.is_some()) {
        // The current (getter, setter) pair, kept for each field the descriptor omits.
        let kept =
            current_accessor.map_or((None, None), |property| (property.getter, property.setter));
        return PropertyDescriptor {
            value: JsValue::Undefined,
            writable: false,
            getter: partial.get.or_current(kept.0),
            setter: partial.set.or_current(kept.1),
            enumerable,
            configurable,
        };
    }
    let current_data = current.filter(|property| !property.is_accessor());
    PropertyDescriptor {
        value: partial
            .value
            .or_else(|| current_data.map(|property| property.value.clone()))
            .unwrap_or(JsValue::Undefined),
        writable: partial
            .writable
            .unwrap_or_else(|| current_data.is_some_and(|property| property.writable)),
        getter: None,
        setter: None,
        enumerable,
        configurable,
    }
}

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
                let object = self.to_object(required_argument(
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
            NativeFunction::ObjectCreate => self.object_create(dom, arguments),
            NativeFunction::ObjectDefineProperty => self.object_define_property(dom, arguments),
            NativeFunction::ObjectDefineProperties => self.object_define_properties(dom, arguments),
            NativeFunction::ObjectGetOwnPropertyDescriptor => {
                self.object_get_own_property_descriptor(dom, arguments)
            }
            NativeFunction::ObjectGetOwnPropertyDescriptors => {
                self.object_get_own_property_descriptors(arguments)
            }
            NativeFunction::ObjectGetOwnPropertyNames => {
                self.object_get_own_property_names(arguments)
            }
            NativeFunction::ObjectGetPrototypeOf => self.object_get_prototype_of(arguments),
            NativeFunction::ObjectSetPrototypeOf => self.object_set_prototype_of(arguments),
            NativeFunction::ObjectHasOwn => self.object_has_own(dom, arguments),
            NativeFunction::ObjectPrototypeHasOwnProperty => {
                self.object_prototype_has_own_property(dom, receiver, arguments)
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
            // ECMA-262 6.1.6.1.9: `Number.prototype.valueOf` returns the
            // receiver's number, not the wrapper, so `Object(1).valueOf() === 1`
            // and `typeof` is `"number"` exactly as in every engine. The
            // String counterpart already unwraps.
            NativeFunction::NumValueOf => match self.realm.host(receiver) {
                Some(ObjectHost::NumberPrimitive(value)) => Ok(JsValue::Number(value)),
                _ => Err(JsError::type_error(
                    "Number.prototype.valueOf requires that 'this' be a Number",
                )),
            },
            NativeFunction::NumToFixed => {
                // ECMA-262 21.1.3.3: the digits argument is converted first and
                // must lie in 0..=100; a non-finite number or one of 1e21 or more
                // prints as `ToString` does.
                let digits =
                    self.to_integer_value(dom, arguments.first().unwrap_or(&JsValue::Undefined))?;
                if !(0.0..=100.0).contains(&digits) {
                    return Err(
                        self.range_error("toFixed() digits argument must be between 0 and 100")
                    );
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "digits was just checked to lie in 0..=100"
                )]
                let digits = digits as usize;
                match self.realm.host(receiver) {
                    Some(ObjectHost::NumberPrimitive(value)) => {
                        if !value.is_finite() || value.abs() >= 1e21 {
                            Ok(JsValue::String(crate::value::number_to_string(value)))
                        } else {
                            Ok(JsValue::String(format!("{value:.digits$}")))
                        }
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
                let precision = self.to_integer_value(dom, argument)?;
                if !(1.0..=100.0).contains(&precision) {
                    return Err(
                        self.range_error("toPrecision() argument must be between 1 and 100")
                    );
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "toPrecision precision is validated in the 1..=100 range"
                )]
                let precision = precision as usize;
                Ok(JsValue::String(format_number_precision(value, precision)))
            }
            // ECMA-262 21.1.3.9: an absent or `undefined` radix means 10, and
            // anything outside 2..=36 is a `RangeError`. Bundled base64 and
            // colour helpers depend on `(0xff).toString(16)`, which previously
            // returned the decimal form.
            NativeFunction::NumToString => {
                let value = match self.realm.host(receiver) {
                    Some(ObjectHost::NumberPrimitive(value)) => value,
                    _ => {
                        return Err(JsError::type_error("incompatible Number method receiver"));
                    }
                };
                let radix = match arguments.first() {
                    None | Some(JsValue::Undefined) => 10u32,
                    Some(argument) => {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "the range check below bounds the value to 2..=36"
                        )]
                        let radix = self.to_integer_value(dom, argument)? as u32;
                        if !(2..=36).contains(&radix) {
                            return Err(self.range_error(
                                "toString() radix must be an integer between 2 and 36",
                            ));
                        }
                        radix
                    }
                };
                Ok(JsValue::String(
                    crate::runtime::convert::number_to_radix_string(value, radix),
                ))
            }
            NativeFunction::BoolToString | NativeFunction::BoolValueOf => {
                match self.realm.host(receiver) {
                    Some(ObjectHost::BooleanPrimitive(value)) => Ok(JsValue::String(
                        if value { "true" } else { "false" }.to_owned(),
                    )),
                    _ => Err(JsError::type_error("incompatible Boolean method receiver")),
                }
            }
            NativeFunction::ObjectPrototypePropertyIsEnumerable => {
                self.object_prototype_property_is_enumerable(dom, receiver, arguments)
            }
            NativeFunction::ObjectPrototypeToString => Ok(JsValue::String(
                self.object_to_string_tag_for_object(receiver),
            )),
            NativeFunction::ObjectPrototypeValueOf => Ok(JsValue::Object(receiver)),
            NativeFunction::ObjectProtoGetter => {
                // Annex B.2.2.1: a primitive wrapper reports its intrinsic
                // prototype; any other object reports its own `[[Prototype]]`,
                // which is `null` for a null-prototype object.
                Ok(self
                    .realm
                    .intrinsic_prototype_for_host(receiver)
                    .or_else(|| self.realm.object(receiver).and_then(JsObject::prototype))
                    .map_or(JsValue::Undefined, JsValue::Object))
            }
            NativeFunction::ObjectProtoSetter => {
                let value = required_argument(arguments, 0, "__proto__")?;
                let prototype = match value {
                    JsValue::Object(object) => Some(*object),
                    JsValue::Null => None,
                    // A primitive that is neither `null` nor an object leaves
                    // the prototype untouched, per the spec's final step.
                    _ => return Ok(JsValue::Undefined),
                };
                self.realm.set_prototype(receiver, prototype);
                Ok(JsValue::Undefined)
            }
            NativeFunction::ObjectDefineGetter => {
                self.object_define_accessor(receiver, arguments, true)
            }
            NativeFunction::ObjectPreventExtensions => {
                let Some(object) = self.integrity_target(arguments, "preventExtensions")? else {
                    return Err(JsError::type_error(
                        "Object.preventExtensions called on null or undefined",
                    ));
                };
                self.realm.prevent_extensions(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectSeal => {
                let Some(object) = self.integrity_target(arguments, "seal")? else {
                    return Err(JsError::type_error(
                        "Object.seal called on null or undefined",
                    ));
                };
                self.realm.seal_object(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectFreeze => {
                let Some(object) = self.integrity_target(arguments, "freeze")? else {
                    return Err(JsError::type_error(
                        "Object.freeze called on null or undefined",
                    ));
                };
                self.realm.freeze_object(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectIsExtensible => {
                let Some(object) = self.integrity_target(arguments, "isExtensible")? else {
                    return Err(JsError::type_error(
                        "Object.isExtensible called on null or undefined",
                    ));
                };
                Ok(JsValue::Boolean(self.realm.is_extensible(object)))
            }
            NativeFunction::ObjectIsSealed => {
                // §20.1.2.13: a non-object target is always sealed.
                let Some(object) = self.integrity_target(arguments, "isSealed")? else {
                    return Ok(JsValue::Boolean(true));
                };
                Ok(JsValue::Boolean(self.realm.is_sealed(object)))
            }
            NativeFunction::ObjectIsFrozen => {
                // §20.1.2.14: a non-object target is always frozen.
                let Some(object) = self.integrity_target(arguments, "isFrozen")? else {
                    return Ok(JsValue::Boolean(true));
                };
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
            NativeFunction::ErrorPrototypeToString => Ok(self.error_to_string(dom, receiver)),
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
    /// Normalized first argument of the object-integrity builtins.
    ///
    /// Primitives coerce to their wrapper, mirroring `Object(...)`. Returns
    /// `None` only for a nullish argument: §20.1.2.{5,7,13,14,15,16} treat a
    /// non-object target as "always sealed/frozen" for the two `is*` queries
    /// and as a `ToObject` throw for the four mutating ones, so the caller
    /// decides. Returning the global object here would report
    /// `Object.isFrozen(null) === false`, which no engine does.
    pub(in crate::runtime) fn integrity_target(
        &mut self,
        arguments: &[JsValue],
        name: &str,
    ) -> Result<Option<ObjectId>, JsError> {
        let value = required_argument(arguments, 0, name)?;
        match value {
            JsValue::Object(object) => Ok(Some(*object)),
            JsValue::Null | JsValue::Undefined => Ok(None),
            other => self.to_object(other).map(Some),
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
        // (core-js gates its whole feature table on this). `Object(null)`
        // and `Object(undefined)` yield a fresh ordinary object, which is
        // the one place ToObject's throw is not observable.
        if let Some(value) = arguments.first()
            && !matches!(value, JsValue::Null | JsValue::Undefined)
        {
            return Ok(JsValue::Object(self.to_object(value)?));
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

    /// ECMA-262 §20.1.3.1 `Error.prototype.toString`, read through `Get` so an
    /// accessor counts.
    ///
    /// `Get`, not the descriptor's value slot: `DOMException.prototype`'s `name`
    /// and `message` are `WebIDL` §2.5.2 readonly attributes, which are accessors
    /// reading internal slots, so a read that skipped the accessor would see the
    /// accessor's unused value slot and answer `undefined` for a perfectly good
    /// exception. The same is true of a user-written `class MyError extends Error
    /// { get name() { return "My"; } }`, which this now also honours.
    pub(in crate::runtime) fn error_to_string(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> JsValue {
        let name = self.error_string_member(dom, receiver, "name", "Error");
        let message = self.error_string_member(dom, receiver, "message", "");
        let result = match (name.is_empty(), message.is_empty()) {
            (true, _) => message,
            (_, true) => name,
            (false, false) => format!("{name}: {message}"),
        };
        JsValue::String(result)
    }

    /// One `Get(this, name)` of `Error.prototype.toString`, with the
    /// `Error.prototype` default the specification gives for a missing member.
    fn error_string_member(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        key: &str,
        fallback: &str,
    ) -> String {
        let value = self
            .realm
            .get_descriptor(receiver, key)
            .map(|descriptor| {
                if descriptor.is_accessor() {
                    self.get_value(dom, receiver, key)
                } else {
                    Ok(descriptor.value)
                }
            })
            .unwrap_or(Ok(JsValue::Undefined));
        match value {
            Ok(JsValue::Undefined) | Err(_) => fallback.to_owned(),
            Ok(value) => value.to_js_string(),
        }
    }

    pub(in crate::runtime) fn object_assign(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target_value = required_argument(arguments, 0, "Object.assign")?;
        // §20.1.2.1 step 1 is `ToObject(Target)`, so a primitive target becomes
        // its wrapper and the wrapper is what comes back. A nullish target is
        // deliberately replaced with a fresh object instead of throwing: real
        // shims pass optional host objects here and expect a usable target.
        let target = if matches!(target_value, JsValue::Null | JsValue::Undefined) {
            self.realm.create_ordinary_object()
        } else {
            self.to_object(target_value)?
        };
        for source in &arguments[1..] {
            match source {
                // A symbol or BigInt source wraps to an object with no own
                // enumerable properties, so there is nothing to copy.
                JsValue::Symbol(_) | JsValue::BigInt(_) => {}
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
        // `EnumerableOwnProperties` (§7.3.20) step 2 is `ToObject`, so a
        // primitive contributes its own enumerable keys: a String primitive's
        // indexed characters, nothing for Number/Boolean/Symbol. A nullish
        // argument is deliberately answered with an empty list rather than a
        // throw, because page feature-detection relies on that leniency.
        let properties = if matches!(value, JsValue::Null | JsValue::Undefined) {
            Vec::new()
        } else {
            let object = self.to_object(value)?;
            if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
                let keys = self.proxy_own_keys(dom, object)?;
                let mut properties = Vec::new();
                for key in keys {
                    if self
                        .proxy_get_own_property_descriptor(dom, object, &key)?
                        .is_some_and(|descriptor| descriptor.enumerable)
                    {
                        let value = if kind == ObjectEntryKind::Keys {
                            JsValue::Undefined
                        } else {
                            self.get_member(dom, object, &key)?
                        };
                        properties.push((key, value));
                    }
                }
                properties
            } else {
                self.realm
                    .enumerable_own_properties(object)
                    .ok_or_else(|| JsError::type_error("object is invalid"))?
            }
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
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.2 step 1: the prototype is an object or null, nothing else.
        let prototype = match required_argument(arguments, 0, "Object.create")? {
            JsValue::Null => None,
            JsValue::Object(object) => Some(*object),
            _ => {
                return Err(JsError::type_error(
                    "Object prototype may only be an Object or null",
                ));
            }
        };
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        // §20.1.2.2 step 2: an absent or undefined `Properties` defines nothing;
        // any other value goes through `ObjectDefineProperties`.
        if let Some(properties) = arguments.get(1)
            && !matches!(properties, JsValue::Undefined)
        {
            self.object_define_properties(dom, &[JsValue::Object(object), properties.clone()])?;
        }
        Ok(JsValue::Object(object))
    }

    /// ECMA-262 7.1.19 `ToPropertyKey`. An object key runs `ToPrimitive` with
    /// the string hint first, so `[1, 2]` names `"1,2"`; a symbol that comes out
    /// of that stays a symbol, which addresses the object's symbol slots.
    pub(in crate::runtime) fn to_property_name(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<PropertyName, JsError> {
        match value {
            JsValue::Symbol(symbol) => Ok(PropertyName::Symbol(symbol.clone())),
            JsValue::Object(_) => {
                match self.to_primitive_with_hint(dom, value.clone(), PrimitiveHint::String)? {
                    JsValue::Symbol(symbol) => Ok(PropertyName::Symbol(symbol)),
                    primitive => Ok(PropertyName::String(primitive.to_js_string())),
                }
            }
            other => Ok(PropertyName::String(other.to_js_string())),
        }
    }

    /// ECMA-262 7.3.12 `HasProperty` for a string key: an ordinary chain lookup,
    /// or the proxy `has` trap.
    fn has_property_named(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
    ) -> Result<bool, JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            return self.proxy_has(dom, object, key);
        }
        Ok(self.realm.get_descriptor(object, key).is_some())
    }

    /// `[[Get]]` of a field of a descriptor object, for a string or symbol key.
    fn get_named_or_symbol(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
    ) -> Result<JsValue, JsError> {
        match key {
            PropertyName::String(name) => self.get_member(dom, object, name),
            PropertyName::Symbol(symbol) => self.get_symbol_value(dom, object, symbol),
        }
    }

    pub(in crate::runtime) fn object_define_property(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.4 step 1: a target that is not an object is a TypeError. Unlike
        // the other `Object` statics there is no `ToObject` here.
        let JsValue::Object(object) = required_argument(arguments, 0, "Object.defineProperty")?
        else {
            return Err(JsError::type_error(
                "Object.defineProperty called on non-object",
            ));
        };
        let object = *object;
        // Step 2 is `ToPropertyKey(P)`. core-js (bilibili log-reporter) installs
        // `Symbol.unscopables` on `Array.prototype` through this path, so a
        // symbol key must keep addressing the symbol slots.
        let key_argument = arguments.get(1).unwrap_or(&JsValue::Undefined);
        let key = self.to_property_name(dom, key_argument)?;
        let descriptor_value = required_argument(arguments, 2, "Object.defineProperty")?;
        // §6.2.6.5 step 1: a descriptor that is not an object (undefined included)
        // is a TypeError.
        let JsValue::Object(descriptor) = descriptor_value else {
            return Err(JsError::type_error(
                "Property description must be an object",
            ));
        };
        let partial = self.to_property_descriptor(dom, *descriptor)?;
        self.apply_property_descriptor(object, &key, partial)?;
        Ok(JsValue::Object(object))
    }

    pub(in crate::runtime) fn object_define_properties(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.3 step 1: a target that is not an object is a TypeError.
        let JsValue::Object(target) = required_argument(arguments, 0, "Object.defineProperties")?
        else {
            return Err(JsError::type_error(
                "Object.defineProperties called on non-object",
            ));
        };
        let target = *target;
        // §20.1.2.3 step 2: `ToObject(Properties)`, so undefined and null throw.
        let descriptors_value = required_argument(arguments, 1, "Object.defineProperties")?;
        let descriptors = self.to_object(descriptors_value)?;
        // Steps 3-4: every descriptor is read before any property is defined. The
        // keys are `OwnPropertyKeys(props)`, strings then symbols; each enumerable
        // one is re-checked and read with [[Get]] when the loop reaches it.
        let mut keys = self
            .realm
            .own_property_names(descriptors)
            .unwrap_or_default()
            .into_iter()
            .map(PropertyName::String)
            .collect::<Vec<_>>();
        keys.extend(
            self.realm
                .own_symbols(descriptors)
                .unwrap_or_default()
                .into_iter()
                .map(PropertyName::Symbol),
        );
        let mut pending = Vec::with_capacity(keys.len());
        for key in keys {
            let enumerable = match &key {
                PropertyName::String(name) => self
                    .realm
                    .own_property(descriptors, name)
                    .is_some_and(|descriptor| descriptor.enumerable),
                PropertyName::Symbol(symbol) => self
                    .realm
                    .own_symbol_property(descriptors, symbol)
                    .is_some_and(|descriptor| descriptor.enumerable),
            };
            if !enumerable {
                continue;
            }
            let JsValue::Object(descriptor) = self.get_named_or_symbol(dom, descriptors, &key)?
            else {
                return Err(JsError::type_error(
                    "Property description must be an object",
                ));
            };
            pending.push((key, self.to_property_descriptor(dom, descriptor)?));
        }
        for (key, partial) in pending {
            self.apply_property_descriptor(target, &key, partial)?;
        }
        Ok(JsValue::Object(target))
    }

    /// ECMA-262 6.2.6.5 `ToPropertyDescriptor`. A field counts as present when
    /// the descriptor has it through `HasProperty`, so an inherited field counts
    /// too; its value is read with [[Get]], so an accessor runs. An absent field
    /// stays `None` so the caller can keep the current value.
    fn to_property_descriptor(
        &mut self,
        dom: &mut Dom,
        descriptor: ObjectId,
    ) -> Result<PartialDescriptor, JsError> {
        let mut partial = PartialDescriptor::default();
        if self.has_property_named(dom, descriptor, "enumerable")? {
            let value = self.get_member(dom, descriptor, "enumerable")?;
            partial.enumerable = Some(value.is_truthy());
        }
        if self.has_property_named(dom, descriptor, "configurable")? {
            let value = self.get_member(dom, descriptor, "configurable")?;
            partial.configurable = Some(value.is_truthy());
        }
        if self.has_property_named(dom, descriptor, "value")? {
            partial.value = Some(self.get_member(dom, descriptor, "value")?);
        }
        if self.has_property_named(dom, descriptor, "writable")? {
            let value = self.get_member(dom, descriptor, "writable")?;
            partial.writable = Some(value.is_truthy());
        }
        if self.has_property_named(dom, descriptor, "get")? {
            let value = self.get_member(dom, descriptor, "get")?;
            partial.get = AccessorField::Present(self.accessor_slot(&value, "get")?);
        }
        if self.has_property_named(dom, descriptor, "set")? {
            let value = self.get_member(dom, descriptor, "set")?;
            partial.set = AccessorField::Present(self.accessor_slot(&value, "set")?);
        }
        let accessor_present = !matches!(partial.get, AccessorField::Absent)
            || !matches!(partial.set, AccessorField::Absent);
        if accessor_present && (partial.value.is_some() || partial.writable.is_some()) {
            return Err(JsError::type_error(
                "Invalid property descriptor: cannot specify accessors together with value or writable",
            ));
        }
        Ok(partial)
    }

    /// A `get` or `set` field: `undefined` is an accessor with no function, and
    /// anything that is not callable is a `TypeError` (§6.2.6.5 steps 8-9).
    fn accessor_slot(&self, value: &JsValue, name: &str) -> Result<Option<ObjectId>, JsError> {
        match value {
            JsValue::Undefined => Ok(None),
            JsValue::Object(function) if JsRuntime::is_callable_object(*function, &self.realm) => {
                Ok(Some(*function))
            }
            _ => Err(JsError::type_error(format!(
                "Property accessor {name:?} must be a function"
            ))),
        }
    }

    /// `DefinePropertyOrThrow`: merge the partial descriptor into the current own
    /// property, then apply it. The object refusing the change (non-extensible
    /// target, or a non-configurable property that cannot take it) is a `TypeError`.
    fn apply_property_descriptor(
        &mut self,
        object: ObjectId,
        key: &PropertyName,
        partial: PartialDescriptor,
    ) -> Result<(), JsError> {
        let applied = match key {
            PropertyName::String(name) => {
                let current = self.realm.own_property(object, name);
                let merged = merge_partial_descriptor(current.as_ref(), partial);
                self.realm.define_property(object, name.clone(), merged)
            }
            PropertyName::Symbol(symbol) => {
                let current = self.realm.own_symbol_property(object, symbol);
                let merged = merge_partial_descriptor(current.as_ref(), partial);
                self.realm.define_symbol_property(object, symbol, merged)
            }
        };
        if applied {
            Ok(())
        } else {
            Err(JsError::type_error("cannot redefine object property"))
        }
    }

    pub(in crate::runtime) fn object_get_own_property_descriptor(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.4: `ToObject` first, so
        // `Object.getOwnPropertyDescriptor("ab", "0")` reports the String
        // exotic object's indexed character with spec attributes.
        let object = self.to_object(required_argument(
            arguments,
            0,
            "Object.getOwnPropertyDescriptor",
        )?)?;
        let key_argument = arguments.get(1).unwrap_or(&JsValue::Undefined);
        let key = self.to_property_name(dom, key_argument)?;
        let descriptor = match &key {
            PropertyName::Symbol(symbol) => self.realm.own_symbol_property(object, symbol),
            PropertyName::String(name) => self.realm.own_property(object, name),
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
        // §20.1.2.7: `ToObject` first, so a primitive contributes its own
        // descriptors (a String primitive's indices and `length`).
        let value = required_argument(arguments, 0, "Object.getOwnPropertyDescriptors")?;
        let object = self.to_object(value)?;
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
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.2: `Object.getPrototypeOf` starts with `ToObject`, so every
        // primitive reports its wrapper's `[[Prototype]]` and only `null` or
        // `undefined` throws. `Object.getPrototypeOf("x") === String.prototype`
        // in every engine, and a 1.3MB production bundle relies on it.
        let value = required_argument(arguments, 0, "Object.getPrototypeOf")?;
        let object = self.to_object(value)?;
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
        // §20.1.2.15: `ToObject` first, so a String primitive reports its
        // indexed characters plus `length` like every other engine.
        let value = required_argument(arguments, 0, "Object.getOwnPropertyNames")?;
        let object = self.to_object(value)?;
        let names = self
            .realm
            .own_property_names(object)
            .ok_or_else(|| JsError::type_error("object is invalid"))?
            .into_iter()
            .map(JsValue::String)
            .collect::<Vec<_>>();
        Ok(JsValue::Object(self.create_array_from_values(&names)?))
    }

    /// Whether `object` has an own property under `key`, string or symbol.
    fn has_own_named(&self, object: ObjectId, key: &PropertyName) -> bool {
        match key {
            PropertyName::Symbol(symbol) => {
                self.realm.own_symbol_property(object, symbol).is_some()
            }
            PropertyName::String(name) => self.realm.own_property(object, name).is_some(),
        }
    }

    pub(in crate::runtime) fn object_has_own(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.18: `ToObject` first, so `Object.hasOwn("a", "0")` is true
        // against a String wrapper's indexed characters; then `ToPropertyKey`.
        let value = required_argument(arguments, 0, "Object.hasOwn")?;
        let object = self.to_object(value)?;
        let key_argument = arguments.get(1).unwrap_or(&JsValue::Undefined);
        let key = self.to_property_name(dom, key_argument)?;
        Ok(JsValue::Boolean(self.has_own_named(object, &key)))
    }

    pub(in crate::runtime) fn object_prototype_has_own_property(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.3.2: `ToPropertyKey(V)` comes before `ToObject(this)`.
        let key_argument = arguments.first().unwrap_or(&JsValue::Undefined);
        let key = self.to_property_name(dom, key_argument)?;
        Ok(JsValue::Boolean(self.has_own_named(receiver, &key)))
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
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.3.4: `ToPropertyKey(V)` first, then `ToObject(this)`.
        let key_argument = arguments.first().unwrap_or(&JsValue::Undefined);
        let key = self.to_property_name(dom, key_argument)?;
        let enumerable = match &key {
            PropertyName::Symbol(symbol) => self
                .realm
                .own_symbol_property(receiver, symbol)
                .is_some_and(|descriptor| descriptor.enumerable),
            PropertyName::String(name) => self
                .realm
                .own_property(receiver, name)
                .is_some_and(|descriptor| descriptor.enumerable),
        };
        Ok(JsValue::Boolean(enumerable))
    }

    /// `Object.prototype.toString` tag for any value (primitives included).
    pub(in crate::runtime) fn object_to_string_tag(&self, value: &JsValue) -> String {
        let builtin = match value {
            JsValue::Undefined => return "[object Undefined]".to_owned(),
            JsValue::Null => return "[object Null]".to_owned(),
            JsValue::Boolean(_) => "Boolean",
            JsValue::Number(_) => "Number",
            JsValue::BigInt(_) => "BigInt",
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
        // ECMA-262 20.1.3.6 step 5: the builtin tag comes from the internal
        // slot, so every host the engine models has to name itself. Polyfills
        // branch on exactly these strings, and a host that falls through to
        // `"Object"` is indistinguishable from a plain object to them.
        let host_tag = match self.realm.host(object) {
            Some(ObjectHost::Array) => "Array",
            Some(ObjectHost::RegExp(_)) => "RegExp",
            Some(ObjectHost::StringPrimitive(_)) => "String",
            Some(ObjectHost::NumberPrimitive(_)) => "Number",
            Some(ObjectHost::BooleanPrimitive(_)) => "Boolean",
            Some(ObjectHost::SymbolInstance(_)) => "Symbol",
            Some(ObjectHost::DateInstance(_)) => "Date",
            Some(ObjectHost::ErrorConstructor(_) | ObjectHost::ErrorInstance) => "Error",
            // WebIDL §2.7.2 gives the interface prototype a @@toStringTag of the
            // interface name, and ECMA-262 `Object.prototype.toString` step 20
            // reads it through the prototype chain, so the tag above already
            // answers "DOMException" for an instance. This arm is the fallback
            // for the case where that tag has been deleted off the prototype
            // chain - and "Error" is right there, because §3.14.1 gives the
            // object an [[ErrorData]] slot like any other built-in exception.
            Some(ObjectHost::DomExceptionConstructor | ObjectHost::DomException { .. }) => {
                "DOMException"
            }
            Some(ObjectHost::Promise(_) | ObjectHost::PromiseSettler { .. }) => "Promise",
            Some(ObjectHost::Collection { kind, .. } | ObjectHost::CollectionConstructor(kind)) => {
                kind.tag()
            }
            Some(ObjectHost::CollectionIterator { .. } | ObjectHost::IteratorHelper { .. }) => {
                match self
                    .realm
                    .get_symbol_descriptor(object, &JsSymbol::well_known("@@toStringTag"))
                {
                    Some(descriptor) => {
                        return format!("[object {}]", descriptor.value.to_js_string());
                    }
                    None => "Object",
                }
            }
            Some(ObjectHost::Document(_)) => "HTMLDocument",
            Some(ObjectHost::TypedArray { kind, .. }) => kind.name(),
            Some(
                ObjectHost::XmlHttpRequest(_)
                | ObjectHost::AbortController
                | ObjectHost::AbortSignal
                | ObjectHost::FormData { .. }
                | ObjectHost::Response { .. }
                | ObjectHost::ResponseHeaders { .. }
                | ObjectHost::Blob { .. }
                | ObjectHost::UrlInstance(_)
                | ObjectHost::UrlSearchParams { .. }
                | ObjectHost::Storage
                | ObjectHost::IntersectionObserver { .. }
                | ObjectHost::MutationObserver { .. }
                | ObjectHost::VideoElement(_),
            ) => "Object",
            _ if Self::is_callable_object(object, &self.realm) => "Function",
            _ => "Object",
        };
        format!("[object {host_tag}]")
    }
}
