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
  define(String.prototype, 'matchAll', function matchAll(pattern) {
    var text = String(this);
    var expression;
    if (pattern instanceof RegExp) {
      if (pattern.flags.indexOf('g') === -1) {
        throw new TypeError('String.prototype.matchAll called with a non-global RegExp argument');
      }
      expression = new RegExp(pattern.source, pattern.flags);
      expression.lastIndex = pattern.lastIndex;
    } else {
      expression = new RegExp(pattern, 'g');
    }
    // Eager rather than lazy: the matches are computed up front and then
    // iterated. Observable only through `lastIndex` side effects mid-iteration.
    var matches = [];
    var match;
    while ((match = expression.exec(text)) !== null) {
      matches.push(match);
      if (match[0] === '') expression.lastIndex++;
    }
    return matches[Symbol.iterator]();
  });
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
})(this);
