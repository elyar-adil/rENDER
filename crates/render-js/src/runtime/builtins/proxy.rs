//! Proxy and Reflect support used by modern application runtimes.

use crate::JsError;
use crate::JsObject;
use crate::JsValue;
use crate::ObjectId;
use crate::PropertyDescriptor;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

use super::array::{MAX_MATERIALIZED_ELEMENTS, to_length};

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_proxy_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        _receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            // Every `Reflect.*` builtin but `Reflect.construct`'s `newTarget`
            // check begins with `ToObject` (§27.1), so a primitive target
            // becomes its wrapper rather than throwing.
            NativeFunction::ReflectGet => {
                let target = self.to_object(required_argument(arguments, 0, "Reflect.get")?)?;
                let key = required_argument(arguments, 1, "Reflect.get")?.to_js_string();
                let receiver = arguments.get(2).cloned().unwrap_or(JsValue::Object(target));
                self.reflect_get(dom, target, &key, receiver)
            }
            NativeFunction::ReflectSet => {
                let target = self.to_object(required_argument(arguments, 0, "Reflect.set")?)?;
                let key = required_argument(arguments, 1, "Reflect.set")?.to_js_string();
                let value = required_argument(arguments, 2, "Reflect.set")?.clone();
                self.set_member(dom, target, &key, value)?;
                Ok(JsValue::Boolean(true))
            }
            NativeFunction::ReflectHas => {
                let target = self.to_object(required_argument(arguments, 0, "Reflect.has")?)?;
                let key = required_argument(arguments, 1, "Reflect.has")?.to_js_string();
                Ok(JsValue::Boolean(self.property_in_value(
                    dom,
                    &JsValue::String(key),
                    &JsValue::Object(target),
                )?))
            }
            NativeFunction::ReflectDeleteProperty => {
                let target =
                    self.to_object(required_argument(arguments, 0, "Reflect.deleteProperty")?)?;
                let key = required_argument(arguments, 1, "Reflect.deleteProperty")?.to_js_string();
                Ok(JsValue::Boolean(
                    self.delete_property_value(dom, target, &key)?,
                ))
            }
            NativeFunction::ReflectOwnKeys => {
                let target = self.to_object(required_argument(arguments, 0, "Reflect.ownKeys")?)?;
                let keys = self
                    .proxy_own_keys(dom, target)?
                    .into_iter()
                    .map(JsValue::String)
                    .collect::<Vec<_>>();
                Ok(JsValue::Object(self.create_array_from_values(&keys)?))
            }
            NativeFunction::ReflectGetOwnPropertyDescriptor => {
                let target = self.to_object(required_argument(
                    arguments,
                    0,
                    "Reflect.getOwnPropertyDescriptor",
                )?)?;
                let key = required_argument(arguments, 1, "Reflect.getOwnPropertyDescriptor")?
                    .to_js_string();
                self.reflect_get_own_property_descriptor(target, &key)
            }
            NativeFunction::ReflectDefineProperty => {
                let target =
                    self.to_object(required_argument(arguments, 0, "Reflect.defineProperty")?)?;
                let key = required_argument(arguments, 1, "Reflect.defineProperty")?.to_js_string();
                let descriptor = required_argument(arguments, 2, "Reflect.defineProperty")?;
                let descriptor = self.property_descriptor_from_value(descriptor)?;
                Ok(JsValue::Boolean(
                    self.realm.define_property(target, key, descriptor),
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
                    .realm
                    .object(target)
                    .and_then(JsObject::prototype)
                    .map_or(JsValue::Null, JsValue::Object))
            }
            NativeFunction::ReflectSetPrototypeOf => {
                // §27.1.13 requires an Object target (no `ToObject`); the
                // engine's own prototype machinery rejects an illegal change.
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
                    self.realm.set_prototype(target, prototype),
                ))
            }
            NativeFunction::ReflectIsExtensible => {
                let target =
                    self.to_object(required_argument(arguments, 0, "Reflect.isExtensible")?)?;
                Ok(JsValue::Boolean(self.realm.is_extensible(target)))
            }
            NativeFunction::ReflectPreventExtensions => {
                let target = self.to_object(required_argument(
                    arguments,
                    0,
                    "Reflect.preventExtensions",
                )?)?;
                self.realm.prevent_extensions(target);
                Ok(JsValue::Boolean(true))
            }
            other => Err(JsError::type_error(format!(
                "unsupported proxy native {other:?}"
            ))),
        }
    }

    pub(in crate::runtime) fn proxy_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = Self::require_object(required_argument(arguments, 0, "Proxy")?)?;
        let handler = Self::require_object(required_argument(arguments, 1, "Proxy")?)?;
        self.ensure_heap_capacity(1)?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let proxy = self.realm.create_object(prototype);
        *self
            .realm
            .host_mut(proxy)
            .expect("fresh proxy has host storage") = ObjectHost::Proxy { target, handler };
        Ok(JsValue::Object(proxy))
    }

    pub(in crate::runtime) fn proxy_get(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        property: &str,
    ) -> Result<JsValue, JsError> {
        let (target, handler) = self.proxy_parts(proxy)?;
        if let Some(trap) = self.proxy_trap(dom, handler, "get")? {
            return self.call_with_this(
                dom,
                trap,
                &[
                    JsValue::Object(target),
                    JsValue::String(property.to_owned()),
                    JsValue::Object(proxy),
                ],
                JsValue::Object(handler),
            );
        }
        self.get_member(dom, target, property)
    }

    pub(in crate::runtime) fn proxy_set(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        property: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        let (target, handler) = self.proxy_parts(proxy)?;
        if let Some(trap) = self.proxy_trap(dom, handler, "set")? {
            let result = self.call_with_this(
                dom,
                trap,
                &[
                    JsValue::Object(target),
                    JsValue::String(property.to_owned()),
                    value,
                    JsValue::Object(proxy),
                ],
                JsValue::Object(handler),
            )?;
            if !result.is_truthy() {
                return Err(JsError::type_error("Proxy set trap returned false"));
            }
            return Ok(());
        }
        self.set_member(dom, target, property, value)
    }

    pub(in crate::runtime) fn proxy_has(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        property: &str,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_parts(proxy)?;
        if let Some(trap) = self.proxy_trap(dom, handler, "has")? {
            let result = self.call_with_this(
                dom,
                trap,
                &[
                    JsValue::Object(target),
                    JsValue::String(property.to_owned()),
                ],
                JsValue::Object(handler),
            )?;
            return Ok(result.is_truthy());
        }
        Ok(self.realm.get_property(target, property).is_some())
    }

    pub(in crate::runtime) fn proxy_delete(
        &mut self,
        dom: &mut Dom,
        proxy: ObjectId,
        property: &str,
    ) -> Result<bool, JsError> {
        let (target, handler) = self.proxy_parts(proxy)?;
        if let Some(trap) = self.proxy_trap(dom, handler, "deleteProperty")? {
            let result = self.call_with_this(
                dom,
                trap,
                &[
                    JsValue::Object(target),
                    JsValue::String(property.to_owned()),
                ],
                JsValue::Object(handler),
            )?;
            return Ok(result.is_truthy());
        }
        Ok(self.realm.delete_property(target, property))
    }

    pub(in crate::runtime) fn proxy_own_keys(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<Vec<String>, JsError> {
        let (target, handler) = match self.realm.host(object) {
            Some(ObjectHost::Proxy { target, handler }) => (target, handler),
            _ => return Ok(self.realm.own_property_names(object).unwrap_or_default()),
        };
        if let Some(trap) = self.proxy_trap(dom, handler, "ownKeys")? {
            let result = self.call_with_this(
                dom,
                trap,
                &[JsValue::Object(target)],
                JsValue::Object(handler),
            )?;
            let values = self.iterate_values(dom, &result)?;
            return Ok(values
                .into_iter()
                .map(|value| value.to_js_string())
                .collect());
        }
        Ok(self.realm.own_property_names(target).unwrap_or_default())
    }

    pub(in crate::runtime) fn proxy_get_own_property_descriptor(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
    ) -> Result<Option<PropertyDescriptor>, JsError> {
        let (target, handler) = match self.realm.host(object) {
            Some(ObjectHost::Proxy { target, handler }) => (target, handler),
            _ => return Ok(self.realm.own_property(object, key)),
        };
        if let Some(trap) = self.proxy_trap(dom, handler, "getOwnPropertyDescriptor")? {
            let value = self.call_with_this(
                dom,
                trap,
                &[JsValue::Object(target), JsValue::String(key.to_owned())],
                JsValue::Object(handler),
            )?;
            if matches!(value, JsValue::Undefined) {
                return Ok(None);
            }
            return self.property_descriptor_from_value(&value).map(Some);
        }
        Ok(self.realm.own_property(target, key))
    }

    pub(in crate::runtime) fn proxy_parts(
        &self,
        proxy: ObjectId,
    ) -> Result<(ObjectId, ObjectId), JsError> {
        match self.realm.host(proxy) {
            Some(ObjectHost::Proxy { target, handler }) => Ok((target, handler)),
            _ => Err(JsError::type_error("object is not a Proxy")),
        }
    }

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

    pub(in crate::runtime) fn reflect_get(
        &mut self,
        dom: &mut Dom,
        target: ObjectId,
        property: &str,
        _receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        self.get_member(dom, target, property)
    }

    /// `Reflect.construct ( target, argumentsList [ , newTarget ] )` (§28.5.33).
    ///
    /// test262's `isConstructor` harness probes constructors exclusively
    /// through this operation, so it must reject non-constructor targets with
    /// a `TypeError` instead of silently returning a value: with the interpreter's
    /// lenient inert-call fallback for missing host hooks, a missing
    /// `Reflect.construct` made `isConstructor` report `true` for every
    /// function and flipped the whole `not-a-constructor` conformance family.
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
    /// element reading every caller needs for `Reflect.construct`.
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

    pub(in crate::runtime) fn property_in_value(
        &mut self,
        dom: &mut Dom,
        key: &JsValue,
        container: &JsValue,
    ) -> Result<bool, JsError> {
        let name = key.to_js_string();
        match container {
            JsValue::Object(object) => {
                if matches!(self.realm.host(*object), Some(ObjectHost::Proxy { .. })) {
                    return self.proxy_has(dom, *object, &name);
                }
                Ok(self.realm.get_property(*object, &name).is_some())
            }
            _ => Err(JsError::type_error(
                "right-hand side of 'in' must be an object",
            )),
        }
    }

    pub(in crate::runtime) fn delete_property_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
    ) -> Result<bool, JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            return self.proxy_delete(dom, object, property);
        }
        Ok(self.realm.delete_property(object, property))
    }

    fn reflect_get_own_property_descriptor(
        &mut self,
        target: ObjectId,
        key: &str,
    ) -> Result<JsValue, JsError> {
        let Some(descriptor) = self.realm.own_property(target, key) else {
            return Ok(JsValue::Undefined);
        };
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        self.realm
            .set_property(result, "value".to_owned(), descriptor.value.clone());
        self.realm.set_property(
            result,
            "writable".to_owned(),
            JsValue::Boolean(descriptor.writable),
        );
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

    pub(in crate::runtime) fn property_descriptor_from_value(
        &self,
        value: &JsValue,
    ) -> Result<PropertyDescriptor, JsError> {
        let object = Self::require_object(value)?;
        let value = self
            .realm
            .get_property(object, "value")
            .unwrap_or(JsValue::Undefined);
        let writable = self
            .realm
            .get_property(object, "writable")
            .is_some_and(|value| value.is_truthy());
        let enumerable = self
            .realm
            .get_property(object, "enumerable")
            .is_some_and(|value| value.is_truthy());
        let configurable = self
            .realm
            .get_property(object, "configurable")
            .is_some_and(|value| value.is_truthy());
        Ok(PropertyDescriptor {
            getter: None,
            setter: None,
            value,
            writable,
            enumerable,
            configurable,
        })
    }
}
