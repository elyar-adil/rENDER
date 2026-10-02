//! Module graph assembly for `<script type="module">` (HTML §8.1.5.3).
//!
//! Like the rest of script handling this performs no network I/O: the
//! embedding asks the graph which URLs it still needs ([`ModuleGraph::take_pending`]),
//! fetches them however it likes, and hands the bytes back through
//! [`ModuleGraph::complete`]. Repeat until nothing is pending, then
//! [`ModuleGraph::entry`] yields the units the page declares and evaluates.
//!
//! Only URL-like specifiers (absolute, `/`, `./`, `../`) resolve. A bare
//! specifier needs an import map, which is not implemented, so it is reported
//! as a diagnostic and left unresolved.

use std::collections::{BTreeMap, BTreeSet};

use url::Url;

use crate::js::{CompiledScript, RuntimeLimits};

/// Upper bound on distinct modules one graph will accept.
pub const MAX_GRAPH_MODULES: usize = 2_048;

#[derive(Clone, Debug, PartialEq)]
struct GraphModule {
    compiled: CompiledScript,
    base: Url,
    /// Specifier as written -> key of the module it resolves to.
    resolutions: BTreeMap<String, String>,
}

/// One module ready to be declared in the page's JavaScript realm.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleUnit {
    pub key: String,
    pub compiled: CompiledScript,
    pub resolutions: BTreeMap<String, String>,
}

/// An entry module plus every module reachable from it that the graph holds.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleEntry {
    pub key: String,
    pub units: Vec<ModuleUnit>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModuleGraph {
    modules: BTreeMap<String, GraphModule>,
    requested: BTreeSet<String>,
    diagnostics: Vec<String>,
}

/// The key a fetched module is stored under: its URL without a fragment.
#[must_use]
pub fn module_key(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.to_string()
}

impl ModuleGraph {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an already-compiled module. `base` is what its specifiers
    /// resolve against; `key` identifies it in the realm.
    pub fn add(&mut self, key: &str, base: Url, compiled: CompiledScript) {
        let mut fetched = base.clone();
        fetched.set_fragment(None);
        self.requested.insert(module_key(&fetched));
        self.modules.insert(
            key.to_owned(),
            GraphModule {
                compiled,
                base,
                resolutions: BTreeMap::new(),
            },
        );
    }

    /// Resolve the specifiers of every module and return the URLs that have
    /// not been requested yet. Each URL is returned once.
    pub fn take_pending(&mut self) -> Vec<Url> {
        let mut pending = Vec::new();
        let keys: Vec<String> = self.modules.keys().cloned().collect();
        for key in keys {
            let requests: Vec<String> = self.modules[&key].compiled.module_requests().to_vec();
            let base = self.modules[&key].base.clone();
            for request in requests {
                if self.modules[&key].resolutions.contains_key(&request) {
                    continue;
                }
                let Some(target) = resolve_specifier(&base, &request) else {
                    self.diagnostics.push(format!(
                        "module specifier {request:?} in {key} is not a URL; import maps are not implemented"
                    ));
                    continue;
                };
                let target_key = module_key(&target);
                if let Some(module) = self.modules.get_mut(&key) {
                    module.resolutions.insert(request, target_key.clone());
                }
                if self.requested.len() >= MAX_GRAPH_MODULES {
                    self.diagnostics.push(format!(
                        "module graph exceeds {MAX_GRAPH_MODULES} modules; {target_key} was not requested"
                    ));
                } else if self.requested.insert(target_key) {
                    pending.push(target);
                }
            }
        }
        pending
    }

    /// Record the outcome of fetching `requested`: the final URL and decoded
    /// source, or a failure description.
    pub fn complete(
        &mut self,
        requested: &Url,
        outcome: Result<(Url, String), String>,
        limits: &RuntimeLimits,
    ) {
        let key = module_key(requested);
        match outcome {
            Ok((final_url, source)) => match CompiledScript::compile_module(&source, limits) {
                Ok(compiled) => {
                    self.modules.insert(
                        key,
                        GraphModule {
                            compiled,
                            base: final_url,
                            resolutions: BTreeMap::new(),
                        },
                    );
                }
                Err(error) => self
                    .diagnostics
                    .push(format!("module {key} failed to compile: {error}")),
            },
            Err(message) => self
                .diagnostics
                .push(format!("module {key} failed to load: {message}")),
        }
    }

    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.modules.contains_key(key)
    }

    /// Drain the problems found so far.
    pub fn take_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.diagnostics)
    }

    /// The entry module and everything reachable from it.
    #[must_use]
    pub fn entry(&self, key: &str) -> Option<ModuleEntry> {
        self.modules.get(key)?;
        let mut units = Vec::new();
        let mut seen = BTreeSet::new();
        let mut stack = vec![key.to_owned()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            let Some(module) = self.modules.get(&current) else {
                continue;
            };
            stack.extend(module.resolutions.values().cloned());
            units.push(ModuleUnit {
                key: current,
                compiled: module.compiled.clone(),
                resolutions: module.resolutions.clone(),
            });
        }
        Some(ModuleEntry {
            key: key.to_owned(),
            units,
        })
    }
}

fn resolve_specifier(base: &Url, specifier: &str) -> Option<Url> {
    if let Ok(absolute) = Url::parse(specifier) {
        return Some(absolute);
    }
    if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        return base.join(specifier).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(source: &str) -> CompiledScript {
        CompiledScript::compile_module(source, &RuntimeLimits::default()).expect("compiles")
    }

    fn url(text: &str) -> Url {
        Url::parse(text).expect("url")
    }

    #[test]
    fn pending_urls_follow_relative_specifiers_and_are_requested_once() {
        let mut graph = ModuleGraph::new();
        graph.add(
            "https://a.test/app/main.html#module-0",
            url("https://a.test/app/main.html"),
            compile(
                "import './x.js'; import '../y.js'; import './x.js'; import 'https://b.test/z.js';",
            ),
        );
        let pending = graph.take_pending();
        assert_eq!(
            pending.iter().map(Url::as_str).collect::<Vec<_>>(),
            [
                "https://a.test/app/x.js",
                "https://a.test/y.js",
                "https://b.test/z.js"
            ]
        );
        assert!(graph.take_pending().is_empty(), "nothing is re-requested");
    }

    #[test]
    fn completed_modules_contribute_their_own_requests() {
        let mut graph = ModuleGraph::new();
        graph.add("main", url("https://a.test/"), compile("import './x.js';"));
        let pending = graph.take_pending();
        graph.complete(
            &pending[0],
            Ok((
                pending[0].clone(),
                "import './deep/y.js'; export const x = 1;".to_owned(),
            )),
            &RuntimeLimits::default(),
        );
        let next = graph.take_pending();
        assert_eq!(next[0].as_str(), "https://a.test/deep/y.js");
    }

    #[test]
    fn redirected_modules_resolve_relative_to_the_final_url() {
        let mut graph = ModuleGraph::new();
        graph.add("main", url("https://a.test/"), compile("import './x.js';"));
        let pending = graph.take_pending();
        graph.complete(
            &pending[0],
            Ok((
                url("https://cdn.test/v2/x.js"),
                "import './y.js';".to_owned(),
            )),
            &RuntimeLimits::default(),
        );
        assert_eq!(graph.take_pending()[0].as_str(), "https://cdn.test/v2/y.js");
    }

    #[test]
    fn bare_specifiers_are_diagnosed_not_fetched() {
        let mut graph = ModuleGraph::new();
        graph.add("main", url("https://a.test/"), compile("import 'react';"));
        assert!(graph.take_pending().is_empty());
        let diagnostics = graph.take_diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("react"));
    }

    #[test]
    fn failures_and_compile_errors_are_diagnosed() {
        let mut graph = ModuleGraph::new();
        graph.add(
            "main",
            url("https://a.test/"),
            compile("import './a.js'; import './b.js';"),
        );
        let pending = graph.take_pending();
        graph.complete(
            &pending[0],
            Err("HTTP 404".to_owned()),
            &RuntimeLimits::default(),
        );
        graph.complete(
            &pending[1],
            Ok((pending[1].clone(), "export const = ;".to_owned())),
            &RuntimeLimits::default(),
        );
        let diagnostics = graph.take_diagnostics();
        assert_eq!(diagnostics.len(), 2);
        assert!(!graph.contains("https://a.test/a.js"));
    }

    #[test]
    fn entry_contains_only_reachable_modules_and_survives_cycles() {
        let mut graph = ModuleGraph::new();
        graph.add("main", url("https://a.test/"), compile("import './x.js';"));
        let pending = graph.take_pending();
        graph.complete(
            &pending[0],
            Ok((pending[0].clone(), "import './main-again.js';".to_owned())),
            &RuntimeLimits::default(),
        );
        let _ = graph.take_pending();
        graph.add("unrelated", url("https://a.test/"), compile(""));
        let entry = graph.entry("main").expect("entry");
        let mut keys: Vec<_> = entry.units.iter().map(|unit| unit.key.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["https://a.test/x.js", "main"]);
    }
}
