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
use super::coroutine::{CoroutineCode, Instr, LoopBinding, LoopInfo, SuspendKind};
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
        if function.kind == FunctionKind::AsyncGenerator {
            return Err(JsError::type_error(
                "async generators are not supported yet",
            ));
        }
        let code = if let Some(code) = self.coroutine_code.get(&function_index) {
            code.clone()
        } else {
            let code = Rc::new(super::coroutine::Compiler::compile(&function.body)?);
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
        }));
        if function.kind == FunctionKind::Generator {
            self.ensure_heap_capacity(1)?;
            return Ok(JsValue::Object(self.realm.generator_object(id)));
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

    fn finish_coroutine(&mut self, id: usize) {
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

    fn reject_with_error(&mut self, promise: usize, error: &JsError) {
        let reason = self
            .error_to_thrown_value(error)
            .unwrap_or_else(|_| JsValue::String(error.message().to_owned()));
        self.reject_promise(promise, &reason);
    }

    /// `await value`: subscribe the coroutine to `PromiseResolve(value)`.
    fn await_value(&mut self, dom: &mut Dom, id: usize, value: &JsValue) -> Result<(), JsError> {
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
    fn promise_resolve(&mut self, dom: &mut Dom, value: &JsValue) -> Result<ObjectId, JsError> {
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
                Err(error) => self.handle_throw(dom, co, error)?,
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
            Resume::Return(value) => self.do_return(dom, co, value),
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
                    SuspendKind::Yield => Ok(Flow::Suspend(CoStep::Yield(value))),
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
            } => {
                let value = self.evaluate(dom, &iterable)?;
                let (iterator, next) = self.delegate_iterator(dom, &value)?;
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
                self.bind_loop_value(dom, &binding, value)?;
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
                self.bind_loop_value(dom, &binding, value)?;
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

    fn bind_loop_value(
        &mut self,
        dom: &mut Dom,
        binding: &LoopBinding,
        value: JsValue,
    ) -> Result<(), JsError> {
        if binding.kind == VariableKind::Var {
            return self.assign_binding(dom, &binding.name, value);
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

    /// Close the loops above `loop_depth` (calling `return` on `for…of`
    /// iterators) and drop scopes above `scope_depth`.
    fn unwind_to(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        scope_depth: usize,
        loop_depth: usize,
        suppress_errors: bool,
    ) -> Result<(), JsError> {
        let mut first_error = None;
        while co.loops.len() > loop_depth {
            let Some(entry) = co.loops.pop() else {
                break;
            };
            if let Some(slot) = entry.info.iterator_slot
                && let Err(error) = self.close_iterator(dom, co, slot)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        self.environment.truncate(scope_depth);
        match first_error {
            Some(error) if !suppress_errors => Err(error),
            _ => Ok(()),
        }
    }

    /// `IteratorClose`: call the iterator's `return`, if it has one.
    fn close_iterator(
        &mut self,
        dom: &mut Dom,
        co: &Coroutine,
        slot: usize,
    ) -> Result<(), JsError> {
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

    /// An error was thrown at the current instruction: find the nearest
    /// `catch` or `finally`, or give up.
    fn handle_throw(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        error: JsError,
    ) -> Result<(), JsError> {
        if error.kind() == JsErrorKind::ResourceLimit {
            return Err(error);
        }
        let mut error = error;
        loop {
            let Some(handler) = co.handlers.pop() else {
                return Err(error);
            };
            // Errors from closing iterators while unwinding a throw are
            // dropped; the original error wins.
            self.unwind_to(dom, co, handler.scope_depth, handler.loop_depth, true)?;
            if let Some(catch_pc) = handler.catch_pc {
                let value = self.error_to_thrown_value(&error)?;
                co.set_hidden(&format!("%e{}", handler.slot), value);
                if handler.finally_pc.is_some() {
                    co.handlers.push(Handler {
                        catch_pc: None,
                        ..handler
                    });
                }
                co.pc = catch_pc;
                return Ok(());
            }
            if let Some(finally_pc) = handler.finally_pc {
                let value = self.error_to_thrown_value(&error)?;
                co.set_pending(handler.slot, Pending::Throw(value));
                co.pc = finally_pc;
                return Ok(());
            }
            error = error.clone();
        }
    }

    /// `return value` from inside the body, running enclosing `finally` blocks.
    fn do_return(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        value: JsValue,
    ) -> Result<Flow, JsError> {
        while let Some(handler) = co.handlers.pop() {
            if let Some(finally_pc) = handler.finally_pc {
                self.unwind_to(dom, co, handler.scope_depth, handler.loop_depth, false)?;
                co.set_pending(handler.slot, Pending::Return(value));
                co.pc = finally_pc;
                return Ok(Flow::Next);
            }
        }
        self.unwind_to(dom, co, 0, 0, false)?;
        Ok(Flow::Done(value))
    }

    /// `break` / `continue`, optionally to a label, running enclosing
    /// `finally` blocks on the way out.
    fn do_jump(
        &mut self,
        dom: &mut Dom,
        co: &mut Coroutine,
        label: Option<&str>,
        is_continue: bool,
    ) -> Result<Flow, JsError> {
        let Some(index) = co.loops.iter().rposition(|entry| match label {
            Some(label) => entry
                .info
                .labels
                .iter()
                .any(|candidate| &**candidate == label),
            None if is_continue => entry.info.continue_pc.is_some(),
            None => entry.info.unlabeled_break,
        }) else {
            return Err(JsError::syntax("no enclosing target for break/continue", 0));
        };
        let (scope_depth, handler_depth) = {
            let entry = &co.loops[index];
            (entry.scope_depth, entry.handler_depth)
        };
        let finally = co.handlers[handler_depth..]
            .iter()
            .rposition(|handler| handler.finally_pc.is_some());
        if let Some(offset) = finally {
            let position = handler_depth + offset;
            let handler = co.handlers[position].clone();
            co.handlers.truncate(position);
            self.unwind_to(dom, co, handler.scope_depth, handler.loop_depth, false)?;
            co.set_pending(
                handler.slot,
                Pending::Jump {
                    is_continue,
                    label: label.map(Rc::from),
                },
            );
            co.pc = handler.finally_pc.unwrap_or(co.pc);
            return Ok(Flow::Next);
        }
        co.handlers.truncate(handler_depth);
        let (continue_pc, break_pc) = {
            let entry = &co.loops[index];
            (entry.info.continue_pc, entry.info.break_pc)
        };
        if is_continue {
            self.unwind_to(dom, co, scope_depth, index + 1, false)?;
            co.pc = continue_pc.unwrap_or(co.pc + 1);
        } else {
            self.unwind_to(dom, co, scope_depth, index, false)?;
            co.pc = break_pc;
        }
        Ok(Flow::Next)
    }

    // ------------------------------------------------------------------ yield*

    /// The iterator `yield*`/`for…of` steps through. Strings are iterated as
    /// code points by materialising them first.
    fn delegate_iterator(
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
