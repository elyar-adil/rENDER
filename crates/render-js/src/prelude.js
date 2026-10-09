// Built-ins that are written in JavaScript. Installed once, before the first
// script, by `JsRuntime::ensure_prelude`. Everything here is defined only when
// the name is absent, as non-enumerable, writable and configurable properties,
// the way the specification defines built-in methods.
(function (global) {
  var defineProperty = Object.defineProperty;
  var slice = Array.prototype.slice;

  function define(target, name, value) {
    if (target[name] === undefined) {
      defineProperty(target, name, {
        value: value,
        writable: true,
        configurable: true,
        enumerable: false
      });
    }
  }

  function defineGetter(target, name, getter, setter) {
    if (!(name in target)) {
      defineProperty(target, name, {
        get: getter,
        set: setter,
        configurable: true,
        enumerable: false
      });
    }
  }

  function toIntegerOrInfinity(value) {
    var number = Number(value);
    if (number !== number) return 0;
    if (number === Infinity || number === -Infinity) return number;
    return number < 0 ? Math.ceil(number) : Math.floor(number);
  }

  function relativeIndex(value, length, fallback) {
    if (value === undefined) return fallback;
    var relative = toIntegerOrInfinity(value);
    if (relative < 0) return Math.max(length + relative, 0);
    return Math.min(relative, length);
  }

  // ---------------------------------------------------------------- Number
  define(Number, 'parseFloat', parseFloat);
  define(Number, 'parseInt', parseInt);

  // ---------------------------------------------------------------- Object
  define(Object, 'is', function is(a, b) {
    if (a === b) return a !== 0 || 1 / a === 1 / b;
    return a !== a && b !== b;
  });
  define(Object, 'fromEntries', function fromEntries(iterable) {
    var result = {};
    for (var entry of iterable) {
      if (entry === null || typeof entry !== 'object') {
        throw new TypeError('Iterator value ' + entry + ' is not an entry object');
      }
      result[entry[0]] = entry[1];
    }
    return result;
  });
  define(Object, 'groupBy', function groupBy(items, callback) {
    var groups = Object.create(null);
    var index = 0;
    for (var item of items) {
      var key = callback(item, index++);
      if (!(key in groups)) groups[key] = [];
      groups[key].push(item);
    }
    return groups;
  });

  // ----------------------------------------------------------------- Array
  define(Array, 'of', function of() {
    return slice.call(arguments);
  });
  define(Array.prototype, 'fill', function fill(value, start, end) {
    var length = this.length >>> 0;
    var from = relativeIndex(start, length, 0);
    var to = relativeIndex(end, length, length);
    for (var i = from; i < to; i++) this[i] = value;
    return this;
  });
  define(Array.prototype, 'flatMap', function flatMap(callback, thisArg) {
    var result = [];
    var length = this.length >>> 0;
    for (var i = 0; i < length; i++) {
      if (!(i in this)) continue;
      var mapped = callback.call(thisArg, this[i], i, this);
      if (Array.isArray(mapped)) {
        for (var j = 0; j < mapped.length; j++) result.push(mapped[j]);
      } else {
        result.push(mapped);
      }
    }
    return result;
  });
  define(Array.prototype, 'lastIndexOf', function lastIndexOf(search, fromIndex) {
    var length = this.length >>> 0;
    var start = arguments.length > 1 ? toIntegerOrInfinity(fromIndex) : length - 1;
    var i = start >= 0 ? Math.min(start, length - 1) : length + start;
    for (; i >= 0; i--) {
      if (i in this && this[i] === search) return i;
    }
    return -1;
  });
  define(Array.prototype, 'copyWithin', function copyWithin(target, start, end) {
    var length = this.length >>> 0;
    var to = relativeIndex(target, length, 0);
    var from = relativeIndex(start, length, 0);
    var final = relativeIndex(end, length, length);
    var count = Math.min(final - from, length - to);
    var step = 1;
    if (from < to && to < from + count) {
      step = -1;
      from += count - 1;
      to += count - 1;
    }
    for (; count > 0; count--, from += step, to += step) {
      if (from in this) this[to] = this[from];
      else delete this[to];
    }
    return this;
  });
  define(Array.prototype, 'toReversed', function toReversed() {
    return slice.call(this).reverse();
  });
  define(Array.prototype, 'toSorted', function toSorted(compare) {
    if (compare !== undefined && typeof compare !== 'function') {
      throw new TypeError('The comparison function must be either a function or undefined');
    }
    return slice.call(this).sort(compare);
  });
  define(Array.prototype, 'toSpliced', function toSpliced(start, deleteCount) {
    var copy = slice.call(this);
    copy.splice.apply(copy, slice.call(arguments));
    return copy;
  });
  define(Array.prototype, 'with', function (index, value) {
    var length = this.length >>> 0;
    var relative = toIntegerOrInfinity(index);
    var actual = relative >= 0 ? relative : length + relative;
    if (actual >= length || actual < 0) throw new RangeError('Invalid index : ' + index);
    var copy = slice.call(this);
    copy[actual] = value;
    return copy;
  });

  // ---------------------------------------------------------------- String
  define(String.prototype, 'trimLeft', String.prototype.trimStart);
  define(String.prototype, 'trimRight', String.prototype.trimEnd);
  define(String.prototype, 'toLocaleLowerCase', function toLocaleLowerCase() {
    return String(this).toLowerCase();
  });
  define(String.prototype, 'toLocaleUpperCase', function toLocaleUpperCase() {
    return String(this).toUpperCase();
  });

  // --------------------------------------------------------------- Promise
  define(Promise, 'withResolvers', function withResolvers() {
    var resolve;
    var reject;
    var promise = new Promise(function (onFulfilled, onRejected) {
      resolve = onFulfilled;
      reject = onRejected;
    });
    return { promise: promise, resolve: resolve, reject: reject };
  });

  // ----------------------------------------------------------------- Error
  define(Error, 'captureStackTrace', function captureStackTrace(target) {
    if (target !== null && (typeof target === 'object' || typeof target === 'function')) {
      defineProperty(target, 'stack', {
        value: String(target.name || 'Error') + (target.message ? ': ' + target.message : ''),
        writable: true,
        configurable: true,
        enumerable: false
      });
    }
  });

  // ------------------------------------------------------------- WeakRef
  // A strong reference: it never lets its target go. The specification allows
  // a collector to keep a target alive for as long as it likes, and does not
  // let a program observe the difference.
  if (typeof WeakRef === 'undefined') {
    var WeakRefImpl = function WeakRef(target) {
      if (target === null || (typeof target !== 'object' && typeof target !== 'function')) {
        throw new TypeError('WeakRef: target must be an object');
      }
      defineProperty(this, '__target', { value: target });
    };
    defineProperty(WeakRefImpl.prototype, 'deref', {
      value: function deref() { return this.__target; },
      writable: true, configurable: true, enumerable: false
    });
    defineProperty(global, 'WeakRef', {
      value: WeakRefImpl, writable: true, configurable: true, enumerable: false
    });
  }
  // Cleanup callbacks are allowed never to run, which is what happens here.
  if (typeof FinalizationRegistry === 'undefined') {
    var FinalizationRegistryImpl = function FinalizationRegistry(callback) {
      if (typeof callback !== 'function') {
        throw new TypeError('FinalizationRegistry: cleanup must be callable');
      }
    };
    defineProperty(FinalizationRegistryImpl.prototype, 'register', {
      value: function register() {}, writable: true, configurable: true, enumerable: false
    });
    defineProperty(FinalizationRegistryImpl.prototype, 'unregister', {
      value: function unregister() { return false; },
      writable: true, configurable: true, enumerable: false
    });
    defineProperty(global, 'FinalizationRegistry', {
      value: FinalizationRegistryImpl, writable: true, configurable: true, enumerable: false
    });
  }

  // ---------------------------------------------------------------- timers
  define(global, 'setImmediate', function setImmediate(callback) {
    var args = slice.call(arguments, 1);
    return setTimeout(function () { callback.apply(undefined, args); }, 0);
  });
  define(global, 'clearImmediate', function clearImmediate(handle) {
    clearTimeout(handle);
  });
  define(global, 'requestIdleCallback', function requestIdleCallback(callback) {
    var start = Date.now();
    return setTimeout(function () {
      callback({
        didTimeout: false,
        timeRemaining: function () { return Math.max(0, 50 - (Date.now() - start)); }
      });
    }, 1);
  });
  define(global, 'cancelIdleCallback', function cancelIdleCallback(handle) {
    clearTimeout(handle);
  });

  // ------------------------------------------------------------ EventTarget
  var nodePrototype = Node.prototype;
  var eventTargetPrototype = Object.getPrototypeOf(nodePrototype);
  var listeners = new WeakMap();

  function listenerOptions(options) {
    if (options !== null && typeof options === 'object') {
      return {
        capture: !!options.capture,
        once: !!options.once,
        passive: !!options.passive,
        signal: options.signal
      };
    }
    return { capture: !!options, once: false, passive: false, signal: undefined };
  }

  function EventTarget() {
    if (!(this instanceof EventTarget)) {
      throw new TypeError("Failed to construct 'EventTarget': Please use the 'new' operator");
    }
  }
  defineProperty(EventTarget, 'prototype', { value: eventTargetPrototype });
  defineProperty(eventTargetPrototype, 'constructor', {
    value: EventTarget, writable: true, configurable: true, enumerable: false
  });
  defineProperty(global, 'EventTarget', {
    value: EventTarget, writable: true, configurable: true, enumerable: false
  });

  // These serve targets that are not DOM nodes (`new EventTarget()` and its
  // subclasses); nodes and the window keep their native implementations, which
  // shadow these on `Node.prototype`.
  define(eventTargetPrototype, 'addEventListener', function addEventListener(type, callback, options) {
    if (callback === null || callback === undefined) return;
    var settings = listenerOptions(options);
    if (settings.signal && settings.signal.aborted) return;
    var byType = listeners.get(this);
    if (!byType) {
      byType = Object.create(null);
      listeners.set(this, byType);
    }
    type = String(type);
    var list = byType[type] || (byType[type] = []);
    for (var i = 0; i < list.length; i++) {
      if (list[i].callback === callback && list[i].capture === settings.capture) return;
    }
    list.push({ callback: callback, capture: settings.capture, once: settings.once });
    if (settings.signal) {
      var target = this;
      settings.signal.addEventListener('abort', function () {
        target.removeEventListener(type, callback, settings.capture);
      });
    }
  });
  define(eventTargetPrototype, 'removeEventListener', function removeEventListener(type, callback, options) {
    var byType = listeners.get(this);
    var list = byType && byType[String(type)];
    if (!list) return;
    var capture = listenerOptions(options).capture;
    for (var i = 0; i < list.length; i++) {
      if (list[i].callback === callback && list[i].capture === capture) {
        list.splice(i, 1);
        return;
      }
    }
  });
  define(eventTargetPrototype, 'dispatchEvent', function dispatchEvent(event) {
    if (event === null || typeof event !== 'object' || typeof event.type !== 'string') {
      throw new TypeError("Failed to execute 'dispatchEvent': parameter 1 is not of type 'Event'.");
    }
    var byType = listeners.get(this);
    var list = byType && byType[event.type];
    event.target = this;
    event.srcElement = this;
    event.currentTarget = this;
    event.eventPhase = 2;
    if (list) {
      var snapshot = list.slice();
      for (var i = 0; i < snapshot.length; i++) {
        var record = snapshot[i];
        if (list.indexOf(record) === -1) continue;
        if (record.once) list.splice(list.indexOf(record), 1);
        try {
          if (typeof record.callback === 'function') record.callback.call(this, event);
          else if (record.callback && typeof record.callback.handleEvent === 'function') {
            record.callback.handleEvent(event);
          }
        } catch (error) {
          reportError(error);
        }
        // A non-node target has no propagation path, so the only flag that
        // matters is the immediate one; `stopPropagation` shares the marker.
        if (event.cancelBubble === true) break;
      }
    }
    event.currentTarget = null;
    event.eventPhase = 0;
    return !event.defaultPrevented;
  });
  define(global, 'reportError', function reportError(error) {
    setTimeout(function () { throw error; }, 0);
  });

  // ----------------------------------------------------------------- events
  function defineEvent(name, Parent, defaults) {
    var constructor = function (type, init) {
      if (arguments.length === 0) {
        throw new TypeError("Failed to construct '" + name + "': 1 argument required, but only 0 present.");
      }
      var event = new Parent(type, init);
      for (var key in defaults) {
        event[key] = init && init[key] !== undefined ? init[key] : defaults[key];
      }
      Object.setPrototypeOf(event, constructor.prototype);
      return event;
    };
    defineProperty(constructor, 'name', { value: name, configurable: true });
    constructor.prototype = Object.create(Parent.prototype, {
      constructor: { value: constructor, writable: true, configurable: true, enumerable: false }
    });
    Object.setPrototypeOf(constructor, Parent);
    defineProperty(global, name, {
      value: constructor, writable: true, configurable: true, enumerable: false
    });
    return constructor;
  }
  var keyModifiers = { ctrlKey: false, shiftKey: false, altKey: false, metaKey: false };
  function withModifiers(extra) {
    var merged = {};
    for (var key in keyModifiers) merged[key] = false;
    for (var other in extra) merged[other] = extra[other];
    return merged;
  }
  if (typeof CustomEvent === 'undefined') {
    var CustomEventImpl = defineEvent('CustomEvent', Event, { detail: null });
    define(CustomEventImpl.prototype, 'initCustomEvent', function initCustomEvent(type, bubbles, cancelable, detail) {
      this.detail = detail === undefined ? null : detail;
    });
  }
  var UIEventImpl = typeof UIEvent === 'undefined'
    ? defineEvent('UIEvent', Event, { detail: 0, view: null })
    : UIEvent;
  var MouseEventImpl = typeof MouseEvent === 'undefined'
    ? defineEvent('MouseEvent', UIEventImpl, withModifiers({
        screenX: 0, screenY: 0, clientX: 0, clientY: 0, pageX: 0, pageY: 0, offsetX: 0, offsetY: 0,
        x: 0, y: 0, movementX: 0, movementY: 0, button: 0, buttons: 0, relatedTarget: null
      }))
    : MouseEvent;
  if (typeof PointerEvent === 'undefined') {
    defineEvent('PointerEvent', MouseEventImpl, {
      pointerId: 0, width: 1, height: 1, pressure: 0, tangentialPressure: 0, tiltX: 0, tiltY: 0,
      twist: 0, pointerType: '', isPrimary: false
    });
  }
  if (typeof WheelEvent === 'undefined') {
    defineEvent('WheelEvent', MouseEventImpl, { deltaX: 0, deltaY: 0, deltaZ: 0, deltaMode: 0 });
  }
  if (typeof KeyboardEvent === 'undefined') {
    var KeyboardEventImpl = defineEvent('KeyboardEvent', UIEventImpl, withModifiers({
      key: '', code: '', location: 0, repeat: false, isComposing: false
    }));
    define(KeyboardEventImpl.prototype, 'getModifierState', function getModifierState(key) {
      return { Control: this.ctrlKey, Shift: this.shiftKey, Alt: this.altKey, Meta: this.metaKey }[key] === true;
    });
  }
  if (typeof FocusEvent === 'undefined') defineEvent('FocusEvent', UIEventImpl, { relatedTarget: null });
  if (typeof InputEvent === 'undefined') {
    defineEvent('InputEvent', UIEventImpl, { data: null, inputType: '', isComposing: false });
  }
  if (typeof CompositionEvent === 'undefined') defineEvent('CompositionEvent', UIEventImpl, { data: '' });
  if (typeof TouchEvent === 'undefined') {
    defineEvent('TouchEvent', UIEventImpl, withModifiers({ touches: [], targetTouches: [], changedTouches: [] }));
  }
  if (typeof ErrorEvent === 'undefined') {
    defineEvent('ErrorEvent', Event, { message: '', filename: '', lineno: 0, colno: 0, error: null });
  }
  if (typeof ProgressEvent === 'undefined') {
    defineEvent('ProgressEvent', Event, { lengthComputable: false, loaded: 0, total: 0 });
  }
  if (typeof HashChangeEvent === 'undefined') defineEvent('HashChangeEvent', Event, { oldURL: '', newURL: '' });
  if (typeof PopStateEvent === 'undefined') defineEvent('PopStateEvent', Event, { state: null });
  if (typeof PromiseRejectionEvent === 'undefined') {
    defineEvent('PromiseRejectionEvent', Event, { promise: undefined, reason: undefined });
  }
  if (typeof AnimationEvent === 'undefined') {
    defineEvent('AnimationEvent', Event, { animationName: '', elapsedTime: 0, pseudoElement: '' });
  }
  if (typeof TransitionEvent === 'undefined') {
    defineEvent('TransitionEvent', Event, { propertyName: '', elapsedTime: 0, pseudoElement: '' });
  }
  var MessageEventImpl = typeof MessageEvent === 'undefined'
    ? defineEvent('MessageEvent', Event, { data: null, origin: '', lastEventId: '', source: null, ports: [] })
    : MessageEvent;

  // --------------------------------------------------------------- Node DOM
  var elementPrototype = Element.prototype;
  var characterDataPrototype = CharacterData.prototype;
  var fragmentPrototype = DocumentFragment.prototype;
  var documentPrototype = Object.getPrototypeOf(document);

  function toNode(value) {
    return value !== null && typeof value === 'object' && typeof value.nodeType === 'number'
      ? value
      : document.createTextNode(String(value));
  }
  function toFragment(values) {
    var fragment = document.createDocumentFragment();
    for (var i = 0; i < values.length; i++) fragment.appendChild(toNode(values[i]));
    return fragment;
  }

  // ChildNode (DOM Standard §4.2.8), on elements and character data.
  [elementPrototype, characterDataPrototype].forEach(function (target) {
    define(target, 'before', function before() {
      var parent = this.parentNode;
      if (!parent) return;
      parent.insertBefore(toFragment(slice.call(arguments)), this);
    });
    define(target, 'after', function after() {
      var parent = this.parentNode;
      if (!parent) return;
      var next = this.nextSibling;
      parent.insertBefore(toFragment(slice.call(arguments)), next);
    });
    define(target, 'replaceWith', function replaceWith() {
      var parent = this.parentNode;
      if (!parent) return;
      var next = this.nextSibling;
      var fragment = toFragment(slice.call(arguments));
      if (this.parentNode === parent) {
        parent.insertBefore(fragment, this);
        parent.removeChild(this);
      } else {
        parent.insertBefore(fragment, next);
      }
    });
    defineGetter(target, 'nextElementSibling', function () {
      var node = this.nextSibling;
      while (node && node.nodeType !== 1) node = node.nextSibling;
      return node || null;
    });
    defineGetter(target, 'previousElementSibling', function () {
      var node = this.previousSibling;
      while (node && node.nodeType !== 1) node = node.previousSibling;
      return node || null;
    });
  });

  // ParentNode (§4.2.6), on elements, fragments and documents.
  [elementPrototype, fragmentPrototype, documentPrototype].forEach(function (target) {
    define(target, 'append', function append() {
      this.appendChild(toFragment(slice.call(arguments)));
    });
    define(target, 'prepend', function prepend() {
      this.insertBefore(toFragment(slice.call(arguments)), this.firstChild);
    });
    define(target, 'replaceChildren', function replaceChildren() {
      var fragment = toFragment(slice.call(arguments));
      while (this.firstChild) this.removeChild(this.firstChild);
      this.appendChild(fragment);
    });
    defineGetter(target, 'firstElementChild', function () {
      var children = this.children;
      return children && children.length ? children[0] : null;
    });
    defineGetter(target, 'lastElementChild', function () {
      var children = this.children;
      return children && children.length ? children[children.length - 1] : null;
    });
    defineGetter(target, 'childElementCount', function () {
      var children = this.children;
      return children ? children.length : 0;
    });
  });

  define(nodePrototype, 'hasChildNodes', function hasChildNodes() {
    return this.firstChild !== null;
  });
  define(nodePrototype, 'getRootNode', function getRootNode() {
    var node = this;
    while (node.parentNode) node = node.parentNode;
    return node;
  });
  defineGetter(nodePrototype, 'isConnected', function () {
    var node = this;
    while (node) {
      if (node.nodeType === 9) return true;
      node = node.parentNode;
    }
    return false;
  });
  define(nodePrototype, 'isSameNode', function isSameNode(other) {
    return this === other;
  });
  define(nodePrototype, 'replaceChild', function replaceChild(replacement, existing) {
    if (existing.parentNode !== this) {
      throw new DOMException("Failed to execute 'replaceChild' on 'Node': The node to be replaced is not a child of this node.", 'NotFoundError');
    }
    this.insertBefore(replacement, existing);
    if (existing.parentNode === this) this.removeChild(existing);
    return existing;
  });
  define(nodePrototype, 'normalize', function normalize() {
    var node = this.firstChild;
    while (node) {
      var next = node.nextSibling;
      if (node.nodeType === 3) {
        if (node.data === '') {
          this.removeChild(node);
        } else {
          while (next && next.nodeType === 3) {
            node.data += next.data;
            var merged = next;
            next = next.nextSibling;
            this.removeChild(merged);
          }
        }
      } else if (node.nodeType === 1) {
        node.normalize();
      }
      node = next;
    }
  });
  define(nodePrototype, 'compareDocumentPosition', function compareDocumentPosition(other) {
    if (this === other) return 0;
    function pathTo(node) {
      var path = [];
      for (; node; node = node.parentNode) path.push(node);
      return path.reverse();
    }
    var mine = pathTo(this);
    var theirs = pathTo(other);
    if (mine[0] !== theirs[0]) return 1 | 32 | 4;
    var shared = 0;
    while (shared < mine.length && shared < theirs.length && mine[shared] === theirs[shared]) shared++;
    if (shared === mine.length) return 16 | 4;
    if (shared === theirs.length) return 8 | 2;
    for (var node = mine[shared]; node; node = node.nextSibling) {
      if (node === theirs[shared]) return 4;
    }
    return 2;
  });
  define(nodePrototype, 'isEqualNode', function isEqualNode(other) {
    if (other === null || other === undefined || this.nodeType !== other.nodeType) return false;
    if (this.nodeName !== other.nodeName || this.nodeValue !== other.nodeValue) return false;
    if (this.nodeType === 1) {
      var mine = this.attributes;
      var theirs = other.attributes;
      if (mine.length !== theirs.length) return false;
      for (var i = 0; i < mine.length; i++) {
        if (other.getAttribute(mine[i].name) !== mine[i].value) return false;
      }
    }
    var a = this.firstChild;
    var b = other.firstChild;
    while (a && b) {
      if (!a.isEqualNode(b)) return false;
      a = a.nextSibling;
      b = b.nextSibling;
    }
    return a === null && b === null;
  });

  // Element (§4.9).
  define(elementPrototype, 'closest', function closest(selector) {
    for (var node = this; node && node.nodeType === 1; node = node.parentNode) {
      if (node.matches(selector)) return node;
    }
    return null;
  });
  define(elementPrototype, 'toggleAttribute', function toggleAttribute(name, force) {
    var present = this.hasAttribute(name);
    if (present && force !== true) {
      this.removeAttribute(name);
      return false;
    }
    if (!present && force !== false) {
      this.setAttribute(name, '');
      return true;
    }
    return present;
  });
  define(elementPrototype, 'getAttributeNames', function getAttributeNames() {
    var attributes = this.attributes;
    var names = [];
    for (var i = 0; i < attributes.length; i++) names.push(attributes[i].name);
    return names;
  });
  define(elementPrototype, 'hasAttributes', function hasAttributes() {
    return this.attributes.length > 0;
  });
  function adjacentTarget(element, position, method) {
    switch (String(position).toLowerCase()) {
      case 'beforebegin': return { parent: element.parentNode, before: element };
      case 'afterbegin': return { parent: element, before: element.firstChild };
      case 'beforeend': return { parent: element, before: null };
      case 'afterend': return { parent: element.parentNode, before: element.nextSibling };
      default:
        throw new DOMException("Failed to execute '" + method + "' on 'Element': The value provided ('" + position + "') is not one of 'beforebegin', 'afterbegin', 'beforeend', or 'afterend'.", 'SyntaxError');
    }
  }
  define(elementPrototype, 'insertAdjacentElement', function insertAdjacentElement(position, element) {
    var where = adjacentTarget(this, position, 'insertAdjacentElement');
    if (!where.parent) return null;
    where.parent.insertBefore(element, where.before);
    return element;
  });
  define(elementPrototype, 'insertAdjacentText', function insertAdjacentText(position, text) {
    var where = adjacentTarget(this, position, 'insertAdjacentText');
    if (!where.parent) return;
    where.parent.insertBefore(document.createTextNode(String(text)), where.before);
  });
  define(elementPrototype, 'insertAdjacentHTML', function insertAdjacentHTML(position, html) {
    var where = adjacentTarget(this, position, 'insertAdjacentHTML');
    if (!where.parent) {
      throw new DOMException("Failed to execute 'insertAdjacentHTML' on 'Element': The element has no parent.", 'NoModificationAllowedError');
    }
    // Parse in the context the new nodes will live in.
    var context = where.parent.nodeType === 1 ? where.parent : document.body;
    var holder = document.createElement(context.localName || 'div');
    holder.innerHTML = html;
    var fragment = document.createDocumentFragment();
    while (holder.firstChild) fragment.appendChild(holder.firstChild);
    where.parent.insertBefore(fragment, where.before);
  });
  // `innerText` here is `textContent`: the engine has no rendered-text
  // algorithm, so hidden or collapsed text is not accounted for.
  defineGetter(elementPrototype, 'innerText', function () {
    return this.textContent;
  }, function (value) {
    this.textContent = value;
  });

  // Document.
  defineGetter(documentPrototype, 'URL', function () { return location.href; });
  defineGetter(documentPrototype, 'documentURI', function () { return location.href; });
  defineGetter(documentPrototype, 'domain', function () { return location.hostname; });
  defineGetter(documentPrototype, 'referrer', function () { return ''; });
  defineGetter(documentPrototype, 'characterSet', function () { return 'UTF-8'; });
  defineGetter(documentPrototype, 'charset', function () { return 'UTF-8'; });
  defineGetter(documentPrototype, 'inputEncoding', function () { return 'UTF-8'; });
  defineGetter(documentPrototype, 'contentType', function () { return 'text/html'; });
  defineGetter(documentPrototype, 'hidden', function () { return false; });
  defineGetter(documentPrototype, 'visibilityState', function () { return 'visible'; });
  defineGetter(documentPrototype, 'doctype', function () {
    for (var node = this.firstChild; node; node = node.nextSibling) {
      if (node.nodeType === 10) return node;
    }
    return null;
  });
  // Static snapshots, not the live `HTMLCollection`s of the specification.
  [['forms', 'form'], ['images', 'img'], ['scripts', 'script'], ['embeds', 'embed'],
   ['links', 'a[href], area[href]'], ['anchors', 'a[name]']].forEach(function (entry) {
    defineGetter(documentPrototype, entry[0], function () {
      return slice.call(this.querySelectorAll(entry[1]));
    });
  });
  define(documentPrototype, 'getElementsByName', function getElementsByName(name) {
    return slice.call(this.querySelectorAll('[name="' + String(name).replace(/"/g, '\\"') + '"]'));
  });
  define(documentPrototype, 'importNode', function importNode(node, deep) {
    return node.cloneNode(!!deep);
  });
  define(documentPrototype, 'adoptNode', function adoptNode(node) {
    if (node.parentNode) node.parentNode.removeChild(node);
    return node;
  });
  define(documentPrototype, 'hasFocus', function hasFocus() { return true; });
  define(documentPrototype, 'execCommand', function execCommand() { return false; });
  define(documentPrototype, 'queryCommandSupported', function queryCommandSupported() { return false; });

  // ------------------------------------------- NodeFilter, TreeWalker, iterators
  var NodeFilterImpl = {
    FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3,
    SHOW_ALL: 0xFFFFFFFF, SHOW_ELEMENT: 0x1, SHOW_ATTRIBUTE: 0x2, SHOW_TEXT: 0x4,
    SHOW_CDATA_SECTION: 0x8, SHOW_PROCESSING_INSTRUCTION: 0x40, SHOW_COMMENT: 0x80,
    SHOW_DOCUMENT: 0x100, SHOW_DOCUMENT_TYPE: 0x200, SHOW_DOCUMENT_FRAGMENT: 0x400
  };
  define(global, 'NodeFilter', NodeFilterImpl);

  function acceptNode(walker, node) {
    if (!(walker.whatToShow & (1 << (node.nodeType - 1)))) return NodeFilterImpl.FILTER_SKIP;
    var filter = walker.filter;
    if (!filter) return NodeFilterImpl.FILTER_ACCEPT;
    return typeof filter === 'function' ? filter(node) : filter.acceptNode(node);
  }
  function TreeWalker(root, whatToShow, filter) {
    this.root = root;
    this.currentNode = root;
    this.whatToShow = whatToShow === undefined ? 0xFFFFFFFF : whatToShow >>> 0;
    this.filter = filter || null;
  }
  TreeWalker.prototype.parentNode = function () {
    var node = this.currentNode;
    while (node && node !== this.root) {
      node = node.parentNode;
      if (node && acceptNode(this, node) === NodeFilterImpl.FILTER_ACCEPT) {
        this.currentNode = node;
        return node;
      }
    }
    return null;
  };
  function walkChild(walker, first) {
    var node = first ? walker.currentNode.firstChild : walker.currentNode.lastChild;
    while (node) {
      var result = acceptNode(walker, node);
      if (result === NodeFilterImpl.FILTER_ACCEPT) {
        walker.currentNode = node;
        return node;
      }
      if (result === NodeFilterImpl.FILTER_SKIP) {
        var inner = first ? node.firstChild : node.lastChild;
        if (inner) {
          node = inner;
          continue;
        }
      }
      while (node) {
        var sibling = first ? node.nextSibling : node.previousSibling;
        if (sibling) {
          node = sibling;
          break;
        }
        node = node.parentNode;
        if (!node || node === walker.root || node === walker.currentNode) return null;
      }
    }
    return null;
  }
  TreeWalker.prototype.firstChild = function () { return walkChild(this, true); };
  TreeWalker.prototype.lastChild = function () { return walkChild(this, false); };
  function walkSibling(walker, next) {
    var node = walker.currentNode;
    if (node === walker.root) return null;
    while (true) {
      var sibling = next ? node.nextSibling : node.previousSibling;
      while (sibling) {
        node = sibling;
        var result = acceptNode(walker, node);
        if (result === NodeFilterImpl.FILTER_ACCEPT) {
          walker.currentNode = node;
          return node;
        }
        sibling = result === NodeFilterImpl.FILTER_REJECT ? null : (next ? node.firstChild : node.lastChild);
        if (!sibling) sibling = next ? node.nextSibling : node.previousSibling;
      }
      node = node.parentNode;
      if (!node || node === walker.root) return null;
      if (acceptNode(walker, node) === NodeFilterImpl.FILTER_ACCEPT) return null;
    }
  }
  TreeWalker.prototype.nextSibling = function () { return walkSibling(this, true); };
  TreeWalker.prototype.previousSibling = function () { return walkSibling(this, false); };
  TreeWalker.prototype.nextNode = function () {
    var node = this.currentNode;
    var result = NodeFilterImpl.FILTER_ACCEPT;
    while (true) {
      while (result !== NodeFilterImpl.FILTER_REJECT && node.firstChild) {
        node = node.firstChild;
        result = acceptNode(this, node);
        if (result === NodeFilterImpl.FILTER_ACCEPT) {
          this.currentNode = node;
          return node;
        }
      }
      var sibling = null;
      var temporary = node;
      while (temporary) {
        if (temporary === this.root) return null;
        sibling = temporary.nextSibling;
        if (sibling) break;
        temporary = temporary.parentNode;
      }
      node = sibling;
      if (!node) return null;
      result = acceptNode(this, node);
      if (result === NodeFilterImpl.FILTER_ACCEPT) {
        this.currentNode = node;
        return node;
      }
    }
  };
  TreeWalker.prototype.previousNode = function () {
    var node = this.currentNode;
    while (node !== this.root) {
      var sibling = node.previousSibling;
      while (sibling) {
        node = sibling;
        var result = acceptNode(this, node);
        while (result !== NodeFilterImpl.FILTER_REJECT && node.lastChild) {
          node = node.lastChild;
          result = acceptNode(this, node);
        }
        if (result === NodeFilterImpl.FILTER_ACCEPT) {
          this.currentNode = node;
          return node;
        }
        sibling = node.previousSibling;
      }
      if (node === this.root || !node.parentNode) return null;
      node = node.parentNode;
      if (acceptNode(this, node) === NodeFilterImpl.FILTER_ACCEPT) {
        this.currentNode = node;
        return node;
      }
    }
    return null;
  };
  define(global, 'TreeWalker', TreeWalker);
  define(documentPrototype, 'createTreeWalker', function createTreeWalker(root, whatToShow, filter) {
    return new TreeWalker(root, whatToShow, filter);
  });
  function NodeIterator(root, whatToShow, filter) {
    this.root = root;
    this.whatToShow = whatToShow === undefined ? 0xFFFFFFFF : whatToShow >>> 0;
    this.filter = filter || null;
    this.referenceNode = root;
    this.pointerBeforeReferenceNode = true;
    this.__walker = new TreeWalker(root, 0xFFFFFFFF, null);
  }
  NodeIterator.prototype.nextNode = function () {
    var walker = this.__walker;
    walker.currentNode = this.referenceNode;
    var node;
    if (this.pointerBeforeReferenceNode) {
      node = this.referenceNode;
      this.pointerBeforeReferenceNode = false;
      if (node && acceptNode(this, node) === NodeFilterImpl.FILTER_ACCEPT) return node;
    }
    while ((node = walker.nextNode())) {
      this.referenceNode = node;
      if (acceptNode(this, node) === NodeFilterImpl.FILTER_ACCEPT) return node;
    }
    return null;
  };
  NodeIterator.prototype.previousNode = function () {
    var walker = this.__walker;
    walker.currentNode = this.referenceNode;
    var node;
    while ((node = walker.previousNode())) {
      this.referenceNode = node;
      if (acceptNode(this, node) === NodeFilterImpl.FILTER_ACCEPT) return node;
    }
    return null;
  };
  NodeIterator.prototype.detach = function () {};
  define(global, 'NodeIterator', NodeIterator);
  define(documentPrototype, 'createNodeIterator', function createNodeIterator(root, whatToShow, filter) {
    return new NodeIterator(root, whatToShow, filter);
  });

  // ------------------------------------------------------------------ window
  define(global, 'devicePixelRatio', 1);
  define(global, 'name', '');
  define(global, 'opener', null);
  define(global, 'closed', false);
  define(global, 'length', 0);
  define(global, 'frames', global);
  define(global, 'origin', location.origin);
  define(global, 'isSecureContext', location.protocol === 'https:' || location.hostname === 'localhost');
  define(global, 'dispatchEvent', function dispatchEvent(event) {
    return document.dispatchEvent(event);
  });
  // No dialog, popup or print UI exists, so each behaves as in a browser that
  // has suppressed them: nothing is shown and the cancelling answer is given.
  define(global, 'alert', function alert() {});
  define(global, 'confirm', function confirm() { return false; });
  define(global, 'prompt', function prompt() { return null; });
  define(global, 'open', function open() { return null; });
  define(global, 'print', function print() {});
  define(global, 'close', function close() {});
  define(global, 'stop', function stop() {});
  define(global, 'focus', function focus() {});
  define(global, 'blur', function blur() {});

  // ---------------------------------------------------------------- messaging
  define(global, 'postMessage', function postMessage(message) {
    var data = structuredClone(message);
    setTimeout(function () {
      var event = new MessageEventImpl('message', {
        data: data, origin: location.origin, source: global
      });
      global.dispatchEvent(event);
    }, 0);
  });
  if (typeof MessageChannel === 'undefined') {
    var MessagePort = function MessagePort() {
      throw new TypeError('Illegal constructor');
    };
    MessagePort.prototype = Object.create(EventTarget.prototype, {
      constructor: { value: MessagePort, writable: true, configurable: true }
    });
    var makePort = function () {
      var port = Object.create(MessagePort.prototype);
      defineProperty(port, '__peer', { value: null, writable: true });
      defineProperty(port, '__open', { value: false, writable: true });
      defineProperty(port, '__onmessage', { value: null, writable: true });
      return port;
    };
    MessagePort.prototype.postMessage = function postMessage(message) {
      var peer = this.__peer;
      if (!peer) return;
      var data = structuredClone(message);
      setTimeout(function () {
        peer.dispatchEvent(new MessageEventImpl('message', { data: data, source: null }));
      }, 0);
    };
    MessagePort.prototype.start = function start() { this.__open = true; };
    MessagePort.prototype.close = function close() { this.__peer = null; };
    defineProperty(MessagePort.prototype, 'onmessage', {
      get: function () { return this.__onmessage; },
      set: function (handler) {
        if (this.__onmessage) this.removeEventListener('message', this.__onmessage);
        this.__onmessage = typeof handler === 'function' ? handler : null;
        if (this.__onmessage) this.addEventListener('message', this.__onmessage);
        this.__open = true;
      },
      configurable: true
    });
    var MessageChannelImpl = function MessageChannel() {
      if (!(this instanceof MessageChannelImpl)) {
        throw new TypeError("Failed to construct 'MessageChannel': Please use the 'new' operator");
      }
      this.port1 = makePort();
      this.port2 = makePort();
      this.port1.__peer = this.port2;
      this.port2.__peer = this.port1;
    };
    define(global, 'MessagePort', MessagePort);
    define(global, 'MessageChannel', MessageChannelImpl);
  }
  if (typeof BroadcastChannel === 'undefined') {
    var channels = Object.create(null);
    var BroadcastChannelImpl = function BroadcastChannel(name) {
      if (!(this instanceof BroadcastChannelImpl)) {
        throw new TypeError("Failed to construct 'BroadcastChannel': Please use the 'new' operator");
      }
      defineProperty(this, 'name', { value: String(name) });
      defineProperty(this, '__closed', { value: false, writable: true });
      (channels[this.name] || (channels[this.name] = [])).push(this);
    };
    BroadcastChannelImpl.prototype = Object.create(EventTarget.prototype, {
      constructor: { value: BroadcastChannelImpl, writable: true, configurable: true }
    });
    defineProperty(BroadcastChannelImpl.prototype, 'onmessage', {
      get: function () { return this.__onmessage || null; },
      set: function (handler) {
        if (this.__onmessage) this.removeEventListener('message', this.__onmessage);
        this.__onmessage = typeof handler === 'function' ? handler : null;
        if (this.__onmessage) this.addEventListener('message', this.__onmessage);
      },
      configurable: true
    });
    // Only channels in this realm exist, so these are all the peers there are.
    BroadcastChannelImpl.prototype.postMessage = function postMessage(message) {
      if (this.__closed) throw new DOMException('Channel is closed', 'InvalidStateError');
      var data = structuredClone(message);
      var sender = this;
      (channels[this.name] || []).slice().forEach(function (peer) {
        if (peer === sender || peer.__closed) return;
        setTimeout(function () {
          peer.dispatchEvent(new MessageEventImpl('message', { data: data, origin: location.origin }));
        }, 0);
      });
    };
    BroadcastChannelImpl.prototype.close = function close() {
      this.__closed = true;
      var list = channels[this.name] || [];
      var index = list.indexOf(this);
      if (index !== -1) list.splice(index, 1);
    };
    define(global, 'BroadcastChannel', BroadcastChannelImpl);
  }

  // ------------------------------------------------------------ typed arrays
  // ECMA-262 §23.2.3: the %TypedArray%.prototype methods the native arrays do
  // not carry, written against indexing and `length` only. Every method defined
  // here is a no-op where the engine already provides it.
  (function () {
    var names = ['Int8Array', 'Uint8Array', 'Uint8ClampedArray', 'Int16Array', 'Uint16Array',
      'Int32Array', 'Uint32Array', 'Float32Array', 'Float64Array'];
    function callable(fn, method) {
      if (typeof fn !== 'function') throw new TypeError(String(fn) + ' is not a function');
      return fn;
    }
    function sameType(array, length) {
      return new array.constructor(length);
    }
    function compareNumbers(a, b) {
      if (a !== a) return b !== b ? 0 : 1;
      if (b !== b) return -1;
      if (a < b) return -1;
      if (a > b) return 1;
      if (a === 0 && b === 0) return 1 / a < 1 / b ? -1 : 1 / a > 1 / b ? 1 : 0;
      return 0;
    }
    var methods = {
      at: function at(index) {
        var relative = toIntegerOrInfinity(index);
        var k = relative >= 0 ? relative : this.length + relative;
        return k < 0 || k >= this.length ? undefined : this[k];
      },
      every: function every(callback, thisArg) {
        callable(callback);
        for (var i = 0; i < this.length; i++) if (!callback.call(thisArg, this[i], i, this)) return false;
        return true;
      },
      some: function some(callback, thisArg) {
        callable(callback);
        for (var i = 0; i < this.length; i++) if (callback.call(thisArg, this[i], i, this)) return true;
        return false;
      },
      find: function find(callback, thisArg) {
        callable(callback);
        for (var i = 0; i < this.length; i++) if (callback.call(thisArg, this[i], i, this)) return this[i];
        return undefined;
      },
      findIndex: function findIndex(callback, thisArg) {
        callable(callback);
        for (var i = 0; i < this.length; i++) if (callback.call(thisArg, this[i], i, this)) return i;
        return -1;
      },
      findLast: function findLast(callback, thisArg) {
        callable(callback);
        for (var i = this.length - 1; i >= 0; i--) if (callback.call(thisArg, this[i], i, this)) return this[i];
        return undefined;
      },
      findLastIndex: function findLastIndex(callback, thisArg) {
        callable(callback);
        for (var i = this.length - 1; i >= 0; i--) if (callback.call(thisArg, this[i], i, this)) return i;
        return -1;
      },
      lastIndexOf: function lastIndexOf(search, from) {
        var length = this.length;
        var k = arguments.length > 1 ? toIntegerOrInfinity(from) : length - 1;
        k = k >= 0 ? Math.min(k, length - 1) : length + k;
        for (; k >= 0; k--) if (this[k] === search) return k;
        return -1;
      },
      reduce: function reduce(callback, initial) {
        callable(callback);
        var i = 0;
        var accumulator;
        if (arguments.length > 1) {
          accumulator = initial;
        } else {
          if (this.length === 0) throw new TypeError('Reduce of empty array with no initial value');
          accumulator = this[i++];
        }
        for (; i < this.length; i++) accumulator = callback(accumulator, this[i], i, this);
        return accumulator;
      },
      reduceRight: function reduceRight(callback, initial) {
        callable(callback);
        var i = this.length - 1;
        var accumulator;
        if (arguments.length > 1) {
          accumulator = initial;
        } else {
          if (this.length === 0) throw new TypeError('Reduce of empty array with no initial value');
          accumulator = this[i--];
        }
        for (; i >= 0; i--) accumulator = callback(accumulator, this[i], i, this);
        return accumulator;
      },
      reverse: function reverse() {
        for (var low = 0, high = this.length - 1; low < high; low++, high--) {
          var swap = this[low];
          this[low] = this[high];
          this[high] = swap;
        }
        return this;
      },
      sort: function sort(compare) {
        if (compare !== undefined) callable(compare);
        var sorted = slice.call(this).sort(compare === undefined ? compareNumbers : compare);
        for (var i = 0; i < sorted.length; i++) this[i] = sorted[i];
        return this;
      },
      copyWithin: function copyWithin(target, start, end) {
        var length = this.length;
        var to = relativeIndex(target, length, 0);
        var from = relativeIndex(start, length, 0);
        var final = relativeIndex(end, length, length);
        var count = Math.min(final - from, length - to);
        var copy = slice.call(this, from, from + Math.max(count, 0));
        for (var i = 0; i < copy.length; i++) this[to + i] = copy[i];
        return this;
      },
      entries: function entries() {
        var self = this;
        var index = 0;
        var iterator = {
          next: function () {
            return index < self.length ? { value: [index, self[index++]], done: false } : { value: undefined, done: true };
          }
        };
        iterator[Symbol.iterator] = function () { return this; };
        return iterator;
      },
      keys: function keys() {
        var self = this;
        var index = 0;
        var iterator = {
          next: function () {
            return index < self.length ? { value: index++, done: false } : { value: undefined, done: true };
          }
        };
        iterator[Symbol.iterator] = function () { return this; };
        return iterator;
      },
      toReversed: function toReversed() {
        var copy = sameType(this, this.length);
        for (var i = 0; i < this.length; i++) copy[i] = this[this.length - 1 - i];
        return copy;
      },
      toSorted: function toSorted(compare) {
        if (compare !== undefined) callable(compare);
        var copy = sameType(this, this.length);
        for (var i = 0; i < this.length; i++) copy[i] = this[i];
        return copy.sort(compare);
      },
      with: function (index, value) {
        var relative = toIntegerOrInfinity(index);
        var k = relative >= 0 ? relative : this.length + relative;
        if (k < 0 || k >= this.length) throw new RangeError('Invalid typed array index');
        var copy = sameType(this, this.length);
        for (var i = 0; i < this.length; i++) copy[i] = this[i];
        copy[k] = value;
        return copy;
      },
      toLocaleString: function toLocaleString() {
        return slice.call(this).map(function (v) { return v.toLocaleString(); }).join(',');
      }
    };
    names.forEach(function (name) {
      var ctor = global[name];
      if (typeof ctor !== 'function') return;
      Object.keys(methods).forEach(function (method) {
        define(ctor.prototype, method, methods[method]);
      });
    });
  })();

  // ------------------------------------------------------------------ crypto
  // Web Crypto §10 `getRandomValues` and `randomUUID`, drawn from the operating
  // system's generator. `crypto.subtle` is absent: there is no digest, key or
  // cipher implementation behind it, and a stub that answers would be wrong.
  if (typeof crypto === 'undefined' && typeof global.__render_random_bytes === 'function') {
    var randomBytes = global.__render_random_bytes;
    delete global.__render_random_bytes;
    var integerViews = ['Int8Array', 'Uint8Array', 'Uint8ClampedArray', 'Int16Array', 'Uint16Array',
      'Int32Array', 'Uint32Array'];
    var CryptoImpl = function Crypto() {
      throw new TypeError('Illegal constructor');
    };
    define(CryptoImpl.prototype, 'getRandomValues', function getRandomValues(array) {
      var integer = integerViews.some(function (name) {
        return typeof global[name] === 'function' && array instanceof global[name];
      });
      if (!integer) {
        throw new DOMException("Failed to execute 'getRandomValues' on 'Crypto': The provided value is not an integer-typed array", 'TypeMismatchError');
      }
      if (array.byteLength > 65536) {
        throw new DOMException("Failed to execute 'getRandomValues' on 'Crypto': The ArrayBufferView's byte length (" + array.byteLength + ') exceeds the number of bytes of entropy available via this API (65536)', 'QuotaExceededError');
      }
      // Written element by element: a typed array does not expose its buffer
      // here. A 32-bit lane is built unsigned, and the element type's own
      // conversion wraps it into range.
      var size = array.byteLength / array.length;
      var bytes = randomBytes(array.byteLength);
      for (var i = 0; i < array.length; i++) {
        var value = 0;
        for (var j = size - 1; j >= 0; j--) value = value * 256 + bytes[i * size + j];
        array[i] = value;
      }
      return array;
    });
    define(CryptoImpl.prototype, 'randomUUID', function randomUUID() {
      var b = randomBytes(16);
      b[6] = (b[6] & 0x0f) | 0x40;
      b[8] = (b[8] & 0x3f) | 0x80;
      var hex = b.map(function (v) { return (v + 256).toString(16).slice(1); }).join('');
      return hex.slice(0, 8) + '-' + hex.slice(8, 12) + '-' + hex.slice(12, 16) + '-' + hex.slice(16, 20) + '-' + hex.slice(20);
    });
    defineProperty(CryptoImpl.prototype, Symbol.toStringTag, { value: 'Crypto', configurable: true });
    defineProperty(global, 'Crypto', { value: CryptoImpl, writable: true, configurable: true, enumerable: false });
    defineProperty(global, 'crypto', {
      value: Object.create(CryptoImpl.prototype), writable: true, configurable: true, enumerable: true
    });
  }

  // ------------------------------------------------------- Headers, Request
  // Fetch Standard §5.2 and §5.3. `Headers` and `Request` are written here;
  // `fetch` is wrapped so it accepts both, and hands the native transfer a
  // plain URL, method, header record, body and signal.
  if (typeof Headers === 'undefined') {
    var tokenPattern = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
    var headerName = function (name, method) {
      name = String(name);
      if (!tokenPattern.test(name)) {
        throw new TypeError("Failed to execute '" + method + "' on 'Headers': Invalid name");
      }
      return name.toLowerCase();
    };
    var headerValue = function (value) {
      return String(value).replace(/^[\t\n\r ]+|[\t\n\r ]+$/g, '');
    };
    var HeadersImpl = function Headers(init) {
      if (!(this instanceof HeadersImpl)) throw new TypeError("Failed to construct 'Headers': Please use the 'new' operator");
      defineProperty(this, '__list', { value: {}, enumerable: false });
      if (init === undefined || init === null) return;
      var self = this;
      if (init instanceof HeadersImpl) {
        init.forEach(function (value, name) { self.append(name, value); });
      } else if (typeof init[Symbol.iterator] === 'function') {
        for (var pair of init) {
          var entry = Array.from(pair);
          if (entry.length !== 2) throw new TypeError("Failed to construct 'Headers': Invalid value");
          self.append(entry[0], entry[1]);
        }
      } else if (typeof init === 'object') {
        Object.keys(init).forEach(function (name) { self.append(name, init[name]); });
      } else {
        throw new TypeError("Failed to construct 'Headers': The provided value is not of type 'HeadersInit'");
      }
    };
    var headersPrototype = HeadersImpl.prototype;
    define(headersPrototype, 'append', function append(name, value) {
      name = headerName(name, 'append');
      value = headerValue(value);
      var list = this.__list;
      list[name] = Object.prototype.hasOwnProperty.call(list, name) ? list[name] + ', ' + value : value;
    });
    define(headersPrototype, 'set', function set(name, value) {
      this.__list[headerName(name, 'set')] = headerValue(value);
    });
    define(headersPrototype, 'get', function get(name) {
      name = headerName(name, 'get');
      return Object.prototype.hasOwnProperty.call(this.__list, name) ? this.__list[name] : null;
    });
    define(headersPrototype, 'has', function has(name) {
      return Object.prototype.hasOwnProperty.call(this.__list, headerName(name, 'has'));
    });
    define(headersPrototype, 'delete', function (name) {
      delete this.__list[headerName(name, 'delete')];
    });
    define(headersPrototype, 'forEach', function forEach(callback, thisArg) {
      var list = this.__list;
      Object.keys(list).sort().forEach(function (name) {
        callback.call(thisArg, list[name], name, this);
      }, this);
    });
    define(headersPrototype, 'entries', function entries() {
      var list = this.__list;
      return Object.keys(list).sort().map(function (name) { return [name, list[name]]; })[Symbol.iterator]();
    });
    define(headersPrototype, 'keys', function keys() {
      return Object.keys(this.__list).sort()[Symbol.iterator]();
    });
    define(headersPrototype, 'values', function values() {
      var list = this.__list;
      return Object.keys(list).sort().map(function (name) { return list[name]; })[Symbol.iterator]();
    });
    defineProperty(headersPrototype, Symbol.iterator, {
      value: headersPrototype.entries, writable: true, configurable: true, enumerable: false
    });
    defineProperty(headersPrototype, Symbol.toStringTag, { value: 'Headers', configurable: true });
    defineProperty(global, 'Headers', {
      value: HeadersImpl, writable: true, configurable: true, enumerable: false
    });

    var RequestImpl = function Request(input, init) {
      if (!(this instanceof RequestImpl)) throw new TypeError("Failed to construct 'Request': Please use the 'new' operator");
      init = init || {};
      var source = input instanceof RequestImpl ? input : null;
      var url = source ? source.url : String(input);
      // The transfer resolves a relative URL against the document itself, so a
      // base the page cannot name (an `about:blank` document) leaves it as is.
      try {
        url = new URL(url, document.baseURI || location.href).href;
      } catch (error) {
        if (/^[a-z][a-z0-9+.-]*:/i.test(url)) {
          throw new TypeError("Failed to construct 'Request': Failed to parse URL from " + url);
        }
      }
      var method = init.method !== undefined ? String(init.method) : source ? source.method : 'GET';
      var upper = method.toUpperCase();
      if (['GET', 'HEAD', 'POST', 'PUT', 'DELETE', 'OPTIONS', 'PATCH'].indexOf(upper) !== -1) method = upper;
      if (!tokenPattern.test(method) || ['CONNECT', 'TRACE', 'TRACK'].indexOf(upper) !== -1) {
        throw new TypeError("Failed to construct 'Request': '" + method + "' is not a valid HTTP method.");
      }
      var body = init.body !== undefined ? init.body : source ? source.__body : null;
      if (body !== null && body !== undefined && (method === 'GET' || method === 'HEAD')) {
        throw new TypeError("Failed to construct 'Request': Request with GET/HEAD method cannot have body.");
      }
      var hidden = {
        url: url,
        method: method,
        headers: new HeadersImpl(init.headers !== undefined ? init.headers : source ? source.headers : undefined),
        signal: init.signal !== undefined ? init.signal : source ? source.signal : new AbortController().signal,
        credentials: init.credentials || (source && source.credentials) || 'same-origin',
        mode: init.mode || (source && source.mode) || 'cors',
        cache: init.cache || (source && source.cache) || 'default',
        redirect: init.redirect || (source && source.redirect) || 'follow',
        referrer: init.referrer !== undefined ? init.referrer : 'about:client',
        __body: body === undefined ? null : body,
        bodyUsed: false
      };
      Object.keys(hidden).forEach(function (key) {
        defineProperty(this, key, { value: hidden[key], writable: key === 'bodyUsed', enumerable: key.indexOf('__') !== 0, configurable: true });
      }, this);
    };
    var requestPrototype = RequestImpl.prototype;
    define(requestPrototype, 'clone', function clone() {
      if (this.bodyUsed) throw new TypeError("Failed to execute 'clone' on 'Request': Request body is already used");
      return new RequestImpl(this);
    });
    function consumeBody(request, convert) {
      if (request.bodyUsed) return Promise.reject(new TypeError('Body is unusable: Body has already been read'));
      request.bodyUsed = true;
      var body = request.__body;
      return Promise.resolve(convert(body === null ? '' : typeof body === 'string' ? body : String(body)));
    }
    define(requestPrototype, 'text', function text() { return consumeBody(this, function (v) { return v; }); });
    define(requestPrototype, 'json', function json() { return consumeBody(this, JSON.parse); });
    defineProperty(requestPrototype, Symbol.toStringTag, { value: 'Request', configurable: true });
    defineProperty(global, 'Request', {
      value: RequestImpl, writable: true, configurable: true, enumerable: false
    });

    var nativeFetch = global.fetch;
    if (typeof nativeFetch === 'function') {
      defineProperty(global, 'fetch', {
        value: function fetch(input, init) {
          var request = input instanceof RequestImpl ? input : null;
          var options = {};
          if (request) {
            options.method = request.method;
            options.headers = request.headers;
            if (request.__body !== null) options.body = request.__body;
            options.signal = request.signal;
          }
          if (init) Object.keys(init).forEach(function (key) { options[key] = init[key]; });
          var headers = options.headers;
          if (headers !== undefined && headers !== null && (headers instanceof HeadersImpl || typeof headers[Symbol.iterator] === 'function')) {
            var record = {};
            new HeadersImpl(headers).forEach(function (value, name) { record[name] = value; });
            options.headers = record;
          }
          return nativeFetch.call(this, request ? request.url : input, options);
        },
        writable: true, configurable: true, enumerable: false
      });
    }
  }

  // ------------------------------------------------------------- AbortSignal
  // DOM Standard §3.2. Signals are made by `AbortController`; this supplies the
  // interface object, `throwIfAborted`, and the `abort`/`timeout`/`any` factories.
  if (typeof AbortSignal === 'undefined' && typeof AbortController === 'function') {
    var AbortSignalImpl = function AbortSignal() {
      throw new TypeError('Illegal constructor');
    };
    AbortSignalImpl.prototype = Object.create(EventTarget.prototype);
    defineProperty(AbortSignalImpl.prototype, 'constructor', {
      value: AbortSignalImpl, writable: true, configurable: true, enumerable: false
    });
    defineProperty(AbortSignalImpl.prototype, Symbol.toStringTag, {
      value: 'AbortSignal', configurable: true
    });
    define(AbortSignalImpl.prototype, 'throwIfAborted', function throwIfAborted() {
      if (this.aborted) throw this.reason;
    });
    define(AbortSignalImpl, 'abort', function abort(reason) {
      var controller = new AbortController();
      controller.abort(reason);
      return controller.signal;
    });
    define(AbortSignalImpl, 'timeout', function timeout(milliseconds) {
      var controller = new AbortController();
      setTimeout(function () {
        controller.abort(new DOMException('The operation timed out.', 'TimeoutError'));
      }, Number(milliseconds));
      return controller.signal;
    });
    define(AbortSignalImpl, 'any', function any(signals) {
      var controller = new AbortController();
      var list = Array.from(signals);
      for (var i = 0; i < list.length; i++) {
        if (list[i].aborted) {
          controller.abort(list[i].reason);
          return controller.signal;
        }
      }
      list.forEach(function (signal) {
        signal.addEventListener('abort', function () { controller.abort(signal.reason); }, { once: true });
      });
      return controller.signal;
    });
    defineProperty(global, 'AbortSignal', {
      value: AbortSignalImpl, writable: true, configurable: true, enumerable: false
    });
    // Signals made before this point (none: the prelude runs first) and after
    // it share the interface prototype.
  }

  // ---------------------------------------------------------- custom elements
  // HTML Standard §4.13. The registry, `define`/`get`/`getName`/`whenDefined`/
  // `upgrade`, the `HTMLElement` constructor for autonomous elements, and the
  // `connectedCallback`, `disconnectedCallback` and `attributeChangedCallback`
  // reactions (no `adoptedCallback`: there is one document) for the attribute APIs that go
  // through `setAttribute`, `removeAttribute`, `toggleAttribute` and the reflecting
  // properties (`id`, `className`, ...). Changes made through `classList`,
  // `style`, `dataset` or `Attr.value` do not reach a callback.
  //
  // The mutation hooks are installed on the first `define`, so a page that
  // never registers an element pays nothing for any of this.
  if (typeof customElements === 'undefined') {
    (function () {
      var NativeHTMLElement = global.HTMLElement;
      var setPrototypeOf = Object.setPrototypeOf;
      var definitions = {};
      var byConstructor = new Map();
      var waiters = {};
      var states = new WeakMap();
      var upgrading = null;
      var hooked = false;
      var reserved = ['annotation-xml', 'color-profile', 'font-face', 'font-face-src',
        'font-face-uri', 'font-face-format', 'font-face-name', 'missing-glyph'];
      var namePattern = /^[a-z][-._0-9a-z·À-￿]*-[-._0-9a-z·À-￿]*$/;

      function report(error) {
        if (typeof global.reportError === 'function') global.reportError(error);
      }
      function isCustomName(name) {
        return namePattern.test(name) && reserved.indexOf(name) === -1;
      }
      defineGetter(elementPrototype, 'localName', function () {
        return String(this.tagName).toLowerCase();
      });
      function localNameOf(element) {
        return String(element.tagName).toLowerCase();
      }
      function isHtml(element) {
        return typeof global.SVGElement !== 'function' || !(element instanceof global.SVGElement);
      }

      function HTMLElementConstructor() {
        var target = new.target;
        var definition;
        if (upgrading !== null) {
          definition = upgrading.definition;
        } else if (target !== undefined && target !== HTMLElementConstructor) {
          definition = byConstructor.get(target);
        }
        if (!definition) throw new TypeError('Illegal constructor');
        var element;
        if (upgrading !== null) {
          element = upgrading.element;
          upgrading = null;
        } else {
          element = createRaw(definition.name);
          states.set(element, 'custom');
        }
        setPrototypeOf(element, target.prototype);
        return element;
      }
      defineProperty(HTMLElementConstructor, 'name', { value: 'HTMLElement', configurable: true });
      HTMLElementConstructor.prototype = NativeHTMLElement.prototype;
      defineProperty(NativeHTMLElement.prototype, 'constructor', {
        value: HTMLElementConstructor, writable: true, configurable: true, enumerable: false
      });
      setPrototypeOf(HTMLElementConstructor, Object.getPrototypeOf(NativeHTMLElement));
      defineProperty(global, 'HTMLElement', {
        value: HTMLElementConstructor, writable: true, configurable: true, enumerable: false
      });

      var nativeCreateElement = document.createElement;
      function createRaw(name) {
        return nativeCreateElement.call(document, name);
      }

      function callback(element, name, args) {
        var definition = definitions[localNameOf(element)];
        if (!definition || states.get(element) !== 'custom') return;
        var fn = definition.callbacks[name];
        if (typeof fn !== 'function') return;
        try {
          fn.apply(element, args);
        } catch (error) {
          report(error);
        }
      }

      function upgrade(element) {
        if (states.has(element) || !isHtml(element)) return;
        var definition = definitions[localNameOf(element)];
        if (!definition) return;
        var attributes = [];
        for (var i = 0; i < element.attributes.length; i++) {
          attributes.push(element.attributes[i]);
        }
        upgrading = { element: element, definition: definition };
        var result;
        try {
          result = new definition.constructor();
          if (result !== element) throw new TypeError('The custom element constructor did not produce the element being upgraded');
        } catch (error) {
          states.set(element, 'failed');
          report(error);
          return;
        } finally {
          upgrading = null;
        }
        states.set(element, 'custom');
        for (var j = 0; j < attributes.length; j++) {
          if (definition.observed.indexOf(attributes[j].name) !== -1) {
            callback(element, 'attributeChangedCallback', [attributes[j].name, null, attributes[j].value, null]);
          }
        }
        if (element.isConnected) callback(element, 'connectedCallback', []);
      }

      // Inclusive descendants that are elements, in tree order, without
      // recursion so a deep tree cannot exhaust the stack.
      function elementsOf(root) {
        var list = [];
        if (root.nodeType !== 1) return list;
        var node = root;
        while (node) {
          if (node.nodeType === 1) list.push(node);
          if (node.firstChild && node.nodeType === 1) {
            node = node.firstChild;
          } else {
            while (node && node !== root && !node.nextSibling) node = node.parentNode;
            if (!node || node === root) break;
            node = node.nextSibling;
          }
        }
        return list;
      }
      function customElementsOf(root) {
        return elementsOf(root).filter(function (element) {
          return states.get(element) === 'custom';
        });
      }
      function connect(root) {
        var elements = elementsOf(root);
        for (var i = 0; i < elements.length; i++) {
          var element = elements[i];
          if (!element.isConnected) continue;
          if (states.get(element) === 'custom') callback(element, 'connectedCallback', []);
          else upgrade(element);
        }
      }
      function disconnect(list) {
        for (var i = 0; i < list.length; i++) {
          if (!list[i].isConnected) callback(list[i], 'disconnectedCallback', []);
        }
      }
      function connectedCustom(root) {
        return root.isConnected ? customElementsOf(root) : [];
      }
      function newlyConnected(root, known) {
        var elements = elementsOf(root);
        for (var i = 0; i < elements.length; i++) {
          var element = elements[i];
          if (known.indexOf(element) !== -1 || !element.isConnected) continue;
          if (states.get(element) === 'custom') callback(element, 'connectedCallback', []);
          else upgrade(element);
        }
      }

      function ownerOf(name) {
        var proto = elementPrototype;
        while (proto && !Object.prototype.hasOwnProperty.call(proto, name)) proto = Object.getPrototypeOf(proto);
        return proto;
      }
      // Some element methods are not prototype properties: the engine resolves
      // them on the node itself. A property defined on the prototype is found
      // first, so wrapping the function value read off a real element is enough.
      function patchMethod(name, make) {
        var owner = ownerOf(name);
        var original = owner ? owner[name] : document.documentElement[name];
        if (typeof original !== 'function') return;
        defineProperty(owner || elementPrototype, name, {
          value: make(original), writable: true, configurable: true, enumerable: false
        });
      }

      function install() {
        if (hooked) return;
        hooked = true;
        // Insertion: the moved nodes are the roots whose subtrees connect.
        ['appendChild', 'insertBefore'].forEach(function (name) {
          patchMethod(name, function (original) {
            return function (node) {
              var moved = node && node.nodeType === 11 ? slice.call(node.childNodes) : [node];
              var leaving = node && node.nodeType !== 11 ? connectedCustom(node) : [];
              var result = original.apply(this, arguments);
              disconnect(leaving);
              for (var i = 0; i < moved.length; i++) {
                if (moved[i] && moved[i].isConnected) connect(moved[i]);
              }
              return result;
            };
          });
        });
        patchMethod('replaceChild', function (original) {
          return function (node, old) {
            var moved = node && node.nodeType === 11 ? slice.call(node.childNodes) : [node];
            var leaving = connectedCustom(old).concat(node && node.nodeType !== 11 ? connectedCustom(node) : []);
            var result = original.apply(this, arguments);
            disconnect(leaving);
            for (var i = 0; i < moved.length; i++) {
              if (moved[i] && moved[i].isConnected) connect(moved[i]);
            }
            return result;
          };
        });
        patchMethod('removeChild', function (original) {
          return function (child) {
            var leaving = child ? connectedCustom(child) : [];
            var result = original.apply(this, arguments);
            disconnect(leaving);
            return result;
          };
        });
        patchMethod('remove', function (original) {
          return function () {
            var leaving = connectedCustom(this);
            var result = original.apply(this, arguments);
            disconnect(leaving);
            return result;
          };
        });
        // Operations that replace a subtree wholesale are reconciled by
        // comparing the connected custom elements of the container before and
        // after, so every path through them reacts the same way.
        function reconcile(name, scope) {
          patchMethod(name, function (original) {
            return function () {
              var root = scope(this);
              var before = connectedCustom(root);
              var result = original.apply(this, arguments);
              disconnect(before);
              newlyConnected(root, before);
              return result;
            };
          });
        }
        function self(element) { return element; }
        function parentOrSelf(element) { return element.parentNode || element; }
        reconcile('replaceChildren', self);
        reconcile('insertAdjacentHTML', parentOrSelf);
        reconcile('insertAdjacentElement', parentOrSelf);
        // Attributes.
        function observed(element, name) {
          var definition = definitions[localNameOf(element)];
          return states.get(element) === 'custom' && definition && definition.observed.indexOf(name) !== -1;
        }
        patchMethod('setAttribute', function (original) {
          return function (name, value) {
            name = String(name);
            if (this.hasAttribute && /[A-Z]/.test(name) && isHtml(this)) name = name.toLowerCase();
            var watch = observed(this, name);
            var old = watch ? this.getAttribute(name) : null;
            var result = original.apply(this, arguments);
            if (watch) callback(this, 'attributeChangedCallback', [name, old, this.getAttribute(name), null]);
            return result;
          };
        });
        patchMethod('removeAttribute', function (original) {
          return function (name) {
            name = String(name).toLowerCase();
            var watch = observed(this, name);
            var old = watch ? this.getAttribute(name) : null;
            var result = original.apply(this, arguments);
            if (watch && old !== null) callback(this, 'attributeChangedCallback', [name, old, null, null]);
            return result;
          };
        });
        patchMethod('toggleAttribute', function (original) {
          return function (name) {
            name = String(name).toLowerCase();
            var watch = observed(this, name);
            var old = watch ? this.getAttribute(name) : null;
            var result = original.apply(this, arguments);
            var now = watch ? this.getAttribute(name) : null;
            if (watch && old !== now) callback(this, 'attributeChangedCallback', [name, old, now, null]);
            return result;
          };
        });
        // Property writes the engine performs natively (`innerHTML`, `id`,
        // `className`, ...) call this before they run; the function it returns
        // is called after. See `set_member` in the evaluator.
        var propertyAttribute = { className: 'class', tabIndex: 'tabindex', readOnly: 'readonly', srcSet: 'srcset' };
        defineProperty(global, '__customElementReaction', {
          value: function (node, property) {
            if (property === 'innerHTML' || property === 'textContent' || property === 'outerHTML') {
              var root = property === 'outerHTML' ? (node.parentNode || node) : node;
              var before = connectedCustom(root);
              return function () {
                disconnect(before);
                newlyConnected(root, before);
              };
            }
            var name = propertyAttribute[property] || property;
            if (!observed(node, name)) return undefined;
            var old = node.getAttribute(name);
            return function () {
              var now = node.getAttribute(name);
              if (old !== now) callback(node, 'attributeChangedCallback', [name, old, now, null]);
            };
          },
          writable: true, configurable: true, enumerable: false
        });
        patchMethod('cloneNode', function (original) {
          return function () {
            var copy = original.apply(this, arguments);
            elementsOf(copy).forEach(upgrade);
            return copy;
          };
        });
        defineProperty(documentPrototype, 'createElement', {
          value: function createElement(name) {
            var element = nativeCreateElement.apply(this, arguments);
            if (typeof name === 'string' && isHtml(element)) upgrade(element);
            return element;
          },
          writable: true, configurable: true, enumerable: false
        });
      }

      function CustomElementRegistryImpl() {
        throw new TypeError('Illegal constructor');
      }
      CustomElementRegistryImpl.prototype.define = function define(name, constructor, options) {
        name = String(name);
        if (typeof constructor !== 'function' || !constructor.prototype) {
          throw new TypeError("Failed to execute 'define' on 'CustomElementRegistry': parameter 2 is not of type 'Function'.");
        }
        if (!isCustomName(name)) {
          throw new DOMException("Failed to execute 'define' on 'CustomElementRegistry': \"" + name + '" is not a valid custom element name', 'SyntaxError');
        }
        if (Object.prototype.hasOwnProperty.call(definitions, name)) {
          throw new DOMException("Failed to execute 'define' on 'CustomElementRegistry': the name \"" + name + '" has already been used with this registry', 'NotSupportedError');
        }
        if (byConstructor.has(constructor)) {
          throw new DOMException("Failed to execute 'define' on 'CustomElementRegistry': this constructor has already been used with this registry", 'NotSupportedError');
        }
        if (options && options.extends !== undefined) {
          throw new DOMException("Failed to execute 'define' on 'CustomElementRegistry': customized built-in elements are not supported", 'NotSupportedError');
        }
        var prototype = constructor.prototype;
        var callbacks = {};
        ['connectedCallback', 'disconnectedCallback', 'attributeChangedCallback'].forEach(function (key) {
          var value = prototype[key];
          if (value !== undefined && typeof value !== 'function') {
            throw new TypeError("The '" + key + "' property of the custom element prototype is not a function");
          }
          callbacks[key] = value;
        });
        var observed = [];
        if (callbacks.attributeChangedCallback) {
          var list = constructor.observedAttributes;
          if (list !== undefined) {
            for (var item of list) observed.push(String(item));
          }
        }
        var definition = { name: name, constructor: constructor, callbacks: callbacks, observed: observed };
        definitions[name] = definition;
        byConstructor.set(constructor, definition);
        install();
        elementsOf(document.documentElement || document.createElement('div')).forEach(function (element) {
          if (localNameOf(element) === name) upgrade(element);
        });
        var waiting = waiters[name];
        if (waiting) {
          delete waiters[name];
          waiting.forEach(function (resolve) { resolve(constructor); });
        }
      };
      CustomElementRegistryImpl.prototype.get = function get(name) {
        var definition = definitions[String(name)];
        return definition ? definition.constructor : undefined;
      };
      CustomElementRegistryImpl.prototype.getName = function getName(constructor) {
        var definition = byConstructor.get(constructor);
        return definition ? definition.name : null;
      };
      CustomElementRegistryImpl.prototype.whenDefined = function whenDefined(name) {
        name = String(name);
        if (!isCustomName(name)) {
          return Promise.reject(new DOMException("Failed to execute 'whenDefined' on 'CustomElementRegistry': \"" + name + '" is not a valid custom element name', 'SyntaxError'));
        }
        if (Object.prototype.hasOwnProperty.call(definitions, name)) return Promise.resolve(definitions[name].constructor);
        return new Promise(function (resolve) {
          (waiters[name] = waiters[name] || []).push(resolve);
        });
      };
      CustomElementRegistryImpl.prototype.upgrade = function upgradeRoot(root) {
        elementsOf(root).forEach(upgrade);
      };
      defineProperty(CustomElementRegistryImpl.prototype, Symbol.toStringTag, {
        value: 'CustomElementRegistry', configurable: true
      });
      var registry = Object.create(CustomElementRegistryImpl.prototype);
      defineProperty(global, 'CustomElementRegistry', {
        value: CustomElementRegistryImpl, writable: true, configurable: true, enumerable: false
      });
      defineProperty(global, 'customElements', {
        value: registry, writable: true, configurable: true, enumerable: true
      });
    })();
  }

  // --------------------------------------------------------------- selection
  // There is no way to select text, so the selection is always empty.
  if (typeof getSelection === 'undefined') {
    var emptySelection = {
      anchorNode: null, anchorOffset: 0, focusNode: null, focusOffset: 0,
      isCollapsed: true, rangeCount: 0, type: 'None',
      toString: function () { return ''; },
      removeAllRanges: function () {}, empty: function () {}, addRange: function () {},
      removeRange: function () {}, collapse: function () {}, collapseToStart: function () {},
      collapseToEnd: function () {}, selectAllChildren: function () {}, extend: function () {},
      containsNode: function () { return false; }, getRangeAt: function () {
        throw new DOMException('Index or size is negative or greater than the allowed amount', 'IndexSizeError');
      }
    };
    define(global, 'getSelection', function getSelection() { return emptySelection; });
    define(documentPrototype, 'getSelection', function getSelection() { return emptySelection; });
  }
})(this);
