//! `Function.prototype` helpers that are not plain method bodies (ECMA-262 20.2.3).

use crate::JsError;
use crate::JsValue;
use crate::runtime::JsRuntime;
use crate::value::ObjectHost;
use render_dom::Dom;

impl JsRuntime {
    /// `OrdinaryHasInstance(C, O)` (ECMA-262 7.3.21). A bound function defers to
    /// its target through `InstanceofOperator`, and a non-object `O` or a
    /// non-callable `C` answers `false`.
    pub(in crate::runtime) fn ordinary_has_instance(
        &mut self,
        dom: &mut Dom,
        constructor: &JsValue,
        value: &JsValue,
    ) -> Result<bool, JsError> {
        let JsValue::Object(constructor) = constructor else {
            return Ok(false);
        };
        if !Self::is_callable_object(*constructor, &self.realm) {
            return Ok(false);
        }
        if let Some(ObjectHost::BoundCallable { target, .. }) = self.realm.host(*constructor) {
            return self.instanceof(dom, value, &JsValue::Object(target));
        }
        let JsValue::Object(object) = value else {
            return Ok(false);
        };
        // A primitive `prototype` answers `false`, the lenient reading that
        // transpiled feature probes rely on (a callable shim cannot match).
        let JsValue::Object(prototype) = self.get_member(dom, *constructor, "prototype")? else {
            return Ok(false);
        };
        let mut candidate = self.prototype_of(dom, *object)?;
        let mut visited = 0_usize;
        while let Some(current) = candidate {
            if current == prototype {
                return Ok(true);
            }
            if visited >= self.limits.max_heap_objects {
                return Err(JsError::resource(
                    "prototype chain exceeds the heap object limit",
                ));
            }
            visited = visited.saturating_add(1);
            candidate = self.prototype_of(dom, current)?;
        }
        Ok(false)
    }
}
