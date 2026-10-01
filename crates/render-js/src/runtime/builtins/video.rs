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

//! `HTMLVideoElement` surface: the global `Video` constructor,
//! `play`/`pause`/`load`/`canPlayType`, and the plain playback properties
//! (`src`, `width`/`height`, `currentTime`, `duration`,
//! `videoWidth`/`videoHeight`, `readyState`, `paused`, `ended`).
//!
//! The runtime never performs I/O. A `play()`/`load()` on an unloaded
//! element queues one media transfer in the shared network pending queue;
//! the embedding drains it through `JsRuntime::take_pending_fetch_requests`
//! like any `fetch()` and completes it through
//! [`JsRuntime::settle_video_fetch`], which demuxes the bytes, publishes
//! `duration`/`videoWidth`/`videoHeight`/`readyState`, resolves pending
//! `play()` promises, and queues the `loadedmetadata`/`error` callbacks of
//! the `on*` properties at the next microtask checkpoint.
//!
//! Phase 2 adds the presentation clock: the embedding drives
//! [`JsRuntime::advance_video_playback`] with the page's virtual clock on
//! every turn, which advances `currentTime`, pulls decoded frames up to the
//! clock position, fires the throttled `timeupdate` plus the
//! `play`/`pause`/`ended`/`error` callbacks, and returns the frame
//! publications the paint phase keys to the DOM `<video>` element whose
//! resolved `src` matches the element's media (the explicit node binding
//! a full `HTMLVideoElement` wrapper integration will replace).
//!
//! Audio is deliberately not implemented: this runtime has no audio device.
//!
//! `addEventListener` on video elements is still not wired (use the
//! `on<event>` properties). `playbackRate` stays fixed at 1.0; the property
//! slot exists for a later phase.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::convert::to_number;
use crate::runtime::types::FetchOutcome;
use crate::runtime::types::JsMicrotask;
use crate::runtime::types::PendingFetch;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::VideoElementState;
use crate::value::VideoMedia;
use crate::value::VideoPlayPromise;
use crate::video::PlaybackSignal;
use crate::video::VideoPipeline;
use crate::video::present::FramePublication;
use render_dom::Dom;
use render_dom::Namespace;
use render_dom::NodeId;
use render_dom::NodeKind;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_video_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::VideoPlay => self.video_play(receiver),
            NativeFunction::VideoPause => self.video_pause(receiver),
            NativeFunction::VideoLoad => self.video_load(receiver),
            NativeFunction::VideoCanPlayType => self.video_can_play_type(receiver, arguments),
            other => self.dispatch_events_native(dom, other, receiver, arguments),
        }
    }

    /// `new Video(width, height)`: an instance in the `HAVE_NOTHING` state,
    /// mirroring the `Image` constructor's optional size arguments.
    pub(in crate::runtime) fn video_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let prototype = match self.realm.get_property(constructor, "prototype") {
            Some(JsValue::Object(prototype)) => prototype,
            _ => {
                return Err(JsError::type_error(
                    "Video constructor is missing its prototype object",
                ));
            }
        };
        self.ensure_heap_capacity(1)?;
        let instance = self.realm.create_object(Some(prototype));
        *self
            .realm
            .host_mut(instance)
            .expect("newly created Video has host storage") =
            ObjectHost::VideoElement(VideoElementState::default());
        for (name, value) in [
            ("src", JsValue::String(String::new())),
            ("videoWidth", JsValue::Number(0.0)),
            ("videoHeight", JsValue::Number(0.0)),
            ("currentTime", JsValue::Number(0.0)),
            ("duration", JsValue::Number(f64::NAN)),
            ("readyState", JsValue::Number(0.0)),
            ("paused", JsValue::Boolean(true)),
            ("ended", JsValue::Boolean(false)),
            ("autoplay", JsValue::Boolean(false)),
            ("loop", JsValue::Boolean(false)),
            ("muted", JsValue::Boolean(false)),
            ("controls", JsValue::Boolean(false)),
            ("volume", JsValue::Number(1.0)),
            ("playbackRate", JsValue::Number(1.0)),
            ("error", JsValue::Null),
            ("onloadedmetadata", JsValue::Null),
            ("onerror", JsValue::Null),
            ("ontimeupdate", JsValue::Null),
            ("onplay", JsValue::Null),
            ("onpause", JsValue::Null),
            ("onended", JsValue::Null),
        ] {
            self.realm.set_property(instance, name.to_owned(), value);
        }
        for (index, name) in [(0_usize, "width"), (1_usize, "height")] {
            if let Some(value) = arguments.get(index)
                && !matches!(value, JsValue::Undefined)
            {
                let number = to_number(value)?.max(0.0).floor();
                self.realm
                    .set_property(instance, name.to_owned(), JsValue::Number(number));
            } else {
                self.realm
                    .set_property(instance, name.to_owned(), JsValue::Number(0.0));
            }
        }
        Ok(JsValue::Object(instance))
    }

    /// `video.play()`: starts playback when media is ready, otherwise queues
    /// the media transfer and returns a promise resolved by
    /// [`Self::settle_video_fetch`].
    fn video_play(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        if !matches!(self.realm.host(receiver), Some(ObjectHost::VideoElement(_))) {
            return Err(JsError::type_error(
                "incompatible Video method receiver (entry point VideoPlay)",
            ));
        }
        let (promise, value) = self.create_promise()?;
        if self.number_property(receiver, "readyState") >= 1.0 {
            // HAVE_METADATA or better: playback starts immediately.
            // Restarting after the end rewinds to the start (the play()
            // restart rule), including the script-visible position so the
            // next advance does not read the stale value as a seek.
            if let Some(pipeline) = self.video_pipeline_handle(receiver) {
                let mut pipeline = pipeline.borrow_mut();
                if pipeline.playback_ended() {
                    pipeline.seek_to(0.0);
                    self.realm.set_property(
                        receiver,
                        "currentTime".to_owned(),
                        JsValue::Number(0.0),
                    );
                    self.realm
                        .set_property(receiver, "ended".to_owned(), JsValue::Boolean(false));
                }
                pipeline.start_playback();
            }
            self.realm
                .set_property(receiver, "paused".to_owned(), JsValue::Boolean(false));
            self.queue_video_event(receiver, "play");
            self.resolve_promise(promise, &JsValue::Undefined);
            return Ok(value);
        }
        let src = self.string_property(receiver, "src");
        if src.is_empty() {
            let reason = self.media_rejection(
                DomExceptionName::NotSupported,
                "HTMLVideoElement has no src set",
            );
            self.reject_promise(promise, &reason);
            return Ok(value);
        }
        let already_loading = self
            .with_video_state(receiver, |state| state.load_id.is_some())
            .unwrap_or(false);
        if already_loading {
            // A load is already queued; this play() joins its waiters.
            if let JsValue::Object(object) = value
                && let Some(ObjectHost::VideoElement(state)) = self.realm.host_mut(receiver)
            {
                state.pending_play_promises.push(VideoPlayPromise {
                    record: promise,
                    object,
                });
            }
            self.realm
                .set_property(receiver, "paused".to_owned(), JsValue::Boolean(false));
            return Ok(value);
        }
        let resolved = match self.resolve_fetch_url(&src) {
            Ok(resolved) => resolved,
            Err(message) => {
                let reason = self.media_rejection(
                    DomExceptionName::NotSupported,
                    &format!("HTMLVideoElement src is invalid: {message}"),
                );
                self.reject_promise(promise, &reason);
                return Ok(value);
            }
        };
        self.queue_media_load(receiver, resolved);
        self.with_video_state_mut(receiver, |state| {
            if let JsValue::Object(object) = value {
                state.pending_play_promises.push(VideoPlayPromise {
                    record: promise,
                    object,
                });
            }
        });
        self.realm
            .set_property(receiver, "paused".to_owned(), JsValue::Boolean(false));
        Ok(value)
    }

    /// `video.pause()`: freezes the presentation clock at the last published
    /// position and queues the `pause` callback on the paused->transition.
    fn video_pause(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        if !matches!(self.realm.host(receiver), Some(ObjectHost::VideoElement(_))) {
            return Err(JsError::type_error(
                "incompatible Video method receiver (entry point VideoPause)",
            ));
        }
        let was_playing = self
            .video_pipeline_handle(receiver)
            .is_some_and(|pipeline| pipeline.borrow().is_playing());
        if let Some(pipeline) = self.video_pipeline_handle(receiver) {
            pipeline.borrow_mut().pause_playback();
        }
        self.realm
            .set_property(receiver, "paused".to_owned(), JsValue::Boolean(true));
        if was_playing {
            self.queue_video_event(receiver, "pause");
        }
        let (promise, value) = self.create_promise()?;
        self.resolve_promise(promise, &JsValue::Undefined);
        Ok(value)
    }

    /// `video.load()`: drops loaded media and pending play waiters, then
    /// re-queues a media transfer when a source is set.
    fn video_load(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        if !matches!(self.realm.host(receiver), Some(ObjectHost::VideoElement(_))) {
            return Err(JsError::type_error(
                "incompatible Video method receiver (entry point VideoLoad)",
            ));
        }
        let pending = self
            .with_video_state_mut(receiver, |state| {
                state.resolved_src = None;
                state.load_id = None;
                state.generation += 1;
                state.media = None;
                std::mem::take(&mut state.pending_play_promises)
            })
            .unwrap_or_default();
        for play_promise in pending {
            let reason = self.media_rejection(
                DomExceptionName::Abort,
                "HTMLVideoElement.load() aborted playback",
            );
            self.reject_promise(play_promise.record, &reason);
        }
        for (name, value) in [
            ("readyState", JsValue::Number(0.0)),
            ("duration", JsValue::Number(f64::NAN)),
            ("videoWidth", JsValue::Number(0.0)),
            ("videoHeight", JsValue::Number(0.0)),
            ("currentTime", JsValue::Number(0.0)),
            ("ended", JsValue::Boolean(false)),
            ("error", JsValue::Null),
        ] {
            self.realm.set_property(receiver, name.to_owned(), value);
        }
        let src = self.string_property(receiver, "src");
        if !src.is_empty()
            && let Ok(resolved) = self.resolve_fetch_url(&src)
        {
            self.queue_media_load(receiver, resolved);
        }
        Ok(JsValue::Undefined)
    }

    /// `video.canPlayType(type)`: `"maybe"` for MP4 (the container this
    /// build demuxes), `""` otherwise. Codec-specific `"probably"` answers
    /// wait for the pixel-decode backend.
    fn video_can_play_type(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if !matches!(self.realm.host(receiver), Some(ObjectHost::VideoElement(_))) {
            return Err(JsError::type_error(
                "incompatible Video method receiver (entry point VideoCanPlayType)",
            ));
        }
        let requested = arguments
            .first()
            .map(JsValue::to_js_string)
            .unwrap_or_default();
        let requested = requested.trim().to_ascii_lowercase();
        let answer = if requested.is_empty() {
            ""
        } else {
            let mut parts = requested.split(';');
            let mime = parts.next().unwrap_or("").trim();
            let codecs = parts
                .next()
                .and_then(|parameter| parameter.split_once('='))
                .filter(|(name, _)| name.trim() == "codecs")
                .map(|(_, value)| value.trim().trim_matches('"').to_ascii_lowercase());
            match (mime, codecs) {
                ("video/mp4", None) => "maybe",
                ("video/mp4", Some(codecs)) if supports_avc(codecs.as_str()) => "maybe",
                _ => "",
            }
        };
        Ok(JsValue::String(answer.to_owned()))
    }

    /// Queue one media transfer for `receiver` and record it as the
    /// element's pending load.
    fn queue_media_load(&mut self, receiver: ObjectId, resolved: url::Url) {
        let id = self.next_fetch_id;
        self.next_fetch_id += 1;
        self.pending_fetch_requests.push(PendingFetch {
            id,
            url: resolved.to_string(),
            method: "GET".to_owned(),
            headers: Vec::new(),
            body: None,
        });
        // Keeping the element as the transfer target roots it (and, through
        // its state, the pending play promises) until the load settles.
        self.pending_fetch_targets.insert(id, receiver);
        self.with_video_state_mut(receiver, |state| {
            state.resolved_src = Some(resolved);
            state.load_id = Some(id);
            state.generation += 1;
            state.media = None;
        });
    }

    /// Complete one video media transfer previously drained from
    /// [`Self::take_pending_fetch_requests`].
    ///
    /// A 2xx response is demuxed on the spot: track metadata becomes
    /// `duration`/`videoWidth`/`videoHeight`, `readyState` reaches
    /// `HAVE_METADATA`, pending `play()` promises fulfill, and the
    /// `onloadedmetadata` callback runs at the next microtask checkpoint.
    /// Transport failures and demux failures reject the pending `play()`
    /// promises and queue `onerror`. Unknown ids (page navigated between
    /// queueing and settling, or a stale generation) are ignored silently.
    pub fn settle_video_fetch(
        &mut self,
        dom: &mut Dom,
        id: u64,
        outcome: Result<FetchOutcome, String>,
    ) {
        // The DOM handle is reserved for future direct event dispatch;
        // settlement only enqueues microtasks today.
        let _ = dom;
        self.pending_fetch_promises.remove(&id);
        self.pending_fetch_targets.remove(&id);
        let Some(element) = self.find_video_element_by_load_id(id) else {
            return;
        };
        match outcome {
            Ok(outcome) if (200..300).contains(&outcome.status) => {
                #[cfg(test)]
                let opened = take_test_video_decoder().map_or_else(
                    || VideoPipeline::open(&outcome.body),
                    |decoder| VideoPipeline::with_decoder(&outcome.body, decoder),
                );
                #[cfg(not(test))]
                let opened = VideoPipeline::open(&outcome.body);
                match opened {
                    Ok(pipeline) => {
                        let (duration, width, height) = {
                            let track = pipeline.track();
                            (
                                track.info.duration_seconds,
                                f64::from(track.info.width),
                                f64::from(track.info.height),
                            )
                        };
                        let media = VideoMedia::new(pipeline);
                        let applied = self
                            .with_video_state_mut(element, |state| {
                                if state.load_id != Some(id) {
                                    return false;
                                }
                                state.load_id = None;
                                state.media = Some(media);
                                true
                            })
                            .unwrap_or(false);
                        if !applied {
                            return;
                        }
                        for (name, value) in [
                            ("readyState", JsValue::Number(1.0)),
                            ("duration", JsValue::Number(duration.unwrap_or(f64::NAN))),
                            ("videoWidth", JsValue::Number(width)),
                            ("videoHeight", JsValue::Number(height)),
                            ("currentTime", JsValue::Number(0.0)),
                            ("ended", JsValue::Boolean(false)),
                            ("error", JsValue::Null),
                        ] {
                            self.realm.set_property(element, name.to_owned(), value);
                        }
                        let pending = self
                            .with_video_state_mut(element, |state| {
                                std::mem::take(&mut state.pending_play_promises)
                            })
                            .unwrap_or_default();
                        for play_promise in pending {
                            self.resolve_promise(play_promise.record, &JsValue::Undefined);
                        }
                        self.queue_video_event(element, "loadedmetadata");
                        // A `play()` that initiated this load begins playback
                        // once metadata is available (it already unpaused the
                        // element when its promise joined the load).
                        if matches!(
                            self.realm.get_property(element, "paused"),
                            Some(JsValue::Boolean(false))
                        ) && let Some(pipeline) = self.video_pipeline_handle(element)
                        {
                            pipeline.borrow_mut().start_playback();
                            self.queue_video_event(element, "play");
                        }
                    }
                    Err(error) => self.fail_video_load(element, id, &error.to_string()),
                }
            }
            Ok(outcome) => self.fail_video_load(
                element,
                id,
                &format!("media response status {}", outcome.status),
            ),
            Err(message) => self.fail_video_load(element, id, &message),
        }
    }

    /// Whether a drained fetch transfer belongs to a video media load and
    /// must be settled through [`Self::settle_video_fetch`] instead of
    /// [`Self::settle_fetch`].
    #[must_use]
    pub fn is_pending_video_fetch(&self, id: u64) -> bool {
        self.find_video_element_by_load_id(id).is_some()
    }

    /// Shared pipeline handle of a loaded video element, for the paint
    /// phase's frame consumption.
    #[must_use]
    pub fn video_pipeline(&self, element: ObjectId) -> Option<Rc<RefCell<VideoPipeline>>> {
        self.video_pipeline_handle(element)
    }

    /// Cloned pipeline handle of a loaded video element, or `None` while no
    /// media is loaded.
    fn video_pipeline_handle(&self, element: ObjectId) -> Option<Rc<RefCell<VideoPipeline>>> {
        match self.realm.host(element) {
            Some(ObjectHost::VideoElement(state)) => {
                state.media.as_ref().map(|media| media.pipeline().clone())
            }
            _ => None,
        }
    }

    /// Advance every loaded video element's presentation clock to the page's
    /// current virtual instant and return the paint-phase publications.
    ///
    /// Each step: honor script `currentTime` writes as seeks (within the
    /// decoded-frame window), run the clock, pull frames up to the position,
    /// publish `currentTime`/`ended`, and queue the `on*` callbacks
    /// (`timeupdate` throttled to ~250 ms, then `ended`/`error`). The
    /// publications bind each element to the DOM `<video>` element whose
    /// resolved `src` matches its media, so the embedding can key frames to
    /// the node the paint phase draws.
    pub fn advance_video_playback(&mut self, dom: &Dom, now: Duration) -> Vec<FramePublication> {
        let mut handles: Vec<(ObjectId, Rc<RefCell<VideoPipeline>>)> = Vec::new();
        for (index, object) in self.realm.objects().iter().enumerate() {
            if let ObjectHost::VideoElement(state) = &object.host
                && let Some(media) = &state.media
            {
                handles.push((ObjectId::from_index(index), media.pipeline().clone()));
            }
        }
        if handles.is_empty() {
            return Vec::new();
        }
        let mut claimed: HashSet<NodeId> = HashSet::new();
        for (_, pipeline) in &handles {
            if let Some(node) = pipeline.borrow().bound_node() {
                claimed.insert(node);
            }
        }
        let mut candidates: Option<Vec<(NodeId, url::Url)>> = None;
        let mut publications = Vec::new();
        for (element, pipeline) in handles {
            let resolved_src = match self.realm.host(element) {
                Some(ObjectHost::VideoElement(state)) => state.resolved_src.clone(),
                _ => None,
            };
            // Keep the DOM node binding current: a node whose `src` no longer
            // resolves to this element's media is released, and an unbound
            // element claims a matching node once.
            let mut bound = pipeline.borrow().bound_node();
            let binding_valid = bound.is_some_and(|node| {
                resolved_src
                    .as_ref()
                    .is_some_and(|resolved| self.video_node_matches(dom, node, resolved))
            });
            if !binding_valid {
                if let Some(node) = bound.take() {
                    claimed.remove(&node);
                }
                pipeline.borrow_mut().unbind_node();
            }
            if bound.is_none() && resolved_src.is_some() {
                let entries = candidates.get_or_insert_with(|| {
                    Self::collect_video_src_nodes(dom)
                        .into_iter()
                        .filter(|(node, _)| !claimed.contains(node))
                        .filter_map(|(node, source)| {
                            self.resolve_fetch_url(&source).ok().map(|url| (node, url))
                        })
                        .collect()
                });
                if let Some((node, _)) = entries.iter().find(|(node, url)| {
                    Some(url) == resolved_src.as_ref() && !claimed.contains(node)
                }) {
                    pipeline.borrow_mut().bind_to_node(*node);
                    claimed.insert(*node);
                    bound = Some(*node);
                }
            }
            // A script `currentTime` write diverges from the published
            // position only when script moved it.
            let requested = self.number_property(element, "currentTime");
            let published = pipeline.borrow().published_position();
            if requested.is_finite() && requested != published {
                pipeline.borrow_mut().seek_to(requested);
                self.queue_video_event(element, "timeupdate");
            }
            let looping = matches!(
                self.realm.get_property(element, "loop"),
                Some(JsValue::Boolean(true))
            );
            let signals = pipeline.borrow_mut().advance_playback(now, looping);
            let position = pipeline.borrow().published_position();
            if position != self.number_property(element, "currentTime") {
                self.realm.set_property(
                    element,
                    "currentTime".to_owned(),
                    JsValue::Number(position),
                );
            }
            let ended_now = pipeline.borrow().playback_ended();
            let ended_before = matches!(
                self.realm.get_property(element, "ended"),
                Some(JsValue::Boolean(true))
            );
            if ended_now != ended_before {
                self.realm
                    .set_property(element, "ended".to_owned(), JsValue::Boolean(ended_now));
            }
            for signal in signals {
                match signal {
                    PlaybackSignal::TimeUpdate => self.queue_video_event(element, "timeupdate"),
                    PlaybackSignal::Ended => self.queue_video_event(element, "ended"),
                    PlaybackSignal::DecodeFailed => self.queue_video_event(element, "error"),
                }
            }
            if let (Some(node), Some(media_url)) = (bound, resolved_src) {
                publications.push(FramePublication {
                    node,
                    media_url,
                    frame: pipeline.borrow().current_frame_rgba(),
                });
            }
        }
        publications
    }

    /// Whether the `<video>` element at `node` still carries `resolved` as
    /// its current `src`.
    fn video_node_matches(&self, dom: &Dom, node: NodeId, resolved: &url::Url) -> bool {
        if !matches!(
            dom.node(node).map(render_dom::Node::kind),
            Some(NodeKind::Element(element))
                if element.namespace == Namespace::Html && element.local_name == "video"
        ) {
            return false;
        }
        match dom.attribute(node, "src") {
            Ok(Some(source)) => match self.resolve_fetch_url(source) {
                Ok(url) => &url == resolved,
                Err(_) => false,
            },
            _ => false,
        }
    }

    /// DOM `<video>` elements with a non-empty `src` attribute, tree order.
    fn collect_video_src_nodes(dom: &Dom) -> Vec<(NodeId, String)> {
        let mut found = Vec::new();
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            let Some(current) = dom.node(node) else {
                continue;
            };
            if matches!(
                current.kind(),
                NodeKind::Element(element)
                    if element.namespace == Namespace::Html && element.local_name == "video"
            ) && let Ok(Some(source)) = dom.attribute(node, "src")
                && !source.is_empty()
            {
                found.push((node, source.to_owned()));
            }
            pending.extend(current.children().iter().rev().copied());
        }
        found
    }

    /// Reject a pending load: pending `play()` promises get a `TypeError`
    /// and `onerror` runs at the microtask checkpoint.
    fn fail_video_load(&mut self, element: ObjectId, id: u64, message: &str) {
        let applied = self
            .with_video_state_mut(element, |state| {
                if state.load_id != Some(id) {
                    return false;
                }
                state.load_id = None;
                true
            })
            .unwrap_or(false);
        if !applied {
            return;
        }
        let pending = self
            .with_video_state_mut(element, |state| {
                std::mem::take(&mut state.pending_play_promises)
            })
            .unwrap_or_default();
        let reason = self.media_rejection(
            DomExceptionName::NotSupported,
            &format!("loading media failed: {message}"),
        );
        for play_promise in pending {
            self.reject_promise(play_promise.record, &reason);
        }
        self.queue_video_event(element, "error");
    }

    /// The element whose queued load carries `id`, if any.
    fn find_video_element_by_load_id(&self, id: u64) -> Option<ObjectId> {
        for (index, object) in self.realm.objects().iter().enumerate() {
            if let ObjectHost::VideoElement(state) = &object.host
                && state.load_id == Some(id)
            {
                return Some(ObjectId::from_index(index));
            }
        }
        None
    }

    /// Queue the `on<type>` property callback of a video element as a
    /// microtask with a minimal event object, mirroring XHR completion.
    fn queue_video_event(&mut self, receiver: ObjectId, event_type: &str) {
        let Some(JsValue::Object(callback)) = self
            .realm
            .get_property(receiver, &format!("on{event_type}"))
        else {
            return;
        };
        if !JsRuntime::is_callable_object(callback, &self.realm) {
            return;
        }
        if self.ensure_heap_capacity(2).is_err() {
            return;
        }
        let event = self.realm.create_ordinary_object();
        for (name, value) in [
            ("type", JsValue::String(event_type.to_owned())),
            ("target", JsValue::Object(receiver)),
            ("currentTarget", JsValue::Object(receiver)),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        let bound = self.realm.bound_callable(
            callback,
            JsValue::Object(receiver),
            vec![JsValue::Object(event)],
        );
        self.pending_microtasks.push(JsMicrotask::Callback(bound));
    }

    fn with_video_state<T>(
        &self,
        receiver: ObjectId,
        apply: impl FnOnce(&VideoElementState) -> T,
    ) -> Option<T> {
        match self.realm.host(receiver) {
            Some(ObjectHost::VideoElement(state)) => Some(apply(&state)),
            _ => None,
        }
    }

    fn with_video_state_mut<T>(
        &mut self,
        receiver: ObjectId,
        apply: impl FnOnce(&mut VideoElementState) -> T,
    ) -> Option<T> {
        match self.realm.host_mut(receiver) {
            Some(ObjectHost::VideoElement(state)) => Some(apply(state)),
            _ => None,
        }
    }

    fn number_property(&self, receiver: ObjectId, name: &str) -> f64 {
        match self.realm.get_property(receiver, name) {
            Some(JsValue::Number(number)) => number,
            _ => 0.0,
        }
    }

    fn string_property(&self, receiver: ObjectId, name: &str) -> String {
        match self.realm.get_property(receiver, name) {
            Some(value) => value.to_js_string(),
            None => String::new(),
        }
    }

    /// A throw-ready rejection reason for `HTMLMediaElement.play()`.
    ///
    /// HTML §"media load algorithm" rejects a `play()` promise with a
    /// `DOMException`, never with a plain `Error`:
    ///
    /// - no `src`, or a `src` that does not parse, or media data that cannot
    ///   be fetched at all: "reject promise with a `NotSupportedError`
    ///   `DOMException`";
    /// - a `load()` that supersedes an in-flight one: "reject promise with an
    ///   `AbortError` `DOMException`".
    ///
    /// Both used to be ECMAScript `TypeError`s, which is why the engine could
    /// not answer `e instanceof DOMException` for any of them.
    fn media_rejection(&mut self, name: DomExceptionName, message: &str) -> JsValue {
        self.construct_dom_exception(name, message)
            .unwrap_or_else(|_| JsValue::String(format!("{}: {message}", name.as_str())))
    }
}

/// Whether a `codecs=` parameter value names an H.264 codec this build can
/// demux (`avc1...`, `avc3...`, or `h264`).
fn supports_avc(codecs: &str) -> bool {
    codecs
        .split(',')
        .map(str::trim)
        .any(|codec| codec.starts_with("avc1") || codec.starts_with("avc3") || codec == "h264")
}

#[cfg(test)]
thread_local! {
    /// Decoder backends queued by tests and consumed one per settled media
    /// load, so the JS playback surface can be exercised end to end before
    /// a real H.264 backend is wired.
    static TEST_VIDEO_DECODERS: RefCell<std::collections::VecDeque<Box<dyn crate::video::VideoDecoder>>> =
        RefCell::new(std::collections::VecDeque::new());
}

/// Queue a decoder backend the next settled media load will use (tests).
#[cfg(test)]
pub(crate) fn push_test_video_decoder(decoder: Box<dyn crate::video::VideoDecoder>) {
    TEST_VIDEO_DECODERS.with(|queue| queue.borrow_mut().push_back(decoder));
}

#[cfg(test)]
fn take_test_video_decoder() -> Option<Box<dyn crate::video::VideoDecoder>> {
    TEST_VIDEO_DECODERS.with(|queue| queue.borrow_mut().pop_front())
}

#[cfg(test)]
mod tests {
    use crate::JsRuntime;
    use crate::JsValue;
    use crate::runtime::types::FetchOutcome;
    use crate::runtime::types::PendingFetch;
    use crate::video::VideoPipeline;
    use crate::video::test_mp4::ColorTestDecoder;
    use crate::video::test_mp4::TestMp4Builder;
    use render_dom::Dom;
    use render_html::parse_document;
    use std::cell::RefCell;
    use std::time::Duration;
    use url::Url;

    /// A runtime on `https://example.test/watch` with microtask and media
    /// settlement helpers, mirroring the runtime test harness.
    struct Harness {
        runtime: JsRuntime,
        dom: Dom,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_html("<!doctype html><p>video</p>")
        }

        fn with_html(html: &str) -> Self {
            let mut parsed = parse_document(html);
            let url = Url::parse("https://example.test/watch").expect("test URL");
            let runtime = JsRuntime::with_url(&parsed.dom, &url);
            let dom = std::mem::take(&mut parsed.dom);
            Self { runtime, dom }
        }

        fn execute(&mut self, source: &str) -> JsValue {
            self.runtime
                .execute(&mut self.dom, source)
                .map(|outcome| outcome.value)
                .expect("script executes")
        }

        fn string(&mut self, source: &str) -> String {
            match self.execute(source) {
                JsValue::String(text) => text,
                other => panic!("script returned {other:?}, expected a string"),
            }
        }

        /// Evaluate a primitive-valued expression and render it as a string.
        fn scalar(&mut self, source: &str) -> String {
            match self.execute(&format!("String({source})")) {
                JsValue::String(text) => text,
                other => panic!("script returned {other:?}, expected a string"),
            }
        }

        fn drain_microtasks(&mut self) {
            loop {
                let pending = self.runtime.take_pending_microtasks();
                if pending.is_empty() {
                    return;
                }
                for microtask in pending {
                    self.runtime
                        .invoke_microtask(&mut self.dom, microtask)
                        .expect("microtask executes");
                }
            }
        }

        fn take_video_fetch(&mut self) -> PendingFetch {
            let requests = self.runtime.take_pending_fetch_requests();
            assert_eq!(requests.len(), 1, "exactly one media transfer queued");
            let request = requests.into_iter().next().expect("one request");
            assert_eq!(request.method, "GET");
            assert!(self.runtime.is_pending_video_fetch(request.id));
            request
        }

        fn settle_ok(&mut self, id: u64, body: Vec<u8>) {
            self.runtime.settle_video_fetch(
                &mut self.dom,
                id,
                Ok(FetchOutcome {
                    status: 200,
                    status_text: "OK".to_owned(),
                    headers: Vec::new(),
                    body,
                }),
            );
            self.drain_microtasks();
        }

        fn settle_failed(&mut self, id: u64, message: &str) {
            self.runtime
                .settle_video_fetch(&mut self.dom, id, Err(message.to_owned()));
            self.drain_microtasks();
        }

        /// Advance the presentation clocks to `now` and run the callbacks
        /// the step queued.
        fn advance(&mut self, now: Duration) -> Vec<crate::video::present::FramePublication> {
            let publications = self.runtime.advance_video_playback(&self.dom, now);
            self.drain_microtasks();
            publications
        }

        fn advance_ms(&mut self, millis: u64) -> Vec<crate::video::present::FramePublication> {
            self.advance(Duration::from_millis(millis))
        }

        fn pipeline(&mut self, variable: &str) -> std::rc::Rc<RefCell<VideoPipeline>> {
            match self.execute(variable) {
                JsValue::Object(element) => self
                    .runtime
                    .video_pipeline(element)
                    .expect("media is loaded"),
                other => panic!("video element is {other:?}"),
            }
        }

        /// The `NodeId` of the first DOM `<video>` element.
        fn video_node(&self) -> render_dom::NodeId {
            let dom = &self.dom;
            let mut pending = vec![dom.document()];
            while let Some(node) = pending.pop() {
                if matches!(
                    dom.node(node).map(render_dom::Node::kind),
                    Some(render_dom::NodeKind::Element(element))
                        if element.namespace == render_dom::Namespace::Html
                            && element.local_name == "video"
                ) {
                    return node;
                }
                pending.extend(dom.children(node).unwrap_or_default().iter().rev().copied());
            }
            panic!("test markup has no <video> element");
        }
    }

    #[test]
    fn constructor_exposes_default_playback_properties() {
        let mut harness = Harness::new();
        harness.execute("var v = new Video();");
        assert_eq!(
            harness.string(
                "typeof v + ':' + v.readyState + ':' + v.paused + ':' + \
                 isNaN(v.duration) + ':' + v.videoWidth + ':' + v.videoHeight + ':' + \
                 v.currentTime + ':' + v.src + ':' + (v instanceof Object)"
            ),
            "object:0:true:true:0:0:0::true"
        );
        assert_eq!(
            harness.string("new Video(640, 480).width + 'x' + new Video(640, 480).height"),
            "640x480"
        );
        // Image-style legacy call without `new` also constructs.
        assert_eq!(harness.string("typeof Video()"), "object");
    }

    #[test]
    fn can_play_type_answers_for_demuxable_containers() {
        let mut harness = Harness::new();
        harness.execute("var v = new Video();");
        assert_eq!(harness.string("v.canPlayType('video/mp4')"), "maybe");
        assert_eq!(
            harness.string("v.canPlayType('video/mp4; codecs=\"avc1.42E01E, mp4a.40.2\"')"),
            "maybe"
        );
        assert_eq!(harness.string("v.canPlayType('video/mp4; codecs=vp9')"), "");
        assert_eq!(harness.string("v.canPlayType('video/webm')"), "");
        assert_eq!(harness.string("v.canPlayType('')"), "");
    }

    #[test]
    fn play_without_source_rejects_the_promise() {
        let mut harness = Harness::new();
        harness.execute(
            "var v = new Video(); var outcome = 'pending'; \
             v.play().then(function () { outcome = 'resolved'; }, \
             function () { outcome = 'rejected'; });",
        );
        assert_eq!(harness.string("outcome"), "pending");
        harness.drain_microtasks();
        assert_eq!(harness.string("outcome"), "rejected");
        assert_eq!(harness.scalar("v.paused"), "true");
    }

    #[test]
    fn play_queues_media_load_and_settlement_publishes_metadata() {
        let mut harness = Harness::new();
        harness.execute(
            "var v = new Video(); v.src = 'movie.mp4'; \
             var events = []; var resolved = false; \
             v.onloadedmetadata = function (event) { \
                 events.push(event.type + ':' + (this === v)); }; \
             v.play().then(function () { resolved = true; });",
        );
        assert_eq!(harness.scalar("v.paused"), "false");
        let request = harness.take_video_fetch();
        assert_eq!(request.url, "https://example.test/movie.mp4");
        // Metadata is not published before settlement.
        assert_eq!(harness.scalar("v.readyState"), "0");
        assert_eq!(harness.scalar("isNaN(v.duration)"), "true");

        harness.settle_ok(request.id, TestMp4Builder::new().build());
        assert_eq!(
            harness.string(
                "v.readyState + ':' + v.duration + ':' + v.videoWidth + ':' + \
                 v.videoHeight + ':' + v.currentTime + ':' + resolved + ':' + \
                 events.join('|')"
            ),
            "1:2:64:48:0:true:loadedmetadata:true"
        );

        // Once media is ready, play() resolves immediately and playback
        // starts (unpauses) without another transfer.
        harness.execute("var again = false;");
        assert_eq!(
            harness
                .string("(v.play().then(function () { again = true; }), v.paused + ':' + again)"),
            "false:false"
        );
        harness.drain_microtasks();
        assert_eq!(harness.scalar("again"), "true");
        assert!(harness.runtime.pending_fetch_queue_empty());

        // The demuxed pipeline is reachable for the future paint phase.
        let element = match harness.execute("v") {
            JsValue::Object(object) => object,
            other => panic!("video element is {other:?}"),
        };
        let pipeline = harness
            .runtime
            .video_pipeline(element)
            .expect("media loaded");
        assert_eq!(pipeline.borrow().track().info.sample_count, 4);
        assert_eq!(pipeline.borrow().track().info.width, 64);
    }

    #[test]
    fn concurrent_play_calls_join_one_transfer() {
        let mut harness = Harness::new();
        harness.execute(
            "var v = new Video(); v.src = 'movie.mp4'; \
             var resolved = 0; \
             v.play().then(function () { resolved += 1; }); \
             v.play().then(function () { resolved += 1; });",
        );
        let request = harness.take_video_fetch();
        harness.settle_ok(request.id, TestMp4Builder::new().build());
        assert_eq!(harness.scalar("resolved"), "2");
    }

    #[test]
    fn undecodable_bytes_reject_and_fire_error() {
        let mut harness = Harness::new();
        harness.execute(
            "var v = new Video(); v.src = 'movie.mp4'; \
             var events = []; var reason = ''; \
             v.onerror = function () { events.push('error'); }; \
             v.play().then(function () {}, function (error) { reason = String(error); });",
        );
        let request = harness.take_video_fetch();
        harness.settle_ok(request.id, b"definitely not an mp4".to_vec());
        assert_eq!(
            harness.string("v.readyState + ':' + events.join(',')"),
            "0:error"
        );
        assert!(harness.string("reason").contains("loading media failed"));
        assert_eq!(harness.scalar("isNaN(v.duration)"), "true");
    }

    #[test]
    fn transport_failure_rejects_and_non_2xx_status_fails_too() {
        let mut harness = Harness::new();
        harness.execute(
            "var v = new Video(); v.src = 'movie.mp4'; \
             var failures = 0; v.onerror = function () { failures += 1; }; \
             v.play().then(function () {}, function () { failures += 10; });",
        );
        let request = harness.take_video_fetch();
        harness.settle_failed(request.id, "connection reset");
        assert_eq!(harness.scalar("failures"), "11");

        harness.execute("v.play()");
        let request = harness.take_video_fetch();
        // A 404 is a completed transfer but a failed media load.
        harness.runtime.settle_video_fetch(
            &mut harness.dom,
            request.id,
            Ok(FetchOutcome {
                status: 404,
                status_text: "Not Found".to_owned(),
                headers: Vec::new(),
                body: Vec::new(),
            }),
        );
        harness.drain_microtasks();
        assert_eq!(harness.scalar("failures"), "12");
    }

    #[test]
    fn load_resets_state_and_requeues() {
        let mut harness = Harness::new();
        harness.execute(
            "var v = new Video(); v.src = 'movie.mp4'; \
             var aborted = false; \
             v.play().then(function () {}, function () { aborted = true; });",
        );
        let first = harness.take_video_fetch();
        harness.execute("v.load()");
        harness.drain_microtasks();
        assert_eq!(harness.scalar("aborted"), "true");
        assert_eq!(harness.scalar("v.readyState"), "0");
        // The aborted transfer settles silently: its element moved on.
        harness.runtime.settle_video_fetch(
            &mut harness.dom,
            first.id,
            Ok(FetchOutcome {
                status: 200,
                status_text: "OK".to_owned(),
                headers: Vec::new(),
                body: TestMp4Builder::new().build(),
            }),
        );
        harness.drain_microtasks();
        assert_eq!(harness.scalar("v.readyState"), "0");
        // load() with a source queued a fresh transfer.
        let second = harness.take_video_fetch();
        harness.settle_ok(second.id, TestMp4Builder::new().build());
        assert_eq!(harness.string("v.readyState + ':' + v.duration"), "1:2");
    }

    #[test]
    fn pending_play_promises_survive_garbage_collection() {
        let mut harness = Harness::new();
        harness.execute(
            "(function () { \
                 var v = new Video(); v.src = 'movie.mp4'; \
                 v.onloadedmetadata = function () { window.__loaded = true; }; \
                 v.play().then(function () { window.__resolved = true; }); \
             })();",
        );
        let request = harness.take_video_fetch();
        // The element is now unreachable from script; only the pending
        // transfer (and, through its state, the play promise) roots it.
        harness.runtime.collect_garbage();
        harness.settle_ok(request.id, TestMp4Builder::new().build());
        assert_eq!(
            harness.string("window.__resolved + ':' + window.__loaded"),
            "true:true"
        );
    }

    #[test]
    fn receiver_mismatch_names_the_entry_point() {
        let mut harness = Harness::new();
        let error = harness
            .runtime
            .execute(
                &mut harness.dom,
                "var fake = {}; fake.play = Video.prototype.play; fake.play();",
            )
            .expect_err("receiver mismatch throws");
        assert!(
            error.to_string().contains("Video method receiver"),
            "unexpected error: {error}"
        );
    }

    /// Drive one loaded, playing element through settlement.
    fn playing_harness(html: &str, script: &str) -> Harness {
        super::push_test_video_decoder(Box::new(ColorTestDecoder::new(64, 48)));
        let mut harness = Harness::with_html(html);
        harness.execute(script);
        let request = harness.take_video_fetch();
        harness.settle_ok(request.id, TestMp4Builder::new().build());
        harness
    }

    #[test]
    fn playback_clock_advances_current_time_and_frames() {
        let mut harness = playing_harness(
            "<!doctype html><p>video</p>",
            "var v = new Video(); v.src = 'movie.mp4'; \
             var updates = 0; v.ontimeupdate = function () { updates += 1; }; \
             v.play();",
        );
        harness.advance_ms(0);
        assert_eq!(
            harness.scalar("v.paused + ':' + v.ended + ':' + updates"),
            "false:false:1"
        );
        harness.advance_ms(700);
        assert_eq!(harness.scalar("v.currentTime + ':' + updates"), "0.7:2");
        let pipeline = harness.pipeline("v");
        assert_eq!(pipeline.borrow().current_frame_timestamp(), Some(0.5));
        assert_eq!(
            pipeline
                .borrow()
                .current_frame_rgba()
                .expect("frame presented")
                .bytes[0],
            2
        );
    }

    #[test]
    fn pause_and_resume_fire_events_and_freeze_position() {
        let mut harness = playing_harness(
            "<!doctype html><p>video</p>",
            "var v = new Video(); v.src = 'movie.mp4'; \
             var events = ''; v.onplay = function () { events += 'p'; }; \
             v.onpause = function () { events += 'P'; }; v.play();",
        );
        harness.advance_ms(0);
        harness.advance_ms(600);
        assert_eq!(harness.scalar("v.currentTime"), "0.6");
        harness.execute("v.pause()");
        harness.advance_ms(1_200);
        assert_eq!(harness.scalar("v.paused + ':' + v.currentTime"), "true:0.6");
        harness.execute("v.play()");
        harness.advance_ms(1_300);
        assert_eq!(
            harness.scalar("v.paused + ':' + v.currentTime"),
            "false:0.6"
        );
        harness.advance_ms(1_500);
        assert_eq!(harness.scalar("v.currentTime"), "0.8");
        // Settlement started playback (p), the pause fired P, and the
        // explicit resume fired p again.
        assert_eq!(harness.scalar("events"), "pPp");
    }

    #[test]
    fn ended_transitions_once_and_restarts_on_play() {
        let mut harness = playing_harness(
            "<!doctype html><p>video</p>",
            "var v = new Video(); v.src = 'movie.mp4'; \
             var ended = 0; v.onended = function () { ended += 1; }; \
             var updates = 0; v.ontimeupdate = function () { updates += 1; }; \
             v.play();",
        );
        harness.advance_ms(0);
        harness.advance_ms(2_000);
        assert_eq!(
            harness.scalar("v.ended + ':' + v.currentTime + ':' + ended + ':' + updates"),
            "true:2:1:2"
        );
        // The ended state is frozen: further advances change nothing.
        harness.advance_ms(3_000);
        assert_eq!(
            harness.scalar("v.ended + ':' + ended + ':' + updates"),
            "true:1:2"
        );
        // play() restarts from the beginning.
        harness.execute("v.play()");
        harness.advance_ms(2_100);
        harness.advance_ms(2_200);
        assert_eq!(
            harness.scalar("v.ended + ':' + v.currentTime + ':' + ended"),
            "false:0.1:1"
        );
    }

    #[test]
    fn script_current_time_write_seeks_within_decoded_frames() {
        let mut harness = playing_harness(
            "<!doctype html><p>video</p>",
            "var v = new Video(); v.src = 'movie.mp4'; \
             var updates = 0; v.ontimeupdate = function () { updates += 1; }; \
             v.play();",
        );
        harness.advance_ms(0);
        harness.advance_ms(1_000);
        assert_eq!(harness.scalar("v.currentTime"), "1");
        harness.execute("v.currentTime = 0.5");
        harness.advance_ms(1_005);
        assert_eq!(harness.scalar("v.currentTime"), "0.5");
        // The seek fired a prompt timeupdate on top of the clock ticks.
        assert_eq!(harness.scalar("updates"), "3");
        let pipeline = harness.pipeline("v");
        assert_eq!(pipeline.borrow().current_frame_timestamp(), Some(0.5));
        assert_eq!(
            pipeline
                .borrow()
                .current_frame_rgba()
                .expect("frame presented")
                .bytes[0],
            2
        );
        // Playback continues from the seek target.
        harness.advance_ms(1_100);
        assert_eq!(harness.scalar("v.currentTime"), "0.595");
    }

    #[test]
    fn dom_src_binding_publishes_frames_for_paint() {
        let mut harness = playing_harness(
            "<!doctype html><video id=player src='movie.mp4'></video>",
            "var v = new Video(); v.src = 'movie.mp4'; v.play();",
        );
        let node = harness.video_node();
        harness.advance_ms(0);
        let publications = harness.advance_ms(600);
        assert_eq!(publications.len(), 1, "the DOM element binds to the media");
        assert_eq!(publications[0].node, node);
        assert_eq!(
            publications[0].media_url.as_str(),
            "https://example.test/movie.mp4"
        );
        let frame = publications[0]
            .frame
            .as_ref()
            .expect("a frame is presented");
        assert_eq!((frame.width, frame.height), (64, 48));
        assert_eq!(frame.bytes[0], 2, "the 0.5 s frame is on screen");

        // Losing the src attribute releases the binding and the frame.
        harness
            .dom
            .remove_attribute(node, "src")
            .expect("src is removable");
        assert!(harness.advance_ms(1_200).is_empty());
    }

    #[test]
    fn placeholder_decoder_fires_error_once_and_freezes() {
        let mut harness = Harness::new();
        harness.execute(
            "var v = new Video(); v.src = 'movie.mp4'; \
             var failures = 0; v.onerror = function () { failures += 1; }; \
             v.play();",
        );
        let request = harness.take_video_fetch();
        harness.settle_ok(request.id, TestMp4Builder::new().build());
        harness.advance_ms(0);
        assert_eq!(harness.scalar("failures"), "1");
        harness.advance_ms(100);
        assert_eq!(
            harness.scalar("failures + ':' + v.currentTime + ':' + v.ended"),
            "1:0:false"
        );
    }
}
