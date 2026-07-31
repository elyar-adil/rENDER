//! JavaScript values and the realm-owned object arena.

use std::collections::BTreeMap;
use std::num::FpCategory;

use crate::dom::NodeId;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeFunction {
    GetElementById,
    CreateElement,
    SetAttribute,
    AppendChild,
    QueueMicrotask,
    PromiseResolve,
    PromiseReject,
    PromiseThen,
    PromiseCatch,
    ArrayIsArray,
    ArrayPush,
    ArrayPop,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ObjectHost {
    #[default]
    Ordinary,
    Array,
    Document(NodeId),
    Node(NodeId),
    NativeFunction(NativeFunction),
    BoundFunction {
        function: NativeFunction,
        receiver: ObjectId,
    },
    UserFunction(usize),
    ArrowFunction(usize),
    PromiseConstructor,
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
    array_prototype: ObjectId,
    node_wrappers: BTreeMap<NodeId, ObjectId>,
}

impl Realm {
    pub(crate) fn bootstrap(document_node: NodeId) -> Self {
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
        Self::install_promise(&mut objects, global);
        let array_prototype = Self::install_array(&mut objects, global);
        Self {
            objects,
            global,
            document,
            array_prototype,
            node_wrappers: BTreeMap::new(),
        }
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
                PropertyDescriptor::data(JsValue::Object(method)),
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

    fn install_array(objects: &mut Vec<JsObject>, global: ObjectId) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject::default());
        for (name, function) in [
            ("push", NativeFunction::ArrayPush),
            ("pop", NativeFunction::ArrayPop),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::data(JsValue::Object(method)),
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
            PropertyDescriptor::data(JsValue::Object(is_array)),
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
        self.objects.get(object.0).map(|object| object.host)
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
            host: ObjectHost::Node(node),
            ..JsObject::default()
        });
        self.node_wrappers.insert(node, wrapper);
        wrapper
    }

    pub(crate) fn bound_function(
        &mut self,
        function: NativeFunction,
        receiver: ObjectId,
    ) -> ObjectId {
        self.allocate(JsObject {
            host: ObjectHost::BoundFunction { function, receiver },
            ..JsObject::default()
        })
    }

    pub(crate) fn arrow_function(&mut self, function: usize) -> ObjectId {
        self.allocate(JsObject {
            host: ObjectHost::ArrowFunction(function),
            ..JsObject::default()
        })
    }

    pub(crate) fn user_function(&mut self, function: usize) -> ObjectId {
        let prototype = self.create_object(None);
        let callable = self.allocate(JsObject {
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
            host: ObjectHost::Promise(promise),
            ..JsObject::default()
        })
    }

    pub(crate) fn promise_settler(&mut self, promise: usize, fulfilled: bool) -> ObjectId {
        self.allocate(JsObject {
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

    #[test]
    fn ordinary_properties_follow_the_prototype_chain() {
        let dom = Dom::new();
        let mut realm = Realm::bootstrap(dom.document());
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
}
