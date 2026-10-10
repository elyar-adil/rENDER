//! Proxy exotic objects (ECMA-262 10.5), the Proxy constructor, and the
//! Reflect operations that forward to the same internal methods.

use crate::JsError;
use crate::JsObject;
use crate::JsValue;
use crate::ObjectId;
use crate::PropertyDescriptor;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::object::{AccessorField, PartialDescriptor, PropertyName};
use crate::runtime::convert::required_argument;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::same_value;
use render_dom::Dom;

use super::array::{MAX_MATERIALIZED_ELEMENTS, to_length};

/// `SameValue` on property keys: strings by content, symbols by identity.
fn same_key(left: &PropertyName, right: &PropertyName) -> bool {
    match (left, right) {
        (PropertyName::String(left), PropertyName::String(right)) => left == right,
        (PropertyName::Symbol(left), PropertyName::Symbol(right)) => left.id() == right.id(),
        _ => false,
    }
}

/// The key as a JavaScript value, which is what a trap receives.
fn key_value(key: &PropertyName) -> JsValue {
    match key {
        PropertyName::String(name) => JsValue::String(name.clone()),
        PropertyName::Symbol(symbol) => JsValue::Symbol(symbol.clone()),
    }
}

fn has_accessor_fields(descriptor: &PartialDescriptor) -> bool {
    !matches!(descriptor.get, AccessorField::Absent)
        || !matches!(descriptor.set, AccessorField::Absent)
}

fn has_data_fields(descriptor: &PartialDescriptor) -> bool {
    descriptor.value.is_some() || descriptor.writable.is_some()
}

/// `CompletePropertyDescriptor` (ECMA-262 6.2.6.6): fills the defaults for the
/// fields a descriptor omits.
fn complete_descriptor(mut descriptor: PartialDescriptor) -> PartialDescriptor {
    if has_accessor_fields(&descriptor) {
        if matches!(descriptor.get, AccessorField::Absent) {
            descriptor.get = AccessorField::Present(None);
        }
        if matches!(descriptor.set, AccessorField::Absent) {
            descriptor.set = AccessorField::Present(None);
        }
    } else {
        descriptor.value.get_or_insert(JsValue::Undefined);
        descriptor.writable.get_or_insert(false);
    }
    descriptor.enumerable.get_or_insert(false);
    descriptor.configurable.get_or_insert(false);
    descriptor
}

/// The stored form of a completed descriptor.
fn stored_descriptor(descriptor: &PartialDescriptor) -> PropertyDescriptor {
    let slot = |field: &AccessorField| match field {
        AccessorField::Present(function) => *function,
        AccessorField::Absent => None,
    };
    PropertyDescriptor {
        value: descriptor.value.clone().unwrap_or(JsValue::Undefined),
        writable: descriptor.writable.unwrap_or(false),
        getter: slot(&descriptor.get),
        setter: slot(&descriptor.set),
        enumerable: descriptor.enumerable.unwrap_or(false),
        configurable: descriptor.configurable.unwrap_or(false),
    }
}

/// `IsCompatiblePropertyDescriptor` (ECMA-262 10.1.6.2), which is
/// `ValidateAndApplyPropertyDescriptor` with no object to write to.
fn is_compatible_descriptor(
    extensible: bool,
    descriptor: &PartialDescriptor,
    current: Option<&PropertyDescriptor>,
) -> bool {
    let Some(current) = current else {
        return extensible;
    };
    if current.configurable {
        return true;
    }
    if descriptor.configurable == Some(true) {
        return false;
    }
    if descriptor
        .enumerable
        .is_some_and(|enumerable| enumerable != current.enumerable)
    {
        return false;
    }
    let accessor = has_accessor_fields(descriptor);
    let data = has_data_fields(descriptor);
    if (accessor && !current.is_accessor()) || (data && current.is_accessor()) {
        return false;
    }
    if current.is_accessor() {
        if let AccessorField::Present(getter) = descriptor.get
            && getter != current.getter
        {
            return false;
        }
        if let AccessorField::Present(setter) = descriptor.set
            && setter != current.setter
        {
            return false;
        }
    } else if !current.writable {
        if descriptor.writable == Some(true) {
            return false;
        }
        if let Some(value) = &descriptor.value
            && !same_value(value, &current.value)
        {
            return false;
        }
    }
    true
}

impl JsRuntime {
    /// The Reflect and Proxy natives this module owns (ECMA-262 28.1, 28.2, 28.5).
    pub(in crate::runtime) fn dispatch_proxy_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        bound: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            // Every `Reflect.*` builtin but `Reflect.construct`'s `newTarget`
            // check begins with `ToObject` (§27.1), so a primitive target
            // becomes its wrapper rather than throwing.
            NativeFunction::ReflectGet => {
                let target = self.to_object(required_argument(arguments, 0, "Reflect.get")?)?;
                let key =
                    self.to_property_name(dom, required_argument(arguments, 1, "Reflect.get")?)?;
                let receiver = arguments.get(2).cloned().unwrap_or(JsValue::Object(target));
                self.get_property_from(dom, target, &key, receiver)
            }
            NativeFunction::ReflectSet => {
                let target = self.to_object(required_argument(arguments, 0, "Reflect.set")?)?;
                let key =
                    self.to_property_name(dom, required_argument(arguments, 1, "Reflect.set")?)?;
                let value = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
                let receiver = arguments.get(3).cloned().unwrap_or(JsValue::Object(target));
                Ok(JsValue::Boolean(self.set_value_with_receiver(
                    dom, target, &key, value, receiver,
                )?))
            }
            NativeFunction::ReflectHas => {
                let target = self.to_object(required_argument(arguments, 0, "Reflect.has")?)?;
                let key =
                    self.to_property_name(dom, required_argument(arguments, 1, "Reflect.has")?)?;
                Ok(JsValue::Boolean(
                    self.has_property_value(dom, target, &key)?,
                ))
            }
            NativeFunction::ReflectDeleteProperty => {
                let target =
                    self.to_object(required_argument(arguments, 0, "Reflect.deleteProperty")?)?;
                let key = self.to_property_name(
                    dom,
                    required_argument(arguments, 1, "Reflect.deleteProperty")?,
                )?;
                Ok(JsValue::Boolean(
                    self.delete_property_on(dom, target, &key)?,
                ))
            }
            NativeFunction::ReflectOwnKeys => {
                let target = self.to_object(required_argument(arguments, 0, "Reflect.ownKeys")?)?;
                let keys = self
                    .own_property_keys(dom, target)?
                    .iter()
                    .map(key_value)
                    .collect::<Vec<_>>();
                Ok(JsValue::Object(self.create_array_from_values(&keys)?))
            }
            NativeFunction::ReflectGetOwnPropertyDescriptor => {
                let target = self.to_object(required_argument(
                    arguments,
                    0,
                    "Reflect.getOwnPropertyDescriptor",
                )?)?;
                let key = self.to_property_name(
                    dom,
                    required_argument(arguments, 1, "Reflect.getOwnPropertyDescriptor")?,
                )?;
                match self.own_descriptor(dom, target, &key)? {
                    Some(descriptor) => {
                        Ok(JsValue::Object(self.from_property_descriptor(&descriptor)))
                    }
                    None => Ok(JsValue::Undefined),
                }
            }
            NativeFunction::ReflectDefineProperty => {
                let target =
                    self.to_object(required_argument(arguments, 0, "Reflect.defineProperty")?)?;
                let key = self.to_property_name(
                    dom,
                    required_argument(arguments, 1, "Reflect.defineProperty")?,
                )?;
                let JsValue::Object(descriptor) =
                    required_argument(arguments, 2, "Reflect.defineProperty")?
                else {
                    return Err(JsError::type_error(
                        "Property description must be an object",
                    ));
                };
                let partial = self.to_property_descriptor(dom, *descriptor)?;
                Ok(JsValue::Boolean(
                    self.define_own_property(dom, target, &key, partial)?,
                ))
            }
            NativeFunction::ReflectConstruct => self.reflect_construct(dom, arguments),
            NativeFunction::ReflectApply => {
                let target = self.to_object(required_argument(arguments, 0, "Reflect.apply")?)?;
                if !Self::is_callable_object(target, &self.realm) {
                    return Err(JsError::type_error("Reflect.apply target is not callable"));
                }
                let this_argument = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                let list = required_argument(arguments, 2, "Reflect.apply")?;
                let values = self.create_list_from_array_like(dom, list)?;
                self.call_with_this(dom, target, &values, this_argument)
            }
            NativeFunction::ReflectGetPrototypeOf => {
                // §27.1.7: `ToObject` first, so a primitive reports its
                // wrapper's prototype and only `null`/`undefined` throw.
                let target =
                    self.to_object(required_argument(arguments, 0, "Reflect.getPrototypeOf")?)?;
                Ok(self
                    .prototype_of(dom, target)?
                    .map_or(JsValue::Null, JsValue::Object))
            }
            NativeFunction::ReflectSetPrototypeOf => {
                // §27.1.13 requires an Object target (no `ToObject`).
                let target = Self::require_object(required_argument(
                    arguments,
                    0,
                    "Reflect.setPrototypeOf",
                )?)?;
                let prototype = match required_argument(arguments, 1, "Reflect.setPrototypeOf")? {
                    JsValue::Object(object) => Some(*object),
                    JsValue::Null => None,
                    _ => {
                        return Err(JsError::type_error("prototype must be an object or null"));
                    }
                };
                Ok(JsValue::Boolean(
                    self.set_prototype_of_value(dom, target, prototype)?,
                ))
            }
            NativeFunction::ReflectIsExtensible => {
                let target =
                    self.to_object(required_argument(arguments, 0, "Reflect.isExtensible")?)?;
                Ok(JsValue::Boolean(self.is_extensible_value(dom, target)?))
            }
            NativeFunction::ReflectPreventExtensions => {
                let target = self.to_object(required_argument(
                    arguments,
                    0,
                    "Reflect.preventExtensions",
                )?)?;
                Ok(JsValue::Boolean(
                    self.prevent_extensions_value(dom, target)?,
                ))
            }
            NativeFunction::ProxyRevocable => {
                self.ensure_heap_capacity(2)?;
                let proxy = self.create_proxy(arguments)?;
                let revoke = self.proxy_revoker(proxy)?;
                let result = self.realm.create_ordinary_object();
                self.realm
                    .set_property(result, "proxy".to_owned(), JsValue::Object(proxy));
                self.realm
                    .set_property(result, "revoke".to_owned(), JsValue::Object(revoke));
                Ok(JsValue::Object(result))
            }
            // The revoke function of `Proxy.revocable`: its bound receiver is
            // the proxy it revokes (ECMA-262 28.2.2.1.1).
            NativeFunction::ProxyRevoke => {
                if let Some(ObjectHost::Proxy { handler, .. }) = self.realm.host_mut(bound) {
                    *handler = None;
                }
                Ok(JsValue::Undefined)
            }
            other => Err(JsError::type_error(format!(
                "unsupported proxy native {other:?}"
            ))),
        }
    }

    /// `ProxyCreate` (ECMA-262 10.5.14): both arguments must be objects.
    fn create_proxy(&mut self, arguments: &[JsValue]) -> Result<ObjectId, JsError> {
        let target = Self::require_object(required_argument(arguments, 0, "Proxy")?)?;
        let handler = Self::require_object(required_argument(arguments, 1, "Proxy")?)?;
        self.ensure_heap_capacity(1)?;
        let proxy = self.realm.create_object(None);
        *self
            .realm
            .host_mut(proxy)
            .expect("fresh proxy has host storage") = ObjectHost::Proxy {
            target,
            handler: Some(handler),
        };
        Ok(proxy)
    }

    /// The `new Proxy(target, handler)` construction (ECMA-262 28.2.1.1).
    pub(in crate::runtime) fn proxy_constructor(
        &mut self,
        _constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        Ok(JsValue::Object(self.create_proxy(arguments)?))
    }

    /// The revoke function of a `Proxy.revocable` result: a bound native whose
    /// receiver is the proxy, named `""` and of length 0 (ECMA-262 28.2.2.1.1).
    fn proxy_revoker(&mut self, proxy: ObjectId) -> Result<ObjectId, JsError> {
        let function_prototype = self.realm.function_prototype();
        let revoke = self.realm.create_object(Some(function_prototype));
        *self
            .realm
            .host_mut(revoke)
            .expect("fresh function has host storage") = ObjectHost::BoundFunction {
            function: NativeFunction::ProxyRevoke,
            receiver: proxy,
        };
        let attributes = |value: JsValue| PropertyDescriptor {
            value,
            writable: false,
            getter: None,
            setter: None,
            enumerable: false,
            configurable: true,
        };
        self.realm
            .define_property(revoke, "length", attributes(JsValue::Number(0.0)));
        self.realm
            .define_property(revoke, "name", attributes(JsValue::String(String::new())));
        Ok(revoke)
    }

    /// The target and handler of a live proxy. A revoked proxy has no handler,
    /// and every internal method on it throws (ECMA-262 10.5.x step 1).
    fn proxy_slots(
        &self,
        proxy: ObjectId,
        operation: &str,
    ) -> Result<(ObjectId, ObjectId), JsError> {
        match self.realm.host(proxy) {
            Some(ObjectHost::Proxy {
                target,
                handler: Some(handler),
            }) => Ok((target, handler)),
            Some(ObjectHost::Proxy { handler: None, .. }) => Err(JsError::type_error(format!(
                "Cannot perform '{operation}' on a proxy that has been revoked"
            ))),
            _ => Err(JsError::type_error("object is not a Proxy")),
        }
    }

    /// `GetMethod(handler, name)` for a trap: an absent or nullish trap is
    /// `None`, and any other non-callable value throws.
    pub(in crate::runtime) fn proxy_trap(
        &mut self,
        dom: &mut Dom,
        handler: ObjectId,
        name: &str,
    ) -> Result<Option<ObjectId>, JsError> {
        let value = self.get_member(dom, handler, name)?;
        if matches!(value, JsValue::Undefined | JsValue::Null) {
            return Ok(None);
        }
        Self::require_callable_object(&value, &self.realm).map(Some)
    }

    /// `Call(trap, handler, arguments)`.
    fn call_trap(
        &mut self,
        dom: &mut Dom,
        trap: ObjectId,
        handler: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.call_with_this(dom, trap, arguments, JsValue::Object(handler))
    }

    /// `[[GetPrototypeOf]]` (ECMA-262 10.5.1).
    pub(in crate::runtime) fn proxy_get_prototype_of(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
    ) -> Result<Option<ObjectId>, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "getPrototypeOf")?;
        let Some(trap) = self.proxy_trap(dom, handler, "getPrototypeOf")? else {
            return self.prototype_of(dom, target);
        };
        let prototype = match self.call_trap(dom, trap, handler, &[JsValue::Object(target)])? {
            JsValue::Object(prototype) => Some(prototype),
            JsValue::Null => None,
            _ => {
                return Err(JsError::type_error(
                    "getPrototypeOf trap returned neither object nor null",
                ));
            }
        };
        if self.is_extensible_value(dom, target)? {
            return Ok(prototype);
        }
        if self.prototype_of(dom, target)? != prototype {
            return Err(JsError::type_error(
                "getPrototypeOf trap result differs from the prototype of a non-extensible target",
            ));
        }
        Ok(prototype)
    }

    /// `[[SetPrototypeOf]]` (ECMA-262 10.5.2).
    pub(in crate::runtime) fn proxy_set_prototype_of(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        prototype: Option<ObjectId>,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "setPrototypeOf")?;
        let Some(trap) = self.proxy_trap(dom, handler, "setPrototypeOf")? else {
            return self.set_prototype_of_value(dom, target, prototype);
        };
        let value = prototype.map_or(JsValue::Null, JsValue::Object);
        let arguments = [JsValue::Object(target), value];
        if !self.call_trap(dom, trap, handler, &arguments)?.is_truthy() {
            return Ok(false);
        }
        if self.is_extensible_value(dom, target)? {
            return Ok(true);
        }
        if self.prototype_of(dom, target)? != prototype {
            return Err(JsError::type_error(
                "setPrototypeOf trap returned true for a non-extensible target with a different prototype",
            ));
        }
        Ok(true)
    }

    /// `[[SetPrototypeOf]]` of any object: a proxy's trap, or the ordinary rule
    /// (ECMA-262 10.1.2.1, which refuses a cycle or a non-extensible object).
    pub(in crate::runtime) fn set_prototype_of_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        prototype: Option<ObjectId>,
    ) -> Result<bool, JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            return self.proxy_set_prototype_of(dom, object, prototype);
        }
        Ok(self.realm.set_prototype(object, prototype))
    }

    /// `[[IsExtensible]]` (ECMA-262 10.5.3).
    pub(in crate::runtime) fn proxy_is_extensible(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "isExtensible")?;
        let Some(trap) = self.proxy_trap(dom, handler, "isExtensible")? else {
            return self.is_extensible_value(dom, target);
        };
        let result = self
            .call_trap(dom, trap, handler, &[JsValue::Object(target)])?
            .is_truthy();
        if result != self.is_extensible_value(dom, target)? {
            return Err(JsError::type_error(
                "isExtensible trap result does not reflect the extensibility of the target",
            ));
        }
        Ok(result)
    }

    /// `[[PreventExtensions]]` (ECMA-262 10.5.4).
    pub(in crate::runtime) fn proxy_prevent_extensions(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "preventExtensions")?;
        let Some(trap) = self.proxy_trap(dom, handler, "preventExtensions")? else {
            return self.prevent_extensions_value(dom, target);
        };
        let result = self
            .call_trap(dom, trap, handler, &[JsValue::Object(target)])?
            .is_truthy();
        if result && self.is_extensible_value(dom, target)? {
            return Err(JsError::type_error(
                "preventExtensions trap returned true but the target is extensible",
            ));
        }
        Ok(result)
    }

    /// `[[GetOwnProperty]]` (ECMA-262 10.5.5).
    pub(in crate::runtime) fn proxy_get_own_property(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        key: &PropertyName,
    ) -> Result<Option<PropertyDescriptor>, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "getOwnPropertyDescriptor")?;
        let Some(trap) = self.proxy_trap(dom, handler, "getOwnPropertyDescriptor")? else {
            return self.own_descriptor(dom, target, key);
        };
        let trap_result = match self.call_trap(
            dom,
            trap,
            handler,
            &[JsValue::Object(target), key_value(key)],
        )? {
            JsValue::Undefined => None,
            JsValue::Object(object) => Some(object),
            _ => {
                return Err(JsError::type_error(
                    "getOwnPropertyDescriptor trap returned neither object nor undefined",
                ));
            }
        };
        let target_descriptor = self.own_descriptor(dom, target, key)?;
        let extensible = self.is_extensible_value(dom, target)?;
        let Some(trap_object) = trap_result else {
            let Some(target_descriptor) = target_descriptor else {
                return Ok(None);
            };
            if !target_descriptor.configurable {
                return Err(JsError::type_error(
                    "getOwnPropertyDescriptor trap hid a non-configurable property",
                ));
            }
            if !extensible {
                return Err(JsError::type_error(
                    "getOwnPropertyDescriptor trap hid a property of a non-extensible target",
                ));
            }
            return Ok(None);
        };
        let result = complete_descriptor(self.to_property_descriptor(dom, trap_object)?);
        if !is_compatible_descriptor(extensible, &result, target_descriptor.as_ref()) {
            return Err(JsError::type_error(
                "getOwnPropertyDescriptor trap returned a descriptor incompatible with the target",
            ));
        }
        if result.configurable == Some(false) {
            match &target_descriptor {
                None => {
                    return Err(JsError::type_error(
                        "getOwnPropertyDescriptor trap reported a non-configurable property the target lacks",
                    ));
                }
                Some(current) if current.configurable => {
                    return Err(JsError::type_error(
                        "getOwnPropertyDescriptor trap reported a configurable property as non-configurable",
                    ));
                }
                Some(current) => {
                    if result.writable == Some(false) && current.writable {
                        return Err(JsError::type_error(
                            "getOwnPropertyDescriptor trap reported a writable property as non-writable",
                        ));
                    }
                }
            }
        }
        Ok(Some(stored_descriptor(&result)))
    }

    /// `[[DefineOwnProperty]]` (ECMA-262 10.5.6).
    pub(in crate::runtime) fn proxy_define_own_property(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        key: &PropertyName,
        descriptor: PartialDescriptor,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "defineProperty")?;
        let Some(trap) = self.proxy_trap(dom, handler, "defineProperty")? else {
            return self.define_own_property(dom, target, key, descriptor);
        };
        let descriptor_object = self.partial_descriptor_object(descriptor.clone());
        let arguments = [
            JsValue::Object(target),
            key_value(key),
            JsValue::Object(descriptor_object),
        ];
        if !self.call_trap(dom, trap, handler, &arguments)?.is_truthy() {
            return Ok(false);
        }
        let target_descriptor = self.own_descriptor(dom, target, key)?;
        let extensible = self.is_extensible_value(dom, target)?;
        let setting_non_configurable = descriptor.configurable == Some(false);
        match &target_descriptor {
            None => {
                if !extensible {
                    return Err(JsError::type_error(
                        "defineProperty trap added a property to a non-extensible target",
                    ));
                }
                if setting_non_configurable {
                    return Err(JsError::type_error(
                        "defineProperty trap defined a non-configurable property the target lacks",
                    ));
                }
            }
            Some(current) => {
                if !is_compatible_descriptor(extensible, &descriptor, Some(current)) {
                    return Err(JsError::type_error(
                        "defineProperty trap result is incompatible with the target property",
                    ));
                }
                if setting_non_configurable && current.configurable {
                    return Err(JsError::type_error(
                        "defineProperty trap reported a configurable property as non-configurable",
                    ));
                }
                if !current.configurable && current.writable && descriptor.writable == Some(false) {
                    return Err(JsError::type_error(
                        "defineProperty trap reported a writable property as non-writable",
                    ));
                }
            }
        }
        Ok(true)
    }

    /// `HasProperty` (ECMA-262 7.3.12) of any object: the first proxy on the
    /// chain answers through its `has` trap, otherwise an own property wins.
    pub(in crate::runtime) fn has_property_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
    ) -> Result<bool, JsError> {
        let mut current = object;
        loop {
            if matches!(self.realm.host(current), Some(ObjectHost::Proxy { .. })) {
                return self.proxy_has_property(dom, current, key);
            }
            if self.own_descriptor(dom, current, key)?.is_some() {
                return Ok(true);
            }
            match self.prototype_of(dom, current)? {
                Some(parent) => current = parent,
                None => return Ok(false),
            }
        }
    }

    /// `[[HasProperty]]` (ECMA-262 10.5.7).
    pub(in crate::runtime) fn proxy_has_property(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        key: &PropertyName,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "has")?;
        let Some(trap) = self.proxy_trap(dom, handler, "has")? else {
            return self.has_property_value(dom, target, key);
        };
        let result = self
            .call_trap(
                dom,
                trap,
                handler,
                &[JsValue::Object(target), key_value(key)],
            )?
            .is_truthy();
        if !result && let Some(target_descriptor) = self.own_descriptor(dom, target, key)? {
            if !target_descriptor.configurable {
                return Err(JsError::type_error(
                    "has trap hid a non-configurable property of the target",
                ));
            }
            if !self.is_extensible_value(dom, target)? {
                return Err(JsError::type_error(
                    "has trap hid a property of a non-extensible target",
                ));
            }
        }
        Ok(result)
    }

    /// `[[Get]]` (ECMA-262 10.1.8) of any object with an explicit receiver.
    pub(in crate::runtime) fn get_property_from(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
        receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            return self.proxy_get_property(dom, object, key, receiver);
        }
        if matches!(receiver, JsValue::Object(this) if this == object) {
            return match key {
                PropertyName::String(name) => self.get_member(dom, object, name),
                PropertyName::Symbol(symbol) => self.get_symbol_value(dom, object, symbol),
            };
        }
        let mut current = object;
        loop {
            if matches!(self.realm.host(current), Some(ObjectHost::Proxy { .. })) {
                return self.proxy_get_property(dom, current, key, receiver);
            }
            if let Some(descriptor) = self.own_descriptor(dom, current, key)? {
                if !descriptor.is_accessor() {
                    return Ok(descriptor.value);
                }
                return match descriptor.getter {
                    Some(getter) => self.call_with_this(dom, getter, &[], receiver),
                    None => Ok(JsValue::Undefined),
                };
            }
            match self.prototype_of(dom, current)? {
                Some(parent) => current = parent,
                None => return Ok(JsValue::Undefined),
            }
        }
    }

    /// The first proxy on `object`'s prototype chain, when it comes before the
    /// holder of `property`: that proxy's `[[Get]]` answers for `object`. Returns
    /// `None` when no proxy is consulted, so the ordinary lookup proceeds.
    pub(in crate::runtime) fn proxy_inherited_get(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
    ) -> Result<Option<JsValue>, JsError> {
        let mut current = self.realm.object(object).and_then(JsObject::prototype);
        while let Some(candidate) = current {
            if matches!(self.realm.host(candidate), Some(ObjectHost::Proxy { .. })) {
                let key = PropertyName::String(property.to_owned());
                return self
                    .proxy_get_property(dom, candidate, &key, JsValue::Object(object))
                    .map(Some);
            }
            if self.realm.own_property(candidate, property).is_some() {
                return Ok(None);
            }
            current = self.realm.object(candidate).and_then(JsObject::prototype);
        }
        Ok(None)
    }

    /// `[[Get]]` (ECMA-262 10.5.8).
    pub(in crate::runtime) fn proxy_get_property(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        key: &PropertyName,
        receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "get")?;
        let Some(trap) = self.proxy_trap(dom, handler, "get")? else {
            return self.get_property_from(dom, target, key, receiver);
        };
        let arguments = [JsValue::Object(target), key_value(key), receiver];
        let value = self.call_trap(dom, trap, handler, &arguments)?;
        if let Some(target_descriptor) = self.own_descriptor(dom, target, key)?
            && !target_descriptor.configurable
        {
            if !target_descriptor.is_accessor()
                && !target_descriptor.writable
                && !same_value(&value, &target_descriptor.value)
            {
                return Err(JsError::type_error(
                    "get trap result differs from a non-writable, non-configurable property",
                ));
            }
            if target_descriptor.is_accessor()
                && target_descriptor.getter.is_none()
                && !matches!(value, JsValue::Undefined)
            {
                return Err(JsError::type_error(
                    "get trap must report undefined for a non-configurable accessor without a getter",
                ));
            }
        }
        Ok(value)
    }

    /// `[[Set]]` (ECMA-262 10.1.9) of any object with an explicit receiver. An
    /// ordinary object with its own receiver takes the `set_property_checked`
    /// path, which also owns the typed-array write rules.
    pub(in crate::runtime) fn set_value_with_receiver(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
        value: JsValue,
        receiver: JsValue,
    ) -> Result<bool, JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            return self.proxy_set_property(dom, object, key, value, receiver);
        }
        if matches!(receiver, JsValue::Object(this) if this == object) {
            return self.set_property_checked(dom, object, key, value);
        }
        self.ordinary_set_with_receiver(dom, object, key, value, receiver)
    }

    /// `OrdinarySetWithOwnDescriptor` (ECMA-262 10.1.9.2) for a receiver other
    /// than the object being written.
    fn ordinary_set_with_receiver(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
        value: JsValue,
        receiver: JsValue,
    ) -> Result<bool, JsError> {
        let own = match self.own_descriptor(dom, object, key)? {
            Some(own) => own,
            None => match self.prototype_of(dom, object)? {
                Some(parent) => {
                    return self.set_value_with_receiver(dom, parent, key, value, receiver);
                }
                None => PropertyDescriptor::data(JsValue::Undefined),
            },
        };
        if own.is_accessor() {
            return match own.setter {
                Some(setter) => {
                    self.call_with_this(dom, setter, &[value], receiver)?;
                    Ok(true)
                }
                None => Ok(false),
            };
        }
        if !own.writable {
            return Ok(false);
        }
        let JsValue::Object(receiver_object) = receiver else {
            return Ok(false);
        };
        let partial = match self.own_descriptor(dom, receiver_object, key)? {
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
        self.define_own_property(dom, receiver_object, key, partial)
    }

    /// `[[Set]]` (ECMA-262 10.5.9).
    pub(in crate::runtime) fn proxy_set_property(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        key: &PropertyName,
        value: JsValue,
        receiver: JsValue,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "set")?;
        let Some(trap) = self.proxy_trap(dom, handler, "set")? else {
            return self.set_value_with_receiver(dom, target, key, value, receiver);
        };
        let arguments = [
            JsValue::Object(target),
            key_value(key),
            value.clone(),
            receiver,
        ];
        if !self.call_trap(dom, trap, handler, &arguments)?.is_truthy() {
            return Ok(false);
        }
        if let Some(target_descriptor) = self.own_descriptor(dom, target, key)?
            && !target_descriptor.configurable
        {
            if !target_descriptor.is_accessor()
                && !target_descriptor.writable
                && !same_value(&value, &target_descriptor.value)
            {
                return Err(JsError::type_error(
                    "set trap changed a non-writable, non-configurable property",
                ));
            }
            if target_descriptor.is_accessor() && target_descriptor.setter.is_none() {
                return Err(JsError::type_error(
                    "set trap wrote to a non-configurable accessor without a setter",
                ));
            }
        }
        Ok(true)
    }

    /// `[[Delete]]` (ECMA-262 10.5.10).
    pub(in crate::runtime) fn proxy_delete_property(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        key: &PropertyName,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "deleteProperty")?;
        let Some(trap) = self.proxy_trap(dom, handler, "deleteProperty")? else {
            return self.delete_property_on(dom, target, key);
        };
        let arguments = [JsValue::Object(target), key_value(key)];
        if !self.call_trap(dom, trap, handler, &arguments)?.is_truthy() {
            return Ok(false);
        }
        let Some(target_descriptor) = self.own_descriptor(dom, target, key)? else {
            return Ok(true);
        };
        if !target_descriptor.configurable {
            return Err(JsError::type_error(
                "deleteProperty trap deleted a non-configurable property",
            ));
        }
        if !self.is_extensible_value(dom, target)? {
            return Err(JsError::type_error(
                "deleteProperty trap deleted a property of a non-extensible target",
            ));
        }
        Ok(true)
    }

    /// `[[Delete]]` of any object.
    pub(in crate::runtime) fn delete_property_on(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &PropertyName,
    ) -> Result<bool, JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            return self.proxy_delete_property(dom, object, key);
        }
        Ok(match key {
            PropertyName::String(name) => self.realm.delete_property(object, name),
            PropertyName::Symbol(symbol) => self.realm.delete_symbol_property(object, symbol),
        })
    }

    /// `[[OwnPropertyKeys]]` (ECMA-262 10.5.11).
    pub(in crate::runtime) fn proxy_own_property_keys(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
    ) -> Result<Vec<PropertyName>, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "ownKeys")?;
        let Some(trap) = self.proxy_trap(dom, handler, "ownKeys")? else {
            return self.own_property_keys(dom, target);
        };
        let trap_result = self.call_trap(dom, trap, handler, &[JsValue::Object(target)])?;
        let keys = self.property_key_list(dom, &trap_result)?;
        let extensible = self.is_extensible_value(dom, target)?;
        let mut configurable = Vec::new();
        let mut non_configurable = Vec::new();
        for key in self.own_property_keys(dom, target)? {
            match self.own_descriptor(dom, target, &key)? {
                Some(descriptor) if !descriptor.configurable => non_configurable.push(key),
                _ => configurable.push(key),
            }
        }
        if extensible && non_configurable.is_empty() {
            return Ok(keys);
        }
        let mut unchecked = keys.clone();
        for key in &non_configurable {
            let Some(index) = unchecked.iter().position(|kept| same_key(kept, key)) else {
                return Err(JsError::type_error(
                    "ownKeys trap result omits a non-configurable property of the target",
                ));
            };
            unchecked.remove(index);
        }
        if extensible {
            return Ok(keys);
        }
        for key in &configurable {
            let Some(index) = unchecked.iter().position(|kept| same_key(kept, key)) else {
                return Err(JsError::type_error(
                    "ownKeys trap result omits a property of a non-extensible target",
                ));
            };
            unchecked.remove(index);
        }
        if !unchecked.is_empty() {
            return Err(JsError::type_error(
                "ownKeys trap result adds a property to a non-extensible target",
            ));
        }
        Ok(keys)
    }

    /// `CreateListFromArrayLike(trapResult, « String, Symbol »)` with the
    /// duplicate check of ECMA-262 10.5.11 step 8.
    fn property_key_list(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<Vec<PropertyName>, JsError> {
        let JsValue::Object(array) = value else {
            return Err(JsError::type_error("ownKeys trap result is not an object"));
        };
        let length = to_length(&self.get_member(dom, *array, "length")?)?;
        if length > MAX_MATERIALIZED_ELEMENTS as f64 {
            return Err(JsError::resource(
                "ownKeys trap result exceeds the materialization bound",
            ));
        }
        let mut keys: Vec<PropertyName> = Vec::new();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = length as usize;
        for index in 0..count {
            let key = match self.get_member(dom, *array, &index.to_string())? {
                JsValue::String(name) => PropertyName::String(name),
                JsValue::Symbol(symbol) => PropertyName::Symbol(symbol),
                _ => {
                    return Err(JsError::type_error(
                        "ownKeys trap result contains a value that is not a property key",
                    ));
                }
            };
            if keys.iter().any(|kept| same_key(kept, &key)) {
                return Err(JsError::type_error(
                    "ownKeys trap result contains duplicate entries",
                ));
            }
            keys.push(key);
        }
        Ok(keys)
    }

    /// `[[Call]]` (ECMA-262 10.5.12).
    pub(in crate::runtime) fn proxy_call(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        this_argument: JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "apply")?;
        let Some(trap) = self.proxy_trap(dom, handler, "apply")? else {
            return self.call_with_this(dom, target, arguments, this_argument);
        };
        let argument_array = self.create_array_from_values(arguments)?;
        let trap_arguments = [
            JsValue::Object(target),
            this_argument,
            JsValue::Object(argument_array),
        ];
        self.call_trap(dom, trap, handler, &trap_arguments)
    }

    /// `[[Construct]]` (ECMA-262 10.5.13).
    pub(in crate::runtime) fn proxy_construct(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        arguments: &[JsValue],
        new_target: JsValue,
    ) -> Result<JsValue, JsError> {
        let (target, handler) = self.proxy_slots(proxy, "construct")?;
        let Some(trap) = self.proxy_trap(dom, handler, "construct")? else {
            // `construct_dispatch` reads the instance prototype from the
            // new-target stack top, so the caller's new target is pushed.
            self.new_target_stack.push(new_target);
            let constructed = self.construct_dispatch(dom, target, arguments);
            self.new_target_stack.pop();
            return constructed;
        };
        let argument_array = self.create_array_from_values(arguments)?;
        let trap_arguments = [
            JsValue::Object(target),
            JsValue::Object(argument_array),
            new_target,
        ];
        let result = self.call_trap(dom, trap, handler, &trap_arguments)?;
        if !matches!(result, JsValue::Object(_)) {
            return Err(JsError::type_error("construct trap returned a non-object"));
        }
        Ok(result)
    }

    /// `Reflect.construct`'s argument checks (ECMA-262 28.1.2 steps 2-5) and the
    /// `[[Construct]]` it forwards to.
    fn reflect_construct(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §27.1.5 step 1: `ToObject(target)`. The `newTarget` check below
        // does stay a strict object test.
        let target = self.to_object(required_argument(arguments, 0, "Reflect.construct")?)?;
        if !self.is_constructor(target) {
            return Err(JsError::type_error(
                "Reflect.construct target must be a constructor",
            ));
        }
        let new_target = match arguments.get(2) {
            None | Some(JsValue::Undefined) => target,
            Some(value) => {
                // §27.1.5 step 5: a present `newTarget` must be an Object; a
                // primitive is not coerced.
                let new_target = Self::require_object(value).map_err(|_| {
                    JsError::type_error("Reflect.construct newTarget must be a constructor")
                })?;
                if !self.is_constructor(new_target) {
                    return Err(JsError::type_error(
                        "Reflect.construct newTarget must be a constructor",
                    ));
                }
                new_target
            }
        };
        let list = required_argument(arguments, 1, "Reflect.construct")?;
        let arguments_list = self.create_list_from_array_like(dom, list)?;
        // `construct_dispatch` derives the instance prototype from the
        // new-target stack top, so push the caller-supplied `newTarget` for
        // the duration of the construction.
        self.new_target_stack.push(JsValue::Object(new_target));
        let constructed = self.construct_dispatch(dom, target, &arguments_list);
        self.new_target_stack.pop();
        constructed
    }

    /// `CreateListFromArrayLike` (§7.3.18) restricted to the `length`-indexed
    /// element reading every caller needs for `Reflect.apply` and
    /// `Reflect.construct`.
    fn create_list_from_array_like(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<Vec<JsValue>, JsError> {
        let object = Self::require_object(value).map_err(|_| {
            JsError::type_error("Reflect.construct argumentsList must be an object")
        })?;
        let length = self
            .realm
            .get_property(object, "length")
            .map(|value| to_length(&value))
            .transpose()?
            .unwrap_or(0.0);
        if length > MAX_MATERIALIZED_ELEMENTS as f64 {
            return Err(JsError::resource(
                "Reflect.construct argumentsList exceeds the materialization bound",
            ));
        }
        let mut elements = Vec::new();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = length as usize;
        elements
            .try_reserve_exact(count)
            .map_err(|_| JsError::resource("Reflect.construct argumentsList allocation refused"))?;
        for index in 0..count {
            elements.push(self.get_member(dom, object, &index.to_string())?);
        }
        Ok(elements)
    }

    /// `[[Delete]]` for a string key, as the `delete` operator performs it.
    pub(in crate::runtime) fn delete_property_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
    ) -> Result<bool, JsError> {
        self.delete_property_on(dom, object, &PropertyName::String(property.to_owned()))
    }

    /// `[[Get]]` for a string key, with the object as its own receiver.
    pub(in crate::runtime) fn proxy_get(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        property: &str,
    ) -> Result<JsValue, JsError> {
        let key = PropertyName::String(property.to_owned());
        self.proxy_get_property(dom, proxy, &key, JsValue::Object(proxy))
    }

    /// `[[Set]]` for a string key, as a sloppy-mode assignment performs it: a
    /// `false` result is ignored.
    pub(in crate::runtime) fn proxy_set(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        property: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        let key = PropertyName::String(property.to_owned());
        self.proxy_set_property(dom, proxy, &key, value, JsValue::Object(proxy))?;
        Ok(())
    }

    /// `[[HasProperty]]` for a string key.
    pub(in crate::runtime) fn proxy_has(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        property: &str,
    ) -> Result<bool, JsError> {
        let key = PropertyName::String(property.to_owned());
        self.has_property_value(dom, proxy, &key)
    }

    /// The string keys of `[[OwnPropertyKeys]]`, in order.
    pub(in crate::runtime) fn proxy_own_keys(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<Vec<String>, JsError> {
        Ok(self
            .own_property_keys(dom, object)?
            .into_iter()
            .filter_map(|key| match key {
                PropertyName::String(name) => Some(name),
                PropertyName::Symbol(_) => None,
            })
            .collect())
    }

    /// `[[GetOwnProperty]]` for a string key.
    pub(in crate::runtime) fn proxy_get_own_property_descriptor(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
    ) -> Result<Option<PropertyDescriptor>, JsError> {
        self.own_descriptor(dom, object, &PropertyName::String(key.to_owned()))
    }
}
