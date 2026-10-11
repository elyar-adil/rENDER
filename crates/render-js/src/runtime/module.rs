//! Module records, linking, evaluation, and dynamic import (ECMA-262 §16.2.1
//! and §13.3.10), with the import-defer and source-phase extensions.
//!
//! A module owns one environment. Import bindings are not copies: the
//! importing environment holds an [`ImportRef`] and every read resolves it to
//! the exporter's binding cell, which is what makes bindings live and lets
//! cyclic graphs work for hoisted declarations.
//!
//! Module sources come from the host's [`ModuleLoader`]. [`JsRuntime::load_module`]
//! fetches a module's static graph through it, [`JsRuntime::link_module`] and
//! [`JsRuntime::evaluate_module`] link and evaluate a declared module, and
//! `import()` drives the same steps from script code. Evaluation follows the
//! cyclic-module algorithms of §16.2.1.5: a module whose body has a top-level
//! `await`, or that depends on one, completes asynchronously, its body running
//! as a coroutine.
//!
//! A deferred import (`import defer * as ns`) requests its module without
//! evaluating it. The deferred namespace object evaluates the module the first
//! time a string-keyed property of it is used, synchronously, unless that is not
//! possible (the import-defer proposal's `EnsureDeferredNamespaceEvaluation`).
//!
//! Deviations, deliberate and recorded: import attributes reach the loader only
//! for dynamic imports, since static requests do not record theirs; a source-phase
//! import always fails, as a source-text module has no source object.

use super::build_line_starts;
use super::builtins::promise::PromiseState;
use super::coroutine::{Compiler, statements_have_suspend};
use super::types::{Binding, BindingRead, Environment, EnvironmentRecord, PromiseCapability};
use crate::module::{IMPORT_META_BINDING, ImportName, ImportPhase, ModuleInfo};
use crate::parser::{Statement, VariableKind};
use crate::value::{NamespaceExport, ObjectHost};
use crate::{CompiledScript, JsError, JsRuntime, JsValue, ObjectId, ScriptOutcome};
use render_dom::Dom;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Debug;
use std::rc::Rc;

/// The host's source of modules (ECMA-262 16.2.1.10 `HostLoadImportedModule`).
///
/// The engine calls it for every specifier that a dynamic `import()` names and
/// for every static request of a module it declares. The loader resolves the
/// specifier against `referrer` (the base URL of the module or script that
/// asks), reads and parses the module, and returns its key with the compiled
/// source. An unresolvable or unreadable module, or source that does not parse
/// as a module, is an error.
pub trait ModuleLoader: Debug {
    /// Load the module `specifier` names from `referrer`, with its import
    /// `attributes` as `(key, value)` pairs.
    ///
    /// # Errors
    ///
    /// Returns the error the import rejects with.
    fn load(
        &mut self,
        referrer: &str,
        specifier: &str,
        attributes: &[(String, String)],
    ) -> Result<(String, CompiledScript), JsError>;
}

/// The status of a cyclic module record (ECMA-262 16.2.1.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Unlinked,
    Linking,
    Linked,
    Evaluating,
    EvaluatingAsync,
    Evaluated,
}

/// Where an imported name lives: an export of another declared module.
#[derive(Clone, Debug)]
pub(super) struct ImportRef {
    pub(super) module: String,
    pub(super) name: String,
}

#[derive(Debug)]
pub(super) struct ModuleRecord {
    pub(super) environment: Environment,
    info: ModuleInfo,
    statements: Rc<Vec<Statement>>,
    source: Rc<str>,
    /// Module specifier as written -> key of the module it resolves to.
    resolutions: BTreeMap<String, String>,
    status: Status,
    /// [[`EvaluationError`]]: the error the evaluation of this module completed with.
    evaluation_error: Option<JsError>,
    dfs_index: usize,
    dfs_ancestor_index: usize,
    /// [[`CycleRoot`]]: the module whose evaluation completes this module's strongly connected component.
    cycle_root: Option<String>,
    /// [[`HasTLA`]]: the body has a top-level `await` or `for await`.
    has_tla: bool,
    /// [[`AsyncEvaluation`]], with [[`AsyncEvaluationOrder`]] fixing the order of completion.
    async_evaluation: bool,
    async_order: u64,
    pending_async_dependencies: usize,
    async_parent_modules: Vec<String>,
    /// [[`TopLevelCapability`]]: the promise `Evaluate` returned to its caller.
    pub(super) top_level_capability: Option<usize>,
    /// [[Namespace]]: the namespace object, once it has been created.
    pub(super) namespace: Option<ObjectId>,
    /// The namespace object an `import defer` sees, created apart from [[Namespace]].
    pub(super) deferred_namespace: Option<ObjectId>,
}

/// Where an exported name resolves (ECMA-262 16.2.1.6.2 `ResolveExport`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Resolution {
    /// A binding `local` of `module`'s environment.
    Binding {
        module: String,
        local: String,
    },
    /// The namespace object of `module` (`export * as name`).
    Namespace(String),
    Null,
    Ambiguous,
}

impl JsRuntime {
    /// Install the host's module loader, which dynamic `import()` and
    /// [`Self::load_module`] fetch modules through.
    pub fn set_module_loader(&mut self, loader: Box<dyn ModuleLoader>) {
        self.module_loader = Some(loader);
    }

    /// Instantiate a module: create its environment, hoist its declarations,
    /// and wire its import bindings to the modules named in `resolutions`.
    ///
    /// # Errors
    ///
    /// Fails if `script` was not compiled as a module, a requested specifier
    /// has no resolution, or declaration instantiation fails.
    pub fn declare_module(
        &mut self,
        key: &str,
        script: &CompiledScript,
        resolutions: BTreeMap<String, String>,
    ) -> Result<(), JsError> {
        let Some(info) = script.module.clone() else {
            return Err(JsError::type_error("declare_module needs a module script"));
        };
        for request in &info.requests {
            if !resolutions.contains_key(request) {
                return Err(JsError::reference(format!(
                    "module specifier {request:?} was not resolved"
                )));
            }
        }
        let mut record = EnvironmentRecord {
            function_scope: true,
            module: Some(key.to_owned()),
            ..EnvironmentRecord::default()
        };
        let initialized = |value| Binding {
            value,
            mutable: false,
            initialized: true,
            kind: VariableKind::Const,
        };
        record
            .bindings
            .insert("this".to_owned(), initialized(JsValue::Undefined));
        let meta = self.realm.create_ordinary_object();
        self.realm.set_property(
            meta,
            "url".to_owned(),
            JsValue::String((key.to_owned()).into()),
        );
        record.bindings.insert(
            IMPORT_META_BINDING.to_owned(),
            initialized(JsValue::Object(meta)),
        );
        for entry in &info.imports {
            let target = resolutions[&entry.request].clone();
            match &entry.imported {
                ImportName::Named(name) => {
                    record.imports.insert(
                        entry.local.clone(),
                        ImportRef {
                            module: target,
                            name: name.clone(),
                        },
                    );
                }
                ImportName::Namespace => {
                    // Bound when the module links, once its exports are known.
                    record.bindings.insert(
                        entry.local.clone(),
                        Binding {
                            value: JsValue::Undefined,
                            mutable: false,
                            initialized: false,
                            kind: VariableKind::Const,
                        },
                    );
                }
            }
        }
        let environment: Environment = Rc::new(RefCell::new(record));
        let statements = Rc::new(script.statements.clone());
        let has_tla = statements_have_suspend(&statements);
        self.modules.insert(
            key.to_owned(),
            ModuleRecord {
                environment: environment.clone(),
                info,
                statements: statements.clone(),
                source: Rc::from(script.source()),
                resolutions,
                status: Status::Unlinked,
                evaluation_error: None,
                dfs_index: 0,
                dfs_ancestor_index: 0,
                cycle_root: None,
                has_tla,
                async_evaluation: false,
                async_order: 0,
                pending_async_dependencies: 0,
                async_parent_modules: Vec::new(),
                top_level_capability: None,
                namespace: None,
                deferred_namespace: None,
            },
        );
        let suspended = self.suspend_scopes();
        let saved = std::mem::replace(&mut self.environment, vec![environment]);
        let result = self.instantiate_statements(&statements);
        self.environment = saved;
        self.resume_scopes(suspended);
        result
    }

    /// Declare `script` as the module `key` together with every module its
    /// static graph requests, fetched through the installed loader
    /// (ECMA-262 16.2.1.5.2 `LoadRequestedModules`).
    ///
    /// # Errors
    ///
    /// Returns the first load, parse or declaration error of the graph. A graph
    /// that fails to load declares none of its modules.
    pub fn load_module(&mut self, key: &str, script: &CompiledScript) -> Result<(), JsError> {
        if self.modules.contains_key(key) {
            return Ok(());
        }
        self.with_module_loader(|runtime, loader| runtime.load_graph(loader, key, script.clone()))
    }

    /// Whether a module with this key has been declared.
    #[must_use]
    pub fn has_module(&self, key: &str) -> bool {
        self.modules.contains_key(key)
    }

    /// The error that completed the evaluation of module `key`, if it failed.
    /// An asynchronous evaluation can fail after [`Self::evaluate_module`]
    /// returns, so an embedder reads this once its job queue has drained.
    #[must_use]
    pub fn module_evaluation_error(&self, key: &str) -> Option<JsError> {
        self.modules.get(key)?.evaluation_error.clone()
    }

    /// Link the graph of module `key` (ECMA-262 16.2.1.5.1 Link): every import
    /// and indirect export must resolve. A failure leaves every module this call
    /// started linking unlinked, and no module body has run.
    ///
    /// # Errors
    ///
    /// Returns a `SyntaxError` for an export that is missing or ambiguous, or
    /// for a source-phase import, which a source-text module cannot provide.
    pub fn link_module(&mut self, key: &str) -> Result<(), JsError> {
        let mut stack = Vec::new();
        let result = self.inner_module_linking(key, &mut stack, 0);
        if result.is_err() {
            for module in stack {
                if let Some(record) = self.modules.get_mut(&module) {
                    record.status = Status::Unlinked;
                }
            }
        }
        result.map(|_| ())
    }

    /// Link and evaluate a declared module, and everything it depends on. Each
    /// module body runs at most once; a failure is remembered and re-thrown to
    /// every later importer.
    ///
    /// # Errors
    ///
    /// Returns the link error, or the error the module's synchronous evaluation
    /// completed with. An asynchronous failure is reported by
    /// [`Self::module_evaluation_error`] once the job queue has drained.
    pub fn evaluate_module(&mut self, dom: &mut Dom, key: &str) -> Result<ScriptOutcome, JsError> {
        self.ensure_prelude(dom);
        let from_revision = dom.revision();
        self.transient_roots.clear();
        self.collect_garbage();
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        let suspended = self.suspend_scopes();
        let saved = std::mem::take(&mut self.environment);
        let result = self
            .link_module(key)
            .and_then(|()| self.module_evaluate(dom, key))
            .and_then(|_| match self.modules.get(key) {
                Some(record) if record.status == Status::Evaluated => {
                    record.evaluation_error.clone().map_or(Ok(()), Err)
                }
                _ => Ok(()),
            });
        self.environment = saved;
        self.resume_scopes(suspended);
        if let Err(error) = &result {
            self.report_uncaught_error(dom, error);
        }
        result?;
        self.queue_mutation_deliveries(dom);
        Ok(ScriptOutcome {
            value: JsValue::Undefined,
            from_revision,
            to_revision: dom.revision(),
        })
    }

    /// Run `action` with the installed loader taken out of the runtime, so the
    /// loader and the runtime can be used together. The loader is put back after.
    fn with_module_loader<T>(
        &mut self,
        action: impl FnOnce(&mut Self, &mut Option<Box<dyn ModuleLoader>>) -> T,
    ) -> T {
        let mut loader = self.module_loader.take();
        let result = action(self, &mut loader);
        self.module_loader = loader;
        result
    }

    /// Declare `script` as module `key` and every module its requests reach.
    /// The modules are declared once the whole graph has been fetched, so a
    /// failure declares none of them.
    fn load_graph(
        &mut self,
        loader: &mut Option<Box<dyn ModuleLoader>>,
        key: &str,
        script: CompiledScript,
    ) -> Result<(), JsError> {
        let mut queue = VecDeque::from([(key.to_owned(), script)]);
        let mut queued = BTreeSet::from([key.to_owned()]);
        let mut fetched = Vec::new();
        while let Some((module_key, module_script)) = queue.pop_front() {
            let mut resolutions = BTreeMap::new();
            for request in module_script.module_requests() {
                let Some(loader) = loader.as_mut() else {
                    return Err(JsError::type_error(format!(
                        "module specifier {request:?} cannot be loaded: no module loader is installed"
                    )));
                };
                let (target, target_script) = loader.load(&module_key, request, &[])?;
                if !self.modules.contains_key(&target) && queued.insert(target.clone()) {
                    queue.push_back((target.clone(), target_script));
                }
                resolutions.insert(request.clone(), target);
            }
            fetched.push((module_key, module_script, resolutions));
        }
        for (module_key, module_script, resolutions) in fetched {
            self.declare_module(&module_key, &module_script, resolutions)?;
        }
        Ok(())
    }

    /// The referrer of a dynamic `import()` in the running code (ECMA-262
    /// `GetActiveScriptOrModule`). Module code, and functions created in it, carry
    /// their module's environment in their scope chain, so the innermost one
    /// names the module. Code outside any module refers to the document's base URL.
    fn active_script_or_module(&self) -> String {
        self.environment
            .iter()
            .rev()
            .find_map(|environment| environment.borrow().module.clone())
            .unwrap_or_else(|| self.realm.document_base_url())
    }

    /// `import(specifier, options)` (ECMA-262 13.3.10.1), and its deferred and
    /// source-phase forms. The returned promise settles once the module is loaded:
    /// with its namespace after evaluation, with the deferred namespace at once,
    /// or with a `SyntaxError` for a source-phase import. A failure before the
    /// module is loaded rejects it right away.
    pub(super) fn dynamic_import(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        phase: ImportPhase,
    ) -> Result<JsValue, JsError> {
        let (promise, object) = self.create_promise_record()?;
        let referrer = self.active_script_or_module();
        let specifier = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let options = arguments.get(1).cloned();
        let request = self.import_request(dom, &specifier, options.as_ref());
        let fetched = request.and_then(|(specifier, attributes)| {
            self.with_module_loader(|runtime, loader| {
                let Some(boxed) = loader.as_mut() else {
                    return Err(JsError::type_error(
                        "import() cannot load a module: no module loader is installed",
                    ));
                };
                let (key, script) = boxed.load(&referrer, &specifier, &attributes)?;
                if !runtime.modules.contains_key(&key) {
                    runtime.load_graph(loader, &key, script)?;
                }
                Ok(key)
            })
        });
        match fetched {
            // A module that cannot be fetched rejects the import right away
            // (ContinueDynamicImport with an abrupt completion).
            Err(error) => {
                let reason = self.error_value(&error);
                self.reject_promise(promise, &reason);
            }
            // Otherwise the import continues in a job, when the load promise settles.
            Ok(key) => {
                let (load, _) = self.create_promise_record()?;
                self.resolve_promise(load, &JsValue::Undefined);
                let linked = self.native_callable(ObjectHost::DynamicImportLinked {
                    module: key,
                    promise,
                    object,
                    phase,
                });
                self.perform_then(load, Some(linked), None, None)?;
            }
        }
        Ok(JsValue::Object(object))
    }

    /// The specifier string and import attributes of an `import()` call
    /// (ECMA-262 13.3.10.1 steps 3 to 10). Any failure is the import's rejection.
    fn import_request(
        &mut self,
        dom: &mut Dom,
        specifier: &JsValue,
        options: Option<&JsValue>,
    ) -> Result<(String, Vec<(String, String)>), JsError> {
        let specifier = self.to_string_value(dom, specifier)?.to_lossy();
        let mut attributes = Vec::new();
        let options = match options {
            None | Some(JsValue::Undefined) => return Ok((specifier, attributes)),
            Some(JsValue::Object(options)) => *options,
            Some(_) => {
                return Err(JsError::type_error(
                    "the second argument of import() must be an object",
                ));
            }
        };
        let with = match self.get_member(dom, options, "with")? {
            JsValue::Undefined => return Ok((specifier, attributes)),
            JsValue::Object(with) => with,
            _ => {
                return Err(JsError::type_error(
                    "the 'with' option of import() must be an object",
                ));
            }
        };
        for name in self.own_string_keys(dom, with)? {
            let key = crate::runtime::builtins::object::PropertyName::String(name.clone());
            let Some(descriptor) = self.own_descriptor(dom, with, &key)? else {
                continue;
            };
            if !descriptor.enumerable {
                continue;
            }
            match self.get_member(dom, with, &name)? {
                JsValue::String(value) => attributes.push((name, value.to_lossy())),
                _ => {
                    return Err(JsError::type_error(
                        "import attribute values must be strings",
                    ));
                }
            }
        }
        Ok((specifier, attributes))
    }

    /// `ContinueDynamicImport`'s `onLoadFulfilled` (ECMA-262 13.3.10.1): link the
    /// loaded `module`, then settle `promise` according to the import's `phase`.
    pub(super) fn continue_dynamic_import(
        &mut self,
        dom: &mut Dom,
        module: &str,
        promise: usize,
        object: ObjectId,
        phase: ImportPhase,
    ) -> Result<(), JsError> {
        let linked = self.link_module(module);
        let reason = match (linked, phase) {
            (Err(error), _) => Some(error),
            // A source-text module has no source object to hand over.
            (Ok(()), ImportPhase::Source) => Some(JsError::syntax(
                format!("module {module:?} has no source phase import"),
                0,
            )),
            (Ok(()), ImportPhase::Defer) => {
                let namespace = self.module_namespace(module, true)?;
                self.resolve_promise_with(dom, promise, &JsValue::Object(namespace))?;
                return Ok(());
            }
            (Ok(()), ImportPhase::Evaluation) => None,
        };
        if let Some(error) = reason {
            let reason = self.error_value(&error);
            self.reject_promise(promise, &reason);
            return Ok(());
        }
        let evaluation = self.module_evaluate(dom, module)?;
        let namespace = self.native_callable(ObjectHost::ModuleNamespaceOf {
            module: module.to_owned(),
            deferred: false,
        });
        self.perform_then(
            evaluation,
            Some(namespace),
            None,
            Some(PromiseCapability::Record { promise, object }),
        )
    }

    /// `GetModuleNamespace` (ECMA-262 16.2.1.6.3): the namespace object of
    /// `module`, created once with the exports that resolve unambiguously. A
    /// `deferred` one is the separate object `import defer` sees.
    pub(super) fn module_namespace(
        &mut self,
        module: &str,
        deferred: bool,
    ) -> Result<ObjectId, JsError> {
        let cached = self.modules.get(module).and_then(|record| {
            if deferred {
                record.deferred_namespace
            } else {
                record.namespace
            }
        });
        if let Some(namespace) = cached {
            return Ok(namespace);
        }
        let namespace = self.realm.create_object(None);
        // Recorded before the exports are read, so a cycle of `export * as` finds it.
        if let Some(record) = self.modules.get_mut(module) {
            if deferred {
                record.deferred_namespace = Some(namespace);
            } else {
                record.namespace = Some(namespace);
            }
        }
        let mut names = BTreeSet::new();
        self.export_names(module, &mut BTreeSet::new(), &mut names);
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        let mut exports = Vec::new();
        for name in names {
            match self.resolve_export(module, &name, &mut Vec::new()) {
                Resolution::Binding { module, local } => {
                    let environment = self.modules[&module].environment.clone();
                    exports.push((name, NamespaceExport::Binding { environment, local }));
                }
                Resolution::Namespace(target) => {
                    let target = self.module_namespace(&target, false)?;
                    exports.push((name, NamespaceExport::Namespace(target)));
                }
                Resolution::Null | Resolution::Ambiguous => {}
            }
        }
        self.realm
            .make_module_namespace(namespace, module, deferred, exports);
        Ok(namespace)
    }

    /// `GetExportedNames` (ECMA-262 16.2.1.6.2): the names `module` exports,
    /// including those of its `export *` modules. `visited` bounds the walk.
    fn export_names(
        &self,
        module: &str,
        visited: &mut BTreeSet<String>,
        names: &mut BTreeSet<String>,
    ) {
        if !visited.insert(module.to_owned()) {
            return;
        }
        let Some(record) = self.modules.get(module) else {
            return;
        };
        names.extend(
            record
                .info
                .local_exports
                .iter()
                .map(|(name, _)| name.clone()),
        );
        names.extend(
            record
                .info
                .indirect_exports
                .iter()
                .map(|export| export.exported.clone()),
        );
        for request in &record.info.star_exports {
            let mut inherited = BTreeSet::new();
            self.export_names(&record.resolutions[request], visited, &mut inherited);
            inherited.remove("default");
            names.extend(inherited);
        }
    }

    /// `ResolveExport` (ECMA-262 16.2.1.6.2): where `name` of `module` resolves,
    /// following re-exports. `resolve_set` holds the `(module, name)` pairs being
    /// resolved already, so a cycle resolves to nothing.
    fn resolve_export(
        &self,
        module: &str,
        name: &str,
        resolve_set: &mut Vec<(String, String)>,
    ) -> Resolution {
        if resolve_set
            .iter()
            .any(|(seen_module, seen_name)| seen_module == module && seen_name == name)
        {
            return Resolution::Null;
        }
        resolve_set.push((module.to_owned(), name.to_owned()));
        let Some(record) = self.modules.get(module) else {
            return Resolution::Null;
        };
        for (exported, local) in &record.info.local_exports {
            if exported == name {
                // An export of an imported binding is an indirect export.
                let import = record.environment.borrow().imports.get(local).cloned();
                return match import {
                    Some(import) => self.resolve_export(&import.module, &import.name, resolve_set),
                    None => Resolution::Binding {
                        module: module.to_owned(),
                        local: local.clone(),
                    },
                };
            }
        }
        for indirect in &record.info.indirect_exports {
            if indirect.exported == name {
                let target = record.resolutions[&indirect.request].clone();
                return match &indirect.imported {
                    ImportName::Namespace => Resolution::Namespace(target),
                    ImportName::Named(imported) => {
                        self.resolve_export(&target, imported, resolve_set)
                    }
                };
            }
        }
        if name == "default" {
            return Resolution::Null;
        }
        let mut star = Resolution::Null;
        for request in &record.info.star_exports {
            let target = record.resolutions[request].clone();
            match self.resolve_export(&target, name, resolve_set) {
                Resolution::Ambiguous => return Resolution::Ambiguous,
                Resolution::Null => {}
                found => match &star {
                    Resolution::Null => star = found,
                    existing if *existing == found => {}
                    _ => return Resolution::Ambiguous,
                },
            }
        }
        star
    }

    /// The modules `module` requests, in source order, each once.
    fn requested_modules(&self, module: &str) -> Vec<String> {
        self.modules.get(module).map_or_else(Vec::new, |record| {
            record
                .info
                .requests
                .iter()
                .filter_map(|request| record.resolutions.get(request).cloned())
                .collect()
        })
    }

    /// The modules that `InnerModuleEvaluation` visits for `module`, in order
    /// (ECMA-262 16.2.1.5.3.1 step 7): a deferred request contributes the
    /// asynchronous modules it transitively depends on, and an ordinary one the
    /// module itself.
    fn evaluation_list(&self, module: &str) -> Vec<String> {
        let Some(record) = self.modules.get(module) else {
            return Vec::new();
        };
        let mut list: Vec<String> = Vec::new();
        for (request, phase) in &record.info.request_phases {
            let Some(required) = record.resolutions.get(request) else {
                continue;
            };
            if *phase == ImportPhase::Defer {
                for additional in self.gather_asynchronous_dependencies(required, &mut Vec::new()) {
                    if !list.contains(&additional) {
                        list.push(additional);
                    }
                }
            } else if !list.contains(required) {
                list.push(required.clone());
            }
        }
        list
    }

    /// `GatherAsynchronousTransitiveDependencies` (ECMA-262 16.2.1.5.3.1): the
    /// modules with top-level `await` that `module` depends on, not reached through
    /// another such module, that are not yet evaluated.
    fn gather_asynchronous_dependencies(
        &self,
        module: &str,
        seen: &mut Vec<String>,
    ) -> Vec<String> {
        let mut result = Vec::new();
        if seen.iter().any(|member| member == module) {
            return result;
        }
        seen.push(module.to_owned());
        let Some(record) = self.modules.get(module) else {
            return result;
        };
        if matches!(record.status, Status::Evaluating | Status::Evaluated) {
            return result;
        }
        if record.has_tla {
            result.push(module.to_owned());
            return result;
        }
        for required in self.requested_modules(module) {
            for member in self.gather_asynchronous_dependencies(&required, seen) {
                if !result.contains(&member) {
                    result.push(member);
                }
            }
        }
        result
    }

    /// The cycle root of `module`, or `module` itself before it has one.
    fn cycle_root_of(&self, module: &str) -> String {
        self.modules
            .get(module)
            .and_then(|record| record.cycle_root.clone())
            .unwrap_or_else(|| module.to_owned())
    }

    /// `InnerModuleLinking` (ECMA-262 16.2.1.5.1.1).
    fn inner_module_linking(
        &mut self,
        key: &str,
        stack: &mut Vec<String>,
        mut index: usize,
    ) -> Result<usize, JsError> {
        let Some(status) = self.modules.get(key).map(|record| record.status) else {
            return Err(JsError::reference(format!(
                "module {key:?} is not declared"
            )));
        };
        if status != Status::Unlinked {
            return Ok(index);
        }
        if let Some(record) = self.modules.get_mut(key) {
            record.status = Status::Linking;
            record.dfs_index = index;
            record.dfs_ancestor_index = index;
        }
        index += 1;
        stack.push(key.to_owned());
        for required in self.requested_modules(key) {
            index = self.inner_module_linking(&required, stack, index)?;
            let dependency = self
                .modules
                .get(&required)
                .map(|record| (record.status, record.dfs_ancestor_index));
            if let Some((Status::Linking, ancestor)) = dependency
                && let Some(record) = self.modules.get_mut(key)
            {
                record.dfs_ancestor_index = record.dfs_ancestor_index.min(ancestor);
            }
        }
        self.initialize_environment(key)?;
        let is_root = self
            .modules
            .get(key)
            .is_some_and(|record| record.dfs_ancestor_index == record.dfs_index);
        if is_root {
            pop_component(stack, key, |member| {
                if let Some(record) = self.modules.get_mut(member) {
                    record.status = Status::Linked;
                }
            });
        }
        Ok(index)
    }

    /// `InitializeEnvironment` (ECMA-262 16.2.1.6.4): every indirect export and
    /// named import must resolve to one binding, and namespace imports are bound.
    fn initialize_environment(&mut self, key: &str) -> Result<(), JsError> {
        let (indirect, imports, environment) = {
            let record = &self.modules[key];
            (
                record.info.indirect_exports.clone(),
                record.info.imports.clone(),
                record.environment.clone(),
            )
        };
        for export in &indirect {
            match self.resolve_export(key, &export.exported, &mut Vec::new()) {
                Resolution::Null | Resolution::Ambiguous => {
                    return Err(JsError::syntax(
                        format!(
                            "export {:?} does not resolve to a unique binding",
                            export.exported
                        ),
                        0,
                    ));
                }
                Resolution::Namespace(target) => {
                    self.module_namespace(&target, false)?;
                }
                Resolution::Binding { .. } => {}
            }
        }
        for entry in &imports {
            let target = self.modules[key].resolutions[&entry.request].clone();
            if entry.phase == ImportPhase::Source {
                return Err(JsError::syntax(
                    format!("module {target:?} has no source phase import"),
                    0,
                ));
            }
            match &entry.imported {
                ImportName::Named(name) => {
                    match self.resolve_export(&target, name, &mut Vec::new()) {
                        Resolution::Null | Resolution::Ambiguous => {
                            return Err(JsError::syntax(
                                format!(
                                    "module {target:?} does not provide an export named {name:?}"
                                ),
                                0,
                            ));
                        }
                        // A namespace re-export is read through its namespace object, which must exist.
                        Resolution::Namespace(namespace) => {
                            self.module_namespace(&namespace, false)?;
                        }
                        Resolution::Binding { .. } => {}
                    }
                }
                ImportName::Namespace => {
                    let deferred = entry.phase == ImportPhase::Defer;
                    let namespace = self.module_namespace(&target, deferred)?;
                    if let Some(binding) = environment.borrow_mut().bindings.get_mut(&entry.local) {
                        binding.value = JsValue::Object(namespace);
                        binding.initialized = true;
                    }
                }
            }
        }
        Ok(())
    }

    /// Evaluate (ECMA-262 16.2.1.5.3.1): the promise of module `key`'s
    /// evaluation, started on the first call. A failure, synchronous or not,
    /// rejects it and is recorded on the module.
    fn module_evaluate(&mut self, dom: &mut Dom, key: &str) -> Result<usize, JsError> {
        let root = match self.modules.get(key).map(|record| record.status) {
            Some(Status::EvaluatingAsync | Status::Evaluated) => self.cycle_root_of(key),
            _ => key.to_owned(),
        };
        if let Some(promise) = self
            .modules
            .get(&root)
            .and_then(|record| record.top_level_capability)
        {
            return Ok(promise);
        }
        let (promise, _) = self.create_promise_record()?;
        if let Some(record) = self.modules.get_mut(&root) {
            record.top_level_capability = Some(promise);
        }
        let mut stack = Vec::new();
        if let Err(error) = self.inner_module_evaluation(dom, &root, &mut stack, 0) {
            for member in &stack {
                if let Some(record) = self.modules.get_mut(member) {
                    record.status = Status::Evaluated;
                    record.evaluation_error = Some(error.clone());
                }
            }
            let reason = self.error_value(&error);
            self.reject_promise(promise, &reason);
        } else {
            let asynchronous = self
                .modules
                .get(&root)
                .is_some_and(|record| record.async_evaluation);
            if !asynchronous {
                self.resolve_promise(promise, &JsValue::Undefined);
            }
        }
        Ok(promise)
    }

    /// `InnerModuleEvaluation` (ECMA-262 16.2.1.5.3.1).
    fn inner_module_evaluation(
        &mut self,
        dom: &mut Dom,
        key: &str,
        stack: &mut Vec<String>,
        mut index: usize,
    ) -> Result<usize, JsError> {
        let Some(record) = self.modules.get(key) else {
            return Err(JsError::reference(format!(
                "module {key:?} is not declared"
            )));
        };
        match record.status {
            Status::EvaluatingAsync | Status::Evaluated => {
                return match &record.evaluation_error {
                    Some(error) => Err(error.clone()),
                    None => Ok(index),
                };
            }
            Status::Evaluating => return Ok(index),
            Status::Unlinked | Status::Linking => {
                return Err(JsError::type_error(format!("module {key:?} is not linked")));
            }
            Status::Linked => {}
        }
        if let Some(record) = self.modules.get_mut(key) {
            record.status = Status::Evaluating;
            record.dfs_index = index;
            record.dfs_ancestor_index = index;
            record.pending_async_dependencies = 0;
        }
        index += 1;
        stack.push(key.to_owned());
        for required in self.evaluation_list(key) {
            index = self.inner_module_evaluation(dom, &required, stack, index)?;
            let Some((status, ancestor)) = self
                .modules
                .get(&required)
                .map(|record| (record.status, record.dfs_ancestor_index))
            else {
                continue;
            };
            let mut dependency = required.clone();
            if status == Status::Evaluating {
                if let Some(record) = self.modules.get_mut(key) {
                    record.dfs_ancestor_index = record.dfs_ancestor_index.min(ancestor);
                }
            } else {
                dependency = self.cycle_root_of(&required);
                if let Some(error) = self
                    .modules
                    .get(&dependency)
                    .and_then(|root| root.evaluation_error.clone())
                {
                    return Err(error);
                }
            }
            let asynchronous = self
                .modules
                .get(&dependency)
                .is_some_and(|record| record.async_evaluation);
            if asynchronous {
                if let Some(root) = self.modules.get_mut(&dependency) {
                    root.async_parent_modules.push(key.to_owned());
                }
                if let Some(record) = self.modules.get_mut(key) {
                    record.pending_async_dependencies += 1;
                }
            }
        }
        let (pending, has_tla) = self.modules.get(key).map_or((0, false), |record| {
            (record.pending_async_dependencies, record.has_tla)
        });
        if pending > 0 || has_tla {
            self.async_evaluation_count += 1;
            let order = self.async_evaluation_count;
            if let Some(record) = self.modules.get_mut(key) {
                record.async_evaluation = true;
                record.async_order = order;
            }
            if pending == 0 {
                self.execute_async_module(dom, key);
            }
        } else {
            self.execute_module(dom, key)?;
        }
        let is_root = self
            .modules
            .get(key)
            .is_some_and(|record| record.dfs_ancestor_index == record.dfs_index);
        if is_root {
            pop_component(stack, key, |member| {
                if let Some(record) = self.modules.get_mut(member) {
                    record.status = if record.async_evaluation {
                        Status::EvaluatingAsync
                    } else {
                        Status::Evaluated
                    };
                    record.cycle_root = Some(key.to_owned());
                }
            });
        }
        Ok(index)
    }

    /// `ReadyForSyncExecution` (import defer): whether `module` and its dependencies
    /// can be evaluated now without suspending.
    fn ready_for_sync_execution(&self, module: &str, seen: &mut Vec<String>) -> bool {
        if seen.iter().any(|member| member == module) {
            return true;
        }
        seen.push(module.to_owned());
        let Some(record) = self.modules.get(module) else {
            return false;
        };
        match record.status {
            Status::Evaluated => return true,
            Status::Evaluating | Status::EvaluatingAsync => return false,
            Status::Unlinked | Status::Linking | Status::Linked => {}
        }
        if record.has_tla {
            return false;
        }
        self.requested_modules(module)
            .iter()
            .all(|required| self.ready_for_sync_execution(required, seen))
    }

    /// `EnsureDeferredNamespaceEvaluation` (import defer): a string-keyed operation
    /// on a deferred namespace evaluates its module first. `key` is `None` for an
    /// operation on the object's keys, which always triggers. `"then"` never does,
    /// so a thenable check on a namespace does not run the module.
    pub(super) fn ensure_deferred_namespace(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: Option<&str>,
    ) -> Result<(), JsError> {
        if key == Some("then") {
            return Ok(());
        }
        let Some(module) = self.realm.deferred_namespace_module(object) else {
            return Ok(());
        };
        let status = self
            .modules
            .get(&module)
            .map(|record| (record.status, record.evaluation_error.clone()));
        match status {
            // An evaluated module answers with its evaluation error, which EvaluateSync throws.
            Some((Status::Evaluated, error)) => return error.map_or(Ok(()), Err),
            None => return Ok(()),
            Some(_) => {}
        }
        if !self.ready_for_sync_execution(&module, &mut Vec::new()) {
            return Err(JsError::type_error(format!(
                "cannot evaluate deferred module {module:?} synchronously: it or a dependency awaits"
            )));
        }
        // EvaluateSync: the evaluation is complete, so its promise is settled.
        let promise = self.module_evaluate(dom, &module)?;
        match self
            .promises
            .get(promise)
            .map(|record| record.state.clone())
        {
            Some(PromiseState::Rejected(reason)) => Err(JsError::thrown(reason)),
            _ => Ok(()),
        }
    }

    /// A read that begins at `start` and walks up the prototype chain (ECMA-262
    /// 10.1.8.1 OrdinaryGet): a deferred namespace the walk reaches before a
    /// property holder is triggered, as the read is handed to its [[Get]].
    pub(super) fn ensure_deferred_read(
        &mut self,
        dom: &mut Dom,
        start: Option<ObjectId>,
        key: &str,
    ) -> Result<(), JsError> {
        let mut candidate = start;
        while let Some(ancestor) = candidate {
            if self.realm.deferred_namespace_module(ancestor).is_some() {
                return self.ensure_deferred_namespace(dom, ancestor, Some(key));
            }
            if self.realm.own_property(ancestor, key).is_some() {
                return Ok(());
            }
            candidate = self.realm.get_prototype(ancestor);
        }
        Ok(())
    }

    /// `ExecuteModule` (ECMA-262 16.2.1.5.3.1 step 9) for a module without
    /// top-level `await`: run its body now.
    fn execute_module(&mut self, dom: &mut Dom, key: &str) -> Result<(), JsError> {
        let Some(record) = self.modules.get(key) else {
            return Err(JsError::reference(format!(
                "module {key:?} is not declared"
            )));
        };
        let environment = record.environment.clone();
        let statements = record.statements.clone();
        self.source_line_starts = build_line_starts(&record.source);
        let suspended = self.suspend_scopes();
        let saved = std::mem::replace(&mut self.environment, vec![Rc::clone(&environment)]);
        // Module code is strict (ECMA-262 11.2.2) and has no class frame of its own.
        let previous_root_strict = std::mem::replace(&mut self.root_strict, true);
        let completion = self.evaluate_statements(dom, &statements);
        self.root_strict = previous_root_strict;
        // The module's own `using` resources are disposed when its body is left.
        let completion = self.dispose_scope(dom, &environment, completion);
        self.environment = saved;
        self.resume_scopes(suspended);
        completion
            .map(|_| ())
            .map_err(|error| self.position_error(error))
    }

    /// `ExecuteAsyncModule` (ECMA-262 16.2.1.5.3.2): run the body of a module with
    /// top-level `await` as a coroutine. Its completion settles the module.
    fn execute_async_module(&mut self, dom: &mut Dom, key: &str) {
        let Ok((promise, _)) = self.create_promise_record() else {
            return;
        };
        let fulfilled = self.native_callable(ObjectHost::AsyncModuleStep {
            module: key.to_owned(),
            rejected: false,
        });
        let rejected = self.native_callable(ObjectHost::AsyncModuleStep {
            module: key.to_owned(),
            rejected: true,
        });
        let _ = self.perform_then(promise, Some(fulfilled), Some(rejected), None);
        let Some(record) = self.modules.get(key) else {
            return;
        };
        let environment = record.environment.clone();
        let statements = record.statements.clone();
        self.source_line_starts = build_line_starts(&record.source);
        match Compiler::compile(&statements, false) {
            Ok(code) => self.start_module_coroutine(dom, Rc::new(code), environment, promise, key),
            Err(error) => self.async_module_execution_rejected(key, &error),
        }
    }

    /// `AsyncModuleExecutionFulfilled` (ECMA-262 16.2.1.5.3.3).
    fn async_module_execution_fulfilled(&mut self, dom: &mut Dom, key: &str) {
        let Some(record) = self.modules.get_mut(key) else {
            return;
        };
        if record.status == Status::Evaluated {
            return;
        }
        record.async_evaluation = false;
        record.status = Status::Evaluated;
        let capability = record.top_level_capability;
        if let Some(promise) = capability {
            self.resolve_promise(promise, &JsValue::Undefined);
        }
        let mut exec_list = Vec::new();
        self.gather_available_ancestors(key, &mut exec_list);
        exec_list.sort_by_key(|member| {
            self.modules
                .get(member)
                .map_or(0, |record| record.async_order)
        });
        for member in exec_list {
            let Some((status, has_tla)) = self
                .modules
                .get(&member)
                .map(|record| (record.status, record.has_tla))
            else {
                continue;
            };
            if status == Status::Evaluated {
                continue;
            }
            if has_tla {
                self.execute_async_module(dom, &member);
                continue;
            }
            if let Err(error) = self.execute_module(dom, &member) {
                self.async_module_execution_rejected(&member, &error);
            } else {
                let capability = self.modules.get_mut(&member).and_then(|record| {
                    record.async_evaluation = false;
                    record.status = Status::Evaluated;
                    record.top_level_capability
                });
                if let Some(promise) = capability {
                    self.resolve_promise(promise, &JsValue::Undefined);
                }
            }
        }
    }

    /// `AsyncModuleExecutionRejected` (ECMA-262 16.2.1.5.3.4).
    fn async_module_execution_rejected(&mut self, key: &str, error: &JsError) {
        let Some(record) = self.modules.get_mut(key) else {
            return;
        };
        if record.status == Status::Evaluated {
            return;
        }
        record.evaluation_error = Some(error.clone());
        record.status = Status::Evaluated;
        let parents = record.async_parent_modules.clone();
        let capability = record.top_level_capability;
        for parent in parents {
            self.async_module_execution_rejected(&parent, error);
        }
        if let Some(promise) = capability {
            let reason = self.error_value(error);
            self.reject_promise(promise, &reason);
        }
    }

    /// `GatherAvailableAncestors` (ECMA-262 16.2.1.5.3.3): the asynchronously
    /// evaluating modules that `module` was the last pending dependency of.
    fn gather_available_ancestors(&mut self, module: &str, exec_list: &mut Vec<String>) {
        let parents = self
            .modules
            .get(module)
            .map_or_else(Vec::new, |record| record.async_parent_modules.clone());
        for parent in parents {
            let root = self.cycle_root_of(&parent);
            let errored = self
                .modules
                .get(&root)
                .is_some_and(|record| record.evaluation_error.is_some());
            if exec_list.contains(&parent) || errored {
                continue;
            }
            let Some(record) = self.modules.get_mut(&parent) else {
                continue;
            };
            record.pending_async_dependencies = record.pending_async_dependencies.saturating_sub(1);
            if record.pending_async_dependencies == 0 {
                let has_tla = record.has_tla;
                exec_list.push(parent.clone());
                if !has_tla {
                    self.gather_available_ancestors(&parent, exec_list);
                }
            }
        }
    }

    /// The reaction that completes a module's asynchronous evaluation: its body
    /// settled with `argument`, either fulfilled or rejected.
    pub(super) fn async_module_step(
        &mut self,
        dom: &mut Dom,
        module: &str,
        rejected: bool,
        argument: &JsValue,
    ) {
        if rejected {
            let error = JsError::thrown(argument.clone());
            self.async_module_execution_rejected(module, &error);
        } else {
            self.async_module_execution_fulfilled(dom, module);
        }
    }

    /// A built-in callable whose behaviour is `host`, such as a promise reaction
    /// handler that module code creates.
    fn native_callable(&mut self, host: ObjectHost) -> ObjectId {
        let prototype = self.realm.function_prototype();
        self.realm.allocate_host(Some(prototype), host)
    }

    /// Read an imported binding through to the exporter's current value.
    pub(super) fn read_import(&self, import: &ImportRef) -> Result<JsValue, JsError> {
        match self.resolve_export(&import.module, &import.name, &mut Vec::new()) {
            Resolution::Binding { module, local } => {
                read_binding(&self.modules[&module].environment, &local)
            }
            Resolution::Namespace(target) => self
                .modules
                .get(&target)
                .and_then(|record| record.namespace)
                .map(JsValue::Object)
                .ok_or_else(|| JsError::reference("module namespace is not linked yet")),
            Resolution::Null | Resolution::Ambiguous => Err(JsError::syntax(
                format!(
                    "module {:?} does not provide an export named {:?}",
                    import.module, import.name
                ),
                0,
            )),
        }
    }
}

/// Pop the strongly connected component whose root is `root` off the Tarjan
/// stack, calling `visit` for each member (ECMA-262 16.2.1.5.1.1 step 7).
fn pop_component(stack: &mut Vec<String>, root: &str, mut visit: impl FnMut(&str)) {
    while let Some(member) = stack.pop() {
        visit(&member);
        if member == root {
            break;
        }
    }
}

/// The value of an initialized module binding. An uninitialized one raises the
/// `ReferenceError` of its temporal dead zone.
pub(super) fn read_binding(environment: &Environment, local: &str) -> Result<JsValue, JsError> {
    let environment = environment.borrow();
    match environment.binding_value(local) {
        BindingRead::Value(value) => Ok(value),
        BindingRead::Uninitialized => Err(JsError::reference(format!(
            "cannot access {local} before initialization"
        ))),
        BindingRead::Absent => Err(JsError::reference(format!("{local} is not defined"))),
    }
}
