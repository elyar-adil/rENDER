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
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::runtime::types::ElementRect;
use crate::runtime::types::JsMicrotask;
use crate::value::MutationWatch;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_css::selector::MatchContext;
use render_dom::Dom;
use render_dom::MutationKind;
use render_dom::NodeId;

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
            NativeFunction::WindowMatchMedia => self.window_match_media(arguments),
            NativeFunction::MediaQueryListMediaGetter => {
                self.media_query_list_attribute(receiver, false)
            }
            NativeFunction::MediaQueryListMatchesGetter => {
                self.media_query_list_attribute(receiver, true)
            }
            NativeFunction::MediaQueryListAddEventListener => {
                self.media_query_list_add_event_listener(receiver, arguments)
            }
            NativeFunction::MediaQueryListRemoveEventListener => {
                self.media_query_list_remove_event_listener(receiver, arguments)
            }
            NativeFunction::MediaQueryListAddListener => {
                self.media_query_list_add_listener(receiver, arguments)
            }
            NativeFunction::MediaQueryListRemoveListener => {
                Ok(self.media_query_list_remove_listener(receiver, arguments))
            }
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
                Some(JsValue::Object(array)) => self.array_elements_for(array)?,
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
            .collect::<Result<Vec<_>, _>>()?;
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
            .collect::<Result<Vec<_>, _>>()?;
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
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(None);
        let kind = &record.kind;
        let (type_name, target, attribute_name, added, removed) = match kind {
            MutationKind::ChildList {
                target,
                added,
                removed,
            } => (
                "childList",
                *target,
                None,
                added.as_slice(),
                removed.as_slice(),
            ),
            MutationKind::Attribute { target, local_name } => (
                "attributes",
                *target,
                Some(local_name.clone()),
                &[][..],
                &[][..],
            ),
            MutationKind::CharacterData { target } => {
                ("characterData", *target, None, &[][..], &[][..])
            }
        };
        self.realm.set_property(
            object,
            "type".to_owned(),
            JsValue::String(type_name.to_owned()),
        );
        let wrapper = self.wrap_node(target)?;
        self.realm
            .set_property(object, "target".to_owned(), wrapper);
        self.realm.set_property(
            object,
            "attributeName".to_owned(),
            attribute_name.map_or(JsValue::Null, JsValue::String),
        );
        for (name, nodes) in [("addedNodes", added), ("removedNodes", removed)] {
            let values = nodes
                .iter()
                .map(|node| self.wrap_node(*node))
                .collect::<Result<Vec<_>, _>>()?;
            let list = self.create_array_from_values(&values)?;
            self.realm
                .set_property(object, name.to_owned(), JsValue::Object(list));
        }
        for name in [
            "previousSibling",
            "nextSibling",
            "attributeNamespace",
            "oldValue",
        ] {
            self.realm
                .set_property(object, name.to_owned(), JsValue::Null);
        }
        Ok(JsValue::Object(object))
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

    /// `window.matchMedia(query)` (CSSOM View §"matchMedia").
    ///
    /// The `matches` value is `render_css::cascade::media_query_list_matches`
    /// against the viewport this realm was last given, which is the *same*
    /// function the cascade uses to decide whether a `@media` rule applies. One
    /// evaluator, one answer: a query whose `@media` rule is live and a
    /// `matchMedia` built from the same text cannot disagree, and a hardcoded
    /// `false` would hand every responsive site the wrong branch on first
    /// paint.
    pub(in crate::runtime) fn window_match_media(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let media = required_argument(arguments, 0, "matchMedia")?.to_js_string();
        let matches = self.media_query_matches(&media);
        let prototype = self.realm.media_query_list_prototype();
        self.ensure_heap_capacity(1)?;
        let list = self.realm.create_object(Some(prototype));
        *self
            .realm
            .host_mut(list)
            .expect("a freshly created object has host storage") = ObjectHost::MediaQueryList {
            media,
            matches,
            listeners: Vec::new(),
        };
        // `media` and `matches` are accessors on `MediaQueryList.prototype`
        // reading the host, so the list carries no own copy of either. A
        // `readonly attribute` that is an own data property would have to be
        // rewritten on every viewport change, and a property that is only
        // correct until someone forgets to rewrite it is the failure this
        // interface is supposed to avoid.
        self.media_query_lists.push(list);
        Ok(JsValue::Object(list))
    }

    /// The `MediaQueryList.prototype` `media` and `matches` accessors.
    pub(in crate::runtime) fn media_query_list_attribute(
        &self,
        receiver: ObjectId,
        want_matches: bool,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::MediaQueryList { media, matches, .. }) = self.realm.host(receiver)
        else {
            return Err(JsError::type_error(
                "MediaQueryList attribute read on an incompatible receiver",
            ));
        };
        Ok(if want_matches {
            JsValue::Boolean(matches)
        } else {
            JsValue::String(media)
        })
    }

    /// Evaluate one media query against the current viewport, through the
    /// cascade's own oracle.
    fn media_query_matches(&self, media: &str) -> bool {
        render_css::cascade::media_query_list_matches(
            media,
            &MatchContext {
                viewport_width: Some(self.viewport.width),
                viewport_height: Some(self.viewport.height),
                ..MatchContext::default()
            },
        )
    }

    /// Re-evaluate every live list and queue a `change` dispatch for each one
    /// whose answer flipped.
    ///
    /// The diff is against the *stored* value rather than against the previous
    /// call, which is what makes it correct under repeated evaluation: calling
    /// this twice with the same viewport queues nothing the second time, and a
    /// list that flips true-then-false-then-true across three frames fires once
    /// per flip rather than once per frame.
    pub(in crate::runtime) fn queue_media_query_list_changes(&mut self) {
        if self.media_query_lists.is_empty() {
            return;
        }
        for list in std::mem::take(&mut self.media_query_lists) {
            let (media, previous) = match self.realm.host(list) {
                Some(ObjectHost::MediaQueryList { media, matches, .. }) => (media.clone(), matches),
                // Swept since the last call. Drop it rather than resurrect it.
                _ => continue,
            };
            let current = self.media_query_matches(&media);
            if let Some(ObjectHost::MediaQueryList { matches, .. }) = self.realm.host_mut(list) {
                *matches = current;
            }
            if current == previous {
                self.media_query_lists.push(list);
                continue;
            }
            if let Some(ObjectHost::MediaQueryList { matches, .. }) = self.realm.host_mut(list) {
                *matches = current;
            }
            self.media_query_lists.push(list);
            if !self
                .pending_microtasks
                .iter()
                .any(|task| matches!(task, JsMicrotask::MediaQueryListChange(id) if *id == list))
            {
                self.pending_microtasks
                    .push(JsMicrotask::MediaQueryListChange(list));
            }
        }
    }

    /// Fire one `MediaQueryList`'s `change` notification.
    ///
    /// CSSOM View fires the `change` event at the list itself, so `this` is the
    /// list and the event carries the list's new `matches` and its `media`. The
    /// three registration mechanisms are all honoured, in the order a browser
    /// uses: the `onchange` attribute handler, then the `change` listeners, then
    /// the deprecated `addListener` callbacks - the last because a `MediaQueryList`
    /// is the one interface in wide use where a library may still be using the
    /// pre-2018 API and dropping it would silently break exactly the responsive
    /// code that only works through the listener.
    pub(in crate::runtime) fn notify_media_query_list(
        &mut self,
        dom: &mut Dom,
        list: ObjectId,
    ) -> Result<JsValue, JsError> {
        let (media, matches, listeners) = match self.realm.host(list) {
            Some(ObjectHost::MediaQueryList {
                media,
                matches,
                listeners,
            }) => (media.clone(), matches, listeners.clone()),
            _ => return Ok(JsValue::Undefined),
        };
        let event = self.media_query_list_event(&media, matches)?;
        let mut outcome = Ok(JsValue::Undefined);
        let onchange = self.realm.get_property(list, "onchange");
        if let Some(handler) = onchange.and_then(|value| match value {
            JsValue::Object(handler) if Self::is_callable_object(handler, &self.realm) => {
                Some(handler)
            }
            _ => None,
        }) {
            outcome = self.call_with_this(
                dom,
                handler,
                &[JsValue::Object(event)],
                JsValue::Object(list),
            );
        }
        for listener in listeners {
            let call = self.call_with_this(
                dom,
                listener,
                &[JsValue::Object(event)],
                JsValue::Object(list),
            );
            // A listener that throws does not cancel the remaining listeners, and
            // the first failure is what the caller reports.
            if outcome.is_ok() {
                outcome = call;
            }
        }
        outcome
    }

    /// A `MediaQueryListEvent` (CSSOM View): an `Event` with `type: "change"`,
    /// the list's `media`, and its new `matches`.
    fn media_query_list_event(&mut self, media: &str, matches: bool) -> Result<ObjectId, JsError> {
        let prototype = self.realm.global("Event").and_then(|value| match value {
            JsValue::Object(constructor) => self
                .realm
                .get_property(constructor, "prototype")
                .and_then(|value| match value {
                    JsValue::Object(prototype) => Some(prototype),
                    _ => None,
                }),
            _ => None,
        });
        self.ensure_heap_capacity(1)?;
        let event = self.realm.create_object(prototype);
        for (name, value) in [
            ("type", JsValue::String("change".to_owned())),
            ("media", JsValue::String(media.to_owned())),
            ("matches", JsValue::Boolean(matches)),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        Ok(event)
    }

    /// `MediaQueryList.addEventListener(type, listener)`. Only `change` has an
    /// observable effect, so any other type is accepted and ignored rather than
    /// refused: a `MediaQueryList` is an `EventTarget`, and a target that threw
    /// on an unrelated type would break `addEventListener` in generic code.
    pub(in crate::runtime) fn media_query_list_add_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let event_type = required_argument(arguments, 0, "addEventListener")?.to_js_string();
        let listener = Self::require_callable_object(
            required_argument(arguments, 1, "addEventListener")?,
            &self.realm,
        )?;
        if event_type != "change" {
            return Ok(JsValue::Undefined);
        }
        let Some(ObjectHost::MediaQueryList { listeners, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "addEventListener called on an incompatible MediaQueryList receiver",
            ));
        };
        // DOM §"add an event listener" is a set, so a second registration of the
        // same function is not two notifications.
        if !listeners.contains(&listener) {
            listeners.push(listener);
        }
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn media_query_list_remove_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let listener = Self::require_callable_object(
            required_argument(arguments, 1, "removeEventListener")?,
            &self.realm,
        )?;
        let Some(ObjectHost::MediaQueryList { listeners, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "removeEventListener called on an incompatible MediaQueryList receiver",
            ));
        };
        listeners.retain(|candidate| *candidate != listener);
        Ok(JsValue::Undefined)
    }

    /// The deprecated `addListener`. It shares the listener list with
    /// `addEventListener("change", ...)` and returns the callback, which is what
    /// the pre-2018 contract was: a `MediaQueryList` is the one object whose
    /// listener registration returned its argument.
    pub(in crate::runtime) fn media_query_list_add_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let listener = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        if let JsValue::Object(callback) = listener
            && Self::is_callable_object(callback, &self.realm)
        {
            let Some(ObjectHost::MediaQueryList { listeners, .. }) = self.realm.host_mut(receiver)
            else {
                return Err(JsError::type_error(
                    "addListener called on an incompatible MediaQueryList receiver",
                ));
            };
            if !listeners.contains(&callback) {
                listeners.push(callback);
            }
        }
        Ok(listener)
    }

    pub(in crate::runtime) fn media_query_list_remove_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> JsValue {
        if let Some(JsValue::Object(callback)) = arguments.first()
            && let Some(ObjectHost::MediaQueryList { listeners, .. }) =
                self.realm.host_mut(receiver)
        {
            listeners.retain(|candidate| candidate != callback);
        }
        arguments.first().cloned().unwrap_or(JsValue::Undefined)
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

#[cfg(test)]
mod tests {
    use super::*;
    use render_html::parse_document;

    /// Drive the frame loop the way an embedder does: publish a viewport, then
    /// drain whatever the publish queued. This is the whole of the contract
    /// `matchMedia` needs from the embedding, and it is a contract that already
    /// exists for `IntersectionObserver`, so `MediaQueryList` notification is
    /// not a new frame-ordering question.
    fn frame(runtime: &mut JsRuntime, dom: &mut Dom, width: f32, height: f32) {
        runtime.install_viewport(width, height, 0.0, 0.0);
        for _ in 0..8 {
            let tasks = runtime.take_pending_microtasks();
            if tasks.is_empty() {
                return;
            }
            for task in tasks {
                runtime
                    .invoke_microtask(dom, task)
                    .expect("a MediaQueryList change handler should run");
            }
        }
    }

    fn run(source: &str) -> String {
        let mut parsed = parse_document("<!doctype html><body></body>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(&mut parsed.dom, source)
            .expect("script should execute")
            .value
            .to_js_string()
    }

    /// The initial value comes from the cascade's own media-query evaluator, so
    /// a query whose `@media` rule is live and the same text handed to
    /// `matchMedia` cannot disagree. The `false` cases are the load-bearing ones:
    /// a hardcoded `false` would pass them and hand every responsive site the
    /// wrong branch on first paint.
    #[test]
    fn match_media_evaluates_its_query_against_the_installed_viewport() {
        let mut parsed = parse_document("<!doctype html><body></body>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime.install_viewport(1000.0, 800.0, 0.0, 0.0);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r"
                function answer(query) {
                    var mql = matchMedia(query);
                    return mql.matches;
                }
                [
                    answer('(min-width: 900px)'),
                    answer('(min-width: 1100px)'),
                    answer('screen'),
                    answer('(min-width: 100px) and (min-width: 900px)'),
                    answer('not (min-width: 1100px)'),
                    answer('(backdrop-filter: blur(2px))')
                ].join(',')
            ",
            )
            .expect("matchMedia should execute");
        // The last one is a media *feature* rather than a media query, and
        // Media Queries 4 §2.1.1's "unknown means false" fallback is what makes
        // it answer `false` rather than throwing.
        assert_eq!(
            outcome.value.to_js_string(),
            "true,false,true,true,true,false"
        );
    }

    /// `media` echoes the query as given, which is what a script that re-uses the
    /// string to build a `<link media>` or a class name depends on.
    #[test]
    fn a_media_query_list_reports_its_query_and_its_interface() {
        assert_eq!(
            run(r"
                var mql = matchMedia('(min-width: 900px)');
                [
                    mql.media,
                    typeof mql.matches,
                    Object.prototype.toString.call(mql),
                    Object.getPrototypeOf(mql) === MediaQueryList.prototype,
                    mql instanceof MediaQueryList,
                    typeof mql.addEventListener,
                    typeof mql.removeEventListener,
                    typeof mql.addListener,
                    typeof mql.removeListener
                ].join(',')
            "),
            "(min-width: 900px),boolean,[object MediaQueryList],true,true,function,\
function,function,function"
        );
        // The interface object exists because CSSOM View declares the interface
        // `[Exposed=Window]`, but it has no constructor operation, so
        // `new MediaQueryList()` is a `TypeError` rather than a list.
        assert_eq!(
            run(
                "var threw = 'no'; try { new MediaQueryList(); } catch (e) { threw = e.name; } threw"
            ),
            "TypeError"
        );
        // `onchange` starts `null`, as CSSOM's `EventHandler` attribute does, and
        // is assignable.
        assert_eq!(run("matchMedia('screen').onchange === null"), "true");
        assert_eq!(
            run(
                "var mql = matchMedia('screen'); mql.onchange = function () {}; \
                typeof mql.onchange"
            ),
            "function"
        );
    }

    /// **The case the whole interface exists for.** A `MediaQueryList` whose
    /// `matches` is right and which never fires `change` is the failure this
    /// project treats as worst, and it is the one every responsive site's
    /// listener-shaped code depends on - the initial value is read once at
    /// startup, and everything after that is the event.
    ///
    /// The three assertions are the three halves of the contract: the listener
    /// fires, it fires with the *new* value, and it fires only on a flip. The
    /// third is what stops a resize storm from producing a notification per
    /// frame, which is the difference between a notification and a CPU cost.
    #[test]
    fn a_media_query_list_fires_change_when_the_viewport_flips_its_answer() {
        let mut parsed = parse_document("<!doctype html><body></body>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        // Subscribe under a viewport the query does not match, so the first
        // publish that matches is a real flip.
        runtime.install_viewport(400.0, 300.0, 0.0, 0.0);
        runtime
            .execute(
                &mut parsed.dom,
                r"
            window.log = [];
            window.mql = matchMedia('(min-width: 900px)');
            mql.addEventListener('change', function (event) {
                window.log.push([
                    event.type,
                    event.matches,
                    event.media,
                    this === window.mql,
                    this.matches === event.matches
                ].join('|'));
            });
        ",
            )
            .expect("the subscription should be set up");
        assert_eq!(
            runtime
                .execute(&mut parsed.dom, "mql.matches")
                .expect("matches readable")
                .value
                .to_js_string(),
            "false",
            "a 400px viewport must not satisfy a 900px min-width"
        );

        // Publishing a viewport that still does not match is not a flip.
        frame(&mut runtime, &mut parsed.dom, 500.0, 400.0);
        assert_eq!(
            runtime
                .execute(&mut parsed.dom, "log.length")
                .expect("log readable")
                .value
                .to_js_string(),
            "0"
        );

        // Crossing the threshold is a flip, and the event carries the new value.
        frame(&mut runtime, &mut parsed.dom, 1000.0, 800.0);
        // A second identical frame must be silent: the diff is against the last
        // *evaluation*, not against the previous call.
        frame(&mut runtime, &mut parsed.dom, 1000.0, 800.0);
        let log = runtime
            .execute(&mut parsed.dom, "log.join(';')")
            .expect("log readable")
            .value
            .to_js_string();
        assert_eq!(
            log, "change|true|(min-width: 900px)|true|true",
            "the event must carry the new matches, this must be the list, and \
             reading `matches` off the list during the handler must agree"
        );

        // And back down again, which is the second flip and not a duplicate.
        frame(&mut runtime, &mut parsed.dom, 800.0, 600.0);
        frame(&mut runtime, &mut parsed.dom, 500.0, 400.0);
        let log = runtime
            .execute(&mut parsed.dom, "log.join(';')")
            .expect("log readable")
            .value
            .to_js_string();
        assert_eq!(
            log,
            "change|true|(min-width: 900px)|true|true;\
             change|false|(min-width: 900px)|true|true"
        );
    }

    /// `matches` is readable *before* the notification runs, because the value is
    /// applied when the viewport is published and the event is queued after it.
    /// A caller that reads `mql.matches` in the same turn as the resize - which
    /// is what code that also re-renders does - must not see the old answer.
    #[test]
    fn matches_is_updated_when_the_viewport_is_published_not_when_the_event_runs() {
        let mut parsed = parse_document("<!doctype html><body></body>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime.install_viewport(1000.0, 800.0, 0.0, 0.0);
        runtime
            .execute(
                &mut parsed.dom,
                "var mql = matchMedia('(min-width: 900px)');",
            )
            .expect("matchMedia should execute");
        assert_eq!(
            runtime
                .execute(&mut parsed.dom, "mql.matches")
                .expect("matches readable")
                .value
                .to_js_string(),
            "true"
        );
        // Publish without draining: the notification is queued, not run, and the
        // value is already the new one.
        runtime.install_viewport(500.0, 400.0, 0.0, 0.0);
        assert_eq!(
            runtime
                .execute(&mut parsed.dom, "mql.matches")
                .expect("matches readable")
                .value
                .to_js_string(),
            "false"
        );
    }

    /// All three registration mechanisms, because a `MediaQueryList` is the one
    /// interface where a library may still be on the pre-2018 API and dropping
    /// `addListener` would break exactly the code that only works through a
    /// listener.
    #[test]
    fn onchange_listeners_and_the_deprecated_add_listener_all_fire() {
        let mut parsed = parse_document("<!doctype html><body></body>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime.install_viewport(1000.0, 800.0, 0.0, 0.0);
        runtime
            .execute(
                &mut parsed.dom,
                r"
                window.order = [];
                var mql = matchMedia('(min-width: 900px)');
                mql.onchange = function () { order.push('onchange'); };
                mql.addListener(function () { order.push('addListener'); });
                mql.addEventListener('change', function () { order.push('listener'); });
            ",
            )
            .expect("the three registrations should be accepted");
        frame(&mut runtime, &mut parsed.dom, 500.0, 400.0);
        assert_eq!(
            runtime
                .execute(&mut parsed.dom, "order.join(',')")
                .expect("order readable")
                .value
                .to_js_string(),
            "onchange,addListener,listener"
        );

        // `addListener` is a set, so registering the same function twice fires it
        // once, and `removeListener` takes it back out.
        let mut parsed = parse_document("<!doctype html><body></body>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime.install_viewport(1000.0, 800.0, 0.0, 0.0);
        runtime
            .execute(
                &mut parsed.dom,
                r"
                window.hits = 0;
                var mql = matchMedia('(min-width: 900px)');
                function listener() { hits += 1; }
                mql.addListener(listener);
                mql.addListener(listener);
                mql.removeListener(listener);
            ",
            )
            .expect("the registrations should be accepted");
        frame(&mut runtime, &mut parsed.dom, 500.0, 400.0);
        assert_eq!(
            runtime
                .execute(&mut parsed.dom, "hits")
                .expect("hits readable")
                .value
                .to_js_string(),
            "0"
        );
    }

    /// An `EventTarget` that threw on an unrelated type would break
    /// `addEventListener` in generic code, so a non-`change` type is accepted
    /// and ignored.
    #[test]
    fn a_non_change_event_type_is_accepted_and_ignored() {
        let mut parsed = parse_document("<!doctype html><body></body>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime.install_viewport(1000.0, 800.0, 0.0, 0.0);
        runtime
            .execute(
                &mut parsed.dom,
                r"
                window.other = 0;
                var mql = matchMedia('(min-width: 900px)');
                mql.addEventListener('resize', function () { other += 1; });
                mql.addEventListener('change', function () { other += 10; });
            ",
            )
            .expect("the registrations should be accepted");
        frame(&mut runtime, &mut parsed.dom, 500.0, 400.0);
        assert_eq!(
            runtime
                .execute(&mut parsed.dom, "other")
                .expect("other readable")
                .value
                .to_js_string(),
            "10"
        );
    }

    /// A query that cannot be answered - a feature Media Queries 4 §2.1.1 has no
    /// evaluation for - must not fire on every frame. It is a constant `false`,
    /// and a constant answer produces no events.
    #[test]
    fn a_query_the_engine_cannot_answer_is_a_constant_false_and_fires_nothing() {
        let mut parsed = parse_document("<!doctype html><body></body>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime.install_viewport(1000.0, 800.0, 0.0, 0.0);
        runtime
            .execute(
                &mut parsed.dom,
                r"
                window.fired = 0;
                var mql = matchMedia('(backdrop-filter: blur(2px))');
                mql.addEventListener('change', function () { fired += 1; });
            ",
            )
            .expect("the subscription should be accepted");
        for width in [1000.0_f32, 500.0, 1200.0, 320.0] {
            frame(&mut runtime, &mut parsed.dom, width, 800.0);
        }
        assert_eq!(
            runtime
                .execute(&mut parsed.dom, "fired + ':' + mql.matches")
                .expect("fired readable")
                .value
                .to_js_string(),
            "0:false"
        );
    }

    #[test]
    fn child_list_records_expose_added_nodes_for_modulepreload_observers() {
        let mut parsed = parse_document("<!doctype html><body></body>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(
                &mut parsed.dom,
                r"
            var seen = [];
            var observer = new MutationObserver(function (records) {
                for (var record of records) {
                    if (record.type === 'childList') {
                        for (var node of record.addedNodes) seen.push(node.tagName);
                    }
                }
            });
            observer.observe(document.body, {childList:true, subtree:true});
            document.body.appendChild(document.createElement('link'));
        ",
            )
            .expect("observer setup should execute");
        for _ in 0..16 {
            let tasks = runtime.take_pending_microtasks();
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                runtime
                    .invoke_microtask(&mut parsed.dom, task)
                    .expect("observer callback should iterate addedNodes");
            }
        }
        let result = runtime
            .execute(&mut parsed.dom, "seen.join(',')")
            .expect("observer result should be readable");
        assert_eq!(result.value, JsValue::String("LINK".to_owned()));
    }
}
