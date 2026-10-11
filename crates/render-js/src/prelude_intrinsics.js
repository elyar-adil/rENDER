// ECMAScript built-ins that are written in JavaScript. Every realm gets them:
// the page's realm before its first script (`JsRuntime::ensure_prelude`), and
// each realm that `CreateRealm` makes (`JsRuntime::create_realm`). The browser's
// own built-ins are in `prelude.js`, which only the page's realm runs.
(function (global) {
  var defineProperty = Object.defineProperty;

  // Annex B.2.2.15 and B.2.2.16: `trimLeft` and `trimRight` are the same
  // functions as `trimStart` and `trimEnd`.
  if (String.prototype.trimLeft === undefined) {
    defineProperty(String.prototype, 'trimLeft', {
      value: String.prototype.trimStart, writable: true, configurable: true, enumerable: false
    });
  }
  if (String.prototype.trimRight === undefined) {
    defineProperty(String.prototype, 'trimRight', {
      value: String.prototype.trimEnd, writable: true, configurable: true, enumerable: false
    });
  }

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
})(this);
