//! Module linking and evaluation (ECMA-262 §16.2.1).
//!
//! A module owns one environment. Import bindings are not copies: the
//! importing environment holds an [`ImportRef`] and every read resolves it to
//! the exporter's binding cell, which is what makes bindings live and lets
//! cyclic graphs work for hoisted declarations.
//!
//! The host drives loading: it fetches every module in the graph, calls
//! [`JsRuntime::declare_module`] for each with a specifier-to-key map, and then
//! [`JsRuntime::evaluate_module`] on the entry. The runtime orders evaluation
//! (dependencies first, each module once).
//!
//! Deviations, deliberate and recorded: module code runs in sloppy mode;
//! top-level `await` is not supported; a namespace object is a snapshot taken
//! when the importer links, not a live exotic object.

use super::build_line_starts;
use super::types::{Binding, Environment, EnvironmentRecord};
use crate::module::{DEFAULT_BINDING, IMPORT_META_BINDING, ImportName, ModuleInfo};
use crate::parser::{Statement, VariableKind};
use crate::{CompiledScript, JsError, JsRuntime, JsValue, ObjectId, ScriptOutcome};
use render_dom::Dom;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// Where an imported name lives: an export of another declared module.
#[derive(Clone, Debug)]
pub(super) struct ImportRef {
    pub(super) module: String,
    pub(super) name: String,
}

#[derive(Clone, Debug)]
enum ModuleState {
    Declared,
    Evaluating,
    Evaluated,
    Failed(JsError),
}

#[derive(Debug)]
pub(super) struct ModuleRecord {
    pub(super) environment: Environment,
    info: ModuleInfo,
    statements: Rc<Vec<Statement>>,
    source: Rc<str>,
    /// Module specifier as written -> key of the module it resolves to.
    resolutions: BTreeMap<String, String>,
    state: ModuleState,
    namespace: Option<ObjectId>,
}

/// What an export name resolves to.
enum Resolved {
    Cell(Environment, String),
    Namespace(String),
}

impl JsRuntime {
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
        self.realm
            .set_property(meta, "url".to_owned(), JsValue::String(key.to_owned()));
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
                    // Filled in when the module links, after its dependencies ran.
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
        self.modules.insert(
            key.to_owned(),
            ModuleRecord {
                environment: environment.clone(),
                info,
                statements: statements.clone(),
                source: Rc::from(script.source()),
                resolutions,
                state: ModuleState::Declared,
                namespace: None,
            },
        );
        let saved = std::mem::replace(&mut self.environment, vec![environment]);
        let result = self.instantiate_statements(&statements);
        self.environment = saved;
        result
    }

    /// Whether a module with this key has been declared.
    #[must_use]
    pub fn has_module(&self, key: &str) -> bool {
        self.modules.contains_key(key)
    }

    /// Evaluate a declared module and, first, everything it depends on. Each
    /// module body runs at most once; a failure is remembered and re-thrown to
    /// every later importer.
    ///
    /// # Errors
    ///
    /// Returns the link error or the first uncaught error of the graph.
    pub fn evaluate_module(&mut self, dom: &mut Dom, key: &str) -> Result<ScriptOutcome, JsError> {
        self.ensure_prelude(dom);
        let from_revision = dom.revision();
        self.transient_roots.clear();
        self.collect_garbage();
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        let saved = std::mem::take(&mut self.environment);
        let result = self.evaluate_module_graph(dom, key);
        self.environment = saved;
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

    fn evaluate_module_graph(&mut self, dom: &mut Dom, key: &str) -> Result<(), JsError> {
        let Some(record) = self.modules.get_mut(key) else {
            return Err(JsError::reference(format!(
                "module {key:?} is not declared"
            )));
        };
        match &record.state {
            ModuleState::Evaluated | ModuleState::Evaluating => return Ok(()),
            ModuleState::Failed(error) => return Err(error.clone()),
            ModuleState::Declared => record.state = ModuleState::Evaluating,
        }
        let outcome = self.link_and_run(dom, key);
        let state = match &outcome {
            Ok(()) => ModuleState::Evaluated,
            Err(error) => ModuleState::Failed(error.clone()),
        };
        if let Some(record) = self.modules.get_mut(key) {
            record.state = state;
        }
        outcome
    }

    fn link_and_run(&mut self, dom: &mut Dom, key: &str) -> Result<(), JsError> {
        let record = &self.modules[key];
        let dependencies: Vec<String> = record
            .info
            .requests
            .iter()
            .map(|request| record.resolutions[request].clone())
            .collect();
        let imports = record.info.imports.clone();
        let environment = record.environment.clone();
        let statements = record.statements.clone();
        let source = record.source.clone();
        for dependency in &dependencies {
            self.evaluate_module_graph(dom, dependency)?;
        }
        for entry in &imports {
            let target = self.modules[key].resolutions[&entry.request].clone();
            match &entry.imported {
                ImportName::Namespace => {
                    let namespace = self.module_namespace(&target)?;
                    let mut environment = environment.borrow_mut();
                    if let Some(binding) = environment.bindings.get_mut(&entry.local) {
                        binding.value = JsValue::Object(namespace);
                        binding.initialized = true;
                    }
                }
                ImportName::Named(name) => match self.resolve_export(&target, name)? {
                    None => {
                        return Err(JsError::syntax(
                            format!("module {target:?} does not provide an export named {name:?}"),
                            0,
                        ));
                    }
                    Some(Resolved::Namespace(namespace_key)) => {
                        self.module_namespace(&namespace_key)?;
                    }
                    Some(Resolved::Cell(..)) => {}
                },
            }
        }
        self.source_line_starts = build_line_starts(&source);
        let saved = std::mem::replace(&mut self.environment, vec![environment]);
        let completion = self.evaluate_statements(dom, &statements);
        self.environment = saved;
        completion
            .map(|_| ())
            .map_err(|error| self.position_error(error))
    }

    /// Follow `name` through local, indirect and star exports of `module`.
    /// `Ok(None)` means the module does not export it.
    fn resolve_export(&self, module: &str, name: &str) -> Result<Option<Resolved>, JsError> {
        self.resolve_export_guarded(module, name, &mut BTreeSet::new())
    }

    fn resolve_export_guarded(
        &self,
        module: &str,
        name: &str,
        seen: &mut BTreeSet<(String, String)>,
    ) -> Result<Option<Resolved>, JsError> {
        if !seen.insert((module.to_owned(), name.to_owned())) {
            return Ok(None);
        }
        let record = self
            .modules
            .get(module)
            .ok_or_else(|| JsError::reference(format!("module {module:?} is not declared")))?;
        for (exported, local) in &record.info.local_exports {
            if exported == name {
                let import = record.environment.borrow().imports.get(local).cloned();
                return match import {
                    Some(import) => self.resolve_export_guarded(&import.module, &import.name, seen),
                    None => Ok(Some(Resolved::Cell(
                        record.environment.clone(),
                        local.clone(),
                    ))),
                };
            }
        }
        for indirect in &record.info.indirect_exports {
            if indirect.exported == name {
                let target = record.resolutions[&indirect.request].as_str();
                return match &indirect.imported {
                    ImportName::Namespace => Ok(Some(Resolved::Namespace(target.to_owned()))),
                    ImportName::Named(imported) => {
                        self.resolve_export_guarded(target, imported, seen)
                    }
                };
            }
        }
        if name != "default" {
            for request in &record.info.star_exports {
                let target = record.resolutions[request].as_str();
                if let Some(found) = self.resolve_export_guarded(target, name, seen)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }

    fn export_names(
        &self,
        module: &str,
        seen: &mut BTreeSet<String>,
        names: &mut BTreeSet<String>,
    ) {
        if !seen.insert(module.to_owned()) {
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
            self.export_names(&record.resolutions[request], seen, &mut inherited);
            inherited.remove("default");
            names.extend(inherited);
        }
    }

    /// The namespace object of a module (§10.4.6) as a snapshot of its
    /// exports, cached once the module has finished evaluating.
    fn module_namespace(&mut self, module: &str) -> Result<ObjectId, JsError> {
        if let Some(namespace) = self.modules.get(module).and_then(|record| record.namespace) {
            return Ok(namespace);
        }
        let mut names = BTreeSet::new();
        self.export_names(module, &mut BTreeSet::new(), &mut names);
        let mut properties = Vec::new();
        for name in names {
            let value = match self.resolve_export(module, &name)? {
                Some(resolved) => self.read_resolved(&resolved).unwrap_or(JsValue::Undefined),
                None => continue,
            };
            properties.push((name, value));
        }
        let namespace = self.realm.create_object(None);
        for (name, value) in properties {
            self.realm.set_property(namespace, name, value);
        }
        if matches!(
            self.modules.get(module).map(|record| &record.state),
            Some(ModuleState::Evaluated)
        ) && let Some(record) = self.modules.get_mut(module)
        {
            record.namespace = Some(namespace);
        }
        Ok(namespace)
    }

    fn read_resolved(&self, resolved: &Resolved) -> Result<JsValue, JsError> {
        match resolved {
            Resolved::Cell(environment, local) => {
                let environment = environment.borrow();
                match environment.bindings.get(local) {
                    Some(binding) if binding.initialized => Ok(binding.value.clone()),
                    Some(_) => Err(JsError::reference(format!(
                        "cannot access {local} before initialization"
                    ))),
                    None => Err(JsError::reference(format!("{local} is not defined"))),
                }
            }
            Resolved::Namespace(key) => self
                .modules
                .get(key)
                .and_then(|record| record.namespace)
                .map(JsValue::Object)
                .ok_or_else(|| JsError::reference("module namespace is not linked yet")),
        }
    }

    /// Read an imported binding through to the exporter's current value.
    pub(super) fn read_import(&self, import: &ImportRef) -> Result<JsValue, JsError> {
        match self.resolve_export(&import.module, &import.name)? {
            Some(resolved) => self.read_resolved(&resolved),
            None => Err(JsError::syntax(
                format!(
                    "module {:?} does not provide an export named {:?}",
                    import.module, import.name
                ),
                0,
            )),
        }
    }
}

/// `export default <expr>` is stored here; exposed for the evaluator's tests.
#[allow(dead_code)]
pub(super) const DEFAULT_EXPORT_BINDING: &str = DEFAULT_BINDING;
