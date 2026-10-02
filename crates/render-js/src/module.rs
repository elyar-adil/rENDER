//! Static module metadata produced by the parser (ECMA-262 §16.2).
//!
//! The module body is lowered to ordinary statements; what cannot be lowered —
//! the import and export tables — lives here, and the runtime's module linker
//! consumes it to build live bindings.

/// What an import or re-export asks of the target module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImportName {
    /// `import x from` / `export { default as y } from`.
    Named(String),
    /// `import * as ns from` / `export * as ns from`.
    Namespace,
}

/// One local binding introduced by an `import` declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImportEntry {
    pub(crate) request: String,
    pub(crate) imported: ImportName,
    pub(crate) local: String,
}

/// `export { a as b } from "m"` and `export * as b from "m"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndirectExport {
    pub(crate) exported: String,
    pub(crate) request: String,
    pub(crate) imported: ImportName,
}

/// Import and export tables of one module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModuleInfo {
    /// Module specifiers in source order, without duplicates.
    pub(crate) requests: Vec<String>,
    pub(crate) imports: Vec<ImportEntry>,
    /// `(exported name, local binding name)`.
    pub(crate) local_exports: Vec<(String, String)>,
    pub(crate) indirect_exports: Vec<IndirectExport>,
    /// Specifiers of `export * from`.
    pub(crate) star_exports: Vec<String>,
}

impl ModuleInfo {
    pub(crate) fn add_request(&mut self, specifier: &str) {
        if !self.requests.iter().any(|existing| existing == specifier) {
            self.requests.push(specifier.to_owned());
        }
    }
}

/// Binding name that `export default <expression>` is stored under.
pub(crate) const DEFAULT_BINDING: &str = "*default*";
/// Binding name that `import.meta` is lowered to.
pub(crate) const IMPORT_META_BINDING: &str = "%import.meta";
