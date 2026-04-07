"""DOM/BOM API bindings for the JavaScript interpreter."""
from __future__ import annotations

import logging
_logger = logging.getLogger(__name__)

from js.interpreter import (
    Interpreter, Environment, JSObject, JSArray, JSFunction,
    _UNDEF, _to_str, _to_bool, _get_property, _set_property,
)
from js.event_loop import get_event_loop
from html.dom import Element, Text, Document
import css.selector as _selector_mod


# ---------------------------------------------------------------------------
# Event system
# ---------------------------------------------------------------------------

class JSEvent(JSObject):
    """Minimal DOM Event object."""

    def __init__(self, type_: str, bubbles: bool = False,
                 cancelable: bool = False, detail=_UNDEF):
        super().__init__()
        self['type'] = type_
        self['bubbles'] = bubbles
        self['cancelable'] = cancelable
        self['detail'] = detail
        self['target'] = None
        self['currentTarget'] = None
        self['defaultPrevented'] = False
        self['propagationStopped'] = False
        self['immediatePropagationStopped'] = False
        self['preventDefault'] = lambda: self.__setitem__('defaultPrevented', True)
        self['stopPropagation'] = lambda: self.__setitem__('propagationStopped', True)
        self['stopImmediatePropagation'] = lambda: (
            self.__setitem__('propagationStopped', True) or
            self.__setitem__('immediatePropagationStopped', True)
        )


def _dispatch_event(node, event: JSEvent, binding) -> bool:
    """Dispatch event with capture + bubble phases (simplified).

    Returns False if preventDefault() was called, True otherwise.
    """
    # Build ancestor chain for capture phase
    ancestors: list = []
    p = getattr(node, 'parent', None)
    while p is not None:
        ancestors.append(p)
        p = getattr(p, 'parent', None)

    event['target'] = binding.wrap(node)

    # Capture phase (root → parent)
    for ancestor in reversed(ancestors):
        wrapper = binding.wrap(ancestor)
        if isinstance(wrapper, DOMElement):
            _invoke_listeners(wrapper, event, capture=True)
        if event.get('propagationStopped'):
            break

    # At-target phase
    wrapper = binding.wrap(node)
    if isinstance(wrapper, DOMElement):
        event['currentTarget'] = wrapper
        _invoke_listeners(wrapper, event, capture=False)
        _invoke_listeners(wrapper, event, capture=True)

    # Bubble phase (parent → root)
    if event.get('bubbles') and not event.get('propagationStopped'):
        for ancestor in ancestors:
            wrapper = binding.wrap(ancestor)
            if isinstance(wrapper, DOMElement):
                event['currentTarget'] = wrapper
                _invoke_listeners(wrapper, event, capture=False)
                if event.get('propagationStopped'):
                    break

    return not event.get('defaultPrevented', False)


def _invoke_listeners(wrapper: 'DOMElement', event: JSEvent, capture: bool):
    """Call all registered listeners of the given phase."""
    event_type = event['type']
    listeners = wrapper._event_listeners.get(event_type, [])
    for entry in list(listeners):
        fn = entry['fn']
        if entry.get('capture', False) != capture:
            continue
        try:
            if wrapper._binding.interpreter:
                wrapper._binding.interpreter._call_value(fn, [event])
            elif callable(fn):
                fn(event)
        except Exception as exc:
            _logger.debug('Event listener error: %s', exc)
        if event.get('immediatePropagationStopped'):
            break


# ---------------------------------------------------------------------------
# MutationObserver
# ---------------------------------------------------------------------------

class JSMutationObserver(JSObject):
    """Minimal MutationObserver implementation."""

    def __init__(self, callback, binding):
        super().__init__()
        self._callback = callback
        self._binding = binding
        self._observed: list = []  # (node, options)
        self._records: list = []
        self._delivery_scheduled = False

        self['observe'] = self._observe
        self['disconnect'] = self._disconnect
        self['takeRecords'] = self._take_records

    def _observe(self, target, options=_UNDEF):
        if isinstance(target, DOMElement):
            self._observed.append((target, options if isinstance(options, dict) else {}))
        return _UNDEF

    def _disconnect(self):
        self._observed = []
        return _UNDEF

    def _take_records(self):
        records = JSArray(self._records)
        self._records = []
        return records

    def notify(self, record: JSObject):
        """Called internally when a mutation occurs on an observed node."""
        self._records.append(record)
        if self._delivery_scheduled:
            return
        self._delivery_scheduled = True
        get_event_loop().enqueue_microtask(self._deliver)

    def _deliver(self):
        self._delivery_scheduled = False
        if not self._records:
            return
        records = JSArray(self._records)
        self._records = []
        try:
            if self._binding.interpreter:
                self._binding.interpreter._call_value(
                    self._callback, [records, self]
                )
            elif callable(self._callback):
                self._callback(records, self)
        except Exception as exc:
            _logger.debug('MutationObserver callback error: %s', exc)


class DOMElement(JSObject):
    """Wraps a DOM Element for JavaScript access."""

    def __init__(self, node, binding):
        super().__init__()
        self._node = node
        self._binding = binding
        self._event_listeners = {}  # event_name -> [{'fn': fn, 'capture': bool}, ...]

        # Properties
        super().__setitem__('nodeType', 1)
        super().__setitem__('tagName', node.tag.upper() if hasattr(node, 'tag') else '')
        super().__setitem__('nodeName', self['tagName'])
        super().__setitem__('id', node.attributes.get('id', '') if hasattr(node, 'attributes') else '')
        super().__setitem__('className', node.attributes.get('class', '') if hasattr(node, 'attributes') else '')

        # Methods
        self['getAttribute'] = lambda name: node.attributes.get(name, None) if hasattr(node, 'attributes') else None
        self['setAttribute'] = lambda name, val: self._set_attr(name, val)
        self['removeAttribute'] = lambda name: self._remove_attr(name)
        self['hasAttribute'] = lambda name: name in node.attributes if hasattr(node, 'attributes') else False

        self['querySelector'] = lambda sel: binding.query_selector(node, sel)
        self['querySelectorAll'] = lambda sel: binding.query_selector_all(node, sel)
        self['getElementsByTagName'] = lambda tag: binding.get_elements_by_tag(node, tag)
        self['getElementsByClassName'] = lambda cls: binding.get_elements_by_class(node, cls)

        self['appendChild'] = lambda child: self._append_child(child)
        self['removeChild'] = lambda child: self._remove_child(child)
        self['insertBefore'] = lambda new, ref: self._insert_before(new, ref)
        self['replaceChild'] = lambda new, old: self._replace_child(new, old)
        self['cloneNode'] = lambda deep=False: self  # stub

        self['addEventListener'] = lambda ev, fn, opts=_UNDEF: self._add_event_listener(ev, fn, opts)
        self['removeEventListener'] = lambda ev, fn, opts=_UNDEF: self._remove_event_listener(ev, fn)
        self['dispatchEvent'] = lambda ev: self._dispatch(ev)

        self['contains'] = lambda other: False
        self['matches'] = lambda sel: self._matches(sel)
        self['closest'] = lambda sel: self._closest(sel)
        self['getBoundingClientRect'] = lambda: self._get_bounding_rect()
        self['focus'] = lambda: None
        self['blur'] = lambda: None
        self['click'] = lambda: None

        # classList
        self['classList'] = self._make_class_list()

        # style proxy
        self['style'] = StyleProxy(node, binding)

    def __getitem__(self, key):
        # Dynamic properties
        if key == 'innerHTML':
            return self._get_inner_html()
        if key == 'outerHTML':
            return self._get_outer_html()
        if key == 'innerText' or key == 'textContent':
            return self._get_text_content()
        if key == 'children':
            return self._get_children()
        if key == 'childNodes':
            return self._get_child_nodes()
        if key == 'firstChild':
            kids = self._node.children
            return self._binding.wrap(kids[0]) if kids else None
        if key == 'lastChild':
            kids = self._node.children
            return self._binding.wrap(kids[-1]) if kids else None
        if key == 'firstElementChild':
            for c in self._node.children:
                if isinstance(c, Element):
                    return self._binding.wrap(c)
            return None
        if key == 'parentNode' or key == 'parentElement':
            p = getattr(self._node, 'parent', None)
            return self._binding.wrap(p) if p and isinstance(p, Element) else None
        if key == 'nextSibling' or key == 'nextElementSibling':
            return self._get_sibling(1)
        if key == 'previousSibling' or key == 'previousElementSibling':
            return self._get_sibling(-1)
        if key == 'offsetWidth' or key == 'clientWidth' or key == 'scrollWidth':
            return getattr(getattr(self._node, 'box', None), 'content_width', 0) or 0
        if key == 'offsetHeight' or key == 'clientHeight' or key == 'scrollHeight':
            return getattr(getattr(self._node, 'box', None), 'content_height', 0) or 0
        if key == 'offsetLeft':
            return getattr(getattr(self._node, 'box', None), 'x', 0) or 0
        if key == 'offsetTop':
            return getattr(getattr(self._node, 'box', None), 'y', 0) or 0
        if key == 'offsetParent':
            return None
        if key == 'dataset':
            return self._get_dataset()
        if key == 'id':
            return self._node.attributes.get('id', '') if hasattr(self._node, 'attributes') else ''
        if key == 'className':
            return self._node.attributes.get('class', '') if hasattr(self._node, 'attributes') else ''
        if key == 'value':
            return self._node.attributes.get('value', '') if hasattr(self._node, 'attributes') else ''
        if key == 'href':
            return self._node.attributes.get('href', '') if hasattr(self._node, 'attributes') else ''
        if key == 'src':
            return self._node.attributes.get('src', '') if hasattr(self._node, 'attributes') else ''
        return super().__getitem__(key) if key in dict.keys(self) else _UNDEF

    def __setitem__(self, key, value):
        if key == 'innerHTML':
            self._set_inner_html(value)
            return
        if key == 'innerText' or key == 'textContent':
            self._set_text_content(value)
            return
        if key == 'className':
            if hasattr(self._node, 'attributes'):
                self._node.attributes['class'] = _to_str(value)
                self._binding.invalidate_style(self._node, 'property:className')
                self._notify_mutation(
                    'attributes',
                    attributeName='class',
                    oldValue=None,
                )
            return
        if key == 'id':
            if hasattr(self._node, 'attributes'):
                self._node.attributes['id'] = _to_str(value)
                self._binding.invalidate_style(self._node, 'property:id')
                self._notify_mutation(
                    'attributes',
                    attributeName='id',
                    oldValue=None,
                )
            return
        if key == 'value':
            if hasattr(self._node, 'attributes'):
                self._node.attributes['value'] = _to_str(value)
                self._binding.invalidate_layout(self._node, 'property:value')
                self._notify_mutation(
                    'attributes',
                    attributeName='value',
                    oldValue=None,
                )
            return
        if key == 'href':
            if hasattr(self._node, 'attributes'):
                self._node.attributes['href'] = _to_str(value)
                self._binding.invalidate_style(self._node, 'property:href')
                self._notify_mutation(
                    'attributes',
                    attributeName='href',
                    oldValue=None,
                )
            return
        if key == 'src':
            if hasattr(self._node, 'attributes'):
                self._node.attributes['src'] = _to_str(value)
                self._binding.invalidate_layout(self._node, 'property:src')
                self._notify_mutation(
                    'attributes',
                    attributeName='src',
                    oldValue=None,
                )
            return
        super().__setitem__(key, value)

    def _set_attr(self, name, val):
        if hasattr(self._node, 'attributes'):
            attr_name = _to_str(name)
            old_value = self._node.attributes.get(attr_name)
            self._node.attributes[attr_name] = _to_str(val)
            self._binding.invalidate_style(self._node, f'attribute:{attr_name}')
            self._notify_mutation(
                'attributes',
                attributeName=attr_name,
                oldValue=old_value,
            )

    def _remove_attr(self, name):
        if not hasattr(self._node, 'attributes'):
            return None
        attr_name = _to_str(name)
        old_value = self._node.attributes.pop(attr_name, None)
        if old_value is not None:
            self._binding.invalidate_style(self._node, f'attribute:{attr_name}')
            self._notify_mutation(
                'attributes',
                attributeName=attr_name,
                oldValue=old_value,
            )
        return old_value

    def _get_inner_html(self):
        parts = []
        for child in self._node.children:
            parts.append(_serialize_node(child))
        return ''.join(parts)

    def _get_outer_html(self):
        return _serialize_node(self._node)

    def _get_text_content(self):
        return _extract_text(self._node)

    def _set_inner_html(self, html_str):
        from html.parser import parse as parse_html
        try:
            doc = parse_html(_to_str(html_str))
            # Replace children
            body = _find_body(doc) or doc
            old_children = [self._binding.wrap(child) for child in self._node.children]
            self._node.children = list(body.children)
            for child in self._node.children:
                child.parent = self._node
            new_children = [self._binding.wrap(child) for child in self._node.children]
            self._binding.invalidate_layout(self._node, 'innerHTML')
            self._notify_mutation(
                'childList',
                addedNodes=JSArray(new_children),
                removedNodes=JSArray(old_children),
            )
        except Exception as _exc:
            _logger.debug("Ignored: %s", _exc)

    def _set_text_content(self, text):
        old_children = [self._binding.wrap(child) for child in self._node.children]
        t = Text(_to_str(text))
        t.parent = self._node
        self._node.children = [t]
        self._binding.invalidate_layout(self._node, 'textContent')
        self._notify_mutation(
            'childList',
            addedNodes=JSArray([self._binding.wrap(t)]),
            removedNodes=JSArray(old_children),
        )

    def _get_children(self):
        return JSArray(
            self._binding.wrap(c) for c in self._node.children
            if isinstance(c, Element)
        )

    def _get_child_nodes(self):
        return JSArray(self._binding.wrap(c) for c in self._node.children)

    def _get_sibling(self, direction):
        parent = getattr(self._node, 'parent', None)
        if not parent:
            return None
        kids = parent.children
        try:
            idx = kids.index(self._node) + direction
            if 0 <= idx < len(kids):
                return self._binding.wrap(kids[idx])
        except ValueError:
            pass
        return None

    def _append_child(self, child_wrapper):
        if isinstance(child_wrapper, DOMElement):
            child_node = child_wrapper._node
        elif isinstance(child_wrapper, DOMText):
            child_node = child_wrapper._node
        else:
            return child_wrapper
        # Remove from old parent
        old_parent = getattr(child_node, 'parent', None)
        if old_parent and hasattr(old_parent, 'children'):
            try:
                old_parent.children.remove(child_node)
            except ValueError:
                pass
        child_node.parent = self._node
        self._node.children.append(child_node)
        self._binding.invalidate_layout(self._node, 'appendChild')
        self._notify_mutation('childList',
                              addedNodes=JSArray([child_wrapper]),
                              removedNodes=JSArray())
        return child_wrapper

    def _remove_child(self, child_wrapper):
        if isinstance(child_wrapper, (DOMElement, DOMText)):
            child_node = child_wrapper._node
        else:
            return child_wrapper
        try:
            self._node.children.remove(child_node)
        except ValueError:
            pass
        child_node.parent = None
        self._binding.invalidate_layout(self._node, 'removeChild')
        self._notify_mutation('childList',
                              addedNodes=JSArray(),
                              removedNodes=JSArray([child_wrapper]))
        return child_wrapper

    def _insert_before(self, new_wrapper, ref_wrapper):
        if isinstance(new_wrapper, (DOMElement, DOMText)):
            new_node = new_wrapper._node
        else:
            return new_wrapper
        if ref_wrapper is None or ref_wrapper is _UNDEF:
            return self._append_child(new_wrapper)
        if isinstance(ref_wrapper, (DOMElement, DOMText)):
            ref_node = ref_wrapper._node
        else:
            return self._append_child(new_wrapper)
        # Remove from old parent
        old_parent = getattr(new_node, 'parent', None)
        if old_parent and hasattr(old_parent, 'children'):
            try:
                old_parent.children.remove(new_node)
            except ValueError:
                pass
        new_node.parent = self._node
        try:
            idx = self._node.children.index(ref_node)
            self._node.children.insert(idx, new_node)
        except ValueError:
            self._node.children.append(new_node)
        self._binding.invalidate_layout(self._node, 'insertBefore')
        self._notify_mutation('childList',
                              addedNodes=JSArray([new_wrapper]),
                              removedNodes=JSArray())
        return new_wrapper

    def _replace_child(self, new_wrapper, old_wrapper):
        self._insert_before(new_wrapper, old_wrapper)
        self._remove_child(old_wrapper)
        return old_wrapper

    def _matches(self, sel):
        try:
            return _selector_mod.matches(self._node, sel)
        except Exception as exc:
            _logger.debug('matches(%r) error: %s', sel, exc)
            return False

    def _closest(self, sel):
        node = self._node
        while node and isinstance(node, Element):
            try:
                if _selector_mod.matches(node, sel):
                    return self._binding.wrap(node)
            except Exception as _exc:
                _logger.debug("Ignored: %s", _exc)
            node = getattr(node, 'parent', None)
        return None

    def _get_bounding_rect(self):
        box = getattr(self._node, 'box', None)
        if box:
            return JSObject({
                'x': box.x, 'y': box.y,
                'width': box.content_width, 'height': box.content_height,
                'top': box.y, 'left': box.x,
                'right': box.x + box.content_width,
                'bottom': box.y + box.content_height,
            })
        return JSObject({'x': 0, 'y': 0, 'width': 0, 'height': 0,
                         'top': 0, 'left': 0, 'right': 0, 'bottom': 0})

    def _get_dataset(self):
        ds = JSObject()
        if hasattr(self._node, 'attributes'):
            for k, v in self._node.attributes.items():
                if k.startswith('data-'):
                    # Convert data-some-name to someName
                    camel = _data_attr_to_camel(k[5:])
                    ds[camel] = v
        return ds

    def _make_class_list(self):
        node = self._node
        cl = JSObject()
        cl['add'] = lambda *classes: _classList_add(node, classes, self._binding)
        cl['remove'] = lambda *classes: _classList_remove(node, classes, self._binding)
        cl['toggle'] = lambda cls: _classList_toggle(node, cls, self._binding)
        cl['contains'] = lambda cls: cls in (node.attributes.get('class', '') if hasattr(node, 'attributes') else '').split()
        cl['replace'] = lambda old, new: _classList_replace(node, old, new, self._binding)
        return cl

    def _add_event_listener(self, event, fn, opts=_UNDEF):
        capture = False
        if isinstance(opts, dict):
            capture = bool(opts.get('capture', False))
        elif opts is True:
            capture = True
        self._event_listeners.setdefault(_to_str(event), []).append(
            {'fn': fn, 'capture': capture}
        )

    def _remove_event_listener(self, event, fn):
        listeners = self._event_listeners.get(_to_str(event), [])
        self._event_listeners[_to_str(event)] = [
            entry for entry in listeners if entry['fn'] is not fn
        ]

    def _dispatch(self, event):
        if not isinstance(event, JSEvent):
            return True
        return _dispatch_event(self._node, event, self._binding)

    def _notify_mutation(self, record_type: str, **kwargs):
        """Notify any MutationObservers watching this node."""
        record = JSObject({'type': record_type, 'target': self, **kwargs})
        for obs in self._binding._mutation_observers:
            for (target, opts) in obs._observed:
                if target is self:
                    should_notify = False
                    if record_type == 'childList' and opts.get('childList'):
                        should_notify = True
                    elif record_type == 'attributes' and opts.get('attributes'):
                        should_notify = True
                    elif record_type == 'characterData' and opts.get('characterData'):
                        should_notify = True
                    if should_notify:
                        obs.notify(record)
                    break


class DOMText(JSObject):
    """Wraps a Text node for JavaScript."""
    def __init__(self, node, binding):
        super().__init__()
        self._node = node
        self._binding = binding
        self['nodeType'] = 3
        self['nodeName'] = '#text'

    def __getitem__(self, key):
        if key == 'textContent' or key == 'data' or key == 'nodeValue':
            return self._node.data
        if key == 'parentNode' or key == 'parentElement':
            p = getattr(self._node, 'parent', None)
            return self._binding.wrap(p) if p else None
        return super().__getitem__(key) if key in dict.keys(self) else _UNDEF

    def __setitem__(self, key, value):
        if key in ('textContent', 'data', 'nodeValue'):
            old_value = self._node.data
            self._node.data = _to_str(value)
            self._binding.invalidate_layout(self._node, 'characterData')
            parent = getattr(self._node, 'parent', None)
            if parent is not None:
                parent_wrapper = self._binding.wrap(parent)
                if isinstance(parent_wrapper, DOMElement):
                    parent_wrapper._notify_mutation(
                        'characterData',
                        oldValue=old_value,
                    )
            return
        super().__setitem__(key, value)


class StyleProxy(JSObject):
    """Proxy for element.style that reads/writes to node.style dict."""
    def __init__(self, node, binding):
        super().__init__()
        self._node = node
        self._binding = binding

    def __getitem__(self, key):
        style = getattr(self._node, 'style', {}) or {}
        css_prop = _camel_to_css(key)
        return style.get(css_prop, '')

    def __setitem__(self, key, value):
        if not hasattr(self._node, 'style') or self._node.style is None:
            self._node.style = {}
        css_prop = _camel_to_css(key)
        self._node.style[css_prop] = _to_str(value)
        self._binding.invalidate_style(self._node, f'inline-style:{css_prop}')

    def __contains__(self, key):
        style = getattr(self._node, 'style', {}) or {}
        return _camel_to_css(key) in style


class DOMBinding:
    """Binds JavaScript interpreter to the DOM tree."""

    def __init__(self, document, interpreter: Interpreter, invalidation_graph=None):
        self.document = document
        self.interpreter = interpreter
        self._node_cache = {}  # id(node) -> wrapper
        self._mutation_observers: list = []  # JSMutationObserver instances
        self._custom_elements: dict = {}   # tag -> (constructor_fn, options)
        self._invalidation_graph = invalidation_graph

    def setup(self) -> None:
        """Set up document, window, console objects in JS scope."""
        g = self.interpreter.global_env
        doc_obj = self._make_document()
        g.define('document', doc_obj)

        # MutationObserver constructor
        binding = self
        def _mo_ctor(callback):
            obs = JSMutationObserver(callback, binding)
            binding._mutation_observers.append(obs)
            return obs
        g.define('MutationObserver', _mo_ctor)

        # IntersectionObserver stub
        def _io_ctor(callback, options=_UNDEF):
            obj = JSObject()
            obj['observe'] = lambda el: None
            obj['unobserve'] = lambda el: None
            obj['disconnect'] = lambda: None
            return obj
        g.define('IntersectionObserver', _io_ctor)

        # ResizeObserver stub
        def _ro_ctor(callback):
            obj = JSObject()
            obj['observe'] = lambda el, opts=_UNDEF: None
            obj['unobserve'] = lambda el: None
            obj['disconnect'] = lambda: None
            return obj
        g.define('ResizeObserver', _ro_ctor)

        # CustomEvent constructor
        def _custom_event(type_str, init=_UNDEF):
            bubbles = False
            cancelable = False
            detail = _UNDEF
            if isinstance(init, dict):
                bubbles = bool(init.get('bubbles', False))
                cancelable = bool(init.get('cancelable', False))
                detail = init.get('detail', _UNDEF)
            return JSEvent(_to_str(type_str), bubbles=bubbles,
                           cancelable=cancelable, detail=detail)
        g.define('CustomEvent', _custom_event)

        # Event constructor
        def _event_ctor(type_str, init=_UNDEF):
            bubbles = False
            cancelable = False
            if isinstance(init, dict):
                bubbles = bool(init.get('bubbles', False))
                cancelable = bool(init.get('cancelable', False))
            return JSEvent(_to_str(type_str), bubbles=bubbles, cancelable=cancelable)
        g.define('Event', _event_ctor)

        # customElements registry
        ce_registry = JSObject()
        ce_registry['define'] = lambda name, ctor, opts=_UNDEF: self._register_custom_element(name, ctor, opts)
        ce_registry['get'] = lambda name: self._custom_elements.get(_to_str(name), (None,))[0]
        ce_registry['whenDefined'] = lambda name: _UNDEF
        g.define('customElements', ce_registry)

        # localStorage / sessionStorage
        ls = _make_storage()
        ss = _make_storage()
        g.define('localStorage', ls)
        g.define('sessionStorage', ss)

        # Also set on window
        window = g.get('window')
        if isinstance(window, dict):
            window['document'] = doc_obj
            window['MutationObserver'] = g.get('MutationObserver')
            window['IntersectionObserver'] = g.get('IntersectionObserver')
            window['ResizeObserver'] = g.get('ResizeObserver')
            window['CustomEvent'] = _custom_event
            window['Event'] = _event_ctor
            window['customElements'] = ce_registry
            window['localStorage'] = ls
            window['sessionStorage'] = ss

    def _register_custom_element(self, name, ctor, options=_UNDEF):
        tag = _to_str(name).lower()
        self._custom_elements[tag] = (ctor, options if isinstance(options, dict) else {})
        # Upgrade any existing elements in the DOM
        self._upgrade_custom_elements(tag, ctor)

    def wrap(self, node):
        """Wrap a DOM node into a JS-accessible object."""
        if node is None:
            return None
        nid = id(node)
        if nid in self._node_cache:
            return self._node_cache[nid]
        if isinstance(node, Element):
            w = DOMElement(node, self)
        elif isinstance(node, Text):
            w = DOMText(node, self)
        else:
            return None
        self._node_cache[nid] = w
        return w

    def _upgrade_custom_elements(self, tag: str, ctor):
        """Call constructor/connectedCallback on existing matching elements."""
        for el in _walk(self.document):
            if el.tag == tag:
                wrapper = self.wrap(el)
                try:
                    if self.interpreter:
                        self.interpreter._call_value(ctor, [wrapper])
                except Exception as exc:
                    _logger.debug('Custom element upgrade error: %s', exc)

    def _make_document(self):
        doc = JSObject()
        doc['nodeType'] = 9
        doc['nodeName'] = '#document'

        doc['getElementById'] = lambda id_: self._get_by_id(id_)
        doc['getElementsByClassName'] = lambda cls: self.get_elements_by_class(self.document, cls)
        doc['getElementsByTagName'] = lambda tag: self.get_elements_by_tag(self.document, tag)
        doc['querySelector'] = lambda sel: self.query_selector(self.document, sel)
        doc['querySelectorAll'] = lambda sel: self.query_selector_all(self.document, sel)
        doc['createElement'] = lambda tag: self._create_element(tag)
        doc['createTextNode'] = lambda text: self._create_text_node(text)
        doc['createDocumentFragment'] = lambda: self._create_element('__fragment__')
        doc['createComment'] = lambda text: None
        _doc_listeners: dict = {}
        def _doc_add_listener(ev, fn, opts=_UNDEF):
            _doc_listeners.setdefault(_to_str(ev), []).append(fn)
        def _doc_remove_listener(ev, fn, opts=_UNDEF):
            lst = _doc_listeners.get(_to_str(ev), [])
            _doc_listeners[_to_str(ev)] = [f for f in lst if f is not fn]
        doc['addEventListener'] = _doc_add_listener
        doc['removeEventListener'] = _doc_remove_listener
        doc['dispatchEvent'] = lambda ev: True

        # document.body / document.head / document.documentElement
        doc['body'] = self._find_and_wrap('body')
        doc['head'] = self._find_and_wrap('head')
        doc['documentElement'] = self._find_and_wrap('html')
        doc['title'] = ''
        doc['readyState'] = 'complete'
        doc['cookie'] = ''
        location = (
            self.interpreter.global_env.get('window')['location']
            if isinstance(self.interpreter.global_env.get('window'), dict)
            else JSObject()
        )
        doc['location'] = location
        doc['URL'] = location.get('href', '')
        doc['documentURI'] = location.get('href', '')
        doc['baseURI'] = location.get('href', '')
        doc['referrer'] = ''
        doc['currentScript'] = None

        return doc

    def _find_and_wrap(self, tag):
        node = _find_element_by_tag(self.document, tag)
        return self.wrap(node) if node else None

    def _get_by_id(self, id_str):
        node = _find_element_by_id(self.document, _to_str(id_str))
        return self.wrap(node) if node else None

    def _create_element(self, tag):
        el = Element(_to_str(tag).lower())
        el.style = {}
        el.parent = None
        return self.wrap(el)

    def _create_text_node(self, text):
        t = Text(_to_str(text))
        t.parent = None
        return self.wrap(t)

    def query_selector(self, root, sel):
        try:
            node = _query_one(root, _to_str(sel), _selector_mod)
            return self.wrap(node) if node else None
        except Exception as exc:
            _logger.debug('querySelector(%r) error: %s', sel, exc)
            return None

    def query_selector_all(self, root, sel):
        try:
            nodes = _query_all(root, _to_str(sel), _selector_mod)
            return JSArray(self.wrap(n) for n in nodes)
        except Exception as exc:
            _logger.debug('querySelectorAll(%r) error: %s', sel, exc)
            return JSArray()

    def get_elements_by_tag(self, root, tag):
        tag = _to_str(tag).lower()
        return JSArray(self.wrap(n) for n in _walk(root) if isinstance(n, Element) and n.tag == tag)

    def get_elements_by_class(self, root, cls):
        cls = _to_str(cls)
        return JSArray(
            self.wrap(n) for n in _walk(root)
            if isinstance(n, Element) and cls in n.attributes.get('class', '').split()
        )

    def invalidate_style(self, node, reason: str = '') -> None:
        if self._invalidation_graph is not None:
            self._invalidation_graph.mark_style(node, reason)
        get_event_loop().request_render()

    def invalidate_layout(self, node, reason: str = '') -> None:
        if self._invalidation_graph is not None:
            self._invalidation_graph.mark_layout(node, reason)
        get_event_loop().request_render()

    def invalidate_paint(self, node, reason: str = '') -> None:
        if self._invalidation_graph is not None:
            self._invalidation_graph.mark_paint(node, reason)
        get_event_loop().request_render()


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _make_storage() -> JSObject:
    """Create a localStorage/sessionStorage object (in-memory, per-session)."""
    store: dict = {}
    obj = JSObject()
    obj['getItem'] = lambda k: store.get(_to_str(k), None)
    obj['setItem'] = lambda k, v: store.__setitem__(_to_str(k), _to_str(v))
    obj['removeItem'] = lambda k: store.pop(_to_str(k), None)
    obj['clear'] = lambda: store.clear()
    obj['key'] = lambda n: list(store.keys())[int(n)] if 0 <= int(n) < len(store) else None
    # length as a callable (JS would be a property, we approximate)
    obj['length'] = 0
    _orig_set = obj['setItem']
    _orig_del = obj['removeItem']
    _orig_clr = obj['clear']
    def _set(k, v):
        _orig_set(k, v)
        obj['length'] = len(store)
    def _del(k):
        _orig_del(k)
        obj['length'] = len(store)
    def _clr():
        _orig_clr()
        obj['length'] = 0
    obj['setItem'] = _set
    obj['removeItem'] = _del
    obj['clear'] = _clr
    return obj


def _walk(node):
    """Yield all descendant elements."""
    for child in getattr(node, 'children', []):
        if isinstance(child, Element):
            yield child
            yield from _walk(child)


def _find_element_by_id(root, id_str):
    for el in _walk(root):
        if el.attributes.get('id') == id_str:
            return el
    return None


def _find_element_by_tag(root, tag):
    for el in _walk(root):
        if el.tag == tag:
            return el
    return None


def _find_body(doc):
    return _find_element_by_tag(doc, 'body')


def _query_one(root, sel, selector_mod):
    for el in _walk(root):
        try:
            if selector_mod.matches(el, sel):
                return el
        except Exception:
            continue
    return None


def _query_all(root, sel, selector_mod):
    results = []
    for el in _walk(root):
        try:
            if selector_mod.matches(el, sel):
                results.append(el)
        except Exception:
            continue
    return results


def _extract_text(node):
    if isinstance(node, Text):
        return node.data
    parts = []
    for child in getattr(node, 'children', []):
        parts.append(_extract_text(child))
    return ''.join(parts)


def _serialize_node(node):
    if isinstance(node, Text):
        return node.data
    if isinstance(node, Element):
        attrs = ''
        for k, v in node.attributes.items():
            attrs += f' {k}="{v}"'
        inner = ''.join(_serialize_node(c) for c in node.children)
        return f'<{node.tag}{attrs}>{inner}</{node.tag}>'
    return ''


def _camel_to_css(name):
    """Convert camelCase to kebab-case: backgroundColor → background-color."""
    import re
    if '-' in name:
        return name  # already kebab
    return re.sub(r'([A-Z])', lambda m: '-' + m.group(1).lower(), name)


def _data_attr_to_camel(name):
    """Convert data-some-name to someName."""
    parts = name.split('-')
    return parts[0] + ''.join(p.capitalize() for p in parts[1:])


def _classList_add(node, classes, binding=None):
    if not hasattr(node, 'attributes'):
        return
    old_value = node.attributes.get('class')
    current = set(node.attributes.get('class', '').split())
    for cls in classes:
        current.add(_to_str(cls))
    node.attributes['class'] = ' '.join(current)
    if binding is not None:
        binding.invalidate_style(node, 'classList:add')
        wrapper = binding.wrap(node)
        if isinstance(wrapper, DOMElement):
            wrapper._notify_mutation('attributes', attributeName='class', oldValue=old_value)


def _classList_remove(node, classes, binding=None):
    if not hasattr(node, 'attributes'):
        return
    old_value = node.attributes.get('class')
    current = set(node.attributes.get('class', '').split())
    for cls in classes:
        current.discard(_to_str(cls))
    node.attributes['class'] = ' '.join(current)
    if binding is not None:
        binding.invalidate_style(node, 'classList:remove')
        wrapper = binding.wrap(node)
        if isinstance(wrapper, DOMElement):
            wrapper._notify_mutation('attributes', attributeName='class', oldValue=old_value)


def _classList_toggle(node, cls, binding=None):
    if not hasattr(node, 'attributes'):
        return False
    old_value = node.attributes.get('class')
    current = set(node.attributes.get('class', '').split())
    cls = _to_str(cls)
    if cls in current:
        current.discard(cls)
        node.attributes['class'] = ' '.join(current)
        if binding is not None:
            binding.invalidate_style(node, 'classList:toggle')
            wrapper = binding.wrap(node)
            if isinstance(wrapper, DOMElement):
                wrapper._notify_mutation('attributes', attributeName='class', oldValue=old_value)
        return False
    current.add(cls)
    node.attributes['class'] = ' '.join(current)
    if binding is not None:
        binding.invalidate_style(node, 'classList:toggle')
        wrapper = binding.wrap(node)
        if isinstance(wrapper, DOMElement):
            wrapper._notify_mutation('attributes', attributeName='class', oldValue=old_value)
    return True


def _classList_replace(node, old, new, binding=None):
    if not hasattr(node, 'attributes'):
        return False
    old_value = node.attributes.get('class')
    current = node.attributes.get('class', '').split()
    old, new = _to_str(old), _to_str(new)
    if old in current:
        current = [new if c == old else c for c in current]
        node.attributes['class'] = ' '.join(current)
        if binding is not None:
            binding.invalidate_style(node, 'classList:replace')
            wrapper = binding.wrap(node)
            if isinstance(wrapper, DOMElement):
                wrapper._notify_mutation('attributes', attributeName='class', oldValue=old_value)
        return True
    return False
