//! The `ShadowRealm` proposal: a `ShadowRealm` is a realm of its own inside this
//! agent, reached only through source text and through the functions it wraps.
//!
//! `evaluate` runs a script in the shadow realm and brings the result back to
//! the caller's realm. A primitive comes back unchanged, a callable comes back
//! as a wrapped function, and any other object is refused. A wrapped function
//! carries its arguments into the target's realm and its result back out, and
//! an exception that escapes the target arrives as a `TypeError` of the wrapper's
//! realm.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::RealmId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::object::PropertyName;
use crate::utf16::JsString;
use crate::value::ObjectHost;
use crate::value::PropertyDescriptor;
use render_dom::Dom;

/// A non-writable, non-enumerable, configurable data property, the shape
/// `SetFunctionLength` and `SetFunctionName` define.
fn function_metadata_property(value: JsValue) -> PropertyDescriptor {
    PropertyDescriptor {
        getter: None,
        setter: None,
        value,
        writable: false,
        enumerable: false,
        configurable: true,
    }
}

impl JsRuntime {
    /// `ShadowRealm()`: `OrdinaryCreateFromConstructor` runs first, so the
    /// prototype is read before the new realm exists, then `CreateRealm`.
    pub(in crate::runtime) fn shadow_realm_construct(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
    ) -> Result<JsValue, JsError> {
        // GetPrototypeFromConstructor falls back to the prototype of the realm
        // `newTarget` belongs to.
        let fallback_realm = match new_target {
            JsValue::Object(target) => self.function_realm(*target)?,
            _ => self.realm.current_realm(),
        };
        let fallback = self.realm.record(fallback_realm).shadow_realm_prototype;
        let prototype = self.prototype_from_new_target(dom, new_target, fallback)?;
        let realm = self.create_realm(dom);
        Ok(JsValue::Object(self.realm.allocate_host(
            Some(prototype),
            ObjectHost::ShadowRealm(realm),
        )))
    }

    /// `ShadowRealm.prototype.evaluate(sourceText)`. The current realm is the
    /// caller's: this function's own realm.
    pub(in crate::runtime) fn shadow_realm_evaluate(
        &mut self,
        dom: &mut Dom,
        receiver: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let caller = self.realm.current_realm();
        let shadow = self.validate_shadow_realm(receiver)?;
        let source = match arguments.first() {
            Some(JsValue::String(source)) => source.to_lossy(),
            _ => {
                return Err(JsError::type_error(
                    "ShadowRealm.prototype.evaluate requires a string",
                ));
            }
        };
        // PerformShadowRealmEval: a parse error is the caller's `SyntaxError`, and
        // whatever the evaluation throws becomes the caller's TypeError.
        let (mut statements, strict) = self.parse_for_realm(&source)?;
        let value = self
            .evaluate_parsed_in_realm(dom, &mut statements, strict, shadow)
            .map_err(|error| {
                JsError::type_error(format!("ShadowRealm evaluation threw: {}", error.message()))
            })?;
        self.get_wrapped_value(dom, caller, value)
    }

    /// `ShadowRealm.prototype.importValue(specifier, exportName)` (`ShadowRealm`
    /// proposal). The receiver, `specifier` and `exportName` are checked at once,
    /// and their errors are thrown. Then the module is loaded into the shadow
    /// realm, and the promise settles with its export, wrapped for the caller's
    /// realm. A failure of the loading or of the lookup rejects with a `TypeError`
    /// of the caller's realm.
    pub(in crate::runtime) fn shadow_realm_import_value(
        &mut self,
        dom: &mut Dom,
        receiver: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let caller = self.realm.current_realm();
        let shadow = self.validate_shadow_realm(receiver)?;
        let argument = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let specifier = self.to_string_value(dom, &argument)?.to_lossy();
        let export_name = match arguments.get(1) {
            Some(JsValue::String(name)) => name.to_lossy(),
            _ => {
                return Err(JsError::type_error(
                    "ShadowRealm.prototype.importValue needs an export name string",
                ));
            }
        };
        let (promise, result) = self.create_promise()?;
        let outcome = match self.import_shadow_export(dom, shadow, &specifier, &export_name) {
            Ok(value) => self.get_wrapped_value(dom, caller, value),
            Err(error) => Err(error),
        };
        match outcome {
            Ok(value) => self.resolve_promise(promise, &value),
            Err(error) => {
                let reason = self.error_to_thrown_value(&error)?;
                self.reject_promise(promise, &reason);
            }
        }
        Ok(result)
    }

    /// The export `importValue` names, read from its module in the shadow realm.
    fn import_shadow_export(
        &mut self,
        dom: &mut Dom,
        shadow: RealmId,
        specifier: &str,
        export_name: &str,
    ) -> Result<JsValue, JsError> {
        let namespace = self
            .import_module(dom, shadow, specifier, "")
            .map_err(|error| {
                JsError::type_error(format!("importValue failed: {}", error.message()))
            })?;
        match self.realm.own_property(namespace, export_name) {
            Some(descriptor) => Ok(descriptor.value),
            None => Err(JsError::type_error(format!(
                "the module does not provide an export named {export_name:?}"
            ))),
        }
    }

    /// `ValidateShadowRealmObject(O)`: the realm a `ShadowRealm` instance owns.
    fn validate_shadow_realm(&self, receiver: &JsValue) -> Result<RealmId, JsError> {
        if let JsValue::Object(object) = receiver
            && let Some(ObjectHost::ShadowRealm(realm)) = self.realm.host(*object)
        {
            return Ok(realm);
        }
        Err(JsError::type_error(
            "ShadowRealm.prototype method called on an object that is not a ShadowRealm",
        ))
    }

    /// `GetWrappedValue(realm, value)`: a primitive crosses unchanged, a
    /// callable is wrapped as a function of `realm`, and any other object is a
    /// `TypeError` of the current realm.
    pub(in crate::runtime) fn get_wrapped_value(
        &mut self,
        dom: &mut Dom,
        realm: RealmId,
        value: JsValue,
    ) -> Result<JsValue, JsError> {
        match value {
            JsValue::Object(object) if Self::is_callable_object(object, &self.realm) => {
                let wrapper = self.realm.create_wrapped_function(realm, object);
                self.copy_name_and_length(dom, wrapper, object)
                    .map_err(|error| {
                        JsError::type_error(format!(
                            "wrapping a function failed: {}",
                            error.message()
                        ))
                    })?;
                Ok(JsValue::Object(wrapper))
            }
            JsValue::Object(_) => Err(JsError::type_error(
                "a non-callable object cannot cross a realm boundary",
            )),
            primitive => Ok(primitive),
        }
    }

    /// `CopyNameAndLength(wrapper, target)`: the wrapper takes the target's
    /// `length`, clamped to a non-negative integer, and its `name`, which is ""
    /// unless the target's name is a string.
    fn copy_name_and_length(
        &mut self,
        dom: &mut Dom,
        wrapper: ObjectId,
        target: ObjectId,
    ) -> Result<(), JsError> {
        let length = if self.has_own_named(dom, target, "length")? {
            match self.get_member(dom, target, "length")? {
                JsValue::Number(number) => number.trunc().max(0.0),
                _ => 0.0,
            }
        } else {
            0.0
        };
        if !self.realm.define_property(
            wrapper,
            "length",
            function_metadata_property(JsValue::Number(length)),
        ) {
            return Err(JsError::type_error("cannot define the length of a wrapper"));
        }
        let name = match self.get_member(dom, target, "name")? {
            name @ JsValue::String(_) => name,
            _ => JsValue::String(JsString::from(String::new())),
        };
        if !self
            .realm
            .define_property(wrapper, "name", function_metadata_property(name))
        {
            return Err(JsError::type_error("cannot define the name of a wrapper"));
        }
        Ok(())
    }

    /// `HasOwnProperty(object, key)`, consulting a proxy's trap.
    fn has_own_named(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
    ) -> Result<bool, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Proxy { .. }) => Ok(self
                .proxy_get_own_property(dom, object, &PropertyName::String(key.to_owned()))?
                .is_some()),
            _ => Ok(self.realm.own_property(object, key).is_some()),
        }
    }

    /// `[[Call]]` of a wrapped function. The current realm is the wrapper's
    /// own, the caller's realm. Arguments and `this` cross into the target's
    /// realm, the result crosses back, and an exception of the target is a
    /// `TypeError` of the caller's realm.
    pub(in crate::runtime) fn call_wrapped_function(
        &mut self,
        dom: &mut Dom,
        target: ObjectId,
        arguments: &[JsValue],
        receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        let caller = self.realm.current_realm();
        let target_realm = self.function_realm(target)?;
        // The wrapped values are pinned until the call returns, because a
        // collection may run while later values are being wrapped.
        let pinned = self.transient_roots.len();
        let outcome =
            self.call_wrapped_target(dom, target, target_realm, caller, arguments, receiver);
        self.transient_roots.truncate(pinned);
        outcome
    }

    fn call_wrapped_target(
        &mut self,
        dom: &mut Dom,
        target: ObjectId,
        target_realm: RealmId,
        caller: RealmId,
        arguments: &[JsValue],
        receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        let mut wrapped_arguments = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let wrapped = self.get_wrapped_value(dom, target_realm, argument.clone())?;
            if let JsValue::Object(object) = wrapped {
                self.transient_roots.push(object);
            }
            wrapped_arguments.push(wrapped);
        }
        let wrapped_receiver = self.get_wrapped_value(dom, target_realm, receiver)?;
        match self.call_with_this(dom, target, &wrapped_arguments, wrapped_receiver) {
            Ok(value) => self.get_wrapped_value(dom, caller, value),
            Err(error) => Err(JsError::type_error(format!(
                "wrapped function threw: {}",
                error.message()
            ))),
        }
    }

    /// `GetFunctionRealm(obj)` (ECMA-262 7.3.24): a bound function and a proxy
    /// answer for the function they wrap, and every other object answers for
    /// the realm it was created in.
    pub(in crate::runtime) fn function_realm(&self, object: ObjectId) -> Result<RealmId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::BoundCallable { target, .. }) => self.function_realm(target),
            Some(ObjectHost::Proxy { handler: None, .. }) => {
                Err(JsError::type_error("GetFunctionRealm of a revoked proxy"))
            }
            Some(ObjectHost::Proxy { target, .. }) => self.function_realm(target),
            _ => Ok(self.realm.object_realm(object)),
        }
    }
}
