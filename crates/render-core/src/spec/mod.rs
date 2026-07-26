//! Machine-readable conformance inventory for the Web engine.
//!
//! The registry gives specifications, implementation work, diagnostics, and
//! conformance tests one stable identity. It intentionally does not generate
//! complex algorithms from standards prose; it records which hand-written or
//! generated implementation is responsible for each observable feature.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

mod registry;

pub use registry::CURRENT_FEATURES;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeatureId(&'static str);

impl FeatureId {
    #[must_use]
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for FeatureId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StandardFamily {
    Html,
    Dom,
    Css,
    EcmaScript,
    WebIdl,
    Fetch,
    Url,
    Infra,
    Rendering,
    BrowserUi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SupportStatus {
    Missing,
    Partial,
    Conformant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConformanceTest {
    pub suite: &'static str,
    pub path: &'static str,
}

impl ConformanceTest {
    #[must_use]
    pub const fn new(suite: &'static str, path: &'static str) -> Self {
        Self { suite, path }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureDefinition {
    pub id: FeatureId,
    pub family: StandardFamily,
    pub specification: &'static str,
    pub section: &'static str,
    pub status: SupportStatus,
    pub dependencies: &'static [FeatureId],
    pub tests: &'static [ConformanceTest],
}

impl FeatureDefinition {
    #[must_use]
    pub const fn is_available(self) -> bool {
        !matches!(self.status, SupportStatus::Missing)
    }
}

#[derive(Clone, Debug)]
pub struct FeatureRegistry {
    definitions: BTreeMap<FeatureId, &'static FeatureDefinition>,
}

impl FeatureRegistry {
    /// Validate and build a registry from static feature definitions.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate feature IDs, unknown dependencies,
    /// dependency cycles, invalid metadata, or a conformant claim without a
    /// conformance-suite mapping.
    pub fn new(definitions: &'static [FeatureDefinition]) -> Result<Self, FeatureRegistryError> {
        let mut by_id = BTreeMap::new();
        for definition in definitions {
            validate_metadata(definition)?;
            if by_id.insert(definition.id, definition).is_some() {
                return Err(FeatureRegistryError::Duplicate(definition.id));
            }
        }
        for definition in definitions {
            for dependency in definition.dependencies {
                if !by_id.contains_key(dependency) {
                    return Err(FeatureRegistryError::UnknownDependency {
                        feature: definition.id,
                        dependency: *dependency,
                    });
                }
            }
        }
        validate_acyclic(&by_id)?;
        Ok(Self { definitions: by_id })
    }

    /// The conformance inventory compiled into this engine build.
    ///
    /// # Panics
    ///
    /// Panics when the engine's built-in feature definitions violate registry
    /// invariants. This indicates a build-time programming error rather than
    /// invalid runtime input.
    #[must_use]
    pub fn current() -> Self {
        Self::new(CURRENT_FEATURES).expect("built-in feature registry must be valid")
    }

    #[must_use]
    pub fn get(&self, id: FeatureId) -> Option<&FeatureDefinition> {
        self.definitions.get(&id).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = &FeatureDefinition> + '_ {
        self.definitions.values().copied()
    }

    #[must_use]
    pub fn status_counts(&self) -> BTreeMap<SupportStatus, usize> {
        let mut counts = BTreeMap::new();
        for definition in self.iter() {
            *counts.entry(definition.status).or_insert(0) += 1;
        }
        counts
    }

    /// Whether this feature and all declared dependencies have at least a
    /// partial implementation in this build.
    #[must_use]
    pub fn is_available_with_dependencies(&self, id: FeatureId) -> bool {
        let Some(definition) = self.get(id) else {
            return false;
        };
        definition.is_available()
            && definition
                .dependencies
                .iter()
                .all(|dependency| self.is_available_with_dependencies(*dependency))
    }
}

fn validate_metadata(definition: &FeatureDefinition) -> Result<(), FeatureRegistryError> {
    let id = definition.id.as_str();
    if id.is_empty()
        || id.starts_with('.')
        || id.ends_with('.')
        || !id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
        })
    {
        return Err(FeatureRegistryError::InvalidId(definition.id));
    }
    if definition.specification.trim().is_empty() || definition.section.trim().is_empty() {
        return Err(FeatureRegistryError::MissingSpecification(definition.id));
    }
    if definition.status == SupportStatus::Conformant && definition.tests.is_empty() {
        return Err(FeatureRegistryError::ConformantWithoutTests(definition.id));
    }
    Ok(())
}

fn validate_acyclic(
    definitions: &BTreeMap<FeatureId, &'static FeatureDefinition>,
) -> Result<(), FeatureRegistryError> {
    fn visit(
        id: FeatureId,
        definitions: &BTreeMap<FeatureId, &'static FeatureDefinition>,
        visiting: &mut BTreeSet<FeatureId>,
        visited: &mut BTreeSet<FeatureId>,
    ) -> Result<(), FeatureRegistryError> {
        if visited.contains(&id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(FeatureRegistryError::DependencyCycle(id));
        }
        for dependency in definitions[&id].dependencies {
            visit(*dependency, definitions, visiting, visited)?;
        }
        visiting.remove(&id);
        visited.insert(id);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for id in definitions.keys() {
        visit(*id, definitions, &mut visiting, &mut visited)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureRegistryError {
    Duplicate(FeatureId),
    InvalidId(FeatureId),
    MissingSpecification(FeatureId),
    ConformantWithoutTests(FeatureId),
    UnknownDependency {
        feature: FeatureId,
        dependency: FeatureId,
    },
    DependencyCycle(FeatureId),
}

impl fmt::Display for FeatureRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(id) => write!(formatter, "duplicate feature ID '{id}'"),
            Self::InvalidId(id) => write!(formatter, "invalid feature ID '{id}'"),
            Self::MissingSpecification(id) => {
                write!(formatter, "feature '{id}' has no specification section")
            }
            Self::ConformantWithoutTests(id) => {
                write!(formatter, "conformant feature '{id}' has no test mapping")
            }
            Self::UnknownDependency {
                feature,
                dependency,
            } => write!(
                formatter,
                "feature '{feature}' references unknown dependency '{dependency}'"
            ),
            Self::DependencyCycle(id) => {
                write!(formatter, "feature dependency cycle includes '{id}'")
            }
        }
    }
}

impl Error for FeatureRegistryError {}

#[cfg(test)]
mod tests {
    use super::{
        ConformanceTest, FeatureDefinition, FeatureId, FeatureRegistry, FeatureRegistryError,
        StandardFamily, SupportStatus,
    };

    const BASE: FeatureId = FeatureId::new("dom.tree");
    const CHILD: FeatureId = FeatureId::new("html.tree-builder");
    const TESTS: &[ConformanceTest] = &[ConformanceTest::new("wpt", "dom/nodes/")];

    #[test]
    fn current_inventory_is_valid_and_does_not_overclaim_conformance() {
        let registry = FeatureRegistry::current();
        assert!(registry.iter().count() >= 10);
        assert!(registry.iter().all(|feature| {
            feature.status != SupportStatus::Conformant || !feature.tests.is_empty()
        }));
    }

    #[test]
    fn availability_includes_transitive_dependencies() {
        static FEATURES: &[FeatureDefinition] = &[
            FeatureDefinition {
                id: BASE,
                family: StandardFamily::Dom,
                specification: "DOM",
                section: "Trees",
                status: SupportStatus::Partial,
                dependencies: &[],
                tests: TESTS,
            },
            FeatureDefinition {
                id: CHILD,
                family: StandardFamily::Html,
                specification: "HTML",
                section: "Tree construction",
                status: SupportStatus::Partial,
                dependencies: &[BASE],
                tests: &[],
            },
        ];
        let registry = FeatureRegistry::new(FEATURES).unwrap();
        assert!(registry.is_available_with_dependencies(CHILD));
    }

    #[test]
    fn invalid_registry_metadata_fails_closed() {
        static UNKNOWN: FeatureId = FeatureId::new("missing.feature");
        static FEATURES: &[FeatureDefinition] = &[FeatureDefinition {
            id: CHILD,
            family: StandardFamily::Html,
            specification: "HTML",
            section: "Tree construction",
            status: SupportStatus::Partial,
            dependencies: &[UNKNOWN],
            tests: &[],
        }];
        assert_eq!(
            FeatureRegistry::new(FEATURES).unwrap_err(),
            FeatureRegistryError::UnknownDependency {
                feature: CHILD,
                dependency: UNKNOWN,
            }
        );
    }
}
