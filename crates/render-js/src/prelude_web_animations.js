// Web Animations API core: `Element.animate`, playback control, animation
// events and a frame-driven timeline.
//
// The rendering pipeline is single-shot, so an animation writes the sampled
// value into the element's inline style and the normal DOM invalidation path
// repaints it. The embedding calls `__renderTickWebAnimations` with the same
// document timestamp used by the CSS animation sampler, giving CSS and WAAPI a
// single timeline. `performance.now()` remains the fallback clock for a realm
// without an embedding frame.
(function (global) {
  if (typeof Element === 'undefined' || typeof Element.prototype === 'undefined') return;
  if (typeof Element.prototype.animate === 'function') return;

  var defineProperty = Object.defineProperty;
  var animations = [];
  var timelineStart = null;

  function now() {
    return typeof performance !== 'undefined' && typeof performance.now === 'function'
      ? performance.now()
      : Date.now();
  }

  if (timelineStart === null) timelineStart = now();

  function number(value, fallback) {
    var result = Number(value);
    return result === result ? result : fallback;
  }

  function timingFrom(options) {
    options = options || {};
    return {
      duration: Math.max(0, number(options.duration, 0)),
      delay: number(options.delay, 0),
      endDelay: number(options.endDelay, 0),
      iterations: options.iterations === Infinity ? Infinity : Math.max(0, number(options.iterations, 1)),
      direction: options.direction || 'normal',
      fill: options.fill || 'none',
      easing: options.easing || 'linear'
    };
  }

  function camelToKebab(name) {
    if (name === 'cssFloat') return 'float';
    return String(name).replace(/[A-Z]/g, function (letter) {
      return '-' + letter.toLowerCase();
    });
  }

  function keyframeOffset(value, index, count) {
    if (value !== undefined && value !== null && isFinite(Number(value))) {
      return Math.max(0, Math.min(1, Number(value)));
    }
    return count <= 1 ? 0 : index / (count - 1);
  }

  // A property track is a sorted list of {offset, value}; keyframes may use the
  // array form ([{opacity: 0}, {opacity: 1}]) or the property-list form
  // ({opacity: [0, 1]}).
  function buildTracks(keyframes, defaultComposite) {
    var tracks = Object.create(null);
    var meta = { offset: true, easing: true, composite: true };
    function add(name, offset, value, composite) {
      if (meta[name] || value === undefined || value === null) return;
      name = camelToKebab(name);
      (tracks[name] || (tracks[name] = [])).push({
        offset: offset,
        value: value,
        composite: composite || defaultComposite || 'replace'
      });
    }
    if (Array.isArray(keyframes)) {
      var count = keyframes.length;
      for (var index = 0; index < count; index++) {
        var frame = keyframes[index];
        if (frame === null || typeof frame !== 'object') {
          throw new TypeError('Keyframe must be an object');
        }
        var offset = keyframeOffset(frame.offset, index, count);
        var names = Object.keys(frame);
        for (var n = 0; n < names.length; n++) {
          add(names[n], offset, frame[names[n]], frame.composite);
        }
      }
    } else if (keyframes && typeof keyframes === 'object') {
      var names = Object.keys(keyframes);
      for (var n = 0; n < names.length; n++) {
        var values = keyframes[names[n]];
        if (!Array.isArray(values)) values = [values];
        for (var index = 0; index < values.length; index++) {
          add(names[n], keyframeOffset(null, index, values.length), values[index], defaultComposite);
        }
      }
    }
    var result = [];
    Object.keys(tracks).forEach(function (name) {
      var points = tracks[name];
      points.sort(function (a, b) { return a.offset - b.offset; });
      result.push({
        property: name,
        points: points,
        composite: points[points.length - 1].composite || 'replace'
      });
    });
    return result;
  }

  function splitNumbers(value) {
    var text = String(value);
    var parts = text.split(/([+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?)/i);
    var literals = [];
    var numbers = [];
    for (var i = 0; i < parts.length; i++) {
      if (i % 2 === 0) literals.push(parts[i]);
      else numbers.push(Number(parts[i]));
    }
    return { literals: literals, numbers: numbers, count: numbers.length };
  }

  function sameLiterals(a, b) {
    if (a.literals.length !== b.literals.length) return false;
    for (var i = 0; i < a.literals.length; i++) {
      if (a.literals[i] !== b.literals[i]) return false;
    }
    return true;
  }

  function formatNumber(value) {
    if (Math.abs(value - Math.round(value)) < 0.0001) return String(Math.round(value));
    return String(Math.round(value * 10000) / 10000);
  }

  function zeroTransform(value) {
    if (String(value) === 'none') return 'none';
    return String(value).replace(/([a-zA-Z0-9]+)\(([^)]*)\)/g, function (_, name, args) {
      var identity = /^scale/i.test(name) ? '1' : '0';
      return name + '(' + args.replace(/[+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?/gi, identity) + ')';
    });
  }

  function parseHexColor(value) {
    var text = String(value).trim();
    if (text.charAt(0) !== '#') return null;
    text = text.substring(1);
    if (text.length === 3 || text.length === 4) {
      text = text.split('').map(function (digit) { return digit + digit; }).join('');
    }
    if (text.length !== 6 && text.length !== 8) return null;
    return {
      r: parseInt(text.substring(0, 2), 16),
      g: parseInt(text.substring(2, 4), 16),
      b: parseInt(text.substring(4, 6), 16),
      a: text.length === 8 ? parseInt(text.substring(6, 8), 16) / 255 : 1
    };
  }

  function interpolateColor(a, b, progress) {
    return 'rgba(' +
      Math.round(a.r + (b.r - a.r) * progress) + ', ' +
      Math.round(a.g + (b.g - a.g) * progress) + ', ' +
      Math.round(a.b + (b.b - a.b) * progress) + ', ' +
      formatNumber(a.a + (b.a - a.a) * progress) + ')';
  }

  function interpolateValue(from, to, progress) {
    var a = String(from);
    var b = String(to);
    if (a === b) return a;
    var colorA = parseHexColor(a);
    var colorB = parseHexColor(b);
    if (colorA && colorB) return interpolateColor(colorA, colorB, progress);
    if (a === 'none' || b === 'none') {
      if (a === 'none') a = zeroTransform(b);
      else b = zeroTransform(a);
    }
    var splitA = splitNumbers(a);
    var splitB = splitNumbers(b);
    if (splitA.count > 0 && splitA.count === splitB.count && sameLiterals(splitA, splitB)) {
      var output = '';
      for (var i = 0; i < splitA.count; i++) {
        output += splitA.literals[i] +
          formatNumber(splitA.numbers[i] + (splitB.numbers[i] - splitA.numbers[i]) * progress);
      }
      return output + splitA.literals[splitA.literals.length - 1];
    }
    return progress < 0.5 ? a : b;
  }

  function addValues(base, value) {
    if (base === '' || base === 'none') return value;
    if (value === '' || value === 'none') return base;
    var a = splitNumbers(base);
    var b = splitNumbers(value);
    if (a.count > 0 && a.count === b.count && sameLiterals(a, b)) {
      var output = '';
      for (var i = 0; i < a.count; i++) {
        output += a.literals[i] + formatNumber(a.numbers[i] + b.numbers[i]);
      }
      return output + a.literals[a.literals.length - 1];
    }
    if (/^[a-zA-Z]+\(/.test(base) && /^[a-zA-Z]+\(/.test(value)) {
      return base + ' ' + value;
    }
    return value;
  }

  function cubicBezier(x1, y1, x2, y2, x) {
    function coordinate(t, first, second) {
      var inverse = 1 - t;
      return 3 * inverse * inverse * t * first + 3 * inverse * t * t * second + t * t * t;
    }
    function slope(t, first, second) {
      var inverse = 1 - t;
      return 3 * inverse * inverse * first + 6 * inverse * t * (second - first) + 3 * t * t * (1 - second);
    }
    var guess = x;
    for (var i = 0; i < 8; i++) {
      var current = coordinate(guess, x1, x2);
      var derivative = slope(guess, x1, x2);
      if (Math.abs(current - x) < 0.00001 || Math.abs(derivative) < 0.00001) break;
      guess = Math.max(0, Math.min(1, guess - (current - x) / derivative));
    }
    return Math.max(0, Math.min(1, coordinate(guess, y1, y2)));
  }

  function easing(value, progress) {
    var text = String(value || 'linear').split(',')[0].trim();
    if (text.indexOf('cubic-bezier(') === 0) {
      var values = text.substring(13, text.length - 1).split(',').map(Number);
      if (values.length === 4) return cubicBezier(values[0], values[1], values[2], values[3], progress);
    }
    if (text.indexOf('steps(') === 0) {
      var parts = text.substring(6, text.length - 1).split(',');
      var count = Math.max(1, Number(parts[0]) || 1);
      var position = (parts[1] || 'end').trim();
      var stepped = /start|jump-start/i.test(position)
        ? Math.ceil(progress * count) / count
        : Math.floor(progress * count) / count;
      return Math.max(0, Math.min(1, stepped));
    }
    if (text === 'ease-in') return cubicBezier(0.42, 0, 1, 1, progress);
    if (text === 'ease-out') return cubicBezier(0, 0, 0.58, 1, progress);
    if (text === 'ease-in-out') return cubicBezier(0.42, 0, 0.58, 1, progress);
    if (text === 'ease') return cubicBezier(0.25, 0.1, 0.25, 1, progress);
    return progress;
  }

  function sampleTrack(points, progress) {
    if (points.length === 0) return undefined;
    if (progress <= points[0].offset) return points[0].value;
    if (progress >= points[points.length - 1].offset) return points[points.length - 1].value;
    for (var i = 1; i < points.length; i++) {
      if (progress <= points[i].offset) {
        var before = points[i - 1];
        var after = points[i];
        var local = (progress - before.offset) / (after.offset - before.offset);
        return interpolateValue(before.value, after.value, local);
      }
    }
    return points[points.length - 1].value;
  }

  function activeProgress(timing, elapsed, playbackRate) {
    var start = timing.delay;
    var total = timing.duration * timing.iterations;
    var backwards = timing.fill.indexOf('backwards') >= 0 || timing.fill.indexOf('both') >= 0;
    var forwards = timing.fill.indexOf('forwards') >= 0 || timing.fill.indexOf('both') >= 0;
    if (playbackRate < 0 && elapsed <= 0) {
      return { progress: backwards ? 0 : null, finished: true };
    }
    if (elapsed < start) return { progress: backwards ? 0 : null, finished: playbackRate < 0 };
    if (playbackRate < 0 && elapsed >= start + total) {
      var finalIteration = Math.max(0, Math.ceil(timing.iterations) - 1);
      var finalReverse = timing.direction === 'reverse' ||
        (timing.direction === 'alternate' && finalIteration % 2 === 1) ||
        (timing.direction === 'alternate-reverse' && finalIteration % 2 === 0);
      return { progress: forwards ? (finalReverse ? 0 : 1) : null, finished: false };
    }
    if (total === 0) return { progress: forwards ? 1 : null, finished: true };
    var active = elapsed - start;
    if (total !== Infinity && active >= total) {
      var lastIteration = Math.max(0, Math.ceil(timing.iterations) - 1);
      var reverse = timing.direction === 'reverse' ||
        (timing.direction === 'alternate' && lastIteration % 2 === 1) ||
        (timing.direction === 'alternate-reverse' && lastIteration % 2 === 0);
      return { progress: forwards ? (reverse ? 0 : 1) : null, finished: true };
    }
    var position = active / timing.duration;
    var iteration = Math.floor(position);
    var local = position - iteration;
    var reverse = timing.direction === 'reverse' ||
      (timing.direction === 'alternate' && iteration % 2 === 1) ||
      (timing.direction === 'alternate-reverse' && iteration % 2 === 0);
    return { progress: reverse ? 1 - local : local, finished: false };
  }

  function fire(animation, type) {
    var event = {
      type: type,
      target: animation,
      currentTarget: animation,
      bubbles: false,
      cancelable: false,
      defaultPrevented: false,
      timeStamp: now(),
      preventDefault: function () {},
      stopPropagation: function () {},
      stopImmediatePropagation: function () {}
    };
    var handler = animation['on' + type];
    if (typeof handler === 'function') {
      try { handler.call(animation, event); } catch (error) { setTimeout(function () { throw error; }, 0); }
    }
    var listeners = animation._listeners[type];
    if (listeners) {
      listeners.slice().forEach(function (listener) {
        try { listener.call(animation, event); } catch (error) { setTimeout(function () { throw error; }, 0); }
      });
    }
  }

  // The embedding drives this function once per document animation frame. It
  // uses the same document timestamp as the CSS animation sampler, so CSS and
  // WAAPI share one timeline instead of running on separate timers.
  global.__renderTickWebAnimations = function (time) {
    var sampleTime = Number(time);
    if (!isFinite(sampleTime)) sampleTime = now();
    var running = false;
    for (var i = 0; i < animations.length; i++) {
      var animation = animations[i];
      if (animation._playState === 'running') {
        animation._sample(sampleTime);
        if (animation._playState === 'running') running = true;
      }
    }
    return running;
  };

  function Animation(target, keyframes, options) {
    this._target = target;
    this._timing = timingFrom(options);
    this._playbackRate = options && options.playbackRate !== undefined ? number(options.playbackRate, 1) : 1;
    this._composite = (options && options.composite) || 'replace';
    this._tracks = buildTracks(keyframes, this._composite);
    this.timeline = timeline;
    this._playState = 'idle';
    this._startTime = 0;
    this._holdTime = 0;
    this._currentTime = 0;
    this._finished = false;
    this._listeners = Object.create(null);
    this._saved = Object.create(null);
    this._tracks.forEach(function (track) {
      try { this._saved[track.property] = target.style.getPropertyValue(track.property); }
      catch (error) { this._saved[track.property] = ''; }
    }, this);
    var self = this;
    this.finished = new Promise(function (resolve, reject) {
      self._resolveFinished = resolve;
      self._rejectFinished = reject;
    });
    this.ready = Promise.resolve(this);
    this.effect = {
      target: target,
      getTiming: function () {
        return {
          duration: self._timing.duration,
          delay: self._timing.delay,
          endDelay: self._timing.endDelay,
          iterations: self._timing.iterations,
          direction: self._timing.direction,
          fill: self._timing.fill,
          easing: self._timing.easing
        };
      },
      getKeyframes: function () {
        return self._tracks.map(function (track) {
          return { offset: null, easing: self._timing.easing, property: track.property };
        });
      }
    };
    animations.push(this);
  }

  Animation.prototype._restore = function () {
    var self = this;
    Object.keys(this._saved).forEach(function (name) {
      var previous = self._saved[name];
      if (previous === '' || previous === null) self._target.style.removeProperty(name);
      else self._target.style.setProperty(name, previous);
    });
  };

  Animation.prototype._apply = function (progress) {
    if (progress === null) {
      this._restore();
      return;
    }
    var self = this;
    this._tracks.forEach(function (track) {
      var value = sampleTrack(track.points, easing(self._timing.easing, progress));
      if (value === undefined || value === null) return;
      var composite = track.composite || self._composite;
      if (composite === 'add' || composite === 'accumulate') {
        var base = self._target.style.getPropertyValue(track.property);
        value = addValues(base, String(value));
      }
      self._target.style.setProperty(track.property, String(value));
    });
  };

  Animation.prototype._endTime = function () {
    var total = this._timing.duration * this._timing.iterations;
    return this._timing.delay + (total === Infinity ? Infinity : total + this._timing.endDelay);
  };

  Animation.prototype._finish = function () {
    if (this._playState === 'finished') return;
    this._playState = 'finished';
    this._currentTime = this._playbackRate < 0 ? 0 : this._endTime();
    if (this._timing.fill.indexOf('forwards') < 0 && this._timing.fill.indexOf('both') < 0) this._restore();
    this._finished = true;
    if (this._resolveFinished) this._resolveFinished(this);
    fire(this, 'finish');
  };

  Animation.prototype._sample = function (time) {
    if (this._playState !== 'running') return;
    var elapsed = (time - this._startTime) * this._playbackRate;
    this._currentTime = elapsed;
    var result = activeProgress(this._timing, elapsed, this._playbackRate);
    this._apply(result.progress);
    if (result.finished) this._finish();
  };

  Animation.prototype.play = function () {
    if (this._playState === 'running') return;
    var rate = this._playbackRate;
    if (rate === 0) return;
    if (this._playState === 'paused') {
      this._startTime = now() - this._holdTime / rate;
    } else {
      var start = rate < 0 ? this._endTime() : 0;
      this._startTime = now() - start / rate;
      this._currentTime = start;
    }
    this._finished = false;
    this._playState = 'running';
    this._sample(now());
  };

  Animation.prototype.pause = function () {
    if (this._playState !== 'running') return;
    this._holdTime = this._currentTime;
    this._playState = 'paused';
    this._sample(now());
  };

  Animation.prototype.finish = function () {
    var end = this._playbackRate < 0 ? 0 : this._endTime();
    this._playState = 'running';
    this._startTime = now() - end / this._playbackRate;
    this._currentTime = end;
    this._sample(now());
    if (this._playState !== 'finished') this._finish();
  };

  Animation.prototype.cancel = function () {
    if (this._playState === 'idle') return;
    this._playState = 'idle';
    this._currentTime = 0;
    this._restore();
    if (this._rejectFinished && !this._finished) {
      var error;
      try { error = new DOMException('The animation was canceled', 'AbortError'); }
      catch (ignored) { error = new Error('The animation was canceled'); }
      this._rejectFinished(error);
    }
    this._finished = true;
    fire(this, 'cancel');
  };

  Animation.prototype.addEventListener = function (type, callback) {
    if (typeof callback !== 'function') return;
    (this._listeners[String(type)] || (this._listeners[String(type)] = [])).push(callback);
  };

  Animation.prototype.removeEventListener = function (type, callback) {
    var list = this._listeners[String(type)];
    if (list) this._listeners[String(type)] = list.filter(function (listener) { return listener !== callback; });
  };

  defineProperty(Animation.prototype, 'playState', {
    get: function () { return this._playState; },
    configurable: true
  });

  defineProperty(Animation.prototype, 'currentTime', {
    get: function () {
      if (this._playState === 'running') return (now() - this._startTime) * this._playbackRate;
      return this._currentTime;
    },
    set: function (value) {
      var next = Number(value);
      if (next !== next) return;
      this._holdTime = next;
      this._currentTime = next;
      if (this._playState === 'running' && this._playbackRate !== 0) {
        this._startTime = now() - next / this._playbackRate;
      }
      var result = activeProgress(this._timing, next, this._playbackRate);
      this._apply(result.progress);
    },
    configurable: true
  });

  defineProperty(Animation.prototype, 'playbackRate', {
    get: function () { return this._playbackRate; },
    set: function (value) {
      var next = Number(value);
      if (next !== next) return;
      var current = this.currentTime;
      this._playbackRate = next;
      if (next === 0) {
        if (this._playState === 'running') {
          this._holdTime = current;
          this._currentTime = current;
          this._playState = 'paused';
        }
      } else if (this._playState === 'running') {
        this._startTime = now() - current / next;
      }
    },
    configurable: true
  });

  Animation.prototype.updatePlaybackRate = function (value) {
    this.playbackRate = value;
  };

  Animation.prototype.reverse = function () {
    this.playbackRate = -this._playbackRate;
    if (this._playState === 'finished' || this._playState === 'idle') this.play();
  };

  defineProperty(Element.prototype, 'animate', {
    value: function animate(keyframes, options) {
      var animation = new Animation(this, keyframes, options);
      animation.play();
      return animation;
    },
    writable: true,
    configurable: true
  });

  defineProperty(Element.prototype, 'getAnimations', {
    value: function getAnimations() {
      var target = this;
      return animations.filter(function (animation) {
        return animation._target === target && animation._playState !== 'idle';
      });
    },
    writable: true,
    configurable: true
  });

  var timeline = {
    get currentTime() { return now() - timelineStart; }
  };
  if (typeof Document !== 'undefined' && Document.prototype && !Document.prototype.timeline) {
    defineProperty(Document.prototype, 'timeline', {
      get: function () { return timeline; },
      configurable: true
    });
  }

  defineProperty(global, 'Animation', {
    value: Animation,
    writable: true,
    configurable: true,
    enumerable: false
  });
})(this);
