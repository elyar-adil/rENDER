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
use crate::runtime::builtins::object::PropertyName;
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
        self.evaluate_class_with_function_name(dom, name, name, super_class, elements)
    }

    /// An anonymous class that takes its name from `NamedEvaluation`. The
    /// constructor carries `function_name`, but the class binds no name inside
    /// its own body, which is what a named class expression does.
    pub(super) fn evaluate_anonymous_class_named(
        &mut self,
        dom: &mut Dom,
        function_name: &str,
        super_class: Option<&Expr>,
        elements: &[ClassElement],
    ) -> Result<JsValue, JsError> {
        self.evaluate_class_with_function_name(
            dom,
            None,
            Some(function_name),
            super_class,
            elements,
        )
    }

    /// Evaluates a class with its inner binding (`name`) and its constructor's
    /// `name` kept separate, since `NamedEvaluation` supplies only the latter.
    fn evaluate_class_with_function_name(
        &mut self,
        dom: &mut Dom,
        name: Option<&str>,
        function_name: Option<&str>,
        super_class: Option<&Expr>,
        elements: &[ClassElement],
    ) -> Result<JsValue, JsError> {
        // 1. The class scope holds the immutable inner name binding. It exists
        // while the heritage is evaluated, when the binding is still in its
        // temporal dead zone (§15.7.14 steps 2-8).
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
        let result = self.evaluate_class_in_scope(dom, name, function_name, super_class, elements);
        self.environment.pop();
        result
    }

    /// The body of `ClassDefinitionEvaluation`, run inside the class scope.
    fn evaluate_class_in_scope(
        &mut self,
        dom: &mut Dom,
        name: Option<&str>,
        function_name: Option<&str>,
        super_class: Option<&Expr>,
        elements: &[ClassElement],
    ) -> Result<JsValue, JsError> {
        // 2. Heritage: the parent constructor and the prototype of the new
        // class's prototype object.
        let (super_constructor, super_prototype) = match super_class {
            None => (None, Some(self.realm.object_prototype())),
            Some(expression) => {
                let value = self.evaluate(dom, expression)?;
                match value {
                    JsValue::Null => (None, None),
                    // A user arrow, method, generator, or async function has no
                    // [[Construct]], so it cannot be extended (§15.7.14 step 6.a).
                    JsValue::Object(object)
                        if Self::is_callable_object(object, &self.realm)
                            && (!matches!(
                                self.realm.host(object),
                                Some(ObjectHost::UserFunction(_))
                            ) || self.is_constructor(object)) =>
                    {
                        // `Get(superclass, "prototype")` runs a getter, and the
                        // result must be an object or null (§15.7.14 step 7.a).
                        let prototype = match self.get_value(dom, object, "prototype")? {
                            JsValue::Object(prototype) => Some(prototype),
                            JsValue::Null => None,
                            _ => {
                                return Err(JsError::type_error(
                                    "Class extends value does not have valid prototype property",
                                ));
                            }
                        };
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

        // 3. Reserve a private-name id for every `#name` in the body, so
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

        self.evaluate_class_body(
            dom,
            function_name,
            name,
            super_constructor,
            super_prototype,
            derived,
            &private_names,
            elements,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_class_body(
        &mut self,
        dom: &mut Dom,
        name: Option<&str>,
        binding: Option<&str>,
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
            binding,
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
        binding: Option<&str>,
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
                PropertyKey::Computed(expression) => {
                    let value = self.evaluate(dom, expression)?;
                    match self.to_property_key_value(dom, value)? {
                        JsValue::Symbol(symbol) => ResolvedKey::Symbol(symbol),
                        other => ResolvedKey::Named(other.to_js_string()),
                    }
                }
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
                    ResolvedKey::Private(id, name) => ClassFieldKey::Private(*id, name.clone()),
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
            // SetFunctionName with a prefix: an accessor is named `get x` or `set x`.
            let method_name = match element.kind {
                ClassElementKind::Get => format!("get {}", element_name(key)),
                ClassElementKind::Set => format!("set {}", element_name(key)),
                _ => element_name(key),
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
            // Methods and accessors are not constructors, so they have no
            // `prototype` (only a generator method keeps one).
            if !element.is_generator {
                self.realm.remove_method_prototype(method);
            }
            self.install_class_element(home, key, element.kind, method)?;
        }

        // 7. Initialize the inner class-name binding. Static fields and blocks run
        // after this, so they can name the class they belong to.
        if let Some(binding) = binding {
            self.environment
                .last()
                .expect("class scope was pushed")
                .borrow_mut()
                .bindings
                .insert(
                    binding.to_owned(),
                    Binding {
                        value: JsValue::Object(constructor),
                        mutable: false,
                        initialized: true,
                        kind: VariableKind::Const,
                    },
                );
        }

        // 8. Static fields and initialization blocks, in source order. Their
        // [[HomeObject]] is the constructor, so `super.x` reads from its parent.
        self.class_frames.push(ClassFrame {
            function: Some(Rc::new(ClassFunction {
                home_object: Some(constructor),
                super_constructor: None,
                derived: false,
                fields: Vec::new(),
                private_names: private_names.clone(),
                constructor: false,
                environment: self.environment.clone(),
            })),
            private_scope: Some(private_names.clone()),
        });
        let result = self.define_static_elements(dom, constructor, elements, &resolved_keys);
        self.class_frames.pop();
        result?;

        Ok(JsValue::Object(constructor))
    }

    /// Evaluate the static fields and initialization blocks of a class body,
    /// in source order, against the constructor they initialize.
    fn define_static_elements(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        elements: &[ClassElement],
        resolved_keys: &[ResolvedKey],
    ) -> Result<(), JsError> {
        for (element, key) in elements.iter().zip(resolved_keys) {
            if !element.is_static {
                continue;
            }
            match element.kind {
                ClassElementKind::Field => {
                    let value = match &element.initializer {
                        Some(initializer) => self
                            .with_this(JsValue::Object(constructor), |runtime| {
                                runtime.evaluate_named(dom, initializer, &element_name(key))
                            })?,
                        None => JsValue::Undefined,
                    };
                    self.define_field(dom, constructor, key, value)?;
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
        Ok(())
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
    ) -> Result<(), JsError> {
        let (getter, setter) = match kind {
            ClassElementKind::Get => (Some(method), None),
            ClassElementKind::Set => (None, Some(method)),
            _ => (None, None),
        };
        let descriptor = if getter.is_some() || setter.is_some() {
            let existing = match key {
                ResolvedKey::Named(key) => self.realm.own_property(target, key),
                ResolvedKey::Symbol(symbol) => self.realm.own_symbol_property(target, symbol),
                ResolvedKey::Private(id, _) => self.realm.own_private_method(target, *id),
            };
            PropertyDescriptor {
                getter: getter.or(existing.as_ref().and_then(|d| d.getter)),
                setter: setter.or(existing.as_ref().and_then(|d| d.setter)),
                value: JsValue::Undefined,
                writable: false,
                enumerable: false,
                configurable: true,
            }
        } else {
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(method),
                writable: true,
                enumerable: false,
                configurable: true,
            }
        };
        if self.define_class_element(target, key, descriptor) {
            Ok(())
        } else {
            // DefinePropertyOrThrow (§7.3.8): a public element the target refuses,
            // such as a static `prototype` of a class, is a TypeError.
            Err(JsError::type_error(
                "class element cannot be defined on its target",
            ))
        }
    }

    /// Define one class field on `object` (`DefineField`, ECMA-262 7.3.33): a
    /// public field is `CreateDataPropertyOrThrow`, so a static field named
    /// `prototype` is refused, and a private field is a private element.
    fn define_field(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &ResolvedKey,
        value: JsValue,
    ) -> Result<(), JsError> {
        match key {
            ResolvedKey::Named(name) => {
                let key = PropertyName::String(name.clone());
                self.create_data_field_or_throw(dom, object, &key, value)
            }
            ResolvedKey::Symbol(symbol) => {
                let key = PropertyName::Symbol(symbol.clone());
                self.create_data_field_or_throw(dom, object, &key, value)
            }
            ResolvedKey::Private(id, _) => {
                self.realm.set_private_field(object, *id, value);
                Ok(())
            }
        }
    }

    /// Define a class element on `target`. Private elements always land in the
    /// object's private-method table; public ones report whether the property
    /// definition was accepted.
    fn define_class_element(
        &mut self,
        target: ObjectId,
        key: &ResolvedKey,
        descriptor: PropertyDescriptor,
    ) -> bool {
        match key {
            ResolvedKey::Named(key) => self.realm.define_property(target, key.clone(), descriptor),
            ResolvedKey::Symbol(symbol) => self
                .realm
                .define_symbol_property(target, symbol, descriptor),
            ResolvedKey::Private(id, _) => {
                self.realm.define_private_method(target, *id, descriptor);
                true
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
        // InitializeInstanceElements (ECMA-262 7.3.34): the class's private
        // methods and accessors become own private elements of the instance
        // before any field is defined. They live on the class prototype, which
        // is the constructor's [[HomeObject]].
        let methods = class
            .home_object
            .map(|prototype| self.realm.private_method_entries(prototype))
            .unwrap_or_default();
        for (id, descriptor) in methods {
            // PrivateMethodOrAccessorAdd: a non-extensible object takes no new
            // private elements, so the element cannot be added.
            if self.realm.has_own_private_element(instance, id)
                || !self.realm.is_extensible(instance)
            {
                return Err(JsError::type_error(
                    "private methods cannot be added to this object",
                ));
            }
            self.realm.define_private_method(instance, id, descriptor);
        }
        if class.fields.is_empty() {
            return Ok(());
        }
        let suspended = self.suspend_scopes();
        let previous_environment =
            std::mem::replace(&mut self.environment, class.environment.clone());
        // Field initializers run with `this` bound and the class's own
        // private scope visible to nested classes they may define.
        self.class_frames.push(ClassFrame {
            function: Some(class.clone()),
            private_scope: Some(class.private_names.clone()),
        });
        let result = self.with_this(JsValue::Object(instance), |runtime| {
            for field in &class.fields {
                let value = match &field.initializer {
                    Some(initializer) => {
                        runtime.evaluate_named(dom, initializer, &field_name(&field.key))?
                    }
                    None => JsValue::Undefined,
                };
                match &field.key {
                    ClassFieldKey::Named(name) => {
                        let key = PropertyName::String(name.clone());
                        runtime.create_data_field_or_throw(dom, instance, &key, value)?;
                    }
                    ClassFieldKey::Symbol(symbol) => {
                        let key = PropertyName::Symbol(symbol.clone());
                        runtime.create_data_field_or_throw(dom, instance, &key, value)?;
                    }
                    ClassFieldKey::Private(id, _) => {
                        // PrivateFieldAdd (ECMA-262 7.3.29): an element the
                        // object already has (from a base constructor that
                        // returned it) is an error, not an overwrite.
                        // PrivateFieldAdd likewise refuses a non-extensible object.
                        if runtime.realm.has_own_private_element(instance, *id)
                            || !runtime.realm.is_extensible(instance)
                        {
                            return Err(JsError::type_error(
                                "private field cannot be added to this object",
                            ));
                        }
                        runtime.realm.set_private_field(instance, *id, value);
                    }
                }
            }
            Ok(())
        });
        // The frame and environment are restored on failure too, so an
        // initializer that throws leaves the caller's context intact.
        self.class_frames.pop();
        self.environment = previous_environment;
        self.resume_scopes(suspended);
        result
    }

    /// Give an object-literal method or accessor its `[[HomeObject]]`, the object
    /// literal itself, so `super.name` in its body reads from that object's
    /// prototype (ECMA-262 13.2.5.4 `MethodDefinitionEvaluation`). The method is not
    /// a constructor, as a class method is not, so its metadata says so.
    pub(super) fn set_method_home_object(&mut self, method: &JsValue, home: ObjectId) {
        let JsValue::Object(method) = method else {
            return;
        };
        let Some(ObjectHost::UserFunction(index)) = self.realm.host(*method) else {
            return;
        };
        let private_names = self
            .class_frames
            .last()
            .and_then(|frame| frame.private_scope.clone())
            .unwrap_or_default();
        let metadata = Rc::new(ClassFunction {
            home_object: Some(home),
            private_names,
            environment: self.environment.clone(),
            ..ClassFunction::default()
        });
        if let Some(function) = self.functions.get_mut(index) {
            function.class = Some(metadata);
        }
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
/// The name `SetFunctionName` gives a class element with this key (§15.4.4 and
/// §15.7.14): a symbol key contributes `[description]`, a private name `#name`.
fn element_name(key: &ResolvedKey) -> String {
    match key {
        ResolvedKey::Named(key) => key.clone(),
        ResolvedKey::Symbol(symbol) => symbol
            .description()
            .map_or_else(String::new, |description| format!("[{description}]")),
        ResolvedKey::Private(_, private) => format!("#{private}"),
    }
}

/// The name a field initializer's anonymous function takes (`NamedEvaluation`).
fn field_name(key: &ClassFieldKey) -> String {
    match key {
        ClassFieldKey::Named(key) => key.clone(),
        ClassFieldKey::Symbol(symbol) => symbol
            .description()
            .map_or_else(String::new, |description| format!("[{description}]")),
        ClassFieldKey::Private(_, private) => format!("#{private}"),
    }
}

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
