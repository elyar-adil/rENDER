//! Web Storage: the `Storage` interface behind `localStorage` and
//! `sessionStorage` (WHATWG HTML §11.2).
//!
//! Both areas are per-realm maps. The engine does not write them to disk: the
//! host owns an origin's `localStorage` between documents, seeds it with
//! [`JsRuntime::seed_local_storage`] before the document runs, and reads changes
//! back with [`JsRuntime::local_storage_entries`]. `sessionStorage` has no host
//! store yet, so it starts empty in each document. Entries live in the storage
//! object's own property table, which is what
//! makes `Object.keys(localStorage)` and `for…in` report the stored keys in
//! insertion order without a second bookkeeping structure. `length` and the
//! indexed slots are answered by the host arms in `eval.rs` because they must
//! reflect the live entry count.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::value::NativeFunction;
use render_dom::Dom;

impl JsRuntime {
    /// The entries of `localStorage` in the order the document stored them, for
    /// a host that keeps each origin's storage area between documents.
    #[must_use]
    pub fn local_storage_entries(&self) -> Vec<(String, String)> {
        let Some(storage) = self.local_storage_object() else {
            return Vec::new();
        };
        self.realm
            .own_property_names(storage)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|key| match self.realm.own_property(storage, &key)?.value {
                JsValue::String(value) => Some((key, value)),
                _ => None,
            })
            .collect()
    }

    /// Replace the contents of `localStorage` before the document runs, so the
    /// host can restore an origin's entries from an earlier session.
    pub fn seed_local_storage(&mut self, entries: &[(String, String)]) {
        let Some(storage) = self.local_storage_object() else {
            return;
        };
        for key in self.realm.own_property_names(storage).unwrap_or_default() {
            self.realm.delete_property(storage, &key);
        }
        for (key, value) in entries {
            self.realm
                .set_property(storage, key.clone(), JsValue::String(value.clone()));
        }
    }

    fn local_storage_object(&self) -> Option<ObjectId> {
        match self.realm.global("localStorage")? {
            JsValue::Object(storage) => Some(storage),
            _ => None,
        }
    }

    pub(in crate::runtime) fn dispatch_storage_native(
        &mut self,
        _dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::StorageGetItem => {
                let key = required_argument(arguments, 0, "getItem")?.to_js_string();
                // `getItem` answers `null` for a missing key.
                Ok(self
                    .realm
                    .own_property(receiver, &key)
                    .map_or(JsValue::Null, |descriptor| descriptor.value))
            }
            NativeFunction::StorageSetItem => {
                let key = required_argument(arguments, 0, "setItem")?.to_js_string();
                let value = required_argument(arguments, 1, "setItem")?.to_js_string();
                if !self
                    .realm
                    .set_property(receiver, key, JsValue::String(value))
                {
                    return Err(JsError::type_error("could not store the value"));
                }
                Ok(JsValue::Undefined)
            }
            NativeFunction::StorageRemoveItem => {
                let key = required_argument(arguments, 0, "removeItem")?.to_js_string();
                self.realm.delete_property(receiver, &key);
                Ok(JsValue::Undefined)
            }
            NativeFunction::StorageClear => {
                let keys = self.realm.own_property_names(receiver).unwrap_or_default();
                for key in keys {
                    self.realm.delete_property(receiver, &key);
                }
                Ok(JsValue::Undefined)
            }
            NativeFunction::StorageKey => {
                let index = match arguments.first() {
                    Some(JsValue::Number(number)) if number.is_finite() && *number >= 0.0 => {
                        *number
                    }
                    _ => {
                        return Err(JsError::type_error("Storage.key requires a numeric index"));
                    }
                };
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "the index is validated as a non-negative finite number"
                )]
                let index = index as usize;
                let key = self
                    .realm
                    .own_property_names(receiver)
                    .and_then(|keys| keys.get(index).cloned())
                    .unwrap_or_default();
                Ok(JsValue::String(key))
            }
            other => Err(JsError::type_error(format!(
                "unsupported storage native {other:?}"
            ))),
        }
    }
}
