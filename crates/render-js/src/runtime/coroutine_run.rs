//! Running compiled coroutines: generators and async functions.
//!
//! [`super::coroutine`] turns a function body into instructions; this module
//! executes them with an explicit program counter, a stack of enclosing loops
//! and a stack of `try` handlers, so a body can stop at a `yield` or `await`
//! and be continued from the same place later.
//!
//! Everything a collector needs to see lives in ordinary environment bindings
//! with names a script cannot write (`%t…` temporaries, `%i…` iterators,
//! `%p…` pending `finally` completions). A suspended coroutine therefore
//! keeps its values alive simply by keeping its scope chain alive.

use super::JsRuntime;
use super::coroutine::{
    CoroutineCode, Instr, IteratorClose, LoopBinding, LoopInfo, SuspendKind, awaited_result_name,
    next_result_name,
};
use super::eval::Completion;
use super::types::{Binding, CallFrame, ClassFrame, Environment, EnvironmentRecord, UserFunction};
use crate::parser::{FunctionKind, VariableKind};
use crate::value::ObjectHost;
use crate::{JsError, JsErrorKind, JsValue, ObjectId};
use render_dom::Dom;
use std::cell::RefCell;
use std::rc::Rc;

/// How a coroutine is continued.
#[derive(Clone, Debug)]
pub(super) enum Resume {
    Next(JsValue),
    Throw(JsValue),
    Return(JsValue),
}

/// Why a run stopped.
#[derive(Clone, Debug)]
pub(super) enum CoStep {
    Yield(JsValue),
    Await(JsValue),
    Complete(JsValue),
}

enum Flow {
    Next,
    Suspend(CoStep),
    Done(JsValue),
}

/// An abrupt completion travelling outwards from the current position.
#[derive(Clone, Debug)]
enum Abrupt {
    /// `return value`: runs every enclosing `finally`.
    Return(JsValue),
    /// `break`/`continue` to the loop `label` names, or the innermost one.
    Jump {
        label: Option<Rc<str>>,
        is_continue: bool,
    },
    /// A `break`/`continue` whose loop has been left: resume at `pc`.
    Goto(usize),
    /// A throw: runs `catch` and `finally` handlers and closes loops.
    Throw(JsError),
}

/// What a coroutine does when the `await` it is suspended on settles.
#[derive(Clone, Debug)]
enum AfterAwait {
    /// A `for await` loop closing its iterator. The awaited `return()` result
    /// must be an object unless the completion being carried is a throw, and
    /// the completion then continues outwards.
    CloseIterator(Abrupt),
    /// An async generator resumed at `yield` with `return v`: the awaited value
    /// becomes the return value (ECMA-262 27.6.3.8, AsyncGeneratorUnwrapYieldResumption).
    YieldReturn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoState {
    /// Created, body not started.
    Start,
    /// Stopped at the `Suspend` instruction at `pc`.
    Suspended,
    Done,
}

#[derive(Clone, Debug)]
struct Handler {
    catch_pc: Option<usize>,
    finally_pc: Option<usize>,
    slot: usize,
    scope_depth: usize,
    loop_depth: usize,
}

#[derive(Debug)]
struct LoopEntry {
    info: Rc<LoopInfo>,
    scope_depth: usize,
    handler_depth: usize,
    /// Cleared while the loop fetches its next value: an error there does not
    /// close the iterator (ECMA-262 14.7.5.7 ForIn/OfBodyEvaluation).
    closable: bool,
}

/// Set whether the innermost loop closes its iterator if left abruptly.
fn set_loop_closable(co: &mut Coroutine, closable: bool) {
    if let Some(entry) = co.loops.last_mut() {
        entry.closable = closable;
    }
}

/// A generator or async function activation.
#[derive(Debug)]
pub(super) struct Coroutine {
    code: Rc<CoroutineCode>,
    pc: usize,
    state: CoState,
    /// The scope chain while suspended; empty while running (the live chain
    /// is `JsRuntime::environment`) and once done.
    pub(super) environment: Vec<Environment>,
    call_environment: Environment,
    class_frame: ClassFrame,
    handlers: Vec<Handler>,
    loops: Vec<LoopEntry>,
    /// The promise an async function resolves with its result.
    promise: Option<usize>,
    label: String,
    /// Set while suspended on an `await` whose settlement needs more than
    /// resuming the body (see [`AfterAwait`]).
    after_await: Option<AfterAwait>,
}

/// What a `finally` block must do when it ends.
enum Pending {
    Normal,
    Throw(JsValue),
    Return(JsValue),
    Jump {
        is_continue: bool,
        label: Option<Rc<str>>,
    },
}

const PENDING_NORMAL: f64 = 0.0;
const PENDING_THROW: f64 = 1.0;
const PENDING_RETURN: f64 = 2.0;
const PENDING_BREAK: f64 = 3.0;
const PENDING_CONTINUE: f64 = 4.0;

fn hidden_binding(value: JsValue) -> Binding {
    Binding {
        value,
        mutable: true,
        initialized: true,
        kind: VariableKind::Var,
    }
}

impl Coroutine {
    fn set_hidden(&self, name: &str, value: JsValue) {
        self.call_environment
            .borrow_mut()
            .bindings
            .insert(name.to_owned(), hidden_binding(value));
    }

    fn hidden(&self, name: &str) -> JsValue {
        self.call_environment
            .borrow()
            .bindings
            .get(name)
            .map_or(JsValue::Undefined, |binding| binding.value.clone())
    }

    fn set_pending(&self, slot: usize, pending: Pending) {
        let (kind, value, label) = match pending {
            Pending::Normal => (PENDING_NORMAL, JsValue::Undefined, JsValue::Undefined),
            Pending::Throw(value) => (PENDING_THROW, value, JsValue::Undefined),
            Pending::Return(value) => (PENDING_RETURN, value, JsValue::Undefined),
            Pending::Jump { is_continue, label } => (
                if is_continue {
                    PENDING_CONTINUE
                } else {
                    PENDING_BREAK
                },
                JsValue::Undefined,
                label.map_or(JsValue::Undefined, |label| {
                    JsValue::String(label.to_string())
                }),
            ),
        };
        self.set_hidden(&format!("%p{slot}"), JsValue::Number(kind));
        self.set_hidden(&format!("%v{slot}"), value);
        self.set_hidden(&format!("%l{slot}"), label);
    }

    fn pending(&self, slot: usize) -> Pending {
        let kind = match self.hidden(&format!("%p{slot}")) {
            JsValue::Number(kind) => kind,
            _ => PENDING_NORMAL,
        };
        let value = self.hidden(&format!("%v{slot}"));
        if kind == PENDING_THROW {
            Pending::Throw(value)
        } else if kind == PENDING_RETURN {
            Pending::Return(value)
        } else if kind == PENDING_BREAK || kind == PENDING_CONTINUE {
            let label = match self.hidden(&format!("%l{slot}")) {
                JsValue::String(label) => Some(Rc::from(label.as_str())),
                _ => None,
            };
            Pending::Jump {
                is_continue: kind == PENDING_CONTINUE,
                label,
            }
        } else {
            Pending::Normal
        }
    }
}

impl JsRuntime {
    /// Called from `call_user` with the callee's scope chain installed: turn
    /// the activation into a generator object or run an async function to its
    /// first `await`.
    pub(super) fn start_coroutine(
        &mut self,
        dom: &mut Dom,
        function_index: usize,
        function: &UserFunction,
        call_environment: &Environment,
    ) -> Result<JsValue, JsError> {
        let is_async_generator = function.kind == FunctionKind::AsyncGenerator;
        let code = if let Some(code) = self.coroutine_code.get(&function_index) {
            code.clone()
        } else {
            let code = Rc::new(super::coroutine::Compiler::compile(
                &function.body,
                is_async_generator,
            )?);
            self.coroutine_code.insert(function_index, code.clone());
            code
        };
        let label = function
            .name
            .clone()
            .unwrap_or_else(|| format!("<anonymous fn #{function_index}>"));
        let id = self.coroutines.len();
        self.coroutines.push(Some(Coroutine {
            code,
            pc: 0,
            state: CoState::Start,
            environment: self.environment.clone(),
            call_environment: call_environment.clone(),
            class_frame: self.class_frames.last().cloned().unwrap_or_default(),
            handlers: Vec::new(),
            loops: Vec::new(),
            promise: None,
            label,
            after_await: None,
        }));
        if function.kind == FunctionKind::Generator {
            self.ensure_heap_capacity(1)?;
            return Ok(JsValue::Object(self.realm.generator_object(id)));
        }
        if is_async_generator {
            self.ensure_heap_capacity(1)?;
            self.start_async_generator(id);
            return Ok(JsValue::Object(self.realm.async_generator_object(id)));
        }
        let (promise, result) = self.create_promise()?;
        if let Some(Some(coroutine)) = self.coroutines.get_mut(id) {
            coroutine.promise = Some(promise);
        }
        self.async_continue(dom, id, Resume::Next(JsValue::Undefined));
        Ok(result)
    }

    // -------------------------------------------------------------- generators

    /// `%GeneratorPrototype%.next/return/throw`.
    pub(super) fn generator_resume(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        resume: Resume,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Generator(id)) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible Generator receiver"));
        };
        let state = match self.coroutines.get(id) {
            Some(Some(coroutine)) => coroutine.state,
            Some(None) => return Err(JsError::type_error("Generator is already running")),
            None => return Err(JsError::type_error("Generator refers to unknown state")),
        };
        match (state, resume) {
            (CoState::Done, Resume::Next(_)) => self.iteration_result(JsValue::Undefined, true),
            (CoState::Done, Resume::Throw(value)) => Err(JsError::thrown(value)),
            (CoState::Done, Resume::Return(value)) => self.iteration_result(value, true),
            (CoState::Start, Resume::Return(value)) => {
                self.finish_coroutine(id);
                self.iteration_result(value, true)
            }
            (CoState::Start, Resume::Throw(value)) => {
                self.finish_coroutine(id);
                Err(JsError::thrown(value))
            }
            (_, resume) => match self.drive(dom, id, resume)? {
                CoStep::Yield(value) => self.iteration_result(value, false),
                CoStep::Complete(value) => self.iteration_result(value, true),
                CoStep::Await(_) => Err(JsError::type_error("await in a synchronous generator")),
            },
        }
    }

    pub(super) fn finish_coroutine(&mut self, id: usize) {
        if let Some(Some(coroutine)) = self.coroutines.get_mut(id) {
            coroutine.state = CoState::Done;
            coroutine.environment.clear();
            coroutine.loops.clear();
            coroutine.handlers.clear();
        }
    }

    pub(super) fn iteration_result(
        &mut self,
        value: JsValue,
        done: bool,
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        self.realm.set_property(result, "value".to_owned(), value);
        self.realm
            .set_property(result, "done".to_owned(), JsValue::Boolean(done));
        Ok(JsValue::Object(result))
    }

    // ------------------------------------------------------------------- async

    /// Run an async function until its next `await` (or its end) and arrange
    /// for the awaited promise to continue it.
    pub(super) fn async_continue(&mut self, dom: &mut Dom, id: usize, resume: Resume) {
        if self.async_generator_state(id).is_some() {
            self.async_generator_continue(dom, id, resume);
            return;
        }
        let promise = self
            .coroutines
            .get(id)
            .and_then(Option::as_ref)
            .and_then(|coroutine| coroutine.promise);
        let outcome = match self.drive(dom, id, resume) {
            Ok(CoStep::Await(value)) => self.await_value(dom, id, &value).map(|()| None),
            Ok(CoStep::Complete(value)) => Ok(Some(value)),
            Ok(CoStep::Yield(_)) => Err(JsError::type_error("yield in an async function")),
            Err(error) => Err(error),
        };
        let Some(promise) = promise else {
            return;
        };
        match outcome {
            Ok(None) => {}
            Ok(Some(value)) => {
                if let Err(error) = self.resolve_promise_value(promise, &value) {
                    self.reject_with_error(promise, &error);
                }
            }
            Err(error) => self.reject_with_error(promise, &error),
        }
    }

    /// The value a script sees for `error`: the thrown value, or an Error object.
    pub(super) fn error_value(&mut self, error: &JsError) -> JsValue {
        self.error_to_thrown_value(error)
            .unwrap_or_else(|_| JsValue::String(error.message().to_owned()))
    }

    fn reject_with_error(&mut self, promise: usize, error: &JsError) {
        let reason = self.error_value(error);
        self.reject_promise(promise, &reason);
    }

    /// `await value`: subscribe the coroutine to `PromiseResolve(value)`.
    pub(super) fn await_value(
        &mut self,
        dom: &mut Dom,
        id: usize,
        value: &JsValue,
    ) -> Result<(), JsError> {
        let promise = self.promise_resolve(dom, value)?;
        self.ensure_heap_capacity(2)?;
        let on_fulfilled = self.realm.async_resume(id, false);
        let on_rejected = self.realm.async_resume(id, true);
        self.perform_promise_then(
            promise,
            &[JsValue::Object(on_fulfilled), JsValue::Object(on_rejected)],
        )?;
        Ok(())
    }

    /// ECMA-262 `PromiseResolve(%Promise%, value)`, including thenable adoption.
    pub(super) fn promise_resolve(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<ObjectId, JsError> {
        if let JsValue::Object(object) = value
            && matches!(self.realm.host(*object), Some(ObjectHost::Promise(_)))
        {
            return Ok(*object);
        }
        let (index, wrapper) = self.create_promise()?;
        let JsValue::Object(wrapper) = wrapper else {
            unreachable!("create_promise returns a promise object");
        };
        if let JsValue::Object(object) = value {
            let then = self.get_member(dom, *object, "then")?;
            if let JsValue::Object(then) = then
                && Self::is_callable_object(then, &self.realm)
            {
                self.ensure_heap_capacity(2)?;
                let resolve = self.realm.promise_settler(index, true);
                let reject = self.realm.promise_settler(index, false);
                if let Err(error) = self.call_with_this(
                    dom,
                    then,
                    &[JsValue::Object(resolve), JsValue::Object(reject)],
                    value.clone(),
                ) {
                    self.reject_with_error(index, &error);
                }
                return Ok(wrapper);
            }
        }
        self.resolve_promise(index, value);
        Ok(wrapper)
    }

    // ------------------------------------------------------------------ driver

    /// Continue coroutine `id` until it suspends, finishes or throws.
    pub(super) fn drive(
        &mut self,
        dom: &mut Dom,
        id: usize,
        resume: Resume,
    ) -> Result<CoStep, JsError> {
        let Some(slot) = self.coroutines.get_mut(id) else {
            return Err(JsError::type_error("coroutine refers to unknown state"));
        };
        let Some(mut coroutine) = slot.take() else {
            return Err(JsError::type_error("generator is already running"));
        };
        let outer_environment = std::mem::replace(
            &mut self.environment,
            std::mem::take(&mut coroutine.environment),
        );
        self.class_frames.push(coroutine.class_frame.clone());
        self.call_stack.push(CallFrame {
            name: coroutine.label.clone(),
        });
        let result = self.run(dom, &mut coroutine, resume);
        self.call_stack.pop();
        self.class_frames.pop();
        coroutine.environment = std::mem::replace(&mut self.environment, outer_environment);
        if matches!(result, Ok(CoStep::Complete(_)) | Err(_)) {
            coroutine.state = CoState::Done;
            coroutine.environment.clear();
            coroutine.loops.clear();
            coroutine.handlers.clear();
        } else {
            coroutine.state = CoState::Suspended;
        }
        self.coroutines[id] = Some(coroutine);
        result
    }

    fn run(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        resume: Resume,
    ) -> Result<CoStep, JsError> {
        let mut action = if co.state == CoState::Start {
            Ok(Flow::Next)
        } else {
            self.apply_resume(dom, co, resume)
        };
        loop {
            match action {
                Ok(Flow::Next) => {}
                Ok(Flow::Suspend(step)) => return Ok(step),
                Ok(Flow::Done(value)) => return Ok(CoStep::Complete(value)),
                Err(error) => {
                    // The error goes to the innermost handler; with none left
                    // it leaves the coroutine.
                    action = Ok(self.unwind(dom, co, Abrupt::Throw(error))?);
                    continue;
                }
            }
            self.consume_step()?;
            action = self.step(dom, co);
        }
    }

    /// Deliver the value a suspended coroutine was resumed with.
    fn apply_resume(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        resume: Resume,
    ) -> Result<Flow, JsError> {
        if let Some(after) = co.after_await.take() {
            return self.resume_after_await(dom, co, after, resume);
        }
        let Instr::Suspend { kind, target, .. } = co.code.instrs[co.pc].clone() else {
            return Err(JsError::type_error(
                "coroutine resumed away from a suspension",
            ));
        };
        if kind == SuspendKind::YieldDelegate {
            return self.delegate_step(dom, co, resume, &target);
        }
        match resume {
            Resume::Next(value) => {
                co.set_hidden(&target, value);
                co.pc += 1;
                Ok(Flow::Next)
            }
            Resume::Throw(value) => Err(JsError::thrown(value)),
            // ECMA-262 27.6.3.8: an async generator awaits a `return` value
            // before the return takes effect.
            Resume::Return(value) if kind == SuspendKind::AsyncYield => {
                co.after_await = Some(AfterAwait::YieldReturn);
                Ok(Flow::Suspend(CoStep::Await(value)))
            }
            Resume::Return(value) => self.do_return(dom, co, value),
        }
    }

    /// Continue after an `await` that [`AfterAwait`] said needs more than the
    /// body resuming with the settled value.
    fn resume_after_await(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        after: AfterAwait,
        resume: Resume,
    ) -> Result<Flow, JsError> {
        match (after, resume) {
            (AfterAwait::YieldReturn, Resume::Next(value)) => {
                self.unwind(dom, co, Abrupt::Return(value))
            }
            (AfterAwait::YieldReturn, Resume::Throw(value)) => {
                self.unwind(dom, co, Abrupt::Throw(JsError::thrown(value)))
            }
            (AfterAwait::YieldReturn, Resume::Return(_)) => {
                Err(JsError::type_error("coroutine resumed with an unexpected return"))
            }
            (AfterAwait::CloseIterator(abrupt), resume) => {
                // AsyncIteratorClose: a throw completion already in flight wins
                // over whatever closing produced; otherwise the awaited
                // `return()` result must be an object.
                let abrupt = match (abrupt, resume) {
                    (throw @ Abrupt::Throw(_), _) => throw,
                    (other, Resume::Next(JsValue::Object(_)) | Resume::Return(_)) => other,
                    (_, Resume::Next(_)) => Abrupt::Throw(JsError::type_error(
                        "iterator result is not an object",
                    )),
                    (_, Resume::Throw(reason)) => Abrupt::Throw(JsError::thrown(reason)),
                };
                self.unwind(dom, co, abrupt)
            }
        }
    }

    // ------------------------------------------------------------ one instruction

    #[allow(clippy::too_many_lines)]
    fn step(&mut self, dom: &mut Dom, co: &mut Coroutine) -> Result<Flow, JsError> {
        // Falling off the end of the body is `return undefined`.
        let Some(instruction) = co.code.instrs.get(co.pc).cloned() else {
            return self.do_return(dom, co, JsValue::Undefined);
        };
        match instruction {
            Instr::Exec(statement) => {
                let completion =
                    self.evaluate_statements(dom, std::slice::from_ref(statement.as_ref()))?;
                match completion {
                    Completion::Normal(_) => {
                        co.pc += 1;
                        Ok(Flow::Next)
                    }
                    Completion::Return(value) => self.do_return(dom, co, value),
                    Completion::Break(label) => self.do_jump(dom, co, label.as_deref(), false),
                    Completion::Continue(label) => self.do_jump(dom, co, label.as_deref(), true),
                }
            }
            Instr::Eval(expression) => {
                self.evaluate(dom, &expression)?;
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::SetTemp(name, expression) => {
                let value = self.evaluate(dom, &expression)?;
                co.set_hidden(&name, value);
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::Suspend {
                kind,
                argument,
                target,
            } => {
                let value = match argument {
                    Some(argument) => self.evaluate(dom, &argument)?,
                    None => JsValue::Undefined,
                };
                match kind {
                    SuspendKind::Yield | SuspendKind::AsyncYield => {
                        Ok(Flow::Suspend(CoStep::Yield(value)))
                    }
                    SuspendKind::Await => Ok(Flow::Suspend(CoStep::Await(value))),
                    SuspendKind::YieldDelegate => {
                        let (iterator, next) = self.delegate_iterator(dom, &value)?;
                        co.set_hidden("%di", JsValue::Object(iterator));
                        co.set_hidden("%dn", JsValue::Object(next));
                        self.delegate_step(dom, co, Resume::Next(JsValue::Undefined), &target)
                    }
                }
            }
            Instr::Jump(target) => {
                co.pc = target;
                Ok(Flow::Next)
            }
            Instr::JumpIfFalse(condition, target) => {
                if self.evaluate(dom, &condition)?.is_truthy() {
                    co.pc += 1;
                } else {
                    co.pc = target;
                }
                Ok(Flow::Next)
            }
            Instr::JumpIfTrue(condition, target) => {
                if self.evaluate(dom, &condition)?.is_truthy() {
                    co.pc = target;
                } else {
                    co.pc += 1;
                }
                Ok(Flow::Next)
            }
            Instr::PushScope(statements) => {
                self.environment
                    .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
                self.instantiate_block_lexicals(&statements)?;
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::PopScope => {
                self.environment.pop();
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::EnterLoop(info) => {
                co.loops.push(LoopEntry {
                    info,
                    scope_depth: self.environment.len(),
                    handler_depth: co.handlers.len(),
                    closable: true,
                });
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::ExitLoop => {
                co.loops.pop();
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::Break(label) => self.do_jump(dom, co, label.as_deref(), false),
            Instr::Continue(label) => self.do_jump(dom, co, label.as_deref(), true),
            Instr::Return(expression) => {
                let value = match expression {
                    Some(expression) => self.evaluate(dom, &expression)?,
                    None => JsValue::Undefined,
                };
                self.do_return(dom, co, value)
            }
            Instr::ForOfInit {
                iterable,
                slot,
                binding,
                is_await,
            } => {
                let value = self.evaluate(dom, &iterable)?;
                let (iterator, next) = if is_await {
                    self.async_delegate_iterator(dom, &value)?
                } else {
                    self.delegate_iterator(dom, &value)?
                };
                if binding.kind == VariableKind::Var {
                    self.create_binding(
                        &binding.name,
                        VariableKind::Var,
                        true,
                        JsValue::Undefined,
                    )?;
                }
                co.set_hidden(&format!("%i{slot}"), JsValue::Object(iterator));
                co.set_hidden(&format!("%n{slot}"), JsValue::Object(next));
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::ForOfNext {
                slot,
                binding,
                done_pc,
            } => {
                self.truncate_to_loop(co);
                set_loop_closable(co, false);
                let (JsValue::Object(iterator), JsValue::Object(next)) = (
                    co.hidden(&format!("%i{slot}")),
                    co.hidden(&format!("%n{slot}")),
                ) else {
                    return Err(JsError::type_error("for-of lost its iterator"));
                };
                let result = self.call_with_this(dom, next, &[], JsValue::Object(iterator))?;
                let JsValue::Object(result) = result else {
                    return Err(JsError::type_error("iterator result is not an object"));
                };
                let done = self.get_member(dom, result, "done")?.is_truthy();
                if done {
                    co.pc = done_pc;
                    return Ok(Flow::Next);
                }
                let value = self.get_member(dom, result, "value")?;
                set_loop_closable(co, true);
                self.bind_loop_value(&binding, value)?;
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::ForAwaitCall { slot } => {
                self.truncate_to_loop(co);
                set_loop_closable(co, false);
                let (JsValue::Object(iterator), JsValue::Object(next)) = (
                    co.hidden(&format!("%i{slot}")),
                    co.hidden(&format!("%n{slot}")),
                ) else {
                    return Err(JsError::type_error("for await lost its iterator"));
                };
                let result = self.call_with_this(dom, next, &[], JsValue::Object(iterator))?;
                co.set_hidden(&next_result_name(slot), result);
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::ForAwaitBind {
                slot,
                binding,
                done_pc,
            } => {
                let JsValue::Object(result) = co.hidden(&awaited_result_name(slot)) else {
                    return Err(JsError::type_error("iterator result is not an object"));
                };
                let done = self.get_member(dom, result, "done")?.is_truthy();
                if done {
                    co.pc = done_pc;
                    return Ok(Flow::Next);
                }
                let value = self.get_member(dom, result, "value")?;
                set_loop_closable(co, true);
                self.bind_loop_value(&binding, value)?;
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::ForInInit {
                iterable,
                slot,
                binding,
            } => {
                let value = self.evaluate(dom, &iterable)?;
                let names = match value {
                    JsValue::Object(object) => self
                        .realm
                        .enumerable_property_names(object)
                        .ok_or_else(|| {
                            JsError::type_error("could not enumerate object properties")
                        })?,
                    _ => Vec::new(),
                };
                let names: Vec<JsValue> = names.into_iter().map(JsValue::String).collect();
                let list = self.create_array_from_values(&names)?;
                if binding.kind == VariableKind::Var {
                    self.create_binding(
                        &binding.name,
                        VariableKind::Var,
                        true,
                        JsValue::Undefined,
                    )?;
                }
                co.set_hidden(&format!("%k{slot}"), JsValue::Object(list));
                co.set_hidden(&format!("%x{slot}"), JsValue::Number(0.0));
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::ForInNext {
                slot,
                binding,
                done_pc,
            } => {
                self.truncate_to_loop(co);
                let JsValue::Object(list) = co.hidden(&format!("%k{slot}")) else {
                    return Err(JsError::type_error("for-in lost its key list"));
                };
                let JsValue::Number(index) = co.hidden(&format!("%x{slot}")) else {
                    return Err(JsError::type_error("for-in lost its position"));
                };
                let values = self.array_elements_for(list)?;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let Some(value) = values.get(index as usize).cloned() else {
                    co.pc = done_pc;
                    return Ok(Flow::Next);
                };
                co.set_hidden(&format!("%x{slot}"), JsValue::Number(index + 1.0));
                self.bind_loop_value(&binding, value)?;
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::CopyScope => {
                if let Some(top) = self.environment.last() {
                    let copy = EnvironmentRecord {
                        bindings: top.borrow().bindings.clone(),
                        function_scope: false,
                        ..EnvironmentRecord::default()
                    };
                    let last = self.environment.len() - 1;
                    self.environment[last] = Rc::new(RefCell::new(copy));
                }
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::TryEnter {
                catch_pc,
                finally_pc,
                slot,
            } => {
                co.handlers.push(Handler {
                    catch_pc,
                    finally_pc,
                    slot,
                    scope_depth: self.environment.len(),
                    loop_depth: co.loops.len(),
                });
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::TryExit {
                finally_pc,
                after_pc,
                slot,
            } => {
                co.handlers.pop();
                match finally_pc {
                    Some(finally_pc) => {
                        co.set_pending(slot, Pending::Normal);
                        co.pc = finally_pc;
                    }
                    None => co.pc = after_pc,
                }
                Ok(Flow::Next)
            }
            Instr::CatchBind {
                parameter,
                lexicals,
                slot,
            } => {
                let environment = Rc::new(RefCell::new(EnvironmentRecord::default()));
                if let Some(parameter) = &parameter {
                    let mut scope = environment.borrow_mut();
                    for name in parameter.names() {
                        scope.bindings.insert(
                            name,
                            Binding {
                                value: JsValue::Undefined,
                                mutable: true,
                                initialized: false,
                                kind: VariableKind::Let,
                            },
                        );
                    }
                }
                self.environment.push(environment);
                if let Some(parameter) = &parameter {
                    let thrown = co.hidden(&format!("%e{slot}"));
                    self.bind_catch_parameter(dom, Some(parameter), thrown)?;
                }
                self.instantiate_block_lexicals(&lexicals)?;
                co.pc += 1;
                Ok(Flow::Next)
            }
            Instr::CatchExit {
                finally_pc,
                after_pc,
                slot,
            } => {
                match finally_pc {
                    Some(finally_pc) => {
                        co.handlers.pop();
                        co.set_pending(slot, Pending::Normal);
                        co.pc = finally_pc;
                    }
                    None => co.pc = after_pc,
                }
                Ok(Flow::Next)
            }
            Instr::FinallyEnd { slot } => match co.pending(slot) {
                Pending::Normal => {
                    co.pc += 1;
                    Ok(Flow::Next)
                }
                Pending::Throw(value) => Err(JsError::thrown(value)),
                Pending::Return(value) => self.do_return(dom, co, value),
                Pending::Jump { is_continue, label } => {
                    self.do_jump(dom, co, label.as_deref(), is_continue)
                }
            },
        }
    }

    /// Drop every scope pushed since the innermost loop was entered, which is
    /// how a loop head starts a fresh iteration.
    fn truncate_to_loop(&mut self, co: &Coroutine) {
        if let Some(entry) = co.loops.last() {
            self.environment.truncate(entry.scope_depth);
        }
    }

    fn bind_loop_value(&mut self, binding: &LoopBinding, value: JsValue) -> Result<(), JsError> {
        if binding.kind == VariableKind::Var {
            return self.assign_binding(&binding.name, value);
        }
        let environment = Rc::new(RefCell::new(EnvironmentRecord::default()));
        environment.borrow_mut().bindings.insert(
            binding.name.clone(),
            Binding {
                value,
                mutable: binding.kind != VariableKind::Const,
                initialized: true,
                kind: binding.kind,
            },
        );
        self.environment.push(environment);
        Ok(())
    }

    // -------------------------------------------------- abrupt control transfer

    /// Carry an abrupt completion outwards from the current position. Handlers
    /// and loops are left innermost first: a `finally` the completion reaches
    /// runs and the completion resumes at its end, and a loop being left
    /// closes its iterator. Closing a `for await` loop suspends until the
    /// `return()` result settles, so the rest of the walk resumes later through
    /// [`AfterAwait::CloseIterator`] instead of recursing.
    fn unwind(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        mut abrupt: Abrupt,
    ) -> Result<Flow, JsError> {
        loop {
            if let Abrupt::Throw(error) = &abrupt
                && error.kind() == JsErrorKind::ResourceLimit
            {
                return Err(error.clone());
            }
            if let Abrupt::Goto(pc) = abrupt {
                co.pc = pc;
                return Ok(Flow::Next);
            }
            // A `break`/`continue` stays inside its target loop: loops above the
            // target are left, and so are handlers entered inside the target.
            let jump = match &abrupt {
                Abrupt::Jump { label, is_continue } => {
                    match Self::jump_target(co, label.as_deref(), *is_continue) {
                        Ok(index) => Some((index, *is_continue)),
                        Err(error) => {
                            abrupt = Abrupt::Throw(error);
                            continue;
                        }
                    }
                }
                _ => None,
            };
            let (loop_floor, handler_floor) = match jump {
                Some((index, _)) => (index + 1, co.loops[index].handler_depth),
                None => (0, 0),
            };
            // A handler entered after the innermost loop is the next thing out.
            if co.handlers.len() > handler_floor
                && co
                    .handlers
                    .last()
                    .is_some_and(|handler| handler.loop_depth >= co.loops.len())
                && let Some(handler) = co.handlers.pop()
            {
                match &abrupt {
                    Abrupt::Throw(error) => {
                        let value = self.error_to_thrown_value(error)?;
                        self.environment.truncate(handler.scope_depth);
                        if let Some(catch_pc) = handler.catch_pc {
                            co.set_hidden(&format!("%e{}", handler.slot), value);
                            if handler.finally_pc.is_some() {
                                co.handlers.push(Handler {
                                    catch_pc: None,
                                    ..handler.clone()
                                });
                            }
                            co.pc = catch_pc;
                            return Ok(Flow::Next);
                        }
                        if let Some(finally_pc) = handler.finally_pc {
                            co.set_pending(handler.slot, Pending::Throw(value));
                            co.pc = finally_pc;
                            return Ok(Flow::Next);
                        }
                    }
                    Abrupt::Return(value) => {
                        if let Some(finally_pc) = handler.finally_pc {
                            self.environment.truncate(handler.scope_depth);
                            co.set_pending(handler.slot, Pending::Return(value.clone()));
                            co.pc = finally_pc;
                            return Ok(Flow::Next);
                        }
                    }
                    Abrupt::Jump { label, is_continue } => {
                        if let Some(finally_pc) = handler.finally_pc {
                            self.environment.truncate(handler.scope_depth);
                            co.set_pending(
                                handler.slot,
                                Pending::Jump {
                                    is_continue: *is_continue,
                                    label: label.clone(),
                                },
                            );
                            co.pc = finally_pc;
                            return Ok(Flow::Next);
                        }
                    }
                    // A resolved jump has no handlers above its loop left.
                    Abrupt::Goto(_) => {}
                }
                continue;
            }
            if co.loops.len() > loop_floor {
                let Some(entry) = co.loops.pop() else {
                    continue;
                };
                self.environment.truncate(entry.scope_depth);
                if entry.closable
                    && let Some(close) = entry.info.iterator
                    && let Some(flow) = self.close_left_iterator(dom, co, close, &mut abrupt)?
                {
                    return Ok(flow);
                }
                continue;
            }
            // Nothing is left above the floor.
            match (abrupt, jump) {
                (Abrupt::Return(value), _) => return Ok(Flow::Done(value)),
                (Abrupt::Throw(error), _) => return Err(error),
                (Abrupt::Goto(pc), _) => {
                    co.pc = pc;
                    return Ok(Flow::Next);
                }
                (Abrupt::Jump { .. }, Some((index, true))) => {
                    // `continue`: the target loop stays entered.
                    let entry = &co.loops[index];
                    self.environment.truncate(entry.scope_depth);
                    let continue_pc = entry.info.continue_pc.unwrap_or(co.pc + 1);
                    abrupt = Abrupt::Goto(continue_pc);
                }
                (Abrupt::Jump { .. }, Some((_, false))) => {
                    // `break`: the target loop is left too, and closes as well.
                    let Some(entry) = co.loops.pop() else {
                        return Err(JsError::syntax("no enclosing loop for break", 0));
                    };
                    self.environment.truncate(entry.scope_depth);
                    abrupt = Abrupt::Goto(entry.info.break_pc);
                    if entry.closable
                        && let Some(close) = entry.info.iterator
                        && let Some(flow) = self.close_left_iterator(dom, co, close, &mut abrupt)?
                    {
                        return Ok(flow);
                    }
                }
                (Abrupt::Jump { .. }, None) => {
                    return Err(JsError::syntax("no enclosing target for break/continue", 0));
                }
            }
        }
    }

    /// `return value` from inside the body, running enclosing `finally` blocks.
    fn do_return(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        value: JsValue,
    ) -> Result<Flow, JsError> {
        self.unwind(dom, co, Abrupt::Return(value))
    }

    /// `break` / `continue`, optionally to a label, running enclosing `finally`
    /// blocks on the way out.
    fn do_jump(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        label: Option<&str>,
        is_continue: bool,
    ) -> Result<Flow, JsError> {
        self.unwind(
            dom,
            co,
            Abrupt::Jump {
                label: label.map(Rc::from),
                is_continue,
            },
        )
    }

    /// The index of the loop a `break`/`continue` targets: the innermost one the
    /// label names, or the innermost one that accepts the statement.
    fn jump_target(co: &Coroutine, label: Option<&str>, is_continue: bool) -> Result<usize, JsError> {
        co.loops
            .iter()
            .rposition(|entry| match label {
                Some(label) => entry
                    .info
                    .labels
                    .iter()
                    .any(|candidate| &**candidate == label),
                None if is_continue => entry.info.continue_pc.is_some(),
                None => entry.info.unlabeled_break,
            })
            .ok_or_else(|| JsError::syntax("no enclosing target for break/continue", 0))
    }

    /// Close the iterator of a loop that `abrupt` is leaving. An error from
    /// closing replaces a non-throw completion; a throw in flight keeps its own
    /// error (ECMA-262 7.4.11 IteratorClose). For `for await`, the `return()`
    /// result is awaited, so `Some(flow)` suspends the coroutine with `abrupt`
    /// parked in [`AfterAwait::CloseIterator`].
    fn close_left_iterator(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        close: IteratorClose,
        abrupt: &mut Abrupt,
    ) -> Result<Option<Flow>, JsError> {
        let outcome = if close.is_async {
            match self.start_async_close(dom, co, close.slot) {
                Ok(Some(result)) => {
                    co.after_await = Some(AfterAwait::CloseIterator(abrupt.clone()));
                    return Ok(Some(Flow::Suspend(CoStep::Await(result))));
                }
                Ok(None) => Ok(()),
                Err(error) => Err(error),
            }
        } else {
            self.close_iterator(dom, co, close.slot)
        };
        if let Err(error) = outcome
            && !matches!(abrupt, Abrupt::Throw(_))
        {
            *abrupt = Abrupt::Throw(error);
        }
        Ok(None)
    }

    /// The call half of `AsyncIteratorClose`: call the iterator's `return` and
    /// hand back its result for the caller to await. `None` when there is none.
    fn start_async_close(
        &mut self,
        dom: &mut Dom,
        co: &Coroutine,
        slot: usize,
    ) -> Result<Option<JsValue>, JsError> {
        let JsValue::Object(iterator) = co.hidden(&format!("%i{slot}")) else {
            return Ok(None);
        };
        match self.get_member(dom, iterator, "return")? {
            JsValue::Undefined | JsValue::Null => Ok(None),
            JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                let result = self.call_with_this(dom, method, &[], JsValue::Object(iterator))?;
                Ok(Some(result))
            }
            _ => Err(JsError::type_error("iterator return is not callable")),
        }
    }

    /// `IteratorClose` for a `for…of` loop being left abruptly.
    fn close_iterator(&mut self, dom: &mut Dom, co: &Coroutine, slot: usize) -> Result<(), JsError> {
        let JsValue::Object(iterator) = co.hidden(&format!("%i{slot}")) else {
            return Ok(());
        };
        self.close_iterator_object(dom, iterator)
    }

    pub(super) fn close_iterator_object(
        &mut self,
        dom: &mut Dom,
        iterator: ObjectId,
    ) -> Result<(), JsError> {
        let method = self.get_member(dom, iterator, "return")?;
        if let JsValue::Object(method) = method
            && Self::is_callable_object(method, &self.realm)
        {
            self.call_with_this(dom, method, &[], JsValue::Object(iterator))?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ yield*

    /// The iterator `yield*`/`for…of` steps through. Strings are iterated as
    /// code points by materialising them first.
    pub(super) fn delegate_iterator(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<(ObjectId, ObjectId), JsError> {
        let source = match value {
            JsValue::String(_) => {
                let items = self.iterate_values(dom, value)?;
                JsValue::Object(self.create_array_from_values(&items)?)
            }
            other => other.clone(),
        };
        match self.get_iterator(dom, &source)? {
            Some(pair) => Ok(pair),
            None => Err(JsError::type_error(format!(
                "{} is not iterable",
                value.to_js_string()
            ))),
        }
    }

    fn delegate_step(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        resume: Resume,
        target: &str,
    ) -> Result<Flow, JsError> {
        let (JsValue::Object(iterator), JsValue::Object(next)) =
            (co.hidden("%di"), co.hidden("%dn"))
        else {
            return Err(JsError::type_error("yield* lost its iterator"));
        };
        let result = match resume {
            Resume::Next(value) => {
                self.call_with_this(dom, next, &[value], JsValue::Object(iterator))?
            }
            Resume::Throw(value) => {
                let method = self.get_member(dom, iterator, "throw")?;
                match method {
                    JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                        self.call_with_this(dom, method, &[value], JsValue::Object(iterator))?
                    }
                    _ => {
                        let _ = self.close_iterator_object(dom, iterator);
                        return Err(JsError::type_error(
                            "the iterator does not provide a 'throw' method",
                        ));
                    }
                }
            }
            Resume::Return(value) => {
                let method = self.get_member(dom, iterator, "return")?;
                match method {
                    JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                        let result =
                            self.call_with_this(dom, method, &[value], JsValue::Object(iterator))?;
                        let JsValue::Object(object) = result else {
                            return Err(JsError::type_error("iterator result is not an object"));
                        };
                        if self.get_member(dom, object, "done")?.is_truthy() {
                            let value = self.get_member(dom, object, "value")?;
                            return self.do_return(dom, co, value);
                        }
                        JsValue::Object(object)
                    }
                    _ => return self.do_return(dom, co, value),
                }
            }
        };
        let JsValue::Object(result) = result else {
            return Err(JsError::type_error("iterator result is not an object"));
        };
        let value = self.get_member(dom, result, "value")?;
        if self.get_member(dom, result, "done")?.is_truthy() {
            co.set_hidden(target, value);
            co.pc += 1;
            Ok(Flow::Next)
        } else {
            Ok(Flow::Suspend(CoStep::Yield(value)))
        }
    }
}
