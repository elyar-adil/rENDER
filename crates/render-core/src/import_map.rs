//! HTML import maps (HTML §8.1.5): parsing and module-specifier resolution.
//!
//! An import map is one JSON document that rewrites bare or URL-like module
//! specifiers before the module graph fetches them. Only `imports` and
//! `scopes` participate: `integrity` metadata is ignored because no fetch
//! integrity check consumes it here. Invalid JSON, a non-object value, or a
//! non-string mapping rejects the whole map, matching the standard's
//! "report an error and do not apply it" rule.

use std::collections::BTreeMap;

use serde_json::Value;
use url::Url;

/// One scope's import map, keyed by the scope URL.
pub type ScopeImports = BTreeMap<String, Option<String>>;

/// A parsed, URL-normalised import map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportMap {
    imports: ScopeImports,
    scopes: BTreeMap<String, ScopeImports>,
}

/// Why a specifier did not resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMapMiss {
    /// No mapping matched and the specifier is not URL-like.
    Unmapped,
    /// A mapping deliberately blocked the specifier with `null`.
    Blocked,
}

impl ImportMap {
    /// Whether the map contains no imports and no scopes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.imports.is_empty() && self.scopes.is_empty()
    }

    /// Parse `json` against the document's base URL.
    ///
    /// # Errors
    ///
    /// Returns a human-readable parse or normalisation error. The caller is
    /// expected to report it and leave the document without an import map.
    pub fn parse(json: &str, base: &Url) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(json).map_err(|error| format!("import map JSON: {error}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| "import map is not a JSON object".to_owned())?;
        let imports = parse_imports(object.get("imports"), base)?;
        let mut scopes = BTreeMap::new();
        if let Some(scope_value) = object.get("scopes") {
            let scope_object = scope_value
                .as_object()
                .ok_or_else(|| "import map scopes is not an object".to_owned())?;
            for (scope, entries) in scope_object {
                let Some(scope_url) = normalize_url_like(scope, base) else {
                    return Err(format!("import map scope {scope:?} is not a URL"));
                };
                scopes.insert(scope_url, parse_imports(Some(entries), base)?);
            }
        }
        Ok(Self { imports, scopes })
    }

    /// Resolve `specifier` as imported from `referrer`.
    ///
    /// Exact matches win, then the longest key ending in `/`, then the
    /// URL-like fallback. A `null` mapping is a deliberate block, not a miss.
    ///
    /// # Errors
    ///
    /// Returns [`ImportMapMiss::Blocked`] when the map maps the specifier to
    /// `null`, and [`ImportMapMiss::Unmapped`] when no entry matches and the
    /// specifier is not URL-like.
    pub fn resolve(&self, referrer: &Url, specifier: &str) -> Result<Url, ImportMapMiss> {
        let normalized = normalize_specifier(specifier, referrer);
        let scope = self
            .scopes
            .iter()
            .filter(|(scope, _)| referrer.as_str().starts_with(scope.as_str()))
            .max_by_key(|(scope, _)| scope.len())
            .map(|(_, imports)| imports);
        if let Some(imports) = scope
            && let Some(result) = resolve_in(imports, &normalized)
        {
            return result;
        }
        if let Some(result) = resolve_in(&self.imports, &normalized) {
            return result;
        }
        if is_url_like(specifier) {
            return referrer
                .join(specifier)
                .map_err(|_| ImportMapMiss::Unmapped);
        }
        Err(ImportMapMiss::Unmapped)
    }
}

fn parse_imports(value: Option<&Value>, base: &Url) -> Result<ScopeImports, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "import map imports is not an object".to_owned())?;
    let mut imports = BTreeMap::new();
    for (key, target) in object {
        let key = normalize_key(key, base);
        let target = match target {
            Value::Null => None,
            Value::String(target) => {
                let url = base
                    .join(target)
                    .map_err(|error| format!("import map target {target:?}: {error}"))?;
                Some(normalize_url(url))
            }
            _ => return Err(format!("import map target for {key:?} is not a string")),
        };
        imports.insert(key, target);
    }
    Ok(imports)
}

fn resolve_in(imports: &ScopeImports, specifier: &str) -> Option<Result<Url, ImportMapMiss>> {
    if let Some(target) = imports.get(specifier) {
        return Some(match target {
            Some(url) => Url::parse(url).map_err(|_| ImportMapMiss::Unmapped),
            None => Err(ImportMapMiss::Blocked),
        });
    }
    let mut prefix = None;
    for (key, target) in imports {
        if key.ends_with('/')
            && specifier.starts_with(key)
            && prefix.is_none_or(|(current, _): (&String, _)| key.len() > current.len())
        {
            prefix = Some((key, target));
        }
    }
    let (key, target) = prefix?;
    Some(match target {
        Some(url) => {
            let remainder = &specifier[key.len()..];
            Url::parse(&format!("{url}{remainder}")).map_err(|_| ImportMapMiss::Unmapped)
        }
        None => Err(ImportMapMiss::Blocked),
    })
}

fn normalize_key(key: &str, base: &Url) -> String {
    normalize_url_like(key, base).unwrap_or_else(|| key.to_owned())
}

fn normalize_specifier(specifier: &str, base: &Url) -> String {
    normalize_url_like(specifier, base).unwrap_or_else(|| specifier.to_owned())
}

fn normalize_url_like(value: &str, base: &Url) -> Option<String> {
    if is_url_like(value) {
        base.join(value).ok().map(normalize_url)
    } else {
        None
    }
}

fn normalize_url(mut url: Url) -> String {
    url.set_fragment(None);
    url.to_string()
}

fn is_url_like(specifier: &str) -> bool {
    Url::parse(specifier).is_ok()
        || specifier.starts_with('/')
        || specifier.starts_with("./")
        || specifier.starts_with("../")
}

#[cfg(test)]
mod tests {
    use super::{ImportMap, ImportMapMiss};

    fn url(text: &str) -> url::Url {
        url::Url::parse(text).expect("url")
    }

    #[test]
    fn exact_and_prefix_mappings_resolve_against_the_document_base() {
        let map = ImportMap::parse(
            r#"{
                "imports": {
                    "widgets": "/bundles/widget.js",
                    "pkg/": "./vendor/pkg/",
                    "blocked": null
                }
            }"#,
            &url("https://example.test/app/page.html"),
        )
        .expect("map parses");
        assert_eq!(
            map.resolve(&url("https://example.test/app/x.js"), "widgets")
                .expect("exact")
                .as_str(),
            "https://example.test/bundles/widget.js"
        );
        assert_eq!(
            map.resolve(&url("https://example.test/app/x.js"), "pkg/a/b.js")
                .expect("prefix")
                .as_str(),
            "https://example.test/app/vendor/pkg/a/b.js"
        );
        assert_eq!(
            map.resolve(&url("https://example.test/app/x.js"), "blocked"),
            Err(ImportMapMiss::Blocked)
        );
        assert_eq!(
            map.resolve(&url("https://example.test/app/x.js"), "bare"),
            Err(ImportMapMiss::Unmapped)
        );
    }

    #[test]
    fn the_most_specific_scope_overrides_top_level_imports() {
        let map = ImportMap::parse(
            r#"{
                "imports": { "x": "/top.js" },
                "scopes": {
                    "/app/": { "x": "/app.js" },
                    "/app/deep/": { "x": "/deep.js" }
                }
            }"#,
            &url("https://example.test/page.html"),
        )
        .expect("map parses");
        let resolve = |referrer: &str| {
            map.resolve(&url(referrer), "x")
                .expect("scope mapping")
                .to_string()
        };
        assert_eq!(
            resolve("https://example.test/app/deep/a.js"),
            "https://example.test/deep.js"
        );
        assert_eq!(
            resolve("https://example.test/app/a.js"),
            "https://example.test/app.js"
        );
        assert_eq!(
            resolve("https://example.test/other/a.js"),
            "https://example.test/top.js"
        );
    }

    #[test]
    fn malformed_maps_are_rejected_as_a_whole() {
        assert!(ImportMap::parse("[]", &url("https://example.test/")).is_err());
        assert!(
            ImportMap::parse(r#"{"imports":{"x":42}}"#, &url("https://example.test/")).is_err()
        );
        assert!(
            ImportMap::parse(
                r#"{"imports":{"./x.js":"/x.js"}}"#,
                &url("https://example.test/")
            )
            .is_ok()
        );
    }
}
