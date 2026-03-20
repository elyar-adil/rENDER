"""DOM/BOM API bindings for the JavaScript interpreter."""
from js.interpreter import (
    Interpreter, Environment, JSObject, JSArray, JSFunction,
    _UNDEF, _to_str, _to_bool, _get_property, _set_property,
)
from js.parser import _UNDEF as UNDEF
from html.dom import Element, Text, Document


class DOMElement(JSObject):
    """Wraps a DOM Element for JavaScript access."""

    def __init__(self, node, binding):
        super().__init__()
        self._node = node
        self._binding = binding
        self._event_listeners = {}  # event_name -> [fn, ...]

        # Properties
        self['nodeType'] = 1
        self['tagName'] = node.tag.upper() if hasattr(node, 'tag') else ''
        self['nodeName'] = self['tagName']
        self['id'] = node.attributes.get('id', '') if hasattr(node, 'attributes') else ''
        self['className'] = node.attributes.get('class', '') if hasattr(node, 'attributes') else ''

        # Methods
        self['getAttribute'] = lambda name: node.attributes.get(name, None) if hasattr(node, 'attributes') else None
        self['setAttribute'] = lambda name, val: self._set_attr(name, val)
        self['removeAttribute'] = lambda name: node.attributes.pop(name, None) if hasattr(node, 'attributes') else None
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

        self['addEventListener'] = lambda ev, fn, *a: self._add_event_listener(ev, fn)
        self['removeEventListener'] = lambda ev, fn, *a: self._remove_event_listener(ev, fn)
        self['dispatchEvent'] = lambda ev: None

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
        self['style'] = StyleProxy(node)

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
            return
        if key == 'id':
            if hasattr(self._node, 'attributes'):
                self._node.attributes['id'] = _to_str(value)
            return
        if key == 'value':
            if hasattr(self._node, 'attributes'):
                self._node.attributes['value'] = _to_str(value)
            return
        if key == 'href':
            if hasattr(self._node, 'attributes'):
                self._node.attributes['href'] = _to_str(value)
            return
        if key == 'src':
            if hasattr(self._node, 'attributes'):
                self._node.attributes['src'] = _to_str(value)
            return
        super().__setitem__(key, value)

    def _set_attr(self, name, val):
        if hasattr(self._node, 'attributes'):
            self._node.attributes[name] = _to_str(val)

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
            self._node.children = list(body.children)
            for child in self._node.children:
                child.parent = self._node
        except Exception:
            pass

    def _set_text_content(self, text):
        t = Text(_to_str(text))
        t.parent = self._node
        self._node.children = [t]

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
        return new_wrapper

    def _replace_child(self, new_wrapper, old_wrapper):
        self._insert_before(new_wrapper, old_wrapper)
        self._remove_child(old_wrapper)
        return old_wrapper

    def _matches(self, sel):
        try:
            import css.selector as selector_mod
            return selector_mod.matches(self._node, sel)
        except Exception:
            return False

    def _closest(self, sel):
        node = self._node
        while node and isinstance(node, Element):
            try:
                import css.selector as selector_mod
                if selector_mod.matches(node, sel):
                    return self._binding.wrap(node)
            except Exception:
                pass
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
        cl['add'] = lambda *classes: _classList_add(node, classes)
        cl['remove'] = lambda *classes: _classList_remove(node, classes)
        cl['toggle'] = lambda cls: _classList_toggle(node, cls)
        cl['contains'] = lambda cls: cls in (node.attributes.get('class', '') if hasattr(node, 'attributes') else '').split()
        cl['replace'] = lambda old, new: _classList_replace(node, old, new)
        return cl

    def _add_event_listener(self, event, fn):
        self._event_listeners.setdefault(event, []).append(fn)

    def _remove_event_listener(self, event, fn):
        listeners = self._event_listeners.get(event, [])
        try:
            listeners.remove(fn)
        except ValueError:
            pass


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
            self._node.data = _to_str(value)
            return
        super().__setitem__(key, value)


class StyleProxy(JSObject):
    """Proxy for element.style that reads/writes to node.style dict."""
    def __init__(self, node):
        super().__init__()
        self._node = node

    def __getitem__(self, key):
        style = getattr(self._node, 'style', {}) or {}
        css_prop = _camel_to_css(key)
        return style.get(css_prop, '')

    def __setitem__(self, key, value):
        if not hasattr(self._node, 'style') or self._node.style is None:
            self._node.style = {}
        css_prop = _camel_to_css(key)
        self._node.style[css_prop] = _to_str(value)

    def __contains__(self, key):
        style = getattr(self._node, 'style', {}) or {}
        return _camel_to_css(key) in style


class DOMBinding:
    """Binds JavaScript interpreter to the DOM tree."""

    def __init__(self, document, interpreter: Interpreter):
        self.document = document
        self.interpreter = interpreter
        self._node_cache = {}  # id(node) -> wrapper

    def setup(self) -> None:
        """Set up document, window, console objects in JS scope."""
        g = self.interpreter.global_env
        doc_obj = self._make_document()
        g.define('document', doc_obj)

        # Also set on window
        window = g.get('window')
        if isinstance(window, dict):
            window['document'] = doc_obj

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
        doc['addEventListener'] = lambda *a: None
        doc['removeEventListener'] = lambda *a: None

        # document.body / document.head / document.documentElement
        doc['body'] = self._find_and_wrap('body')
        doc['head'] = self._find_and_wrap('head')
        doc['documentElement'] = self._find_and_wrap('html')
        doc['title'] = ''
        doc['readyState'] = 'complete'
        doc['cookie'] = ''
        doc['location'] = self.interpreter.global_env.get('window')['location'] if isinstance(self.interpreter.global_env.get('window'), dict) else JSObject()

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
            import css.selector as selector_mod
            node = _query_one(root, _to_str(sel), selector_mod)
            return self.wrap(node) if node else None
        except Exception:
            return None

    def query_selector_all(self, root, sel):
        try:
            import css.selector as selector_mod
            nodes = _query_all(root, _to_str(sel), selector_mod)
            return JSArray(self.wrap(n) for n in nodes)
        except Exception:
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


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

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


def _classList_add(node, classes):
    if not hasattr(node, 'attributes'):
        return
    current = set(node.attributes.get('class', '').split())
    for cls in classes:
        current.add(_to_str(cls))
    node.attributes['class'] = ' '.join(current)


def _classList_remove(node, classes):
    if not hasattr(node, 'attributes'):
        return
    current = set(node.attributes.get('class', '').split())
    for cls in classes:
        current.discard(_to_str(cls))
    node.attributes['class'] = ' '.join(current)


def _classList_toggle(node, cls):
    if not hasattr(node, 'attributes'):
        return False
    current = set(node.attributes.get('class', '').split())
    cls = _to_str(cls)
    if cls in current:
        current.discard(cls)
        node.attributes['class'] = ' '.join(current)
        return False
    current.add(cls)
    node.attributes['class'] = ' '.join(current)
    return True


def _classList_replace(node, old, new):
    if not hasattr(node, 'attributes'):
        return False
    current = node.attributes.get('class', '').split()
    old, new = _to_str(old), _to_str(new)
    if old in current:
        current = [new if c == old else c for c in current]
        node.attributes['class'] = ' '.join(current)
        return True
    return False
