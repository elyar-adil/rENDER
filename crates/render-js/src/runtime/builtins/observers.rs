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

use render_dom::Dom;
use render_dom::MutationKind;
use render_dom::NodeId;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::runtime::types::ElementRect;
use crate::runtime::types::JsMicrotask;
use crate::value::MutationWatch;
use crate::value::NativeFunction;
use crate::value::ObjectHost;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_observers_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::IntersectionObserve => self.intersection_observe(receiver, arguments),
            NativeFunction::IntersectionUnobserve => {
                self.intersection_unobserve(receiver, arguments)
            }
            NativeFunction::IntersectionDisconnect => self.intersection_disconnect(receiver),
            NativeFunction::IntersectionTakeRecords => self.intersection_take_records(receiver),
            NativeFunction::MutationObserve => self.mutation_observe(receiver, arguments),
            NativeFunction::MutationDisconnect => self.mutation_disconnect(receiver),
            NativeFunction::MutationTakeRecords => self.mutation_take_records(receiver),
            other => self.dispatch_residual_native(dom, other, receiver, arguments),
        }
    }
}

impl JsRuntime {
    pub(in crate::runtime) fn intersection_observer_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "IntersectionObserver")?,
            &self.realm,
        )?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(2)?;
        let observer = self.realm.create_object(prototype);
        *self
            .realm
            .host_mut(observer)
            .expect("newly created observer has host storage") = ObjectHost::IntersectionObserver {
            callback,
            targets: Vec::new(),
        };
        let options = arguments.get(1).and_then(|value| match value {
            JsValue::Object(object) => Some(*object),
            _ => None,
        });
        let root = options
            .and_then(|object| self.realm.get_property(object, "root"))
            .unwrap_or(JsValue::Null);
        let root_margin = options
            .and_then(|object| self.realm.get_property(object, "rootMargin"))
            .map_or_else(
                || "0px 0px 0px 0px".to_owned(),
                |value| value.to_js_string(),
            );
        let thresholds =
            match options.and_then(|object| self.realm.get_property(object, "threshold")) {
                Some(JsValue::Object(array)) => self.array_elements_for(array),
                Some(value) if !matches!(value, JsValue::Undefined) => vec![value],
                _ => vec![JsValue::Number(0.0)],
            };
        let thresholds = self.create_array_from_values(&thresholds)?;
        for (name, value) in [
            ("root", root),
            ("rootMargin", JsValue::String(root_margin)),
            ("thresholds", JsValue::Object(thresholds)),
        ] {
            self.realm.set_property(observer, name.to_owned(), value);
        }
        self.intersection_observers.push(observer);
        Ok(JsValue::Object(observer))
    }

    pub(in crate::runtime) fn intersection_observe(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.value_as_node(required_argument(arguments, 0, "observe")?)?;
        let Some(ObjectHost::IntersectionObserver { targets, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible IntersectionObserver receiver",
            ));
        };
        if !targets.contains(&target) {
            targets.push(target);
            self.queue_intersection_observer(receiver);
        }
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn intersection_unobserve(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.value_as_node(required_argument(arguments, 0, "unobserve")?)?;
        let Some(ObjectHost::IntersectionObserver { targets, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible IntersectionObserver receiver",
            ));
        };
        targets.retain(|candidate| *candidate != target);
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn intersection_disconnect(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::IntersectionObserver { targets, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible IntersectionObserver receiver",
            ));
        };
        targets.clear();
        self.pending_microtasks.retain(
            |task| !matches!(task, JsMicrotask::IntersectionObserver(observer) if *observer == receiver),
        );
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn intersection_take_records(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        if !matches!(
            self.realm.host(receiver),
            Some(ObjectHost::IntersectionObserver { .. })
        ) {
            return Err(JsError::type_error(
                "incompatible IntersectionObserver receiver",
            ));
        }
        Ok(JsValue::Object(self.create_array_from_values(&[])?))
    }

    pub(in crate::runtime) fn mutation_observer_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "MutationObserver")?,
            &self.realm,
        )?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(2)?;
        let observer = self.realm.create_object(prototype);
        *self
            .realm
            .host_mut(observer)
            .expect("newly created observer has host storage") = ObjectHost::MutationObserver {
            callback,
            targets: Vec::new(),
            queued: Vec::new(),
        };
        self.mutation_observers.push(observer);
        Ok(JsValue::Object(observer))
    }

    pub(in crate::runtime) fn mutation_observe(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.value_as_node(required_argument(arguments, 0, "observe")?)?;
        let options = arguments.get(1).and_then(|value| match value {
            JsValue::Object(object) => Some(*object),
            _ => None,
        });
        let enabled = |name: &str| {
            options
                .and_then(|object| self.realm.get_property(object, name))
                .is_some_and(|value| matches!(value, JsValue::Boolean(true)))
        };
        let watch = MutationWatch {
            target,
            subtree: enabled("subtree"),
            child_list: enabled("childList"),
            attributes: enabled("attributes"),
            character_data: enabled("characterData"),
        };
        let Some(ObjectHost::MutationObserver { targets, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible MutationObserver receiver",
            ));
        };
        if !targets.contains(&watch) {
            targets.push(watch);
        }
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn mutation_disconnect(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::MutationObserver {
            targets, queued, ..
        }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible MutationObserver receiver",
            ));
        };
        targets.clear();
        queued.clear();
        self.pending_microtasks.retain(
            |task| !matches!(task, JsMicrotask::MutationObserver(observer) if *observer == receiver),
        );
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn mutation_take_records(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::MutationObserver { queued, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible MutationObserver receiver",
            ));
        };
        let drained = std::mem::take(queued);
        let values = drained
            .iter()
            .map(|record| self.mutation_record_value(record))
            .collect::<Vec<_>>();
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    /// Copy journal records newer than the last seen revision into every
    /// registered observer's queue, then schedule one delivery microtask per
    /// observer with pending records.
    pub(in crate::runtime) fn queue_mutation_deliveries(&mut self, dom: &mut Dom) {
        if self.mutation_observers.is_empty() {
            self.mutation_seen_revision = dom.revision();
            return;
        }
        let Ok(batch) = dom.mutations_since(self.mutation_seen_revision) else {
            return;
        };
        self.mutation_seen_revision = batch.to_revision;
        if batch.records.is_empty() {
            return;
        }
        for observer in std::mem::take(&mut self.mutation_observers) {
            let watches = match self.realm.host(observer) {
                Some(ObjectHost::MutationObserver { targets, .. }) => targets.clone(),
                _ => continue,
            };
            if watches.is_empty() {
                self.mutation_observers.push(observer);
                continue;
            }
            let mut delivered = Vec::new();
            for watch in &watches {
                for record in &batch.records {
                    let target = record.kind.target();
                    let relevant = target == watch.target
                        || (watch.subtree && Self::has_ancestor(dom, target, watch.target));
                    let matches = match &record.kind {
                        MutationKind::ChildList { .. } => watch.child_list,
                        MutationKind::Attribute { .. } => watch.attributes,
                        MutationKind::CharacterData { .. } => watch.character_data,
                    };
                    if relevant && matches {
                        delivered.push(record.clone());
                    }
                }
            }
            self.mutation_observers.push(observer);
            if delivered.is_empty() {
                continue;
            }
            if let Some(ObjectHost::MutationObserver { queued, .. }) = self.realm.host_mut(observer)
            {
                queued.extend(delivered);
            }
            if !self
                .pending_microtasks
                .iter()
                .any(|task| matches!(task, JsMicrotask::MutationObserver(id) if *id == observer))
            {
                self.pending_microtasks
                    .push(JsMicrotask::MutationObserver(observer));
            }
        }
    }

    pub(in crate::runtime) fn notify_mutation_observer(
        &mut self,
        dom: &mut Dom,
        observer: ObjectId,
    ) -> Result<JsValue, JsError> {
        let callback = match self.realm.host(observer) {
            Some(ObjectHost::MutationObserver { callback, .. }) => callback,
            _ => return Ok(JsValue::Undefined),
        };
        let drained = match self.realm.host_mut(observer) {
            Some(ObjectHost::MutationObserver { queued, .. }) => std::mem::take(queued),
            _ => Vec::new(),
        };
        if drained.is_empty() {
            return Ok(JsValue::Undefined);
        }
        let records = drained
            .iter()
            .map(|record| self.mutation_record_value(record))
            .collect::<Vec<_>>();
        let records = self.create_array_from_values(&records)?;
        self.call_with_this(
            dom,
            callback,
            &[JsValue::Object(records), JsValue::Object(observer)],
            JsValue::Object(observer),
        )
    }

    pub(in crate::runtime) fn mutation_record_value(
        &mut self,
        record: &render_dom::MutationRecord,
    ) -> JsValue {
        let object = self.realm.create_object(None);
        let kind = &record.kind;
        let (type_name, target, attribute_name) = match kind {
            MutationKind::ChildList { target, .. } => ("childList", *target, None),
            MutationKind::Attribute { target, local_name } => {
                ("attributes", *target, Some(local_name.clone()))
            }
            MutationKind::CharacterData { target } => ("characterData", *target, None),
        };
        self.realm.set_property(
            object,
            "type".to_owned(),
            JsValue::String(type_name.to_owned()),
        );
        let wrapper = self.realm.node_wrapper(target);
        self.realm
            .set_property(object, "target".to_owned(), JsValue::Object(wrapper));
        self.realm.set_property(
            object,
            "attributeName".to_owned(),
            attribute_name.map_or(JsValue::Null, JsValue::String),
        );
        JsValue::Object(object)
    }

    pub(in crate::runtime) fn has_ancestor(dom: &Dom, node: NodeId, ancestor: NodeId) -> bool {
        let mut current = Some(node);
        while let Some(candidate) = current {
            if candidate == ancestor {
                return true;
            }
            current = dom.parent(candidate);
        }
        false
    }

    pub(in crate::runtime) fn queue_intersection_observer(&mut self, observer: ObjectId) {
        if !self
            .pending_microtasks
            .iter()
            .any(|task| matches!(task, JsMicrotask::IntersectionObserver(id) if *id == observer))
        {
            self.pending_microtasks
                .push(JsMicrotask::IntersectionObserver(observer));
        }
    }

    pub(in crate::runtime) fn queue_intersection_observers(&mut self) {
        for observer in self.intersection_observers.clone() {
            if matches!(
                self.realm.host(observer),
                Some(ObjectHost::IntersectionObserver { targets, .. }) if !targets.is_empty()
            ) {
                self.queue_intersection_observer(observer);
            }
        }
    }

    pub(in crate::runtime) fn notify_intersection_observer(
        &mut self,
        dom: &mut Dom,
        observer: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::IntersectionObserver { callback, targets }) =
            self.realm.host(observer)
        else {
            return Ok(JsValue::Undefined);
        };
        let mut entries = Vec::with_capacity(targets.len());
        for target in targets {
            if dom.node(target).is_none() {
                continue;
            }
            entries.push(self.intersection_entry(target)?);
        }
        if entries.is_empty() {
            return Ok(JsValue::Undefined);
        }
        let entries = self.create_array_from_values(&entries)?;
        self.call_with_this(
            dom,
            callback,
            &[JsValue::Object(entries), JsValue::Object(observer)],
            JsValue::Object(observer),
        )
    }

    pub(in crate::runtime) fn intersection_entry(
        &mut self,
        target: NodeId,
    ) -> Result<JsValue, JsError> {
        let document_rect = self
            .element_geometry
            .get(&target.as_u64())
            .copied()
            .unwrap_or(ElementRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            });
        let left = document_rect.x.max(self.viewport.x);
        let top = document_rect.y.max(self.viewport.y);
        let right =
            (document_rect.x + document_rect.width).min(self.viewport.x + self.viewport.width);
        let bottom =
            (document_rect.y + document_rect.height).min(self.viewport.y + self.viewport.height);
        let intersection = ElementRect {
            x: left - self.viewport.x,
            y: top - self.viewport.y,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        };
        let area = document_rect.width.max(0.0) * document_rect.height.max(0.0);
        let intersection_area = intersection.width * intersection.height;
        let is_intersecting = intersection.width > 0.0 && intersection.height > 0.0;
        let ratio = if area > 0.0 {
            intersection_area / area
        } else {
            0.0
        };
        let constructor = self
            .realm
            .global("IntersectionObserverEntry")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let prototype = constructor
            .and_then(|object| self.realm.get_property(object, "prototype"))
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(5)?;
        let entry = self.realm.create_object(prototype);
        let mut viewport_rect = document_rect;
        viewport_rect.x -= self.viewport.x;
        viewport_rect.y -= self.viewport.y;
        let root_bounds = ElementRect {
            x: 0.0,
            y: 0.0,
            width: self.viewport.width,
            height: self.viewport.height,
        };
        let bounding = self.rect_value(viewport_rect);
        let intersection_rect = self.rect_value(intersection);
        let root = self.rect_value(root_bounds);
        let target = self.wrap_node(target)?;
        for (name, value) in [
            ("time", JsValue::Number(Self::monotonic_now_ms())),
            ("target", target),
            ("rootBounds", root),
            ("boundingClientRect", bounding),
            ("intersectionRect", intersection_rect),
            ("isIntersecting", JsValue::Boolean(is_intersecting)),
            ("intersectionRatio", JsValue::Number(f64::from(ratio))),
        ] {
            self.realm.set_property(entry, name.to_owned(), value);
        }
        Ok(JsValue::Object(entry))
    }
}
