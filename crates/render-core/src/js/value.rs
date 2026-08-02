//! JavaScript values and the realm-owned object arena.

use std::collections::BTreeMap;
use std::num::FpCategory;

use crate::dom::NodeId;
use url::Url;

/// Stable identity for an object allocated in a [`Realm`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(usize);

impl ObjectId {
    /// Return the object's arena index. This is useful for diagnostics only.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

/// Values supported by the initial interpreter vertical slice.
#[derive(Clone, Debug, PartialEq)]
pub enum JsValue {
    Undefined,
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    Object(ObjectId),
}

impl JsValue {
    /// Apply the string conversion needed by the initial DOM bindings.
    #[must_use]
    pub fn to_js_string(&self) -> String {
        match self {
            Self::Undefined => "undefined".to_owned(),
            Self::Null => "null".to_owned(),
            Self::Boolean(value) => value.to_string(),
            Self::Number(value) => number_to_string(*value),
            Self::String(value) => value.clone(),
            Self::Object(_) => "[object Object]".to_owned(),
        }
    }
}

fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value.is_sign_positive() {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        }
    } else if value.classify() == FpCategory::Zero {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

fn location_components(url: &Url) -> [(&'static str, String); 9] {
    let hostname = url.host_str().unwrap_or_default().to_owned();
    let port = url.port().map_or_else(String::new, |port| port.to_string());
    let host = if port.is_empty() {
        hostname.clone()
    } else {
        format!("{hostname}:{port}")
    };
    [
        ("href", url.as_str().to_owned()),
        ("origin", url.origin().ascii_serialization()),
        ("protocol", format!("{}:", url.scheme())),
        ("host", host),
        ("hostname", hostname),
        ("port", port),
        ("pathname", url.path().to_owned()),
        (
            "search",
            url.query()
                .map_or_else(String::new, |query| format!("?{query}")),
        ),
        (
            "hash",
            url.fragment()
                .map_or_else(String::new, |fragment| format!("#{fragment}")),
        ),
    ]
}

fn sort_property_names(names: &mut [String]) {
    names.sort_by(|left, right| {
        match (
            left.parse::<u32>()
                .ok()
                .filter(|index| index.to_string() == *left),
            right
                .parse::<u32>()
                .ok()
                .filter(|index| index.to_string() == *right),
        ) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.cmp(right),
        }
    });
}

/// An own data-property descriptor.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyDescriptor {
    pub value: JsValue,
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

impl PropertyDescriptor {
    #[must_use]
    pub const fn data(value: JsValue) -> Self {
        Self {
            value,
            writable: true,
            enumerable: true,
            configurable: true,
        }
    }

    const fn builtin(value: JsValue) -> Self {
        Self {
            value,
            writable: true,
            enumerable: false,
            configurable: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeFunction {
    GetElementById,
    QuerySelector,
    QuerySelectorAll,
    CreateElement,
    SetAttribute,
    GetAttribute,
    HasAttribute,
    RemoveAttribute,
    AppendChild,
    RemoveChild,
    InsertBefore,
    RemoveNode,
    Contains,
    Matches,
    Click,
    AddEventListener,
    RemoveEventListener,
    DispatchEvent,
    EventPreventDefault,
    ClassListAdd,
    ClassListRemove,
    ClassListToggle,
    ClassListContains,
    ClassListItem,
    ClassListToString,
    LocationToString,
    QueueMicrotask,
    PromiseResolve,
    PromiseReject,
    PromiseThen,
    PromiseCatch,
    ArrayIsArray,
    ArrayPush,
    ArrayPop,
    ArrayJoin,
    FunctionPrototype,
    FunctionCall,
    FunctionBind,
    MathPow,
    ObjectAssign,
    ObjectKeys,
    ObjectValues,
    ObjectEntries,
    ObjectCreate,
    ObjectDefineProperty,
    ObjectGetOwnPropertyDescriptor,
    ObjectGetOwnPropertyNames,
    ObjectGetPrototypeOf,
    ObjectHasOwn,
    ObjectPrototypeHasOwnProperty,
    ObjectPrototypeIsPrototypeOf,
    ObjectPrototypePropertyIsEnumerable,
    ErrorPrototypeToString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    Error,
    EvalError,
    RangeError,
    ReferenceError,
    SyntaxError,
    TypeError,
    UriError,
}

impl ErrorKind {
    pub(crate) const ALL: [Self; 7] = [
        Self::Error,
        Self::EvalError,
        Self::RangeError,
        Self::ReferenceError,
        Self::SyntaxError,
        Self::TypeError,
        Self::UriError,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::EvalError => "EvalError",
            Self::RangeError => "RangeError",
            Self::ReferenceError => "ReferenceError",
            Self::SyntaxError => "SyntaxError",
            Self::TypeError => "TypeError",
            Self::UriError => "URIError",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum ObjectHost {
    #[default]
    Ordinary,
    Array,
    Document(NodeId),
    Node(NodeId),
    ClassList(NodeId),
    NativeFunction(NativeFunction),
    BoundFunction {
        function: NativeFunction,
        receiver: ObjectId,
    },
    BoundCallable {
        target: ObjectId,
        receiver: JsValue,
        arguments: Vec<JsValue>,
    },
    UserFunction(usize),
    ArrowFunction(usize),
    PromiseConstructor,
    ObjectConstructor,
    FunctionConstructor,
    StringConstructor,
    EventConstructor,
    Location(Url),
    ErrorConstructor(ErrorKind),
    Promise(usize),
    PromiseSettler {
        promise: usize,
        fulfilled: bool,
    },
}

/// An object stored in a realm. Host identity is intentionally private: DOM
/// wrappers can only be created by the binding layer.
#[derive(Clone, Debug, Default)]
pub struct JsObject {
    properties: BTreeMap<String, PropertyDescriptor>,
    prototype: Option<ObjectId>,
    pub(crate) host: ObjectHost,
}

impl JsObject {
    #[must_use]
    pub fn own_property(&self, key: &str) -> Option<&PropertyDescriptor> {
        self.properties.get(key)
    }

    #[must_use]
    pub const fn prototype(&self) -> Option<ObjectId> {
        self.prototype
    }
}

/// Global state and object identity for one JavaScript realm.
#[derive(Debug)]
pub struct Realm {
    objects: Vec<JsObject>,
    global: ObjectId,
    document: ObjectId,
    object_prototype: ObjectId,
    function_prototype: ObjectId,
    array_prototype: ObjectId,
    node_wrappers: BTreeMap<NodeId, ObjectId>,
    class_list_wrappers: BTreeMap<NodeId, ObjectId>,
}

impl Realm {
    pub(crate) fn bootstrap(document_node: NodeId, document_url: &Url) -> Self {
        let mut objects = vec![JsObject::default()];
        let global = ObjectId(0);
        objects.push(JsObject {
            host: ObjectHost::Document(document_node),
            ..JsObject::default()
        });
        let document = ObjectId(1);
        objects[global.0].properties.insert(
            "document".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(document),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        for (name, value) in [
            ("NaN", JsValue::Number(f64::NAN)),
            ("Infinity", JsValue::Number(f64::INFINITY)),
            ("undefined", JsValue::Undefined),
        ] {
            objects[global.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    value,
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
        }
        let queue_microtask = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::BoundFunction {
                function: NativeFunction::QueueMicrotask,
                receiver: global,
            },
            ..JsObject::default()
        });
        objects[global.0].properties.insert(
            "queueMicrotask".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(queue_microtask),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        let object_prototype = Self::install_object(&mut objects, global);
        let function_prototype = Self::install_function(&mut objects, global, object_prototype);
        Self::install_location(
            &mut objects,
            global,
            document,
            object_prototype,
            function_prototype,
            document_url,
        );
        Self::install_navigator(&mut objects, global, object_prototype);
        Self::install_errors(&mut objects, global, object_prototype, function_prototype);
        Self::install_event(&mut objects, global, object_prototype, function_prototype);
        Self::install_string(&mut objects, global, function_prototype);
        Self::install_math(&mut objects, global, object_prototype);
        Self::install_promise(&mut objects, global);
        let array_prototype = Self::install_array(&mut objects, global, object_prototype);
        for (index, object) in objects.iter_mut().enumerate() {
            if object.prototype.is_none() {
                object.prototype = match &object.host {
                    ObjectHost::NativeFunction(_)
                    | ObjectHost::BoundFunction { .. }
                    | ObjectHost::BoundCallable { .. }
                    | ObjectHost::UserFunction(_)
                    | ObjectHost::ArrowFunction(_)
                    | ObjectHost::FunctionConstructor
                    | ObjectHost::StringConstructor
                    | ObjectHost::EventConstructor
                    | ObjectHost::ErrorConstructor(_)
                    | ObjectHost::PromiseSettler { .. } => Some(function_prototype),
                    _ if index != object_prototype.0 => Some(object_prototype),
                    _ => None,
                };
            }
        }
        Self {
            objects,
            global,
            document,
            object_prototype,
            function_prototype,
            array_prototype,
            node_wrappers: BTreeMap::new(),
            class_list_wrappers: BTreeMap::new(),
        }
    }

    fn install_location(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        document: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
        url: &Url,
    ) -> ObjectId {
        let location = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            host: ObjectHost::Location(url.clone()),
            ..JsObject::default()
        });
        let to_string = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::LocationToString),
            ..JsObject::default()
        });
        objects[location.0].properties.insert(
            "toString".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(to_string)),
        );
        for (name, value) in location_components(url) {
            objects[location.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::String(value)),
            );
        }
        for owner in [global, document] {
            objects[owner.0].properties.insert(
                "location".to_owned(),
                PropertyDescriptor {
                    value: JsValue::Object(location),
                    writable: false,
                    enumerable: true,
                    configurable: false,
                },
            );
        }
        objects[global.0].properties.insert(
            "window".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(global),
                writable: false,
                enumerable: true,
                configurable: false,
            },
        );
        location
    }

    fn install_navigator(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
    ) {
        let navigator = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, value) in [
            ("userAgent", "Mozilla/5.0 rENDER/0.1"),
            ("appName", "Netscape"),
            ("appVersion", "5.0 (rENDER)"),
            ("platform", "Win32"),
            ("language", "zh-CN"),
        ] {
            objects[navigator.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::String(value.to_owned())),
            );
        }
        for (name, value) in [("cookieEnabled", true), ("onLine", true)] {
            objects[navigator.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Boolean(value)),
            );
        }
        objects[global.0].properties.insert(
            "navigator".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(navigator),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
    }

    fn install_object(objects: &mut Vec<JsObject>, global: ObjectId) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject::default());
        for (name, function) in [
            (
                "hasOwnProperty",
                NativeFunction::ObjectPrototypeHasOwnProperty,
            ),
            (
                "isPrototypeOf",
                NativeFunction::ObjectPrototypeIsPrototypeOf,
            ),
            (
                "propertyIsEnumerable",
                NativeFunction::ObjectPrototypePropertyIsEnumerable,
            ),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    value: JsValue::Object(method),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        let object = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::ObjectConstructor,
            ..JsObject::default()
        });
        for (name, function) in [
            ("assign", NativeFunction::ObjectAssign),
            ("keys", NativeFunction::ObjectKeys),
            ("values", NativeFunction::ObjectValues),
            ("entries", NativeFunction::ObjectEntries),
            ("create", NativeFunction::ObjectCreate),
            ("defineProperty", NativeFunction::ObjectDefineProperty),
            (
                "getOwnPropertyDescriptor",
                NativeFunction::ObjectGetOwnPropertyDescriptor,
            ),
            (
                "getOwnPropertyNames",
                NativeFunction::ObjectGetOwnPropertyNames,
            ),
            ("getPrototypeOf", NativeFunction::ObjectGetPrototypeOf),
            ("hasOwn", NativeFunction::ObjectHasOwn),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::BoundFunction {
                    function,
                    receiver: object,
                },
                ..JsObject::default()
            });
            objects[object.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[object.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "Object".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(object),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    fn install_string(objects: &mut Vec<JsObject>, global: ObjectId, function_prototype: ObjectId) {
        let string = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::StringConstructor,
            ..JsObject::default()
        });
        objects[global.0].properties.insert(
            "String".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(string),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    fn install_event(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let prevent_default = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::EventPreventDefault),
            ..JsObject::default()
        });
        objects[prototype.0].properties.insert(
            "preventDefault".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(prevent_default)),
        );

        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::EventConstructor,
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "Event".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    fn install_errors(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let error_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let to_string = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::ErrorPrototypeToString),
            ..JsObject::default()
        });
        objects[error_prototype.0].properties.insert(
            "toString".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(to_string)),
        );

        for kind in ErrorKind::ALL {
            let prototype = if kind == ErrorKind::Error {
                error_prototype
            } else {
                let prototype = ObjectId(objects.len());
                objects.push(JsObject {
                    prototype: Some(error_prototype),
                    ..JsObject::default()
                });
                prototype
            };
            let constructor = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::ErrorConstructor(kind),
                ..JsObject::default()
            });
            objects[constructor.0].properties.insert(
                "prototype".to_owned(),
                PropertyDescriptor {
                    value: JsValue::Object(prototype),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
            objects[constructor.0].properties.insert(
                "name".to_owned(),
                PropertyDescriptor::builtin(JsValue::String(kind.name().to_owned())),
            );
            objects[constructor.0].properties.insert(
                "length".to_owned(),
                PropertyDescriptor::builtin(JsValue::Number(1.0)),
            );
            for (name, value) in [("name", kind.name()), ("message", "")] {
                objects[prototype.0].properties.insert(
                    name.to_owned(),
                    PropertyDescriptor::builtin(JsValue::String(value.to_owned())),
                );
            }
            objects[prototype.0].properties.insert(
                "constructor".to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(constructor)),
            );
            objects[global.0].properties.insert(
                kind.name().to_owned(),
                PropertyDescriptor {
                    value: JsValue::Object(constructor),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
    }

    fn install_function(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
    ) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::FunctionPrototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("call", NativeFunction::FunctionCall),
            ("bind", NativeFunction::FunctionBind),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    value: JsValue::Object(method),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        let function = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(prototype),
            host: ObjectHost::FunctionConstructor,
            ..JsObject::default()
        });
        objects[function.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "Function".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(function),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    fn install_math(objects: &mut Vec<JsObject>, global: ObjectId, object_prototype: ObjectId) {
        let math = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let pow = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::NativeFunction(NativeFunction::MathPow),
            ..JsObject::default()
        });
        objects[math.0].properties.insert(
            "pow".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(pow),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        objects[global.0].properties.insert(
            "Math".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(math),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    fn install_promise(objects: &mut Vec<JsObject>, global: ObjectId) {
        let promise = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::PromiseConstructor,
            ..JsObject::default()
        });
        for (name, function) in [
            ("resolve", NativeFunction::PromiseResolve),
            ("reject", NativeFunction::PromiseReject),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::BoundFunction {
                    function,
                    receiver: promise,
                },
                ..JsObject::default()
            });
            objects[promise.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[global.0].properties.insert(
            "Promise".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(promise),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    fn install_array(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
    ) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("push", NativeFunction::ArrayPush),
            ("pop", NativeFunction::ArrayPop),
            ("join", NativeFunction::ArrayJoin),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        let array = ObjectId(objects.len());
        objects.push(JsObject::default());
        let is_array = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::BoundFunction {
                function: NativeFunction::ArrayIsArray,
                receiver: array,
            },
            ..JsObject::default()
        });
        objects[array.0].properties.insert(
            "isArray".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(is_array)),
        );
        objects[array.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::data(JsValue::Object(prototype)),
        );
        objects[global.0].properties.insert(
            "Array".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(array),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    #[must_use]
    pub const fn global_object(&self) -> ObjectId {
        self.global
    }

    #[must_use]
    pub const fn document_object(&self) -> ObjectId {
        self.document
    }

    #[must_use]
    pub fn object(&self, object: ObjectId) -> Option<&JsObject> {
        self.objects.get(object.0)
    }

    /// Allocate an ordinary object with an optional prototype.
    pub fn create_object(&mut self, prototype: Option<ObjectId>) -> ObjectId {
        self.allocate(JsObject {
            prototype,
            ..JsObject::default()
        })
    }

    pub(crate) fn create_ordinary_object(&mut self) -> ObjectId {
        self.create_object(Some(self.object_prototype))
    }

    pub(crate) fn create_error(
        &mut self,
        prototype: ObjectId,
        message: Option<String>,
    ) -> ObjectId {
        let error = self.create_object(Some(prototype));
        if let Some(message) = message {
            self.objects[error.0].properties.insert(
                "message".to_owned(),
                PropertyDescriptor::builtin(JsValue::String(message)),
            );
        }
        error
    }

    pub(crate) fn create_array(&mut self) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.array_prototype),
            host: ObjectHost::Array,
            ..JsObject::default()
        })
    }

    /// Define or replace an own data property.
    ///
    /// Returns `false` if a non-configurable property prevents replacement or
    /// the object does not exist.
    pub fn define_property(
        &mut self,
        object: ObjectId,
        key: impl Into<String>,
        descriptor: PropertyDescriptor,
    ) -> bool {
        let Some(target) = self.objects.get_mut(object.0) else {
            return false;
        };
        let key = key.into();
        if target
            .properties
            .get(&key)
            .is_some_and(|current| !current.configurable)
        {
            return false;
        }
        target.properties.insert(key, descriptor);
        true
    }

    #[must_use]
    pub fn global(&self, key: &str) -> Option<JsValue> {
        self.get_property(self.global, key)
    }

    pub(crate) fn set_global(&mut self, key: String, value: JsValue) -> bool {
        self.set_property(self.global, key, value)
    }

    pub(crate) fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub(crate) fn host(&self, object: ObjectId) -> Option<ObjectHost> {
        self.objects.get(object.0).map(|object| object.host.clone())
    }

    pub(crate) fn get_property(&self, object: ObjectId, key: &str) -> Option<JsValue> {
        let mut candidate = Some(object);
        let mut visited = 0usize;
        while let Some(id) = candidate {
            let current = self.objects.get(id.0)?;
            if let Some(property) = current.properties.get(key) {
                return Some(property.value.clone());
            }
            candidate = current.prototype;
            visited = visited.saturating_add(1);
            if visited > self.objects.len() {
                return None;
            }
        }
        None
    }

    pub(crate) fn own_property(&self, object: ObjectId, key: &str) -> Option<PropertyDescriptor> {
        self.objects.get(object.0)?.properties.get(key).cloned()
    }

    pub(crate) fn enumerable_own_properties(
        &self,
        object: ObjectId,
    ) -> Option<Vec<(String, JsValue)>> {
        let target = self.objects.get(object.0)?;
        let mut properties = target
            .properties
            .iter()
            .filter(|(_, descriptor)| descriptor.enumerable)
            .map(|(key, descriptor)| (key.clone(), descriptor.value.clone()))
            .collect::<Vec<_>>();
        properties.sort_by(|(left, _), (right, _)| {
            match (
                left.parse::<u32>()
                    .ok()
                    .filter(|index| index.to_string() == *left),
                right
                    .parse::<u32>()
                    .ok()
                    .filter(|index| index.to_string() == *right),
            ) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.cmp(right),
            }
        });
        Some(properties)
    }

    pub(crate) fn own_property_names(&self, object: ObjectId) -> Option<Vec<String>> {
        let mut names = self
            .objects
            .get(object.0)?
            .properties
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        sort_property_names(&mut names);
        Some(names)
    }

    pub(crate) fn enumerable_property_names(&self, object: ObjectId) -> Option<Vec<String>> {
        let mut names = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut candidate = Some(object);
        let mut visited = 0usize;
        while let Some(id) = candidate {
            let current = self.objects.get(id.0)?;
            for (key, descriptor) in &current.properties {
                if seen.insert(key.clone()) && descriptor.enumerable {
                    names.push(key.clone());
                }
            }
            candidate = current.prototype;
            visited = visited.saturating_add(1);
            if visited > self.objects.len() {
                return None;
            }
        }
        Some(names)
    }

    pub(crate) fn delete_property(&mut self, object: ObjectId, key: &str) -> bool {
        let Some(target) = self.objects.get_mut(object.0) else {
            return false;
        };
        if target
            .properties
            .get(key)
            .is_some_and(|descriptor| !descriptor.configurable)
        {
            return false;
        }
        target.properties.remove(key);
        true
    }

    pub(crate) fn remove_property(&mut self, object: ObjectId, key: &str) -> Option<JsValue> {
        self.objects
            .get_mut(object.0)?
            .properties
            .remove(key)
            .map(|descriptor| descriptor.value)
    }

    pub(crate) fn set_property(&mut self, object: ObjectId, key: String, value: JsValue) -> bool {
        let Some(target) = self.objects.get_mut(object.0) else {
            return false;
        };
        if let Some(property) = target.properties.get_mut(&key) {
            if !property.writable {
                return false;
            }
            property.value = value;
        } else {
            target
                .properties
                .insert(key, PropertyDescriptor::data(value));
        }
        true
    }

    pub(crate) fn node_wrapper(&mut self, node: NodeId) -> ObjectId {
        if let Some(wrapper) = self.node_wrappers.get(&node) {
            return *wrapper;
        }
        let wrapper = self.allocate(JsObject {
            prototype: Some(self.object_prototype),
            host: ObjectHost::Node(node),
            ..JsObject::default()
        });
        self.node_wrappers.insert(node, wrapper);
        wrapper
    }

    pub(crate) fn class_list_wrapper(&mut self, node: NodeId) -> ObjectId {
        if let Some(wrapper) = self.class_list_wrappers.get(&node) {
            return *wrapper;
        }
        let wrapper = self.allocate(JsObject {
            prototype: Some(self.object_prototype),
            host: ObjectHost::ClassList(node),
            ..JsObject::default()
        });
        self.class_list_wrappers.insert(node, wrapper);
        wrapper
    }

    pub(crate) fn bound_function(
        &mut self,
        function: NativeFunction,
        receiver: ObjectId,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::BoundFunction { function, receiver },
            ..JsObject::default()
        })
    }

    pub(crate) fn bound_callable(
        &mut self,
        target: ObjectId,
        receiver: JsValue,
        arguments: Vec<JsValue>,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::BoundCallable {
                target,
                receiver,
                arguments,
            },
            ..JsObject::default()
        })
    }

    pub(crate) fn arrow_function(&mut self, function: usize) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::ArrowFunction(function),
            ..JsObject::default()
        })
    }

    pub(crate) fn user_function(&mut self, function: usize) -> ObjectId {
        let prototype = self.create_ordinary_object();
        let callable = self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::UserFunction(function),
            ..JsObject::default()
        });
        self.objects[callable.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(prototype),
                writable: true,
                enumerable: false,
                configurable: false,
            },
        );
        self.objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor {
                value: JsValue::Object(callable),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        callable
    }

    pub(crate) fn promise(&mut self, promise: usize) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.object_prototype),
            host: ObjectHost::Promise(promise),
            ..JsObject::default()
        })
    }

    pub(crate) fn promise_settler(&mut self, promise: usize, fulfilled: bool) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.object_prototype),
            host: ObjectHost::PromiseSettler { promise, fulfilled },
            ..JsObject::default()
        })
    }

    fn allocate(&mut self, object: JsObject) -> ObjectId {
        let id = ObjectId(self.objects.len());
        self.objects.push(object);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::{JsValue, PropertyDescriptor, Realm};
    use crate::dom::Dom;
    use url::Url;

    #[test]
    fn ordinary_properties_follow_the_prototype_chain() {
        let dom = Dom::new();
        let mut realm = Realm::bootstrap(
            dom.document(),
            &Url::parse("about:blank").expect("test URL"),
        );
        let prototype = realm.create_object(None);
        assert!(realm.define_property(
            prototype,
            "answer",
            PropertyDescriptor::data(JsValue::Number(42.0)),
        ));
        let object = realm.create_object(Some(prototype));
        assert_eq!(
            realm.get_property(object, "answer"),
            Some(JsValue::Number(42.0))
        );
    }

    #[test]
    fn global_numeric_constants_are_immutable_and_non_enumerable() {
        let dom = Dom::new();
        let mut realm = Realm::bootstrap(
            dom.document(),
            &Url::parse("about:blank").expect("test URL"),
        );
        let global = realm.global_object();

        assert!(matches!(realm.global("NaN"), Some(JsValue::Number(value)) if value.is_nan()));
        assert_eq!(
            realm.global("Infinity"),
            Some(JsValue::Number(f64::INFINITY))
        );
        assert_eq!(realm.global("undefined"), Some(JsValue::Undefined));
        for name in ["NaN", "Infinity", "undefined"] {
            let descriptor = realm
                .own_property(global, name)
                .expect("global constant should have an own descriptor");
            assert!(!descriptor.writable);
            assert!(!descriptor.enumerable);
            assert!(!descriptor.configurable);
            assert!(!realm.set_global(name.to_owned(), JsValue::Number(1.0)));
            assert!(!realm.delete_property(global, name));
        }
    }
}
