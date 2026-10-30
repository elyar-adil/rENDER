// IndexedDB, in memory.
//
// The API follows the IndexedDB standard at the level pages use for caching
// and feature detection: `open` with version upgrade and `upgradeneeded`,
// object stores, read/write transactions that auto-commit, requests firing
// success/error, `get`/`put`/`add`/`delete`/`clear`/`count`, key ranges and
// cursors. Data lives for the lifetime of the realm: there is no on-disk
// persistence yet, so a reopened page starts from an empty database.
//
// Requests fire from microtasks. IndexedDB specifies tasks, but the runtime's
// event loop only exposes microtasks to self-hosted built-ins; ordering
// within a transaction is preserved either way.
(function (global) {
  if (global.indexedDB !== undefined) return;

  var defineProperty = Object.defineProperty;
  var queue = typeof global.queueMicrotask === 'function'
    ? function (callback) { global.queueMicrotask(callback); }
    : function (callback) { Promise.resolve().then(callback); };
  var databases = Object.create(null);
  var loaded = Object.create(null);
  var nextRequestId = 1;
  var persisted = null;

  // The host seeds `__renderIndexedDbData` with this origin's databases before
  // document scripts run. Keeping it lazy means the prelude can be evaluated
  // before that seed and still observe it on the first `open`.
  function persistedStore() {
    if (persisted !== null) return persisted;
    persisted = {};
    try {
      if (typeof global.__renderIndexedDbData === 'string' && global.__renderIndexedDbData) {
        persisted = JSON.parse(global.__renderIndexedDbData);
      }
    } catch (error) {
      persisted = {};
    }
    return persisted;
  }

  function persistedCommit() {
    try {
      global.__renderIndexedDbData = JSON.stringify(persistedStore());
    } catch (error) {}
  }

  function storageGet(key) {
    var value = persistedStore()[key];
    return value === undefined ? null : JSON.stringify(value);
  }

  function storageSet(key, value) {
    try {
      persistedStore()[key] = JSON.parse(value);
      persistedCommit();
    } catch (error) {}
  }

  function storageRemove(key) {
    delete persistedStore()[key];
    persistedCommit();
  }

  function encodePersisted(value) {
    if (value instanceof Date) return { __renderType: 'date', value: value.getTime() };
    if (Array.isArray(value)) {
      return { __renderType: 'array', value: value.map(encodePersisted) };
    }
    if (value && typeof value === 'object') {
      var object = {};
      var names = Object.keys(value);
      for (var index = 0; index < names.length; index++) {
        object[names[index]] = encodePersisted(value[names[index]]);
      }
      return object;
    }
    return value;
  }

  function decodePersisted(value) {
    if (value && typeof value === 'object') {
      if (value.__renderType === 'date') return new Date(value.value);
      if (value.__renderType === 'array') {
        return (value.value || []).map(decodePersisted);
      }
      var object = {};
      var names = Object.keys(value);
      for (var index = 0; index < names.length; index++) {
        object[names[index]] = decodePersisted(value[names[index]]);
      }
      return object;
    }
    return value;
  }

  function loadDatabase(name) {
    if (loaded[name]) return;
    loaded[name] = true;
    var raw = storageGet(name);
    if (!raw) return;
    try {
      var data = JSON.parse(raw);
      var meta = {
        name: name,
        version: Number(data.version) || 0,
        stores: Object.create(null),
        connections: []
      };
      var storeNames = data.stores ? Object.keys(data.stores) : [];
      for (var storeIndex = 0; storeIndex < storeNames.length; storeIndex++) {
        var storeName = storeNames[storeIndex];
        var saved = data.stores[storeName];
        var store = {
          name: storeName,
          keyPath: saved.keyPath === undefined ? null : decodePersisted(saved.keyPath),
          autoIncrement: !!saved.autoIncrement,
          nextKey: Number(saved.nextKey) || 1,
          indexes: Object.create(null),
          records: []
        };
        var indexNames = saved.indexes ? Object.keys(saved.indexes) : [];
        for (var indexIndex = 0; indexIndex < indexNames.length; indexIndex++) {
          var indexName = indexNames[indexIndex];
          var savedIndex = saved.indexes[indexName];
          store.indexes[indexName] = {
            name: indexName,
            keyPath: decodePersisted(savedIndex.keyPath),
            unique: !!savedIndex.unique,
            multiEntry: !!savedIndex.multiEntry
          };
        }
        var records = saved.records || [];
        for (var recordIndex = 0; recordIndex < records.length; recordIndex++) {
          store.records.push({
            key: decodePersisted(records[recordIndex].key),
            value: decodePersisted(records[recordIndex].value)
          });
        }
        meta.stores[storeName] = store;
      }
      databases[name] = meta;
    } catch (error) {
      // A damaged persisted database starts empty, like a fresh profile.
      databases[name] = {
        name: name,
        version: 0,
        stores: Object.create(null),
        connections: []
      };
    }
  }

  function saveDatabase(name) {
    var meta = databases[name];
    if (!meta) return;
    var stores = {};
    var storeNames = Object.keys(meta.stores);
    for (var storeIndex = 0; storeIndex < storeNames.length; storeIndex++) {
      var storeName = storeNames[storeIndex];
      var store = meta.stores[storeName];
      var indexes = {};
      var indexNames = Object.keys(store.indexes || {});
      for (var indexIndex = 0; indexIndex < indexNames.length; indexIndex++) {
        var indexName = indexNames[indexIndex];
        var savedIndex = store.indexes[indexName];
        indexes[indexName] = {
          keyPath: encodePersisted(savedIndex.keyPath),
          unique: !!savedIndex.unique,
          multiEntry: !!savedIndex.multiEntry
        };
      }
      stores[storeName] = {
        keyPath: encodePersisted(store.keyPath),
        autoIncrement: !!store.autoIncrement,
        nextKey: store.nextKey,
        indexes: indexes,
        records: store.records.map(function (record) {
          return {
            key: encodePersisted(record.key),
            value: encodePersisted(record.value)
          };
        })
      };
    }
    storageSet(name, JSON.stringify({
      version: meta.version,
      stores: stores
    }));
  }

  function schedule(callback) {
    queue(callback);
  }

  function domError(name, message) {
    if (typeof DOMException === 'function') {
      try { return new DOMException(message, name); } catch (error) {}
    }
    var error = new Error(message);
    error.name = name;
    return error;
  }

  function clone(value) {
    if (value === undefined) return undefined;
    if (typeof structuredClone === 'function') {
      try { return structuredClone(value); } catch (error) { return value; }
    }
    return value;
  }

  function cloneKey(key) {
    if (key instanceof Date) return new Date(key.getTime());
    if (Array.isArray(key)) return key.map(cloneKey);
    return key;
  }

  function keyRank(key) {
    if (typeof key === 'number') return 1;
    if (key instanceof Date) return 2;
    if (typeof key === 'string') return 3;
    if (Array.isArray(key)) return 4;
    return 0;
  }

  function validKey(key) {
    if (typeof key === 'number') return key === key;
    if (typeof key === 'string') return true;
    if (key instanceof Date) return !isNaN(key.getTime());
    if (Array.isArray(key)) {
      for (var index = 0; index < key.length; index++) {
        if (!validKey(key[index])) return false;
      }
      return true;
    }
    return false;
  }

  function compareKeys(left, right) {
    var leftRank = keyRank(left);
    var rightRank = keyRank(right);
    if (leftRank !== rightRank) return leftRank < rightRank ? -1 : 1;
    if (leftRank === 1) return left < right ? -1 : left > right ? 1 : 0;
    if (leftRank === 2) {
      var leftTime = left.getTime();
      var rightTime = right.getTime();
      return leftTime < rightTime ? -1 : leftTime > rightTime ? 1 : 0;
    }
    if (leftRank === 3) return left < right ? -1 : left > right ? 1 : 0;
    if (leftRank === 4) {
      var count = Math.min(left.length, right.length);
      for (var index = 0; index < count; index++) {
        var compared = compareKeys(left[index], right[index]);
        if (compared !== 0) return compared;
      }
      return left.length < right.length ? -1 : left.length > right.length ? 1 : 0;
    }
    return 0;
  }

  function addListener(target, type, callback, options) {
    if (callback === null || callback === undefined) return;
    var listeners = target._listeners[type];
    if (!listeners) listeners = target._listeners[type] = [];
    listeners.push({
      callback: callback,
      once: !!(options && typeof options === 'object' && options.once)
    });
  }

  function removeListener(target, type, callback) {
    var listeners = target._listeners[type];
    if (!listeners) return;
    for (var index = listeners.length - 1; index >= 0; index--) {
      if (listeners[index].callback === callback) listeners.splice(index, 1);
    }
  }

  function fire(target, type, init) {
    var event = {
      type: type,
      target: target,
      currentTarget: target,
      bubbles: false,
      cancelable: false,
      defaultPrevented: false,
      preventDefault: function () {
        if (event.cancelable) event.defaultPrevented = true;
      },
      stopPropagation: function () {},
      stopImmediatePropagation: function () {}
    };
    if (init) {
      for (var key in init) event[key] = init[key];
    }
    var failure = null;
    var handler = target['on' + type];
    if (typeof handler === 'function') {
      try { handler.call(target, event); } catch (error) { failure = error; }
    }
    var listeners = target._listeners[type];
    if (listeners) {
      var copy = listeners.slice();
      for (var index = 0; index < copy.length; index++) {
        var listener = copy[index];
        if (listener.once) removeListener(target, type, listener.callback);
        try { listener.callback.call(target, event); } catch (error) {
          if (!failure) failure = error;
        }
      }
    }
    if (failure) {
      schedule(function () { throw failure; });
    }
    return !event.defaultPrevented;
  }

  function eventTarget(prototype) {
    defineProperty(prototype, 'addEventListener', {
      value: function (type, callback, options) {
        addListener(this, String(type), callback, options);
      },
      writable: true,
      configurable: true
    });
    defineProperty(prototype, 'removeEventListener', {
      value: function (type, callback) {
        removeListener(this, String(type), callback);
      },
      writable: true,
      configurable: true
    });
    defineProperty(prototype, 'dispatchEvent', {
      value: function (event) {
        return fire(this, String(event.type), event);
      },
      writable: true,
      configurable: true
    });
  }

  // ------------------------------------------------------------------ requests
  function IDBRequest() {
    this.result = undefined;
    this.error = null;
    this.readyState = 'pending';
    this.source = null;
    this.transaction = null;
    this.onsuccess = null;
    this.onerror = null;
    this._listeners = Object.create(null);
    this._id = nextRequestId++;
  }
  eventTarget(IDBRequest.prototype);

  IDBRequest.prototype._finish = function (ok, value) {
    if (this.readyState === 'done') return;
    this.readyState = 'done';
    if (ok) {
      this.result = value;
      fire(this, 'success');
    } else {
      this.error = value;
      this.result = undefined;
      fire(this, 'error');
    }
  };

  function IDBOpenDBRequest() {
    IDBRequest.call(this);
    this.onblocked = null;
    this.onupgradeneeded = null;
  }
  IDBOpenDBRequest.prototype = Object.create(IDBRequest.prototype);
  IDBOpenDBRequest.prototype.constructor = IDBOpenDBRequest;

  // ------------------------------------------------------------------ key range
  function IDBKeyRange(lower, upper, lowerOpen, upperOpen) {
    this.lower = lower;
    this.upper = upper;
    this.lowerOpen = lowerOpen;
    this.upperOpen = upperOpen;
  }
  IDBKeyRange.only = function (value) {
    if (!validKey(value)) throw domError('DataError', 'The key is not a valid key');
    return new IDBKeyRange(value, value, false, false);
  };
  IDBKeyRange.lowerBound = function (value, open) {
    if (!validKey(value)) throw domError('DataError', 'The key is not a valid key');
    return new IDBKeyRange(value, undefined, !!open, true);
  };
  IDBKeyRange.upperBound = function (value, open) {
    if (!validKey(value)) throw domError('DataError', 'The key is not a valid key');
    return new IDBKeyRange(undefined, value, true, !!open);
  };
  IDBKeyRange.bound = function (lower, upper, lowerOpen, upperOpen) {
    if (!validKey(lower) || !validKey(upper)) {
      throw domError('DataError', 'The key is not a valid key');
    }
    if (compareKeys(lower, upper) > 0) {
      throw domError('DataError', 'The lower key is greater than the upper key');
    }
    return new IDBKeyRange(lower, upper, !!lowerOpen, !!upperOpen);
  };
  IDBKeyRange.prototype.contains = function (key) {
    if (!validKey(key)) return false;
    if (this.lower !== undefined) {
      var lower = compareKeys(key, this.lower);
      if (lower < 0 || (lower === 0 && this.lowerOpen)) return false;
    }
    if (this.upper !== undefined) {
      var upper = compareKeys(key, this.upper);
      if (upper > 0 || (upper === 0 && this.upperOpen)) return false;
    }
    return true;
  };

  function inRange(range, key) {
    if (range === null || range === undefined) return true;
    if (typeof range.contains === 'function') return range.contains(key);
    return true;
  }

  // ------------------------------------------------------------------ stores
  function extractKey(keyPath, value) {
    if (keyPath === null || keyPath === undefined) return undefined;
    var path = Array.isArray(keyPath) ? keyPath : String(keyPath).split('.');
    var current = value;
    for (var index = 0; index < path.length; index++) {
      if (current === null || current === undefined) return undefined;
      current = current[path[index]];
    }
    return current;
  }

  function findRecord(records, key) {
    for (var index = 0; index < records.length; index++) {
      if (compareKeys(records[index].key, key) === 0) return records[index];
    }
    return null;
  }

  function sortedRecords(store) {
    return store.records.slice().sort(function (left, right) {
      return compareKeys(left.key, right.key);
    });
  }

  function indexKeys(store, index, value) {
    var extracted = extractKey(index.keyPath, value);
    if (extracted === undefined) return [];
    var keys = index.multiEntry && Array.isArray(extracted) ? extracted : [extracted];
    var result = [];
    for (var indexKey = 0; indexKey < keys.length; indexKey++) {
      if (validKey(keys[indexKey])) result.push(keys[indexKey]);
    }
    return result;
  }

  function indexEntries(store, index) {
    var entries = [];
    var records = store.records;
    for (var recordIndex = 0; recordIndex < records.length; recordIndex++) {
      var record = records[recordIndex];
      var keys = indexKeys(store, index, record.value);
      for (var keyIndex = 0; keyIndex < keys.length; keyIndex++) {
        entries.push({
          key: keys[keyIndex],
          primaryKey: record.key,
          value: record.value
        });
      }
    }
    entries.sort(function (left, right) {
      var compared = compareKeys(left.key, right.key);
      return compared === 0 ? compareKeys(left.primaryKey, right.primaryKey) : compared;
    });
    return entries;
  }

  function validateIndexConstraints(store, record, ignoredRecord) {
    var indexNames = Object.keys(store.indexes || {});
    for (var indexIndex = 0; indexIndex < indexNames.length; indexIndex++) {
      var index = store.indexes[indexNames[indexIndex]];
      if (!index.unique) continue;
      var keys = indexKeys(store, index, record.value);
      for (var keyIndex = 0; keyIndex < keys.length; keyIndex++) {
        for (var recordIndex = 0; recordIndex < store.records.length; recordIndex++) {
          var existing = store.records[recordIndex];
          if (existing === ignoredRecord) continue;
          var existingKeys = indexKeys(store, index, existing.value);
          for (var existingIndex = 0; existingIndex < existingKeys.length; existingIndex++) {
            if (compareKeys(existingKeys[existingIndex], keys[keyIndex]) === 0) {
              throw domError('ConstraintError', 'The index has a uniqueness constraint');
            }
          }
        }
      }
    }
  }

  function nameList(meta) {
    var names = Object.keys(meta.stores);
    var list = names.slice();
    list.contains = function (name) { return names.indexOf(String(name)) !== -1; };
    list.item = function (index) {
      return names[index] === undefined ? null : names[index];
    };
    return list;
  }

  function IDBDatabase(meta) {
    this.name = meta.name;
    this.version = meta.version;
    this.onabort = null;
    this.onclose = null;
    this.onerror = null;
    this.onversionchange = null;
    this._meta = meta;
    this._upgrading = false;
    this._closed = false;
    this._listeners = Object.create(null);
    this.objectStoreNames = nameList(meta);
    if (!meta.connections) meta.connections = [];
    meta.connections.push(this);
  }
  eventTarget(IDBDatabase.prototype);

  IDBDatabase.prototype.createObjectStore = function (name, options) {
    name = String(name);
    if (!this._upgrading) {
      throw domError('InvalidStateError', 'A version change transaction is required');
    }
    if (this._meta.stores[name] !== undefined) {
      throw domError('ConstraintError', 'An object store with that name already exists');
    }
    options = options || {};
    var keyPath = options.keyPath === undefined ? null : options.keyPath;
    if (keyPath !== null) {
      if (Array.isArray(keyPath)) {
        for (var index = 0; index < keyPath.length; index++) {
          if (typeof keyPath[index] !== 'string') {
            throw domError('SyntaxError', 'The key path is not a valid key path');
          }
        }
      } else if (typeof keyPath !== 'string') {
        throw domError('SyntaxError', 'The key path is not a valid key path');
      }
    }
    this._meta.stores[name] = {
      name: name,
      keyPath: keyPath,
      autoIncrement: !!options.autoIncrement,
      indexes: Object.create(null),
      records: [],
      nextKey: 1
    };
    this.objectStoreNames = nameList(this._meta);
    return new IDBObjectStore(this._meta.stores[name], null);
  };

  IDBDatabase.prototype.deleteObjectStore = function (name) {
    name = String(name);
    if (!this._upgrading) {
      throw domError('InvalidStateError', 'A version change transaction is required');
    }
    if (this._meta.stores[name] === undefined) {
      throw domError('NotFoundError', 'No object store with that name exists');
    }
    delete this._meta.stores[name];
    this.objectStoreNames = nameList(this._meta);
  };

  IDBDatabase.prototype.transaction = function (storeNames, mode) {
    if (this._closed) throw domError('InvalidStateError', 'The database connection is closed');
    var names;
    if (typeof storeNames === 'string') names = [storeNames];
    else names = Array.prototype.slice.call(storeNames);
    if (names.length === 0) {
      throw domError('InvalidAccessError', 'A transaction needs at least one object store');
    }
    for (var index = 0; index < names.length; index++) {
      if (this._meta.stores[names[index]] === undefined) {
        throw domError('NotFoundError', 'No object store named ' + names[index] + ' exists');
      }
    }
    mode = mode === undefined ? 'readonly' : String(mode);
    if (mode !== 'readonly' && mode !== 'readwrite') {
      throw new TypeError('The transaction mode must be readonly or readwrite');
    }
    return new IDBTransaction(this, names, mode);
  };

  IDBDatabase.prototype.close = function () {
    this._closed = true;
    var connections = this._meta.connections || [];
    var position = connections.indexOf(this);
    if (position !== -1) connections.splice(position, 1);
  };

  // ------------------------------------------------------------------ transactions
  function IDBTransaction(db, names, mode) {
    this.db = db;
    this.mode = mode;
    this.durability = 'default';
    this.objectStoreNames = names.slice();
    this.objectStoreNames.contains = function (name) {
      return names.indexOf(String(name)) !== -1;
    };
    this.onabort = null;
    this.oncomplete = null;
    this.onerror = null;
    this._listeners = Object.create(null);
    this._pending = 0;
    this._state = 'active';
  }
  eventTarget(IDBTransaction.prototype);

  IDBTransaction.prototype.objectStore = function (name) {
    name = String(name);
    if (this.objectStoreNames.contains(name) === false) {
      throw domError('NotFoundError', 'No object store named ' + name + ' exists in this transaction');
    }
    if (this._state !== 'active') {
      throw domError('TransactionInactiveError', 'The transaction is no longer active');
    }
    return new IDBObjectStore(this.db._meta.stores[name], this);
  };

  IDBTransaction.prototype.abort = function () {
    if (this._state === 'finished') return;
    this._state = 'finished';
    fire(this, 'abort');
  };

  IDBTransaction.prototype._requestFinished = function (failed) {
    this._pending--;
    if (this._state !== 'active') return;
    if (failed) {
      this._state = 'finished';
      fire(this, 'error');
      fire(this, 'abort');
      return;
    }
    if (this._pending === 0) {
      var self = this;
      this._state = 'committing';
      schedule(function () {
        if (self._state !== 'committing') return;
        self._state = 'finished';
        saveDatabase(self.db.name);
        fire(self, 'complete');
      });
    }
  };

  function requestFor(transaction, source, work) {
    var request = new IDBRequest();
    request.source = source;
    request.transaction = transaction;
    transaction._pending++;
    schedule(function () {
      if (transaction._state === 'finished') {
        request._finish(false, domError('TransactionInactiveError', 'The transaction is no longer active'));
        return;
      }
      var result;
      try {
        result = work();
      } catch (error) {
        request._finish(false, error);
        transaction._requestFinished(true);
        return;
      }
      request._finish(true, result);
      transaction._requestFinished(false);
    });
    return request;
  }

  // ------------------------------------------------------------------ object store
  function IDBObjectStore(meta, transaction) {
    this.name = meta.name;
    this.keyPath = meta.keyPath;
    this.autoIncrement = meta.autoIncrement;
    this.indexNames = Object.keys(meta.indexes || {});
    this.transaction = transaction;
    this._meta = meta;
    this._transaction = transaction;
  }

  IDBObjectStore.prototype._writeCheck = function () {
    if (!this._transaction) {
      throw domError('InvalidStateError', 'The object store has no transaction');
    }
    if (this._transaction.mode === 'readonly') {
      throw domError('ReadOnlyError', 'The transaction is read-only');
    }
    if (this._transaction._state !== 'active') {
      throw domError('TransactionInactiveError', 'The transaction is no longer active');
    }
  };

  IDBObjectStore.prototype._readCheck = function () {
    if (!this._transaction) {
      throw domError('InvalidStateError', 'The object store has no transaction');
    }
    if (this._transaction._state !== 'active') {
      throw domError('TransactionInactiveError', 'The transaction is no longer active');
    }
  };

  IDBObjectStore.prototype.get = function (key) {
    this._readCheck();
    if (!validKey(key)) throw domError('DataError', 'The key is not a valid key');
    var meta = this._meta;
    return requestFor(this._transaction, this, function () {
      var record = findRecord(meta.records, key);
      return record ? clone(record.value) : undefined;
    });
  };

  IDBObjectStore.prototype.getKey = function (key) {
    this._readCheck();
    if (!validKey(key)) throw domError('DataError', 'The key is not a valid key');
    var meta = this._meta;
    return requestFor(this._transaction, this, function () {
      var record = findRecord(meta.records, key);
      return record ? cloneKey(record.key) : undefined;
    });
  };

  IDBObjectStore.prototype.getAll = function (range) {
    this._readCheck();
    var meta = this._meta;
    return requestFor(this._transaction, this, function () {
      var values = [];
      var records = sortedRecords(meta);
      for (var index = 0; index < records.length; index++) {
        if (inRange(range, records[index].key)) values.push(clone(records[index].value));
      }
      return values;
    });
  };

  IDBObjectStore.prototype.getAllKeys = function (range) {
    this._readCheck();
    var meta = this._meta;
    return requestFor(this._transaction, this, function () {
      var keys = [];
      var records = sortedRecords(meta);
      for (var index = 0; index < records.length; index++) {
        if (inRange(range, records[index].key)) keys.push(cloneKey(records[index].key));
      }
      return keys;
    });
  };

  IDBObjectStore.prototype.put = function (value, key) {
    return this._write(value, key, false);
  };

  IDBObjectStore.prototype.add = function (value, key) {
    return this._write(value, key, true);
  };

  IDBObjectStore.prototype._write = function (value, key, rejectDuplicates) {
    this._writeCheck();
    var meta = this._meta;
    var finalKey = key;
    if (finalKey === undefined) finalKey = extractKey(meta.keyPath, value);
    if (finalKey === undefined) {
      if (!meta.autoIncrement) {
        throw domError('DataError', 'The object store uses in-line keys and no key was given');
      }
      finalKey = meta.nextKey++;
    } else if (meta.autoIncrement && typeof finalKey === 'number' && finalKey >= meta.nextKey) {
      meta.nextKey = finalKey + 1;
    }
    if (!validKey(finalKey)) throw domError('DataError', 'The key is not a valid key');
    var keyCopy = cloneKey(finalKey);
    var stored = clone(value);
    return requestFor(this._transaction, this, function () {
      var record = findRecord(meta.records, keyCopy);
      if (record && rejectDuplicates) {
        throw domError('ConstraintError', 'A record with that key already exists');
      }
      validateIndexConstraints(meta, { key: keyCopy, value: stored }, record);
      if (record) {
        record.value = stored;
      } else {
        meta.records.push({ key: keyCopy, value: stored });
      }
      return cloneKey(keyCopy);
    });
  };

  IDBObjectStore.prototype.delete = function (key) {
    this._writeCheck();
    if (!validKey(key)) throw domError('DataError', 'The key is not a valid key');
    var meta = this._meta;
    return requestFor(this._transaction, this, function () {
      var record = findRecord(meta.records, key);
      if (record) meta.records.splice(meta.records.indexOf(record), 1);
      return undefined;
    });
  };

  IDBObjectStore.prototype.clear = function () {
    this._writeCheck();
    var meta = this._meta;
    return requestFor(this._transaction, this, function () {
      meta.records.length = 0;
      return undefined;
    });
  };

  IDBObjectStore.prototype.count = function (range) {
    this._readCheck();
    var meta = this._meta;
    return requestFor(this._transaction, this, function () {
      if (range === null || range === undefined) return meta.records.length;
      var count = 0;
      for (var index = 0; index < meta.records.length; index++) {
        if (inRange(range, meta.records[index].key)) count++;
      }
      return count;
    });
  };

  IDBObjectStore.prototype.openCursor = function (range, direction) {
    this._readCheck();
    direction = direction === undefined ? 'next' : String(direction);
    var transaction = this._transaction;
    var request = new IDBRequest();
    request.source = this;
    request.transaction = transaction;
    var cursor = new IDBCursor(this._meta, request, range, direction);
    transaction._pending++;
    schedule(function () { cursor._emit(); });
    return request;
  };

  IDBObjectStore.prototype.openKeyCursor = function (range, direction) {
    var request = this.openCursor(range, direction);
    request.source = this;
    request._keyOnly = true;
    return request;
  };

  IDBObjectStore.prototype.createIndex = function (name, keyPath, options) {
    if (this._transaction) {
      throw domError('InvalidStateError', 'Indexes are created during a version change');
    }
    name = String(name);
    if (Array.isArray(keyPath)) {
      for (var pathIndex = 0; pathIndex < keyPath.length; pathIndex++) {
        if (typeof keyPath[pathIndex] !== 'string') {
          throw domError('SyntaxError', 'The key path is not a valid key path');
        }
      }
    } else if (typeof keyPath !== 'string') {
      throw domError('SyntaxError', 'The key path is not a valid key path');
    }
    if (this._meta.indexes[name] !== undefined) {
      throw domError('ConstraintError', 'An index with that name already exists');
    }
    options = options || {};
    this._meta.indexes[name] = {
      name: name,
      keyPath: keyPath,
      unique: !!options.unique,
      multiEntry: !!options.multiEntry
    };
    this.indexNames = Object.keys(this._meta.indexes);
    return new IDBIndex(this._meta, this._meta.indexes[name], null);
  };

  IDBObjectStore.prototype.deleteIndex = function (name) {
    if (this._transaction) {
      throw domError('InvalidStateError', 'Indexes are deleted during a version change');
    }
    name = String(name);
    if (this._meta.indexes[name] === undefined) {
      throw domError('NotFoundError', 'No index with that name exists');
    }
    delete this._meta.indexes[name];
    this.indexNames = Object.keys(this._meta.indexes);
  };

  IDBObjectStore.prototype.index = function (name) {
    name = String(name);
    if (!this._meta.indexes || this._meta.indexes[name] === undefined) {
      throw domError('NotFoundError', 'No index with that name exists');
    }
    return new IDBIndex(this._meta, this._meta.indexes[name], this._transaction);
  };

  // ------------------------------------------------------------------ index
  function IDBIndex(store, index, transaction) {
    this.name = index.name;
    this.keyPath = index.keyPath;
    this.unique = !!index.unique;
    this.multiEntry = !!index.multiEntry;
    this.objectStore = store;
    this._store = store;
    this._index = index;
    this._transaction = transaction;
  }

  IDBIndex.prototype._readCheck = function () {
    if (this._transaction === null || this._transaction === undefined) {
      throw domError('InvalidStateError', 'The index has no transaction');
    }
    if (this._transaction._state !== 'active') {
      throw domError('TransactionInactiveError', 'The transaction is no longer active');
    }
  };

  IDBIndex.prototype.get = function (key) {
    this._readCheck();
    if (!validKey(key)) throw domError('DataError', 'The key is not a valid key');
    var self = this;
    return requestFor(this._transaction, this, function () {
      var entries = indexEntries(self._store, self._index);
      for (var index = 0; index < entries.length; index++) {
        if (compareKeys(entries[index].key, key) === 0) return clone(entries[index].value);
      }
      return undefined;
    });
  };

  IDBIndex.prototype.getKey = function (key) {
    this._readCheck();
    if (!validKey(key)) throw domError('DataError', 'The key is not a valid key');
    var self = this;
    return requestFor(this._transaction, this, function () {
      var entries = indexEntries(self._store, self._index);
      for (var index = 0; index < entries.length; index++) {
        if (compareKeys(entries[index].key, key) === 0) return cloneKey(entries[index].primaryKey);
      }
      return undefined;
    });
  };

  IDBIndex.prototype.getAll = function (range) {
    this._readCheck();
    var self = this;
    return requestFor(this._transaction, this, function () {
      var values = [];
      var entries = indexEntries(self._store, self._index);
      for (var index = 0; index < entries.length; index++) {
        if (inRange(range, entries[index].key)) values.push(clone(entries[index].value));
      }
      return values;
    });
  };

  IDBIndex.prototype.getAllKeys = function (range) {
    this._readCheck();
    var self = this;
    return requestFor(this._transaction, this, function () {
      var keys = [];
      var entries = indexEntries(self._store, self._index);
      for (var index = 0; index < entries.length; index++) {
        if (inRange(range, entries[index].key)) keys.push(cloneKey(entries[index].primaryKey));
      }
      return keys;
    });
  };

  IDBIndex.prototype.count = function (range) {
    this._readCheck();
    var self = this;
    return requestFor(this._transaction, this, function () {
      var count = 0;
      var entries = indexEntries(self._store, self._index);
      for (var index = 0; index < entries.length; index++) {
        if (inRange(range, entries[index].key)) count++;
      }
      return count;
    });
  };

  IDBIndex.prototype.openCursor = function (range, direction) {
    this._readCheck();
    direction = direction === undefined ? 'next' : String(direction);
    var request = new IDBRequest();
    request.source = this;
    request.transaction = this._transaction;
    var cursor = new IDBCursor(this._store, request, range, direction);
    cursor._indexMeta = this._index;
    this._transaction._pending++;
    schedule(function () { cursor._emit(); });
    return request;
  };

  IDBIndex.prototype.openKeyCursor = function (range, direction) {
    var request = this.openCursor(range, direction);
    request.source = this;
    request._keyOnly = true;
    return request;
  };

  // ------------------------------------------------------------------ cursor
  function IDBCursor(meta, request, range, direction) {
    this.key = undefined;
    this.primaryKey = undefined;
    this.value = undefined;
    this.direction = direction;
    this.source = null;
    this.request = request;
    this._meta = meta;
    this._range = range;
    this._records = null;
    this._indexMeta = null;
    this._index = -1;
    this._emitted = false;
  }

  IDBCursor.prototype._emit = function () {
    var transaction = this.request.transaction;
    if (this._records === null) {
      // Snapshot when the cursor's first request runs, not when openCursor()
      // was called: earlier requests in the same transaction must be visible.
      if (this._indexMeta) {
        this._records = indexEntries(this._meta, this._indexMeta);
      } else {
        this._records = sortedRecords(this._meta);
      }
      if (this.direction === 'prev' || this.direction === 'prevunique') {
        this._records.reverse();
      }
      if (this._range !== null && this._range !== undefined) {
        var range = this._range;
        this._records = this._records.filter(function (record) {
          return inRange(range, record.key);
        });
      }
    }
    this._index++;
    if (transaction._state !== 'active') {
      this.request._finish(false, domError('TransactionInactiveError', 'The transaction is no longer active'));
      return;
    }
    if (this._index >= this._records.length) {
      this.request._finish(true, null);
      transaction._requestFinished(false);
      return;
    }
    var record = this._records[this._index];
    this.key = cloneKey(record.key);
    this.primaryKey = cloneKey(record.primaryKey === undefined ? record.key : record.primaryKey);
    this.value = clone(record.value);
    this._emitted = true;
    this.request._finish(true, this);
    transaction._requestFinished(false);
  };

  IDBCursor.prototype.continue = function (key) {
    if (!this._emitted) {
      throw domError('InvalidStateError', 'The cursor has no current record');
    }
    this._emitted = false;
    var transaction = this.request.transaction;
    if (transaction._state !== 'active') {
      throw domError('TransactionInactiveError', 'The transaction is no longer active');
    }
    this.request.readyState = 'pending';
    transaction._pending++;
    var self = this;
    schedule(function () { self._emit(); });
  };

  IDBCursor.prototype.advance = function (count) {
    var steps = Math.floor(Number(count));
    if (!(steps > 0)) throw new TypeError('The count must be greater than zero');
    this._emitted = false;
    var transaction = this.request.transaction;
    this.request.readyState = 'pending';
    transaction._pending++;
    var self = this;
    schedule(function () {
      self._index += steps - 1;
      self._emit();
    });
  };

  IDBCursor.prototype.delete = function () {
    var meta = this._meta;
    var key = cloneKey(this.primaryKey);
    if (!this._emitted) {
      throw domError('InvalidStateError', 'The cursor has no current record');
    }
    this._emitted = false;
    return requestFor(this.request.transaction, this.source, function () {
      var record = findRecord(meta.records, key);
      if (record) meta.records.splice(meta.records.indexOf(record), 1);
      return undefined;
    });
  };

  // ------------------------------------------------------------------ factory
  function IDBFactory() {}
  IDBFactory.prototype.open = function (name, version) {
    name = String(name);
    var requested = version === undefined ? undefined : Number(version);
    if (requested !== undefined &&
        (!isFinite(requested) || Math.floor(requested) !== requested || requested < 1)) {
      throw new TypeError('The version must be a positive integer');
    }
    var request = new IDBOpenDBRequest();
    schedule(function () {
      loadDatabase(name);
      var meta = databases[name];
      var current = meta === undefined ? 0 : meta.version;
      var target = requested === undefined ? Math.max(current, 1) : requested;
      if (target < current) {
        request._finish(false, domError('VersionError',
          'The requested version (' + target + ') is less than the existing version (' + current + ')'));
        return;
      }
      if (meta === undefined) {
        meta = { name: name, version: 0, stores: Object.create(null) };
        databases[name] = meta;
      }
      if (target > current) {
        var connections = (meta.connections || []).slice();
        for (var connectionIndex = 0; connectionIndex < connections.length; connectionIndex++) {
          fire(connections[connectionIndex], 'versionchange', {
            oldVersion: current,
            newVersion: target
          });
        }
        meta.version = target;
        var database = new IDBDatabase(meta);
        database._upgrading = true;
        request.result = database;
        fire(request, 'upgradeneeded', {
          oldVersion: current,
          newVersion: target
        });
        database._upgrading = false;
        saveDatabase(name);
        schedule(function () { request._finish(true, database); });
      } else {
        request._finish(true, new IDBDatabase(meta));
      }
    });
    return request;
  };

  IDBFactory.prototype.deleteDatabase = function (name) {
    name = String(name);
    var request = new IDBRequest();
    schedule(function () {
      loadDatabase(name);
      var meta = databases[name];
      if (meta) {
        var connections = (meta.connections || []).slice();
        for (var index = 0; index < connections.length; index++) {
          fire(connections[index], 'versionchange', {
            oldVersion: meta.version,
            newVersion: null
          });
        }
      }
      delete databases[name];
      storageRemove(name);
      request._finish(true, undefined);
    });
    return request;
  };

  IDBFactory.prototype.cmp = function (first, second) {
    if (!validKey(first) || !validKey(second)) {
      throw domError('DataError', 'The key is not a valid key');
    }
    return compareKeys(first, second);
  };

  // ------------------------------------------------------------------ globals
  defineProperty(global, 'indexedDB', {
    value: new IDBFactory(),
    writable: true,
    configurable: true,
    enumerable: true
  });
  var interfaces = {
    IDBFactory: IDBFactory,
    IDBDatabase: IDBDatabase,
    IDBObjectStore: IDBObjectStore,
    IDBTransaction: IDBTransaction,
    IDBRequest: IDBRequest,
    IDBOpenDBRequest: IDBOpenDBRequest,
    IDBCursor: IDBCursor,
    IDBKeyRange: IDBKeyRange
  };
  for (var name in interfaces) {
    defineProperty(global, name, {
      value: interfaces[name],
      writable: true,
      configurable: true,
      enumerable: false
    });
  }
})(this);
