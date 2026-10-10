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
use crate::runtime::builtins::array::array_index;
use crate::runtime::convert::format_number_precision;
use crate::runtime::convert::required_argument;
use crate::runtime::convert::uint32_of_number;
use crate::runtime::eval::PrimitiveHint;
use crate::runtime::types::ObjectEntryKind;
use crate::value::ErrorKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::same_value;
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
#[derive(Clone, Default)]
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

/// Whether two complete descriptors name the same attributes and value, which
/// `ValidateAndApplyPropertyDescriptor` accepts even on a non-configurable slot.
fn same_descriptor(left: &PropertyDescriptor, right: &PropertyDescriptor) -> bool {
    left.enumerable == right.enumerable
        && left.configurable == right.configurable
        && left.getter == right.getter
        && left.setter == right.setter
        && left.writable == right.writable
        && same_value(&left.value, &right.value)
}

/// The `length` slot of an array as a `u32`, and whether it is writable.
fn array_length_slot(descriptor: Option<&PropertyDescriptor>) -> (u32, bool) {
    match descriptor {
        Some(descriptor) => match descriptor.value {
            JsValue::Number(length) => (uint32_of_number(length), descriptor.writable),
            _ => (0, true),
        },
        None => (0, true),
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
            NativeFunction::ObjectAssign => self.object_assign(dom, arguments),
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
                self.object_get_own_property_descriptors(dom, arguments)
            }
            NativeFunction::ObjectGetOwnPropertyNames => {
                self.object_get_own_property_names(dom, arguments)
            }
            NativeFunction::ObjectGetPrototypeOf => self.object_get_prototype_of(dom, arguments),
            NativeFunction::ObjectSetPrototypeOf => self.object_set_prototype_of(arguments),
            NativeFunction::ObjectHasOwn => self.object_has_own(dom, arguments),
            NativeFunction::ObjectPrototypeHasOwnProperty => {
                self.object_prototype_has_own_property(dom, receiver, arguments)
            }
            NativeFunction::ObjectPrototypeIsPrototypeOf => {
                self.object_prototype_is_prototype_of(dom, receiver, arguments)
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
            NativeFunction::BoolToString => match self.realm.host(receiver) {
                Some(ObjectHost::BooleanPrimitive(value)) => Ok(JsValue::String(
                    if value { "true" } else { "false" }.to_owned(),
                )),
                _ => Err(JsError::type_error("incompatible Boolean method receiver")),
            },
            // ECMA-262 20.3.3.3: `valueOf` returns the Boolean itself.
            NativeFunction::BoolValueOf => match self.realm.host(receiver) {
                Some(ObjectHost::BooleanPrimitive(value)) => Ok(JsValue::Boolean(value)),
                _ => Err(JsError::type_error("incompatible Boolean method receiver")),
            },
            NativeFunction::ObjectPrototypePropertyIsEnumerable => {
                self.object_prototype_property_is_enumerable(dom, receiver, arguments)
            }
            NativeFunction::ObjectPrototypeToString => {
                let tag = self.object_to_string_tag(dom, &JsValue::Object(receiver))?;
                Ok(JsValue::String(tag))
            }
            // ECMA-262 20.1.3.5: `Invoke(this, "toString")`.
            NativeFunction::ObjectPrototypeToLocaleString => {
                let method = self.get_member(dom, receiver, "toString")?;
                let method = Self::require_callable_object(&method, &self.realm)?;
                self.call_with_this(dom, method, &[], JsValue::Object(receiver))
            }
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
            // Annex B.2.2.1.2: a non-object value, or a non-object receiver,
            // leaves the prototype untouched; a refused change throws.
            NativeFunction::ObjectProtoSetter => {
                let prototype = match arguments.first() {
                    Some(JsValue::Object(object)) => Some(*object),
                    Some(JsValue::Null) => None,
                    _ => return Ok(JsValue::Undefined),
                };
                if !self.realm.set_prototype(receiver, prototype) {
                    return Err(JsError::type_error("cannot set prototype"));
                }
                Ok(JsValue::Undefined)
            }
            NativeFunction::ObjectDefineGetter => {
                self.object_define_accessor(dom, receiver, arguments, true)
            }
            NativeFunction::ObjectDefineSetter => {
                self.object_define_accessor(dom, receiver, arguments, false)
            }
            NativeFunction::ObjectLookupGetter => {
                self.object_lookup_accessor(dom, receiver, arguments, true)
            }
            NativeFunction::ObjectLookupSetter => {
                self.object_lookup_accessor(dom, receiver, arguments, false)
            }
            // §20.1.2.5, 20.1.2.7, 20.1.2.15, 20.1.2.16: a non-object argument
            // is returned unchanged; the `is*` queries answer for it directly
            // (§20.1.2.13 and 20.1.2.14 say sealed and frozen, and §20.1.2.11
            // says not extensible), with no `ToObject`.
            NativeFunction::ObjectPreventExtensions => match Self::integrity_subject(arguments) {
                Ok(object) => {
                    if self.prevent_extensions_value(dom, object)? {
                        Ok(JsValue::Object(object))
                    } else {
                        Err(JsError::type_error("cannot prevent extensions"))
                    }
                }
                Err(value) => Ok(value),
            },
            NativeFunction::ObjectSeal => match Self::integrity_subject(arguments) {
                Ok(object) => {
                    self.set_integrity_level(dom, object, false)?;
                    Ok(JsValue::Object(object))
                }
                Err(value) => Ok(value),
            },
            NativeFunction::ObjectFreeze => match Self::integrity_subject(arguments) {
                Ok(object) => {
                    self.set_integrity_level(dom, object, true)?;
                    Ok(JsValue::Object(object))
                }
                Err(value) => Ok(value),
            },
            NativeFunction::ObjectIsExtensible => match Self::integrity_subject(arguments) {
                Ok(object) => Ok(JsValue::Boolean(self.is_extensible_value(dom, object)?)),
                Err(_) => Ok(JsValue::Boolean(false)),
            },
            NativeFunction::ObjectIsSealed => match Self::integrity_subject(arguments) {
                Ok(object) => Ok(JsValue::Boolean(
                    self.test_integrity_level(dom, object, false)?,
                )),
                Err(_) => Ok(JsValue::Boolean(true)),
            },
            NativeFunction::ObjectIsFrozen => match Self::integrity_subject(arguments) {
                Ok(object) => Ok(JsValue::Boolean(
                    self.test_integrity_level(dom, object, true)?,
                )),
                Err(_) => Ok(JsValue::Boolean(true)),
            },
            NativeFunction::ErrorPrototypeToString => Ok(self.error_to_string(dom, receiver)),
            other => self.dispatch_math_native(dom, other, receiver, arguments),
        }
    }

    /// The object an integrity builtin acts on, or the argument itself when it
    /// is not an object.
    fn integrity_subject(arguments: &[JsValue]) -> Result<ObjectId, JsValue> {
        match arguments.first() {
            Some(JsValue::Object(object)) => Ok(*object),
            other => Err(other.cloned().unwrap_or(JsValue::Undefined)),
        }
    }

    /// `Object.prototype.__defineGetter__` and `__defineSetter__` (Annex B.2.2.2
    /// and B.2.2.3): the callable check precedes `ToPropertyKey(P)`, and the
    /// define is a `DefinePropertyOrThrow` of an accessor with only one half.
    pub(in crate::runtime) fn object_define_accessor(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        getter: bool,
    ) -> Result<JsValue, JsError> {
        let function = Self::require_callable_object(
            arguments.get(1).unwrap_or(&JsValue::Undefined),
            &self.realm,
        )?;
        let key = self.to_property_name(dom, arguments.first().unwrap_or(&JsValue::Undefined))?;
        let accessor = AccessorField::Present(Some(function));
        let partial = if getter {
            PartialDescriptor {
                get: accessor,
                enumerable: Some(true),
                configurable: Some(true),
                ..PartialDescriptor::default()
            }
        } else {
            PartialDescriptor {
                set: accessor,
                enumerable: Some(true),
                configurable: Some(true),
                ..PartialDescriptor::default()
            }
        };
        self.define_property_or_throw(dom, receiver, &key, partial)?;
        Ok(JsValue::Undefined)
    }

    /// `Object.prototype.__lookupGetter__` and `__lookupSetter__` (Annex
    /// B.2.2.4 and B.2.2.5): the first accessor on the prototype chain for the
    /// key, or `undefined` when a data property or nothing is found first.
    pub(in crate::runtime) fn object_lookup_accessor(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        getter: bool,
    ) -> Result<JsValue, JsError> {
        let key = self.to_property_name(dom, arguments.first().unwrap_or(&JsValue::Undefined))?;
        let mut holder = Some(receiver);
        while let Some(object) = holder {
            if let Some(descriptor) = self.own_descriptor(dom, object, &key)? {
                if !descriptor.is_accessor() {
                    return Ok(JsValue::Undefined);
                }
                let slot = if getter {
                    descriptor.getter
                } else {
                    descriptor.setter
                };
                return Ok(slot.map_or(JsValue::Undefined, JsValue::Object));
            }
            holder = self.prototype_of(dom, object)?;
        }
        Ok(JsValue::Undefined)
    }

    /// ECMA-262 7.3.15 `SetIntegrityLevel`, for `Object.seal` (`frozen` false)
    /// and `Object.freeze` (`frozen` true).
    fn set_integrity_level(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        frozen: bool,
    ) -> Result<(), JsError> {
        if !self.prevent_extensions_value(dom, object)? {
            return Err(JsError::type_error("cannot prevent extensions"));
        }
        for key in self.own_property_keys(dom, object)? {
            let partial = if frozen {
                match self.own_descriptor(dom, object, &key)? {
                    None => continue,
                    Some(descriptor) if descriptor.is_accessor() => PartialDescriptor {
                        configurable: Some(false),
                        ..PartialDescriptor::default()
                    },
                    Some(_) => PartialDescriptor {
                        configurable: Some(false),
                        writable: Some(false),
                        ..PartialDescriptor::default()
                    },
                }
            } else {
                PartialDescriptor {
                    configurable: Some(false),
                    ..PartialDescriptor::default()
                }
            };
            self.define_property_or_throw(dom, object, &key, partial)?;
        }
        Ok(())
    }

    /// ECMA-262 7.3.16 `TestIntegrityLevel`, for `Object.isSealed` and
    /// `Object.isFrozen`.
    fn test_integrity_level(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        frozen: bool,
    ) -> Result<bool, JsError> {
        if self.is_extensible_value(dom, object)? {
            return Ok(false);
        }
        for key in self.own_property_keys(dom, object)? {
            let Some(descriptor) = self.own_descriptor(dom, object, &key)? else {
                continue;
            };
            if descriptor.configurable {
                return Ok(false);
            }
            if frozen && !descriptor.is_accessor() && descriptor.writable {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// `[[PreventExtensions]]`, through the proxy trap when there is one.
    fn prevent_extensions_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<bool, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Proxy { target, handler }) => {
                match self.proxy_trap(dom, handler, "preventExtensions")? {
                    Some(trap) => {
                        let result = self.call_with_this(
                            dom,
                            trap,
                            &[JsValue::Object(target)],
                            JsValue::Object(handler),
                        )?;
                        Ok(result.is_truthy())
                    }
                    None => self.prevent_extensions_value(dom, target),
                }
            }
            _ => Ok(self.realm.prevent_extensions(object)),
        }
    }

    /// `[[IsExtensible]]`, through the proxy trap when there is one.
    fn is_extensible_value(&mut self, dom: &mut Dom, object: ObjectId) -> Result<bool, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Proxy { target, handler }) => {
                match self.proxy_trap(dom, handler, "isExtensible")? {
                    Some(trap) => {
                        let result = self.call_with_this(
                            dom,
                            trap,
                            &[JsValue::Object(target)],
                            JsValue::Object(handler),
                        )?;
                        Ok(result.is_truthy())
                    }
                    None => self.is_extensible_value(dom, target),
                }
            }
            _ => Ok(self.realm.is_extensible(object)),
        }
    }

    /// Whether `object` is an Array exotic object, looking through proxies
    /// (ECMA-262 7.2.2 `IsArray`).
    fn is_array_object(&self, object: ObjectId) -> bool {
        match self.realm.host(object) {
            Some(ObjectHost::Array) => true,
            Some(ObjectHost::Proxy { target, .. }) => self.is_array_object(target),
            _ => false,
        }
    }

    /// Whether `object` has a [[Call]], looking through proxies.
    fn is_callable_value(&self, object: ObjectId) -> bool {
        match self.realm.host(object) {
            Some(ObjectHost::Proxy { target, .. }) => self.is_callable_value(target),
            host => host.is_some_and(|host| host.is_callable()),
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

    /// ECMA-262 20.1.2.1 `Object.assign`: each enumerable own property of each
    /// source is read with [[Get]] and written with a throwing [[Set]]. Symbol
    /// keys are copied too, after the string keys.
    pub(in crate::runtime) fn object_assign(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target_value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let target = self.to_object(&target_value)?;
        for source in arguments.iter().skip(1) {
            if matches!(source, JsValue::Undefined | JsValue::Null) {
                continue;
            }
            let from = self.to_object(source)?;
            for key in self.own_property_keys(dom, from)? {
                let Some(descriptor) = self.own_descriptor(dom, from, &key)? else {
                    continue;
                };
                if !descriptor.enumerable {
                    continue;
                }
                let value = self.get_named_or_symbol(dom, from, &key)?;
                if !self.set_property_checked(dom, target, &key, value)? {
                    return Err(JsError::type_error(
                        "Object.assign could not write target property",
                    ));
                }
            }
        }
        Ok(JsValue::Object(target))
    }

    /// `Object.keys`, `Object.values` and `Object.entries`
    /// (ECMA-262 7.3.22 `EnumerableOwnProperties`): a nullish argument throws,
    /// a value is read with [[Get]] only for `values` and `entries`.
    pub(in crate::runtime) fn object_entries(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        kind: ObjectEntryKind,
    ) -> Result<JsValue, JsError> {
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let object = self.to_object(&value)?;
        let pinned = self.transient_roots.len();
        let mut output = Vec::new();
        for name in self.own_string_keys(dom, object)? {
            let key = PropertyName::String(name.clone());
            let Some(descriptor) = self.own_descriptor(dom, object, &key)? else {
                continue;
            };
            if !descriptor.enumerable {
                continue;
            }
            output.push(match kind {
                ObjectEntryKind::Keys => JsValue::String(name),
                ObjectEntryKind::Values => self.get_member(dom, object, &name)?,
                ObjectEntryKind::Entries => {
                    let value = self.get_member(dom, object, &name)?;
                    let pair = self.create_array_from_values(&[JsValue::String(name), value])?;
                    // A later allocation may collect, so the pair stays pinned.
                    self.transient_roots.push(pair);
                    JsValue::Object(pair)
                }
            });
        }
        let result = self.create_array_from_values(&output)?;
        self.transient_roots.truncate(pinned);
        Ok(JsValue::Object(result))
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

    /// ECMA-262 20.1.2.4 `Object.defineProperty`.
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
        self.define_property_or_throw(dom, object, &key, partial)?;
        Ok(JsValue::Object(object))
    }

    /// ECMA-262 20.1.2.3 `Object.defineProperties`: every descriptor is read
    /// before any property is defined.
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
        let descriptors_value = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let descriptors = self.to_object(&descriptors_value)?;
        // Steps 3-4: the keys are `OwnPropertyKeys(props)`, strings then symbols;
        // each enumerable one is re-checked and read with [[Get]] when reached.
        let mut pending = Vec::new();
        for key in self.own_property_keys(dom, descriptors)? {
            let Some(descriptor) = self.own_descriptor(dom, descriptors, &key)? else {
                continue;
            };
            if !descriptor.enumerable {
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
            self.define_property_or_throw(dom, target, &key, partial)?;
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

    /// ECMA-262 7.3.8 `DefinePropertyOrThrow`.
    fn define_property_or_throw(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
        partial: PartialDescriptor,
    ) -> Result<(), JsError> {
        if self.define_own_property(dom, object, key, partial)? {
            Ok(())
        } else {
            Err(JsError::type_error("cannot redefine object property"))
        }
    }

    /// `CreateDataPropertyOrThrow` (ECMA-262 7.3.7) for a class field: an
    /// enumerable, writable, configurable data property defined through
    /// `[[DefineOwnProperty]]`, so a proxy's `defineProperty` trap sees it. A
    /// refusal is a `TypeError`.
    pub(in crate::runtime) fn create_data_field_or_throw(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
        value: JsValue,
    ) -> Result<(), JsError> {
        let partial = PartialDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
            ..PartialDescriptor::default()
        };
        if self.define_own_property(dom, object, key, partial)? {
            Ok(())
        } else {
            Err(JsError::type_error(
                "class field cannot be defined on its target",
            ))
        }
    }

    /// ECMA-262 10.1.6 `[[DefineOwnProperty]]`, dispatched on the exotic kind
    /// of `object`: a proxy runs its `defineProperty` trap, an Array runs
    /// 10.4.2.1, and anything else is ordinary.
    fn define_own_property(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
        partial: PartialDescriptor,
    ) -> Result<bool, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Proxy { .. }) => {
                return self.proxy_define_own_property(dom, object, key, partial);
            }
            Some(ObjectHost::Array) => {
                if let PropertyName::String(name) = key {
                    if name == "length" {
                        return self.array_set_length(dom, object, partial);
                    }
                    if let Some(index) = array_index(name) {
                        return Ok(self.array_define_index(object, name, index, partial));
                    }
                }
            }
            _ => {}
        }
        Ok(self.ordinary_define_own_property(object, key, partial))
    }

    /// ECMA-262 10.1.6.1 `OrdinaryDefineOwnProperty` with
    /// `ValidateAndApplyPropertyDescriptor`: the partial descriptor is merged
    /// with the current property, and the realm refuses an illegal change to a
    /// non-configurable slot.
    fn ordinary_define_own_property(
        &mut self,
        object: ObjectId,
        key: &PropertyName,
        partial: PartialDescriptor,
    ) -> bool {
        match key {
            PropertyName::String(name) => {
                let current = self.realm.own_property(object, name);
                let merged = merge_partial_descriptor(current.as_ref(), partial);
                if self
                    .realm
                    .define_property(object, name.clone(), merged.clone())
                {
                    return true;
                }
                // A String wrapper's virtual slots refuse every define in the
                // realm, but a redefinition that changes nothing is still valid.
                current.is_some_and(|current| same_descriptor(&current, &merged))
            }
            PropertyName::Symbol(symbol) => {
                let current = self.realm.own_symbol_property(object, symbol);
                let merged = merge_partial_descriptor(current.as_ref(), partial);
                self.realm.define_symbol_property(object, symbol, merged)
            }
        }
    }

    /// ECMA-262 10.4.2.1 `ArrayDefineOwnProperty` for an array index: an index
    /// at or past a non-writable `length` is refused, and an accepted index past
    /// the end grows `length`.
    fn array_define_index(
        &mut self,
        object: ObjectId,
        name: &str,
        index: u32,
        partial: PartialDescriptor,
    ) -> bool {
        let (old_length, length_writable) =
            array_length_slot(self.realm.own_property(object, "length").as_ref());
        if index >= old_length && !length_writable {
            return false;
        }
        let key = PropertyName::String(name.to_owned());
        if !self.ordinary_define_own_property(object, &key, partial) {
            return false;
        }
        if index >= old_length {
            self.realm.set_property(
                object,
                "length".to_owned(),
                JsValue::Number(f64::from(index) + 1.0),
            );
        }
        true
    }

    /// ECMA-262 10.4.2.4 `ArraySetLength`: a shrink deletes the indices at or
    /// past the new length from the top down, and stops at the first one that
    /// refuses to go, leaving `length` one past it.
    fn array_set_length(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        partial: PartialDescriptor,
    ) -> Result<bool, JsError> {
        let length_key = PropertyName::String("length".to_owned());
        let Some(value) = partial.value.clone() else {
            return Ok(self.ordinary_define_own_property(object, &length_key, partial));
        };
        // `ToUint32(Desc.[[Value]])` and then `ToNumber(Desc.[[Value]])`, so an
        // object's `valueOf` runs twice; the two must agree.
        let new_length = uint32_of_number(self.to_number_value(dom, &value)?);
        let number = self.to_number_value(dom, &value)?;
        if f64::from(new_length) != number {
            return Err(self.range_error("Invalid array length"));
        }
        let (old_length, length_writable) =
            array_length_slot(self.realm.own_property(object, "length").as_ref());
        let mut partial = partial;
        partial.value = Some(JsValue::Number(f64::from(new_length)));
        if new_length >= old_length {
            return Ok(self.ordinary_define_own_property(object, &length_key, partial));
        }
        if !length_writable {
            return Ok(false);
        }
        // The final writable state applies once the deletions are done, so the
        // length is kept writable while they run.
        let requested_writable = partial.writable;
        partial.writable = Some(true);
        if !self.ordinary_define_own_property(object, &length_key, partial) {
            return Ok(false);
        }
        let mut doomed = self
            .realm
            .own_property_names(object)
            .unwrap_or_default()
            .iter()
            .filter_map(|name| array_index(name))
            .filter(|index| *index >= new_length)
            .collect::<Vec<_>>();
        doomed.sort_unstable_by(|left, right| right.cmp(left));
        for index in doomed {
            if !self.realm.delete_property(object, &index.to_string()) {
                let mut stopped = PartialDescriptor {
                    value: Some(JsValue::Number(f64::from(index) + 1.0)),
                    ..PartialDescriptor::default()
                };
                if requested_writable == Some(false) {
                    stopped.writable = Some(false);
                }
                self.ordinary_define_own_property(object, &length_key, stopped);
                return Ok(false);
            }
        }
        if requested_writable == Some(false) {
            self.ordinary_define_own_property(
                object,
                &length_key,
                PartialDescriptor {
                    writable: Some(false),
                    ..PartialDescriptor::default()
                },
            );
        }
        Ok(true)
    }

    /// ECMA-262 10.5.5 `[[DefineOwnProperty]]` for a proxy: the `defineProperty`
    /// trap decides, and without one the define goes to the target.
    fn proxy_define_own_property(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        key: &PropertyName,
        partial: PartialDescriptor,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_parts(proxy)?;
        let Some(trap) = self.proxy_trap(dom, handler, "defineProperty")? else {
            return self.define_own_property(dom, target, key, partial);
        };
        let descriptor = self.partial_descriptor_object(partial);
        let key_value = match key {
            PropertyName::String(name) => JsValue::String(name.clone()),
            PropertyName::Symbol(symbol) => JsValue::Symbol(symbol.clone()),
        };
        let result = self.call_with_this(
            dom,
            trap,
            &[
                JsValue::Object(target),
                key_value,
                JsValue::Object(descriptor),
            ],
            JsValue::Object(handler),
        )?;
        Ok(result.is_truthy())
    }

    /// ECMA-262 6.2.6.4 `FromPropertyDescriptor` for a partial descriptor: only
    /// the fields it has become properties of the object.
    fn partial_descriptor_object(&mut self, partial: PartialDescriptor) -> ObjectId {
        let object = self.realm.create_ordinary_object();
        if let Some(value) = partial.value {
            self.realm.set_property(object, "value".to_owned(), value);
        }
        if let Some(writable) = partial.writable {
            self.realm
                .set_property(object, "writable".to_owned(), JsValue::Boolean(writable));
        }
        for (name, field) in [("get", partial.get), ("set", partial.set)] {
            if let AccessorField::Present(function) = field {
                let value = function.map_or(JsValue::Undefined, JsValue::Object);
                self.realm.set_property(object, name.to_owned(), value);
            }
        }
        if let Some(enumerable) = partial.enumerable {
            self.realm.set_property(
                object,
                "enumerable".to_owned(),
                JsValue::Boolean(enumerable),
            );
        }
        if let Some(configurable) = partial.configurable {
            self.realm.set_property(
                object,
                "configurable".to_owned(),
                JsValue::Boolean(configurable),
            );
        }
        object
    }

    /// ECMA-262 10.1.5 `[[GetOwnProperty]]` for either key kind, through the
    /// proxy `getOwnPropertyDescriptor` trap for string keys.
    fn own_descriptor(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
    ) -> Result<Option<PropertyDescriptor>, JsError> {
        match key {
            PropertyName::String(name) => self.proxy_get_own_property_descriptor(dom, object, name),
            PropertyName::Symbol(symbol) => {
                let Some(ObjectHost::Proxy { target, handler }) = self.realm.host(object) else {
                    return Ok(self.realm.own_symbol_property(object, symbol));
                };
                let Some(trap) = self.proxy_trap(dom, handler, "getOwnPropertyDescriptor")? else {
                    return self.own_descriptor(dom, target, key);
                };
                let value = self.call_with_this(
                    dom,
                    trap,
                    &[JsValue::Object(target), JsValue::Symbol(symbol.clone())],
                    JsValue::Object(handler),
                )?;
                if matches!(value, JsValue::Undefined) {
                    return Ok(None);
                }
                self.property_descriptor_from_value(&value).map(Some)
            }
        }
    }

    /// The string keys of `[[OwnPropertyKeys]]`, in order.
    fn own_string_keys(&mut self, dom: &mut Dom, object: ObjectId) -> Result<Vec<String>, JsError> {
        Ok(self
            .own_property_keys(dom, object)?
            .into_iter()
            .filter_map(|key| match key {
                PropertyName::String(name) => Some(name),
                PropertyName::Symbol(_) => None,
            })
            .collect())
    }

    /// ECMA-262 10.1.11 `[[OwnPropertyKeys]]`: the string keys in order, then
    /// the symbol keys. A proxy answers through its `ownKeys` trap.
    fn own_property_keys(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<Vec<PropertyName>, JsError> {
        if let Some(ObjectHost::Proxy { target, handler }) = self.realm.host(object) {
            let Some(trap) = self.proxy_trap(dom, handler, "ownKeys")? else {
                return self.own_property_keys(dom, target);
            };
            let result = self.call_with_this(
                dom,
                trap,
                &[JsValue::Object(target)],
                JsValue::Object(handler),
            )?;
            // CreateListFromArrayLike(trapResult, « String, Symbol »).
            let mut keys = Vec::new();
            for value in self.iterate_values(dom, &result)? {
                keys.push(match value {
                    JsValue::String(name) => PropertyName::String(name),
                    JsValue::Symbol(symbol) => PropertyName::Symbol(symbol),
                    _ => {
                        return Err(JsError::type_error(
                            "proxy ownKeys trap returned a non-property key",
                        ));
                    }
                });
            }
            return Ok(keys);
        }
        let mut keys = self
            .realm
            .own_property_names(object)
            .unwrap_or_default()
            .into_iter()
            .map(PropertyName::String)
            .collect::<Vec<_>>();
        keys.extend(
            self.realm
                .own_symbols(object)
                .unwrap_or_default()
                .into_iter()
                .map(PropertyName::Symbol),
        );
        Ok(keys)
    }

    /// ECMA-262 10.1.1 `[[GetPrototypeOf]]`, through the proxy trap when there
    /// is one.
    fn prototype_of(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<Option<ObjectId>, JsError> {
        if let Some(ObjectHost::Proxy { target, handler }) = self.realm.host(object) {
            let Some(trap) = self.proxy_trap(dom, handler, "getPrototypeOf")? else {
                return self.prototype_of(dom, target);
            };
            let result = self.call_with_this(
                dom,
                trap,
                &[JsValue::Object(target)],
                JsValue::Object(handler),
            )?;
            return match result {
                JsValue::Object(prototype) => Ok(Some(prototype)),
                JsValue::Null => Ok(None),
                _ => Err(JsError::type_error(
                    "getPrototypeOf trap returned neither object nor null",
                )),
            };
        }
        Ok(self.realm.object(object).and_then(JsObject::prototype))
    }

    /// ECMA-262 10.1.9.2 `OrdinarySetWithOwnDescriptor` for a receiver that is
    /// `object` itself: the first own descriptor on the chain decides, an
    /// accessor runs its setter, and a data write is defined on the receiver.
    /// Returns `false` where the spec's `[[Set]]` returns `false`.
    pub(in crate::runtime) fn set_property_checked(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
        value: JsValue,
    ) -> Result<bool, JsError> {
        if let (PropertyName::String(name), Some(ObjectHost::TypedArray { .. })) =
            (key, self.realm.host(object))
        {
            // Integer-indexed writes keep their buffer semantics in the set path.
            self.set_member(dom, object, name, value)?;
            return Ok(true);
        }
        let mut inherited = None;
        let mut holder = Some(object);
        while let Some(current) = holder {
            if let (PropertyName::String(name), Some(ObjectHost::Proxy { .. })) =
                (key, self.realm.host(current))
            {
                self.proxy_set(dom, current, name, value)?;
                return Ok(true);
            }
            if let Some(descriptor) = self.own_descriptor(dom, current, key)? {
                inherited = Some(descriptor);
                break;
            }
            holder = self.prototype_of(dom, current)?;
        }
        if let Some(descriptor) = inherited {
            if descriptor.is_accessor() {
                return match descriptor.setter {
                    Some(setter) => {
                        self.call_with_this(dom, setter, &[value], JsValue::Object(object))?;
                        Ok(true)
                    }
                    None => Ok(false),
                };
            }
            if !descriptor.writable {
                return Ok(false);
            }
        }
        let partial = match self.own_descriptor(dom, object, key)? {
            Some(existing) => {
                if existing.is_accessor() || !existing.writable {
                    return Ok(false);
                }
                PartialDescriptor {
                    value: Some(value),
                    ..PartialDescriptor::default()
                }
            }
            None => PartialDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
                ..PartialDescriptor::default()
            },
        };
        self.define_own_property(dom, object, key, partial)
    }

    /// ECMA-262 6.2.6.4 `FromPropertyDescriptor` for a complete descriptor.
    fn from_property_descriptor(&mut self, descriptor: &PropertyDescriptor) -> ObjectId {
        let result = self.realm.create_ordinary_object();
        if descriptor.is_accessor() {
            for (name, slot) in [("get", descriptor.getter), ("set", descriptor.setter)] {
                let value = slot.map_or(JsValue::Undefined, JsValue::Object);
                self.realm.set_property(result, name.to_owned(), value);
            }
        } else {
            self.realm
                .set_property(result, "value".to_owned(), descriptor.value.clone());
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
        result
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
        match self.own_descriptor(dom, object, &key)? {
            Some(descriptor) => {
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.from_property_descriptor(&descriptor)))
            }
            None => Ok(JsValue::Undefined),
        }
    }

    /// ECMA-262 20.1.2.8 `Object.getOwnPropertyDescriptors`.
    pub(in crate::runtime) fn object_get_own_property_descriptors(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.8: `ToObject` first, so a primitive contributes its own
        // descriptors (a String primitive's indices and `length`).
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let object = self.to_object(&value)?;
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        let pinned = self.transient_roots.len();
        self.transient_roots.push(result);
        for key in self.own_property_keys(dom, object)? {
            let Some(descriptor) = self.own_descriptor(dom, object, &key)? else {
                continue;
            };
            let descriptor_object = self.from_property_descriptor(&descriptor);
            match key {
                PropertyName::String(name) => {
                    self.realm
                        .set_property(result, name, JsValue::Object(descriptor_object));
                }
                PropertyName::Symbol(symbol) => {
                    self.realm.define_symbol_property(
                        result,
                        &symbol,
                        PropertyDescriptor::data(JsValue::Object(descriptor_object)),
                    );
                }
            }
        }
        self.transient_roots.truncate(pinned);
        Ok(JsValue::Object(result))
    }

    pub(in crate::runtime) fn object_get_prototype_of(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.2: `Object.getPrototypeOf` starts with `ToObject`, so every
        // primitive reports its wrapper's `[[Prototype]]` and only `null` or
        // `undefined` throws. `Object.getPrototypeOf("x") === String.prototype`
        // in every engine, and a 1.3MB production bundle relies on it.
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let object = self.to_object(&value)?;
        Ok(self
            .prototype_of(dom, object)?
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

    /// ECMA-262 20.1.2.10 `Object.getOwnPropertyNames`: string keys only, so a
    /// proxy answers through its `ownKeys` trap.
    pub(in crate::runtime) fn object_get_own_property_names(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.10: `ToObject` first, so a String primitive reports its
        // indexed characters plus `length` like every other engine.
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let object = self.to_object(&value)?;
        let names = self
            .own_string_keys(dom, object)?
            .into_iter()
            .map(JsValue::String)
            .collect::<Vec<_>>();
        Ok(JsValue::Object(self.create_array_from_values(&names)?))
    }

    pub(in crate::runtime) fn object_has_own(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §20.1.2.18: `ToObject` first, so `Object.hasOwn("a", "0")` is true
        // against a String wrapper's indexed characters; then `ToPropertyKey`.
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let object = self.to_object(&value)?;
        let key_argument = arguments.get(1).unwrap_or(&JsValue::Undefined);
        let key = self.to_property_name(dom, key_argument)?;
        Ok(JsValue::Boolean(
            self.own_descriptor(dom, object, &key)?.is_some(),
        ))
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
        Ok(JsValue::Boolean(
            self.own_descriptor(dom, receiver, &key)?.is_some(),
        ))
    }

    /// ECMA-262 20.1.3.3 `Object.prototype.isPrototypeOf`: walks the candidate's
    /// prototype chain with `[[GetPrototypeOf]]`, so a proxy's trap runs.
    pub(in crate::runtime) fn object_prototype_is_prototype_of(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(JsValue::Object(candidate)) = arguments.first() else {
            return Ok(JsValue::Boolean(false));
        };
        let mut current = *candidate;
        for _ in 0..=self.realm.object_count() {
            match self.prototype_of(dom, current)? {
                None => return Ok(JsValue::Boolean(false)),
                Some(prototype) if prototype == receiver => {
                    return Ok(JsValue::Boolean(true));
                }
                Some(prototype) => current = prototype,
            }
        }
        Ok(JsValue::Boolean(false))
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
        let enumerable = self
            .own_descriptor(dom, receiver, &key)?
            .is_some_and(|descriptor| descriptor.enumerable);
        Ok(JsValue::Boolean(enumerable))
    }

    /// ECMA-262 20.1.3.6 `Object.prototype.toString`: the `builtinTag` comes
    /// from the object's internal slots (`Array`, `Function`, `Error`, the
    /// primitive wrappers, `Date`, `RegExp`), and a string-valued
    /// `@@toStringTag` read with [[Get]] overrides it. A getter that throws
    /// propagates.
    pub(in crate::runtime) fn object_to_string_tag(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<String, JsError> {
        let object = match value {
            JsValue::Undefined => return Ok("[object Undefined]".to_owned()),
            JsValue::Null => return Ok("[object Null]".to_owned()),
            other => self.to_object(other)?,
        };
        let builtin = if self.is_array_object(object) {
            "Array".to_owned()
        } else if self.is_callable_value(object) {
            "Function".to_owned()
        } else {
            match self.realm.host(object) {
                Some(ObjectHost::ErrorInstance) => "Error".to_owned(),
                Some(ObjectHost::BooleanPrimitive(_)) => "Boolean".to_owned(),
                Some(ObjectHost::NumberPrimitive(_)) => "Number".to_owned(),
                Some(ObjectHost::StringPrimitive(_)) => "String".to_owned(),
                Some(ObjectHost::DateInstance(_)) => "Date".to_owned(),
                Some(ObjectHost::RegExp(_)) => "RegExp".to_owned(),
                Some(ObjectHost::TypedArray { kind, .. }) => kind.name().to_owned(),
                // Hosts whose interface names them through a `@@toStringTag`
                // that the engine does not install on every prototype.
                Some(ObjectHost::Document(_)) => "HTMLDocument".to_owned(),
                Some(ObjectHost::DomException { .. }) => "DOMException".to_owned(),
                _ => "Object".to_owned(),
            }
        };
        let tag = self.get_symbol_value(dom, object, &JsSymbol::well_known("@@toStringTag"))?;
        Ok(match tag {
            JsValue::String(tag) => format!("[object {tag}]"),
            _ => format!("[object {builtin}]"),
        })
    }
}
