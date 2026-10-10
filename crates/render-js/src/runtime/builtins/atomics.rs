//! The `Atomics` namespace object (ECMA-262 25.4).
//!
//! This engine has one agent and runs one operation at a time, so every operation
//! on a shared store is atomic by construction. The waiting operations follow the
//! single-agent reading of the spec: no other agent can notify a blocked `wait`,
//! so it answers only `"not-equal"` or `"timed-out"`, and `notify` wakes only the
//! `waitAsync` waiters of this agent. A `waitAsync` waiter times out through the
//! runtime's timer queue and settles through a promise job, as every promise does.

use std::time::Duration;

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::types::{
    AtomicsWaiter, JsMicrotask, PromiseCapability, TimerKind, TimerRequest,
};
use crate::value::{AtomicsOp, NativeFunction, RmwOp, TypedArrayKind, TypedBuffer};
use render_dom::Dom;

/// Whether this agent can suspend in `Atomics.wait` (ECMA-262 25.4.3.1
/// `AgentCanSuspend`). The engine's one agent can, so a finite blocking wait sleeps
/// for its timeout and an infinite one is a `TypeError`, since nothing could ever
/// notify it. A host whose main thread cannot block, such as a browser's, answers
/// `false`, which makes every blocking `wait` a `TypeError` once its arguments are
/// checked.
const AGENT_CAN_SUSPEND: bool = true;

/// The longest delay a `waitAsync` timeout timer carries. A longer timeout fires at
/// this bound, which is the largest delay the timer queue's `WebIDL` `long` allows.
const MAX_WAIT_TIMER_MS: f64 = 2_147_483_647.0;

/// `2^64`, the modulus of an element's bits: every operand is taken modulo it.
const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;

/// A typed array that passed `ValidateIntegerTypedArray` (ECMA-262 25.4.3.1): the
/// `taRecord` an operation works on, with the element count it had when it was
/// validated.
struct Target {
    receiver: ObjectId,
    kind: TypedArrayKind,
    buffer: TypedBuffer,
    /// The element offset of the view in its buffer.
    start: usize,
    /// The element count when the array was validated.
    length: usize,
}

impl Target {
    /// The byte offset in the buffer of element `index` of the view.
    fn byte_index(&self, index: usize) -> usize {
        (self.start + index) * self.kind.element_size()
    }
}

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_atomics_native(
        &mut self,
        dom: &mut Dom,
        operation: AtomicsOp,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match operation {
            AtomicsOp::ReadModifyWrite(op) => self.atomics_read_modify_write(dom, op, arguments),
            AtomicsOp::CompareExchange => self.atomics_compare_exchange(dom, arguments),
            AtomicsOp::IsLockFree => self.atomics_is_lock_free(dom, arguments),
            AtomicsOp::Load => self.atomics_load(dom, arguments),
            AtomicsOp::Store => self.atomics_store(dom, arguments),
            AtomicsOp::Notify => self.atomics_notify(dom, arguments),
            AtomicsOp::Wait => self.atomics_wait(dom, arguments, false),
            AtomicsOp::WaitAsync => self.atomics_wait(dom, arguments, true),
            AtomicsOp::WaitAsyncTimeout => {
                self.atomics_wait_async_timeout(arguments);
                Ok(JsValue::Undefined)
            }
        }
    }

    /// `ValidateIntegerTypedArray(typedArray, waitable)` (ECMA-262 25.4.3.1): the
    /// argument must be a typed array in bounds of a live buffer, of an integer
    /// element type, or of `Int32` or `BigInt64` when `waitable`.
    fn atomics_target(&self, value: Option<&JsValue>, waitable: bool) -> Result<Target, JsError> {
        let Some(JsValue::Object(receiver)) = value else {
            return Err(JsError::type_error(
                "Atomics requires an integer typed array",
            ));
        };
        let (kind, buffer, start, length) = self.typed_array_host(*receiver)?;
        let accepted = if waitable {
            matches!(kind, TypedArrayKind::Int32 | TypedArrayKind::BigInt64)
        } else {
            !matches!(
                kind,
                TypedArrayKind::Uint8Clamped | TypedArrayKind::Float32 | TypedArrayKind::Float64
            )
        };
        if !accepted {
            return Err(JsError::type_error(
                "Atomics does not operate on this typed array's element type",
            ));
        }
        Ok(Target {
            receiver: *receiver,
            kind,
            buffer,
            start,
            length,
        })
    }

    /// `ValidateAtomicAccess(taRecord, requestIndex)` (ECMA-262 25.4.3.2): the
    /// element index after `ToIndex`. The length is the one `target` was validated
    /// with, read before the index is coerced, so a coercion that shrinks the
    /// buffer is caught afterwards by [`Self::atomics_revalidate`].
    fn atomics_index(
        &mut self,
        dom: &mut Dom,
        target: &Target,
        value: Option<&JsValue>,
    ) -> Result<usize, JsError> {
        let index = match value {
            Some(value) => self.typed_index(dom, value)?,
            None => 0,
        };
        if index >= target.length {
            return Err(self.range_error("Atomics index is out of range of the typed array"));
        }
        Ok(index)
    }

    /// `RevalidateAtomicAccess` (ECMA-262 25.4.3.3): after a coercion that may run
    /// user code, the array must still be in bounds (else a `TypeError`), and the
    /// element must still exist (else a `RangeError`).
    fn atomics_revalidate(&mut self, target: &Target, index: usize) -> Result<(), JsError> {
        let (_, _, _, current) = self.typed_array_parts(target.receiver)?;
        let Some(current) = current else {
            return Err(JsError::type_error(
                "Atomics operation on a typed array that is out of bounds of its ArrayBuffer",
            ));
        };
        if index >= current {
            return Err(self.range_error("Atomics index is out of range of the typed array"));
        }
        Ok(())
    }

    /// The operand of a write, converted for the element type: `ToBigInt` for the
    /// `BigInt` kinds and `ToIntegerOrInfinity` for the rest (ECMA-262 25.4.3.17
    /// `AtomicReadModifyWrite`, 25.4.5 `compareExchange` and 25.4.12 `store`).
    fn atomics_operand(
        &mut self,
        dom: &mut Dom,
        kind: TypedArrayKind,
        value: Option<&JsValue>,
    ) -> Result<JsValue, JsError> {
        let value = value.cloned().unwrap_or(JsValue::Undefined);
        if kind.is_bigint() {
            Ok(JsValue::BigInt(self.to_bigint_value(dom, &value)?))
        } else {
            Ok(JsValue::Number(self.to_integer_value(dom, &value)?))
        }
    }

    /// `Atomics.add`, `and`, `exchange`, `or`, `sub` and `xor` (ECMA-262 25.4.3.17
    /// `AtomicReadModifyWrite`). The result is the element as it was read.
    fn atomics_read_modify_write(
        &mut self,
        dom: &mut Dom,
        op: RmwOp,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.atomics_target(arguments.first(), false)?;
        let index = self.atomics_index(dom, &target, arguments.get(1))?;
        let operand = self.atomics_operand(dom, target.kind, arguments.get(2))?;
        self.atomics_revalidate(&target, index)?;
        let size = target.kind.element_size();
        let byte_index = target.byte_index(index);
        let old = read_bits(&target.buffer, byte_index, size)?;
        let new = combine(op, old, operand_bits(&operand));
        write_bits(&target.buffer, byte_index, size, new);
        Ok(element_value(target.kind, old))
    }

    /// `Atomics.compareExchange` (ECMA-262 25.4.5): the expected value is compared
    /// with the stored bytes, after converting to the element type, so `-1` matches
    /// a stored `255` in a `Uint8Array`. The result is the element as it was read.
    fn atomics_compare_exchange(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.atomics_target(arguments.first(), false)?;
        let index = self.atomics_index(dom, &target, arguments.get(1))?;
        let expected = self.atomics_operand(dom, target.kind, arguments.get(2))?;
        let replacement = self.atomics_operand(dom, target.kind, arguments.get(3))?;
        self.atomics_revalidate(&target, index)?;
        let size = target.kind.element_size();
        let byte_index = target.byte_index(index);
        let old = read_bits(&target.buffer, byte_index, size)?;
        if old == operand_bits(&expected) & element_mask(size) {
            write_bits(&target.buffer, byte_index, size, operand_bits(&replacement));
        }
        Ok(element_value(target.kind, old))
    }

    /// `Atomics.isLockFree(size)` (ECMA-262 25.4.8). Every access here is a
    /// single-threaded element access, so each size the spec names as lock-free is
    /// lock-free: 1, 2, 4 and 8.
    fn atomics_is_lock_free(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let size = match arguments.first() {
            Some(value) => self.to_integer_value(dom, value)?,
            None => 0.0,
        };
        Ok(JsValue::Boolean([1.0, 2.0, 4.0, 8.0].contains(&size)))
    }

    /// `Atomics.load(typedArray, index)` (ECMA-262 25.4.6).
    fn atomics_load(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let target = self.atomics_target(arguments.first(), false)?;
        let index = self.atomics_index(dom, &target, arguments.get(1))?;
        self.atomics_revalidate(&target, index)?;
        let size = target.kind.element_size();
        let bits = read_bits(&target.buffer, target.byte_index(index), size)?;
        Ok(element_value(target.kind, bits))
    }

    /// `Atomics.store(typedArray, index, value)` (ECMA-262 25.4.12). The result is
    /// the converted operand, not the wrapped element value.
    fn atomics_store(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let target = self.atomics_target(arguments.first(), false)?;
        let index = self.atomics_index(dom, &target, arguments.get(1))?;
        let operand = self.atomics_operand(dom, target.kind, arguments.get(2))?;
        self.atomics_revalidate(&target, index)?;
        let size = target.kind.element_size();
        write_bits(
            &target.buffer,
            target.byte_index(index),
            size,
            operand_bits(&operand),
        );
        Ok(match operand {
            // Adding `+0` turns `-0` into `+0`, as the spec's `ToIntegerOrInfinity`
            // result does (25.4.12 step 3).
            JsValue::Number(number) => JsValue::Number(number + 0.0),
            other => other,
        })
    }

    /// `Atomics.notify(typedArray, index[, count])` (ECMA-262 25.4.15). A
    /// non-shared buffer has no waiters, so it answers 0 once the arguments are
    /// valid.
    #[allow(
        clippy::cast_precision_loss,
        reason = "waiter counts stay far below 2^53"
    )]
    fn atomics_notify(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let target = self.atomics_target(arguments.first(), true)?;
        let index = self.atomics_index(dom, &target, arguments.get(1))?;
        // The count is coerced before the buffer is checked (25.4.15 steps 3 to 6).
        let count = match arguments.get(2) {
            None | Some(JsValue::Undefined) => f64::INFINITY,
            Some(value) => self.to_integer_value(dom, value)?.max(0.0),
        };
        if !target.buffer.is_shared() {
            return Ok(JsValue::Number(0.0));
        }
        let woken = self.wake_atomics_waiters(&target.buffer, target.byte_index(index), count);
        Ok(JsValue::Number(woken as f64))
    }

    /// `Atomics.wait` and `Atomics.waitAsync` (ECMA-262 25.4.3.14 `DoWait`), with
    /// the checks and conversions in the spec's order. A blocking wait answers a
    /// string; an asynchronous one answers a result object.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a finite timeout is non-negative, and the sleep saturates at u64::MAX milliseconds"
    )]
    fn atomics_wait(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        asynchronous: bool,
    ) -> Result<JsValue, JsError> {
        let target = self.atomics_target(arguments.first(), true)?;
        if !target.buffer.is_shared() {
            return Err(JsError::type_error(
                "Atomics.wait and Atomics.waitAsync require a SharedArrayBuffer",
            ));
        }
        let index = self.atomics_index(dom, &target, arguments.get(1))?;
        let expected = self.atomics_operand(dom, target.kind, arguments.get(2))?;
        let timeout = match arguments.get(3) {
            Some(value) => self.to_number_value(dom, value)?,
            None => f64::NAN,
        };
        let timeout = wait_timeout(timeout);
        if !asynchronous && !AGENT_CAN_SUSPEND {
            return Err(JsError::type_error(
                "Atomics.wait cannot suspend this agent",
            ));
        }
        let size = target.kind.element_size();
        let byte_index = target.byte_index(index);
        let expected = operand_bits(&expected) & element_mask(size);
        if read_bits(&target.buffer, byte_index, size)? != expected {
            return self.atomics_wait_result(asynchronous, "not-equal");
        }
        if timeout <= 0.0 {
            return self.atomics_wait_result(asynchronous, "timed-out");
        }
        if asynchronous {
            return self.add_atomics_waiter(target.buffer, byte_index, timeout);
        }
        if timeout.is_infinite() {
            return Err(JsError::type_error(
                "Atomics.wait would block forever: no other agent can notify it",
            ));
        }
        std::thread::sleep(Duration::from_millis(timeout as u64));
        Ok(JsValue::String("timed-out".to_owned()))
    }

    /// The answer of a wait that does not suspend: the string for `wait`, or a
    /// result object whose `async` is false for `waitAsync`.
    fn atomics_wait_result(
        &mut self,
        asynchronous: bool,
        outcome: &str,
    ) -> Result<JsValue, JsError> {
        if !asynchronous {
            return Ok(JsValue::String(outcome.to_owned()));
        }
        self.ensure_heap_capacity(1)?;
        let result = self.atomics_result_object(false, JsValue::String(outcome.to_owned()));
        Ok(JsValue::Object(result))
    }

    /// The `{ async, value }` result object of `waitAsync` (ECMA-262 25.4.3.14).
    /// The caller has reserved the heap space for it.
    fn atomics_result_object(&mut self, is_async: bool, value: JsValue) -> ObjectId {
        let result = self.realm.create_ordinary_object();
        self.realm
            .set_property(result, "async".to_owned(), JsValue::Boolean(is_async));
        self.realm.set_property(result, "value".to_owned(), value);
        result
    }

    /// `AddWaiter` for an asynchronous wait that must suspend (ECMA-262 25.4.3.14):
    /// the promise the result settles, and a timeout timer when the wait is finite.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "waiter and timer ids are small positive integers, exact as f64"
    )]
    fn add_atomics_waiter(
        &mut self,
        buffer: TypedBuffer,
        byte_index: usize,
        timeout: f64,
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(3)?;
        let (promise, promise_object) = self.create_promise_record()?;
        let id = self.next_atomics_waiter_id;
        self.next_atomics_waiter_id += 1;
        let timer = if timeout.is_finite() {
            let callback = self
                .realm
                .native_object(NativeFunction::Atomics(AtomicsOp::WaitAsyncTimeout));
            let delay = timeout.min(MAX_WAIT_TIMER_MS);
            let arguments = vec![JsValue::Number(id as f64)];
            let timer_id =
                self.register_timer_entry(callback, delay, TimerKind::Timeout, arguments);
            Some(timer_id as u64)
        } else {
            None
        };
        self.atomics_waiters.push(AtomicsWaiter {
            id,
            buffer,
            byte_index,
            promise,
            promise_object,
            timer,
        });
        let result = self.atomics_result_object(true, JsValue::Object(promise_object));
        Ok(JsValue::Object(result))
    }

    /// The timeout job of an `Atomics.waitAsync` waiter (ECMA-262 25.4.3.14). A
    /// waiter that is still waiting times out; one notified first has already left
    /// the list, so the job does nothing.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the id was written from a waiter id, a positive integer"
    )]
    fn atomics_wait_async_timeout(&mut self, arguments: &[JsValue]) {
        let Some(JsValue::Number(id)) = arguments.first() else {
            return;
        };
        let id = *id as u64;
        if let Some(position) = self
            .atomics_waiters
            .iter()
            .position(|waiter| waiter.id == id)
        {
            let waiter = self.atomics_waiters.remove(position);
            self.finish_atomics_waiter(&waiter, "timed-out");
        }
    }

    /// Wake up to `count` waiters of a location, first come first served, and return
    /// how many woke (ECMA-262 25.4.3.15 `RemoveWaiters` and `NotifyWaiter`).
    #[allow(
        clippy::cast_precision_loss,
        reason = "waiter counts stay far below 2^53"
    )]
    fn wake_atomics_waiters(
        &mut self,
        buffer: &TypedBuffer,
        byte_index: usize,
        count: f64,
    ) -> usize {
        let mut woken = 0_usize;
        let mut position = 0;
        while position < self.atomics_waiters.len() && (woken as f64) < count {
            let matches = self.atomics_waiters[position].byte_index == byte_index
                && self.atomics_waiters[position].buffer == *buffer;
            if matches {
                let waiter = self.atomics_waiters.remove(position);
                self.finish_atomics_waiter(&waiter, "ok");
                woken += 1;
            } else {
                position += 1;
            }
        }
        woken
    }

    /// Settle a waiter's promise with `outcome` through a promise job, and cancel its
    /// timeout if that timer is still scheduled (ECMA-262 25.4.3.14
    /// `EnqueueResolveInAgentJob`).
    fn finish_atomics_waiter(&mut self, waiter: &AtomicsWaiter, outcome: &str) {
        if let Some(timer) = waiter.timer
            && self.timers.remove(&timer).is_some()
        {
            self.pending_timer_requests
                .push(TimerRequest::Cancel { id: timer });
        }
        // A reaction with no handler passes its argument to the capability, which
        // is the resolution the spec's job performs.
        self.pending_microtasks.push(JsMicrotask::PromiseReaction {
            handler: None,
            argument: JsValue::String(outcome.to_owned()),
            fulfilled: true,
            capability: Some(PromiseCapability::Record {
                promise: waiter.promise,
                object: waiter.promise_object,
            }),
        });
    }
}

/// The timeout `t` of a wait (ECMA-262 25.4.3.14 step 9): NaN waits forever, a
/// negative timeout does not wait, and anything else is its own value.
fn wait_timeout(number: f64) -> f64 {
    if number.is_nan() {
        f64::INFINITY
    } else {
        number.max(0.0)
    }
}

/// The bits an operand takes in an element of any integer kind: its value modulo
/// 2^64, whose low bytes are the element's (ECMA-262 7.1.10 through 7.1.16).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the remainder of a finite integer modulo 2^64 is a non-negative integer below 2^64"
)]
fn operand_bits(operand: &JsValue) -> u64 {
    match operand {
        JsValue::BigInt(value) => {
            let mut bytes = [0_u8; 8];
            TypedArrayKind::store_bigint(value, &mut bytes);
            u64::from_le_bytes(bytes)
        }
        // `%` is an exact fmod, so the remainder is exact and below 2^64 in
        // magnitude, and so converts to `u64` without loss. A negative remainder is
        // negated in integer arithmetic: `2^64 - magnitude` is not a double in
        // general, so adding the modulus in floating point would round it.
        JsValue::Number(number) if number.is_finite() => {
            let remainder = number.trunc() % TWO_POW_64;
            let magnitude = remainder.abs() as u64;
            if remainder < 0.0 {
                magnitude.wrapping_neg()
            } else {
                magnitude
            }
        }
        _ => 0,
    }
}

/// The mask of the low `size` bytes of a 64-bit value.
fn element_mask(size: usize) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1_u64 << (8 * size)) - 1
    }
}

/// The bits a read-modify-write stores, from the element's previous bits and the
/// operand's (ECMA-262 25.4.3.17). Only the element's low bytes are written, so a
/// carry past them is dropped.
fn combine(op: RmwOp, old: u64, operand: u64) -> u64 {
    match op {
        RmwOp::Add => old.wrapping_add(operand),
        RmwOp::And => old & operand,
        RmwOp::Exchange => operand,
        RmwOp::Or => old | operand,
        RmwOp::Sub => old.wrapping_sub(operand),
        RmwOp::Xor => old ^ operand,
    }
}

/// The `size` bytes at `byte_index` of the buffer, as the little-endian integer
/// they spell. The caller has validated the range.
fn read_bits(buffer: &TypedBuffer, byte_index: usize, size: usize) -> Result<u64, JsError> {
    let bytes = buffer.read_bytes(byte_index, size)?;
    let mut padded = [0_u8; 8];
    padded[..size].copy_from_slice(&bytes);
    Ok(u64::from_le_bytes(padded))
}

/// Store the low `size` bytes of `bits` at `byte_index` of the buffer.
fn write_bits(buffer: &TypedBuffer, byte_index: usize, size: usize, bits: u64) {
    buffer.write_bytes(byte_index, &bits.to_le_bytes()[..size]);
}

/// The value an element with these bits reads as (ECMA-262 25.1.3.13
/// `RawBytesToNumeric`): a Number, or a `BigInt` for the `BigInt` kinds.
fn element_value(kind: TypedArrayKind, bits: u64) -> JsValue {
    let bytes = bits.to_le_bytes();
    if kind.is_bigint() {
        JsValue::BigInt(kind.load_bigint(&bytes))
    } else {
        JsValue::Number(kind.load(&bytes))
    }
}
