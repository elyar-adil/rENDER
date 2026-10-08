#![allow(
    clippy::cast_precision_loss,
    clippy::match_same_arms,
    clippy::too_many_lines
)]

//! `ClassDefinitionEvaluation`: constructor/prototype wiring, method and
//! accessor installation, instance/static field initialization, `super`
//! lookup, and private-name access.
//!
//! Classes are represented as ordinary constructible `UserFunction` objects
//! whose [`ClassFunction`] metadata carries the home object (for `super`),
//! the parent constructor (for derived construction), the instance-field
//! definitions, and the class's private-name registry. The registry is also
//! reachable lexically: private access resolves the written `#name` through
//! the active call's class frame, which arrows inherit from the function
//! they execute inside.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use render_dom::Dom;

use crate::JsError;
use crate::JsErrorKind;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::parser::ClassElement;
use crate::parser::ClassElementKind;
use crate::parser::Expr;
use crate::parser::FunctionKind;
use crate::parser::PARAMETER_REST_MARKER;
use crate::parser::PropertyKey;
use crate::parser::Statement;
use crate::parser::VariableKind;
use crate::runtime::JsRuntime;
use crate::runtime::types::Binding;
use crate::runtime::types::ClassFieldDefinition;
use crate::runtime::types::ClassFieldKey;
use crate::runtime::types::ClassFrame;
use crate::runtime::types::ClassFunction;
use crate::runtime::types::EnvironmentRecord;
use crate::runtime::types::FunctionFlags;
use crate::runtime::types::PrivateScope;
use crate::value::ObjectHost;
use crate::value::PropertyDescriptor;

impl JsRuntime {
    /// The class metadata of the innermost active class function.
    pub(super) fn class_frame(&self) -> Option<Rc<ClassFunction>> {
        self.class_frames
            .last()
            .and_then(|frame| frame.function.clone())
    }

    /// Resolve a private name through the lexically enclosing class scopes.
    fn private_id(&self, name: &str) -> Result<u64, JsError> {
        self.class_frames
            .last()
            .and_then(|frame| frame.private_scope.as_ref())
            .and_then(|scope| scope.resolve(name))
            .ok_or_else(|| {
                JsError::new(
                    JsErrorKind::Syntax,
                    format!("private name #{name} is not declared in this class"),
                    None,
                )
            })
    }

    /// `ClassDefinitionEvaluation` (§15.7.14) for a class declaration or
    /// expression. The class-name binding, when present, is initialized by
    /// the caller (declaration) or via the pushed class scope (expression).
    pub(super) fn evaluate_class(
        &mut self,
        dom: &mut Dom,
        name: Option<&str>,
        super_class: Option<&Expr>,
        elements: &[ClassElement],
    ) -> Result<JsValue, JsError> {
        // 1. Heritage: the parent constructor and the prototype of the new
        // class's prototype object.
        let (super_constructor, super_prototype) = match super_class {
            None => (None, Some(self.realm.object_prototype())),
            Some(expression) => {
                let value = self.evaluate(dom, expression)?;
                match value {
                    JsValue::Null => (None, None),
                    JsValue::Object(object) if Self::is_callable_object(object, &self.realm) => {
                        let prototype =
                            self.realm
                                .get_property(object, "prototype")
                                .and_then(|value| match value {
                                    JsValue::Object(prototype) => Some(prototype),
                                    _ => None,
                                });
                        (Some(object), prototype)
                    }
                    _ => {
                        return Err(JsError::type_error(
                            "Class extends value is not a constructor or null",
                        ));
                    }
                }
            }
        };
        let derived = super_class.is_some();

        // 2. Reserve a private-name id for every `#name` in the body, so
        // fields, methods, and accessors of one class share identity. The
        // scope chains to the lexically enclosing class, letting a nested
        // class body reach outer private names.
        let mut names: BTreeMap<String, u64> = BTreeMap::new();
        for element in elements {
            if let PropertyKey::Private(element_name) = &element.key
                && !names.contains_key(element_name)
            {
                names.insert(element_name.clone(), self.next_private_id);
                self.next_private_id = self.next_private_id.saturating_add(1);
            }
        }
        let private_names = Rc::new(PrivateScope {
            names,
            outer: self
                .class_frames
                .last()
                .and_then(|frame| frame.private_scope.clone()),
        });

        // 3. Class scope: the class name is visible inside the body.
        let mut class_environment = EnvironmentRecord {
            function_scope: false,
            ..EnvironmentRecord::default()
        };
        if let Some(name) = name {
            class_environment.bindings.insert(
                name.to_owned(),
                Binding {
                    value: JsValue::Undefined,
                    mutable: false,
                    initialized: false,
                    kind: VariableKind::Const,
                },
            );
        }
        self.environment
            .push(Rc::new(RefCell::new(class_environment)));

        let result = self.evaluate_class_body(
            dom,
            name,
            super_constructor,
            super_prototype,
            derived,
            &private_names,
            elements,
        );
        self.environment.pop();
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_class_body(
        &mut self,
        dom: &mut Dom,
        name: Option<&str>,
        super_constructor: Option<ObjectId>,
        super_prototype: Option<ObjectId>,
        derived: bool,
        private_names: &Rc<PrivateScope>,
        elements: &[ClassElement],
    ) -> Result<JsValue, JsError> {
        // The class body itself is a lexical private-name scope: nested
        // class expressions defined in keys, methods, or initializers see
        // this class's names.
        self.class_frames.push(ClassFrame {
            function: None,
            private_scope: Some(private_names.clone()),
        });
        let result = self.evaluate_class_body_inner(
            dom,
            name,
            super_constructor,
            super_prototype,
            derived,
            private_names,
            elements,
        );
        self.class_frames.pop();
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_class_body_inner(
        &mut self,
        dom: &mut Dom,
        name: Option<&str>,
        super_constructor: Option<ObjectId>,
        super_prototype: Option<ObjectId>,
        derived: bool,
        private_names: &Rc<PrivateScope>,
        elements: &[ClassElement],
    ) -> Result<JsValue, JsError> {
        // 4. Resolve every property key once, in source order, before any
        // method or field uses it. Computed keys are evaluated exactly once.
        let mut resolved_keys = Vec::with_capacity(elements.len());
        for element in elements {
            let key = match &element.key {
                PropertyKey::Static(key) => ResolvedKey::Named(key.clone()),
                PropertyKey::Computed(expression) => match self.evaluate(dom, expression)? {
                    JsValue::Symbol(symbol) => ResolvedKey::Symbol(symbol),
                    other => ResolvedKey::Named(other.to_js_string()),
                },
                PropertyKey::Private(private) => ResolvedKey::Private(
                    *private_names
                        .names
                        .get(private)
                        .expect("private names were pre-registered"),
                    private.clone(),
                ),
                PropertyKey::Spread => {
                    return Err(JsError::new(
                        JsErrorKind::Syntax,
                        "spread is not valid in a class body",
                        None,
                    ));
                }
            };
            resolved_keys.push(key);
        }

        // 5. The constructor function: the explicit element or the default.
        let constructor_element = elements
            .iter()
            .find(|element| !element.is_static && element.kind == ClassElementKind::Constructor);
        let (parameters, body) = match constructor_element {
            Some(element) => (element.parameters.clone(), element.body.clone()),
            None if derived => (
                vec![format!("{PARAMETER_REST_MARKER}args")],
                vec![Statement::Expression(Expr::SuperCall {
                    arguments: vec![Expr::Spread(Box::new(Expr::Identifier("args".to_owned())))],
                    offset: 0,
                })],
            ),
            None => (Vec::new(), Vec::new()),
        };
        let fields: Vec<ClassFieldDefinition> = elements
            .iter()
            .zip(&resolved_keys)
            .filter(|(element, _)| !element.is_static && element.kind == ClassElementKind::Field)
            .map(|(element, key)| ClassFieldDefinition {
                key: match key {
                    ResolvedKey::Named(key) => ClassFieldKey::Named(key.clone()),
                    ResolvedKey::Symbol(symbol) => ClassFieldKey::Symbol(symbol.clone()),
                    ResolvedKey::Private(id, _) => ClassFieldKey::Private(*id),
                },
                initializer: element.initializer.clone(),
            })
            .collect();
        let class_metadata = Rc::new(ClassFunction {
            home_object: None,
            super_constructor,
            derived,
            fields,
            private_names: private_names.clone(),
            constructor: true,
            environment: self.environment.clone(),
        });
        let constructor_name = name.unwrap_or("");
        let constructor = self.create_function_meta(
            Some(constructor_name),
            &parameters,
            &body,
            FunctionFlags {
                arrow: false,
                strict: true,
                class: Some(class_metadata),
                kind: FunctionKind::Normal,
            },
        )?;
        let JsValue::Object(constructor) = constructor else {
            return Err(JsError::type_error("class constructor allocation failed"));
        };
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(prototype) => Some(prototype),
                _ => None,
            })
            .unwrap_or_else(|| self.realm.create_ordinary_object());
        self.realm.set_prototype(prototype, super_prototype);
        // Class prototypes and constructors link both ways; the class
        // constructor's `prototype` is non-writable, unlike ordinary
        // functions.
        self.realm.configure_class_prototype(constructor, prototype);
        // The constructor's [[HomeObject]] is the class prototype, so field
        // initializers and constructor bodies resolve `super.x` there.
        if let Some(ObjectHost::UserFunction(index)) = self.realm.host(constructor)
            && let Some(class) = self
                .functions
                .get_mut(index)
                .and_then(|function| function.class.take())
        {
            let updated = Rc::new(ClassFunction {
                home_object: Some(prototype),
                ..(*class).clone()
            });
            self.functions[index].class = Some(updated);
        }
        if let Some(super_constructor) = super_constructor {
            self.realm
                .set_prototype(constructor, Some(super_constructor));
        }

        // 6. Methods and accessors, in source order.
        for (element, key) in elements.iter().zip(&resolved_keys) {
            match element.kind {
                ClassElementKind::Method | ClassElementKind::Get | ClassElementKind::Set => {}
                _ => continue,
            }
            let home = if element.is_static {
                constructor
            } else {
                prototype
            };
            let metadata = Rc::new(ClassFunction {
                home_object: Some(home),
                super_constructor: None,
                derived: false,
                fields: Vec::new(),
                private_names: private_names.clone(),
                constructor: false,
                environment: self.environment.clone(),
            });
            let method_name = match key {
                ResolvedKey::Named(key) => key.clone(),
                // §15.4.4: a method named by a symbol is called `[description]`.
                ResolvedKey::Symbol(symbol) => symbol
                    .description()
                    .map_or_else(String::new, |description| format!("[{description}]")),
                ResolvedKey::Private(_, private) => format!("#{private}"),
            };
            let method = self.create_function_meta(
                Some(&method_name),
                &element.parameters,
                &element.body,
                FunctionFlags {
                    arrow: false,
                    strict: true,
                    class: Some(metadata),
                    kind: FunctionKind::new(element.is_async, element.is_generator),
                },
            )?;
            let JsValue::Object(method) = method else {
                continue;
            };
            self.install_class_element(home, key, element.kind, method);
        }

        // 7. Initialize the inner class-name binding. Static fields and blocks run
        // after this, so they can name the class they belong to.
        if let Some(name) = name {
            self.environment
                .last()
                .expect("class scope was pushed")
                .borrow_mut()
                .bindings
                .insert(
                    name.to_owned(),
                    Binding {
                        value: JsValue::Object(constructor),
                        mutable: false,
                        initialized: true,
                        kind: VariableKind::Const,
                    },
                );
        }

        // 8. Static fields and initialization blocks, in source order.
        for (element, key) in elements.iter().zip(&resolved_keys) {
            if !element.is_static {
                continue;
            }
            match element.kind {
                ClassElementKind::Field => {
                    let value = match &element.initializer {
                        Some(initializer) => self
                            .with_this(JsValue::Object(constructor), |runtime| {
                                runtime.evaluate(dom, initializer)
                            })?,
                        None => JsValue::Undefined,
                    };
                    match key {
                        ResolvedKey::Named(key) => {
                            self.realm.define_property(
                                constructor,
                                key.clone(),
                                PropertyDescriptor {
                                    getter: None,
                                    setter: None,
                                    value,
                                    writable: true,
                                    enumerable: true,
                                    configurable: true,
                                },
                            );
                        }
                        ResolvedKey::Symbol(symbol) => {
                            self.realm.define_symbol_property(
                                constructor,
                                symbol,
                                PropertyDescriptor {
                                    getter: None,
                                    setter: None,
                                    value,
                                    writable: true,
                                    enumerable: true,
                                    configurable: true,
                                },
                            );
                        }
                        ResolvedKey::Private(id, _) => {
                            self.realm.set_private_field(constructor, *id, value);
                        }
                    }
                }
                ClassElementKind::StaticBlock => {
                    self.with_this(JsValue::Object(constructor), |runtime| {
                        // A static block is function-like: its own `let`/`const`
                        // and `var` declarations are local to it.
                        runtime.instantiate_statements(&element.body)?;
                        runtime.evaluate_statements(dom, &element.body)
                    })?;
                }
                _ => {}
            }
        }

        Ok(JsValue::Object(constructor))
    }

    /// Run `body` with `this` bound to `value` in a temporary environment,
    /// restoring the previous environment afterwards.
    fn with_this<T>(
        &mut self,
        value: JsValue,
        body: impl FnOnce(&mut Self) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        let mut environment = EnvironmentRecord {
            function_scope: true,
            ..EnvironmentRecord::default()
        };
        environment.bindings.insert(
            "this".to_owned(),
            Binding {
                value,
                mutable: false,
                initialized: true,
                kind: VariableKind::Var,
            },
        );
        self.environment.push(Rc::new(RefCell::new(environment)));
        let result = body(self);
        self.environment.pop();
        result
    }

    /// Define one method/accessor on a class prototype or constructor.
    /// Accessors merge with an earlier accessor of the opposite kind, and
    /// private elements land in the object's private-method table.
    fn install_class_element(
        &mut self,
        target: ObjectId,
        key: &ResolvedKey,
        kind: ClassElementKind,
        method: ObjectId,
    ) {
        let (getter, setter) = match kind {
            ClassElementKind::Get => (Some(method), None),
            ClassElementKind::Set => (None, Some(method)),
            _ => (None, None),
        };
        if getter.is_some() || setter.is_some() {
            let existing = match key {
                ResolvedKey::Named(key) => self.realm.own_property(target, key),
                ResolvedKey::Symbol(symbol) => self.realm.own_symbol_property(target, symbol),
                ResolvedKey::Private(id, _) => self.realm.own_private_method(target, *id),
            };
            let descriptor = PropertyDescriptor {
                getter: getter.or(existing.as_ref().and_then(|d| d.getter)),
                setter: setter.or(existing.as_ref().and_then(|d| d.setter)),
                value: JsValue::Undefined,
                writable: false,
                enumerable: false,
                configurable: true,
            };
            match key {
                ResolvedKey::Named(key) => {
                    self.realm.define_property(target, key.clone(), descriptor);
                }
                ResolvedKey::Symbol(symbol) => {
                    self.realm
                        .define_symbol_property(target, symbol, descriptor);
                }
                ResolvedKey::Private(id, _) => {
                    self.realm.define_private_method(target, *id, descriptor);
                }
            }
            return;
        }
        let descriptor = PropertyDescriptor {
            getter: None,
            setter: None,
            value: JsValue::Object(method),
            writable: true,
            enumerable: false,
            configurable: true,
        };
        match key {
            ResolvedKey::Named(key) => {
                self.realm.define_property(target, key.clone(), descriptor);
            }
            ResolvedKey::Symbol(symbol) => {
                self.realm
                    .define_symbol_property(target, symbol, descriptor);
            }
            ResolvedKey::Private(id, _) => {
                self.realm.define_private_method(target, *id, descriptor);
            }
        }
    }

    /// Initialize a class's instance fields on `instance`, evaluating each
    /// initializer in the class's captured scope with `this = instance`.
    pub(super) fn run_instance_fields(
        &mut self,
        dom: &mut Dom,
        class: &Rc<ClassFunction>,
        instance: ObjectId,
    ) -> Result<(), JsError> {
        if class.fields.is_empty() {
            return Ok(());
        }
        let previous_environment =
            std::mem::replace(&mut self.environment, class.environment.clone());
        // Field initializers run with `this` bound and the class's own
        // private scope visible to nested classes they may define.
        self.class_frames.push(ClassFrame {
            function: Some(class.clone()),
            private_scope: Some(class.private_names.clone()),
        });
        self.with_this(JsValue::Object(instance), |runtime| {
            for field in &class.fields {
                let value = match &field.initializer {
                    Some(initializer) => runtime.evaluate(dom, initializer)?,
                    None => JsValue::Undefined,
                };
                match &field.key {
                    ClassFieldKey::Named(name) => {
                        runtime.realm.define_property(
                            instance,
                            name.clone(),
                            PropertyDescriptor {
                                getter: None,
                                setter: None,
                                value,
                                writable: true,
                                enumerable: true,
                                configurable: true,
                            },
                        );
                    }
                    ClassFieldKey::Symbol(symbol) => {
                        runtime.realm.define_symbol_property(
                            instance,
                            symbol,
                            PropertyDescriptor {
                                getter: None,
                                setter: None,
                                value,
                                writable: true,
                                enumerable: true,
                                configurable: true,
                            },
                        );
                    }
                    ClassFieldKey::Private(id) => {
                        runtime.realm.set_private_field(instance, *id, value);
                    }
                }
            }
            Ok(())
        })?;
        self.class_frames.pop();
        self.environment = previous_environment;
        Ok(())
    }

    /// `super.property` read: the value found on the home object's prototype
    /// chain, with accessors invoked on the current receiver.
    pub(super) fn read_super_property(
        &mut self,
        dom: &mut Dom,
        property: &str,
    ) -> Result<JsValue, JsError> {
        let class = self.class_frame().ok_or_else(|| {
            JsError::new(JsErrorKind::Syntax, "'super' keyword unexpected here", None)
        })?;
        let Some(home) = class.home_object else {
            return Err(JsError::new(
                JsErrorKind::Syntax,
                "'super' keyword unexpected here",
                None,
            ));
        };
        let receiver = self.current_this()?;
        let base = self.realm.get_prototype(home);
        self.read_from_base(dom, base, property, receiver)
    }

    fn read_from_base(
        &mut self,
        dom: &mut Dom,
        base: Option<ObjectId>,
        property: &str,
        receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        let Some(base) = base else {
            return Ok(JsValue::Undefined);
        };
        if self.realm.get_descriptor(base, property).is_none() {
            return Ok(JsValue::Undefined);
        }
        // Walk to the object that owns the property so accessors resolve from
        // its descriptor rather than the receiver's.
        let mut owner = base;
        loop {
            match self.realm.own_property(owner, property) {
                Some(descriptor) => {
                    if descriptor.is_accessor() {
                        let Some(getter) = descriptor.getter else {
                            return Ok(JsValue::Undefined);
                        };
                        return self.call_with_this(dom, getter, &[], receiver);
                    }
                    return Ok(descriptor.value);
                }
                None => match self.realm.get_prototype(owner) {
                    Some(next) => owner = next,
                    None => return Ok(JsValue::Undefined),
                },
            }
        }
    }

    /// `super.property = value`.
    pub(super) fn write_super_property(
        &mut self,
        dom: &mut Dom,
        property: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        let class = self.class_frame().ok_or_else(|| {
            JsError::new(JsErrorKind::Syntax, "'super' keyword unexpected here", None)
        })?;
        let Some(home) = class.home_object else {
            return Err(JsError::new(
                JsErrorKind::Syntax,
                "'super' keyword unexpected here",
                None,
            ));
        };
        let receiver = self.current_this()?;
        let base = self.realm.get_prototype(home);
        if let Some(base) = base
            && let Some(descriptor) = self.realm.get_descriptor(base, property)
            && descriptor.is_accessor()
        {
            let Some(setter) = descriptor.setter else {
                return Err(JsError::type_error("property has no setter"));
            };
            self.call_with_this(dom, setter, &[value], receiver)?;
            return Ok(());
        }
        let JsValue::Object(receiver) = receiver else {
            return Err(JsError::type_error("cannot set a property on a primitive"));
        };
        self.set_member(dom, receiver, property, value)
    }

    /// `object.#name` read.
    pub(super) fn read_private(
        &mut self,
        dom: &mut Dom,
        object: &JsValue,
        name: &str,
    ) -> Result<JsValue, JsError> {
        let id = self.private_id(name)?;
        let JsValue::Object(object) = object else {
            return Err(JsError::type_error(format!(
                "Cannot read private member #{name} from a non-object"
            )));
        };
        if let Some(value) = self.realm.private_field(*object, id) {
            return Ok(value.clone());
        }
        if let Some(descriptor) = self.realm.find_private_method(*object, id) {
            if descriptor.is_accessor() {
                let Some(getter) = descriptor.getter else {
                    return Err(JsError::type_error(format!(
                        "Cannot read private member #{name}: no getter"
                    )));
                };
                return self.call_with_this(dom, getter, &[], JsValue::Object(*object));
            }
            return Ok(descriptor.value);
        }
        Err(JsError::type_error(format!(
            "Cannot read private member #{name} from an object whose class did not declare it"
        )))
    }

    /// `object.#name = value`.
    pub(super) fn write_private(
        &mut self,
        dom: &mut Dom,
        object: &JsValue,
        name: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        let id = self.private_id(name)?;
        let JsValue::Object(object) = object else {
            return Err(JsError::type_error(format!(
                "Cannot write private member #{name} to a non-object"
            )));
        };
        if self.realm.has_private_field(*object, id) {
            self.realm.set_private_field(*object, id, value);
            return Ok(());
        }
        if let Some(descriptor) = self.realm.find_private_method(*object, id) {
            if descriptor.is_accessor() {
                let Some(setter) = descriptor.setter else {
                    return Err(JsError::type_error(format!(
                        "Cannot write private member #{name}: no setter"
                    )));
                };
                self.call_with_this(dom, setter, &[value], JsValue::Object(*object))?;
                return Ok(());
            }
            return Err(JsError::type_error(format!(
                "Cannot assign to private method #{name}"
            )));
        }
        Err(JsError::type_error(format!(
            "Cannot write private member #{name} to an object whose class did not declare it"
        )))
    }

    /// `#name in object`.
    pub(super) fn private_in(&self, object: &JsValue, name: &str) -> Result<bool, JsError> {
        let id = self.private_id(name)?;
        let JsValue::Object(object) = object else {
            return Err(JsError::type_error(format!(
                "Cannot use 'in' operator to search for '#{name}' in a non-object"
            )));
        };
        Ok(self.realm.has_private_field(*object, id)
            || self.realm.find_private_method(*object, id).is_some())
    }
}

/// A class element key resolved once at class-definition time.
enum ResolvedKey {
    Named(String),
    Symbol(JsSymbol),
    Private(u64, String),
}

impl std::fmt::Debug for ResolvedKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(name) => write!(formatter, "{name}"),
            Self::Symbol(symbol) => write!(formatter, "{symbol:?}"),
            Self::Private(_, name) => write!(formatter, "#{name}"),
        }
    }
}
