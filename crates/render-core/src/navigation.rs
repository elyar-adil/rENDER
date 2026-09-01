//! URL and session-history primitives shared by browser front ends.
//!
//! This module deliberately stops before fetching. It turns user-entered
//! addresses into URL Standard values, represents navigation intent, and owns
//! the traversable session-history list. Network policy and document loading
//! consume these types rather than reinterpreting address-bar strings.

use std::error::Error;
use std::fmt;
use std::net::IpAddr;
use std::path::Path;

use url::{Host, Url};

/// Resource ceilings for untrusted navigation inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavigationLimits {
    pub max_address_input_bytes: usize,
    pub max_url_bytes: usize,
    pub max_history_entries: usize,
    pub max_target_name_bytes: usize,
}

impl Default for NavigationLimits {
    fn default() -> Self {
        Self {
            max_address_input_bytes: 16 * 1024,
            max_url_bytes: 64 * 1024,
            max_history_entries: 512,
            max_target_name_bytes: 1_024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    AddressInput,
    Url,
    HistoryEntries,
    TargetName,
}

/// A configurable URL-based search provider.
///
/// Search terms are encoded with the URL crate's form-url-encoding support;
/// callers never need to concatenate or escape a query string themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchProvider {
    endpoint: Url,
    query_parameter: String,
}

impl SearchProvider {
    /// Creates a provider backed by an HTTP(S) endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`NavigationError::UnsupportedScheme`] for a non-network
    /// endpoint, or [`NavigationError::InvalidSearchProvider`] for an empty
    /// query-parameter name.
    pub fn new(endpoint: Url, query_parameter: impl Into<String>) -> Result<Self, NavigationError> {
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(NavigationError::UnsupportedScheme(
                endpoint.scheme().to_owned(),
            ));
        }
        let query_parameter = query_parameter.into();
        if query_parameter.is_empty() || query_parameter.chars().any(char::is_control) {
            return Err(NavigationError::InvalidSearchProvider);
        }
        Ok(Self {
            endpoint,
            query_parameter,
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    fn search_url(&self, terms: &str) -> Url {
        let mut url = self.endpoint.clone();
        url.query_pairs_mut()
            .append_pair(&self.query_parameter, terms);
        url
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddressInputConfig {
    pub default_scheme: NetworkScheme,
    pub search_provider: Option<SearchProvider>,
    pub limits: NavigationLimits,
}

impl Default for AddressInputConfig {
    fn default() -> Self {
        Self {
            default_scheme: NetworkScheme::Https,
            search_provider: None,
            limits: NavigationLimits::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetworkScheme {
    Http,
    #[default]
    Https,
}

impl NetworkScheme {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

/// The classified result of parsing text from an address field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddressInput {
    Url(Url),
    Search { terms: String, url: Url },
}

impl AddressInput {
    /// Parses an address-field value without consulting the file system or
    /// network. Bare domains, localhost and IP literals receive the configured
    /// network scheme; only syntactically absolute paths become `file:` URLs.
    ///
    /// # Errors
    ///
    /// Returns an error for empty/control-character input, resource-limit
    /// violations, malformed URLs, dangerous or unsupported explicit schemes,
    /// invalid absolute file paths, or search terms without a configured
    /// provider.
    pub fn parse(input: &str, config: &AddressInputConfig) -> Result<Self, NavigationError> {
        enforce_limit(
            ResourceKind::AddressInput,
            input.len(),
            config.limits.max_address_input_bytes,
        )?;
        let value = input.trim();
        if value.is_empty() {
            return Err(NavigationError::EmptyAddress);
        }
        if value.chars().any(char::is_control) {
            return Err(NavigationError::ControlCharacter);
        }

        if is_absolute_local_path(value) {
            let url = file_url(value)?;
            enforce_url_limit(&url, config.limits)?;
            return Ok(Self::Url(url));
        }

        if has_explicit_scheme(value) && !looks_like_host_with_numeric_port(value) {
            let url = Url::parse(value).map_err(|error| NavigationError::InvalidUrl {
                input: value.to_owned(),
                reason: error.to_string(),
            })?;
            validate_address_scheme(&url)?;
            enforce_url_limit(&url, config.limits)?;
            return Ok(Self::Url(url));
        }

        if looks_like_host_input(value) {
            let candidate = format!("{}://{value}", config.default_scheme.as_str());
            let url = Url::parse(&candidate).map_err(|error| NavigationError::InvalidUrl {
                input: value.to_owned(),
                reason: error.to_string(),
            })?;
            if url.host().is_none() {
                return Err(NavigationError::InvalidUrl {
                    input: value.to_owned(),
                    reason: "network address has no host".to_owned(),
                });
            }
            enforce_url_limit(&url, config.limits)?;
            return Ok(Self::Url(url));
        }

        let Some(provider) = &config.search_provider else {
            return Err(NavigationError::SearchUnavailable(value.to_owned()));
        };
        let url = provider.search_url(value);
        enforce_url_limit(&url, config.limits)?;
        Ok(Self::Search {
            terms: value.to_owned(),
            url,
        })
    }

    #[must_use]
    pub fn url(&self) -> &Url {
        match self {
            Self::Url(url) | Self::Search { url, .. } => url,
        }
    }

    #[must_use]
    pub fn into_url(self) -> Url {
        match self {
            Self::Url(url) | Self::Search { url, .. } => url,
        }
    }
}

/// Resolves a URL reference according to the URL Standard.
///
/// # Errors
///
/// Returns an error when joining fails or the serialized result exceeds the
/// configured URL limit.
pub fn resolve_url_reference(
    base: &Url,
    reference: &str,
    limits: NavigationLimits,
) -> Result<Url, NavigationError> {
    enforce_limit(
        ResourceKind::AddressInput,
        reference.len(),
        limits.max_address_input_bytes,
    )?;
    if reference.chars().any(char::is_control) {
        return Err(NavigationError::ControlCharacter);
    }
    let resolved = base
        .join(reference)
        .map_err(|error| NavigationError::InvalidUrl {
            input: reference.to_owned(),
            reason: error.to_string(),
        })?;
    enforce_url_limit(&resolved, limits)?;
    Ok(resolved)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NavigationId(u64);

impl NavigationId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct NavigationIdGenerator {
    last_issued: u64,
}

impl NavigationIdGenerator {
    /// Issues a monotonically increasing, nonzero navigation identifier.
    ///
    /// # Errors
    ///
    /// Returns [`NavigationError::NavigationIdExhausted`] after `u64::MAX`
    /// identifiers have been issued.
    pub fn next_id(&mut self) -> Result<NavigationId, NavigationError> {
        let next = self
            .last_issued
            .checked_add(1)
            .ok_or(NavigationError::NavigationIdExhausted)?;
        self.last_issued = next;
        Ok(NavigationId(next))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowsingContextName(String);

impl BrowsingContextName {
    /// Validates a named browsing-context target.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, reserved, controlled, or over-limit names.
    pub fn new(
        value: impl Into<String>,
        limits: NavigationLimits,
    ) -> Result<Self, NavigationError> {
        let value = value.into();
        enforce_limit(
            ResourceKind::TargetName,
            value.len(),
            limits.max_target_name_bytes,
        )?;
        if value.is_empty()
            || value.chars().any(char::is_control)
            || matches!(
                value.to_ascii_lowercase().as_str(),
                "_self" | "_parent" | "_top" | "_blank" | "_unfencedtop"
            )
        {
            return Err(NavigationError::InvalidTargetName(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NavigationTarget {
    #[default]
    Current,
    Parent,
    Top,
    UnfencedTop,
    Blank,
    Named(BrowsingContextName),
}

impl NavigationTarget {
    /// Parses the reserved HTML target keywords or a context name.
    ///
    /// # Errors
    ///
    /// Returns an error if a non-keyword name is invalid or exceeds its limit.
    pub fn parse(value: &str, limits: NavigationLimits) -> Result<Self, NavigationError> {
        match value.to_ascii_lowercase().as_str() {
            "" | "_self" => Ok(Self::Current),
            "_parent" => Ok(Self::Parent),
            "_top" => Ok(Self::Top),
            "_unfencedtop" => Ok(Self::UnfencedTop),
            "_blank" => Ok(Self::Blank),
            _ => BrowsingContextName::new(value, limits).map(Self::Named),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryBehavior {
    #[default]
    Auto,
    Push,
    Replace,
    Reload,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReferrerPolicy {
    NoReferrer,
    NoReferrerWhenDowngrade,
    Origin,
    OriginWhenCrossOrigin,
    SameOrigin,
    StrictOrigin,
    #[default]
    StrictOriginWhenCrossOrigin,
    UnsafeUrl,
}

/// Policy seam used by navigation without coupling the request type to a
/// particular Fetch implementation or security-context representation.
pub trait ReferrerPolicyHook {
    fn referrer(
        &self,
        source: Option<&Url>,
        destination: &Url,
        policy: ReferrerPolicy,
    ) -> Option<Url>;
}

/// Reference implementation of the Referrer Policy decision table for
/// HTTP(S) referrers.
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardReferrerPolicy;

impl ReferrerPolicyHook for StandardReferrerPolicy {
    fn referrer(
        &self,
        source: Option<&Url>,
        destination: &Url,
        policy: ReferrerPolicy,
    ) -> Option<Url> {
        let source = sanitize_referrer(source?)?;
        let same_origin = same_origin(&source, destination);
        let downgrade = source.scheme() == "https" && destination.scheme() == "http";
        match policy {
            ReferrerPolicy::NoReferrer => None,
            ReferrerPolicy::NoReferrerWhenDowngrade => (!downgrade).then_some(source),
            ReferrerPolicy::Origin => Some(origin_referrer(&source)),
            ReferrerPolicy::OriginWhenCrossOrigin => Some(if same_origin {
                source
            } else {
                origin_referrer(&source)
            }),
            ReferrerPolicy::SameOrigin => same_origin.then_some(source),
            ReferrerPolicy::StrictOrigin => (!downgrade).then(|| origin_referrer(&source)),
            ReferrerPolicy::StrictOriginWhenCrossOrigin => {
                if same_origin {
                    Some(source)
                } else if downgrade {
                    None
                } else {
                    Some(origin_referrer(&source))
                }
            }
            ReferrerPolicy::UnsafeUrl => Some(source),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationRequest {
    pub id: NavigationId,
    pub url: Url,
    pub target: NavigationTarget,
    pub history_behavior: HistoryBehavior,
    pub referrer_policy: ReferrerPolicy,
    pub referrer_source: Option<Url>,
}

impl NavigationRequest {
    /// Creates a typed request after applying URL resource limits.
    ///
    /// # Errors
    ///
    /// Returns an error if either serialized URL exceeds the configured limit.
    pub fn new(
        id: NavigationId,
        url: Url,
        target: NavigationTarget,
        limits: NavigationLimits,
    ) -> Result<Self, NavigationError> {
        enforce_url_limit(&url, limits)?;
        Ok(Self {
            id,
            url,
            target,
            history_behavior: HistoryBehavior::Auto,
            referrer_policy: ReferrerPolicy::default(),
            referrer_source: None,
        })
    }

    #[must_use]
    pub fn computed_referrer(&self, hook: &dyn ReferrerPolicyHook) -> Option<Url> {
        hook.referrer(
            self.referrer_source.as_ref(),
            &self.url,
            self.referrer_policy,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HistoryIndex(usize);

impl HistoryIndex {
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub url: Url,
    pub title: Option<String>,
}

impl HistoryEntry {
    #[must_use]
    pub const fn new(url: Url) -> Self {
        Self { url, title: None }
    }
}

/// A single traversable's linear session-history list.
#[derive(Clone, Debug)]
pub struct SessionHistory {
    entries: Vec<HistoryEntry>,
    current: HistoryIndex,
    limits: NavigationLimits,
}

impl SessionHistory {
    /// Starts history with one active entry.
    ///
    /// # Errors
    ///
    /// Returns an explicit resource error if history is disabled by a zero
    /// limit or the initial URL exceeds its limit.
    pub fn new(initial: HistoryEntry, limits: NavigationLimits) -> Result<Self, NavigationError> {
        enforce_limit(ResourceKind::HistoryEntries, 1, limits.max_history_entries)?;
        enforce_url_limit(&initial.url, limits)?;
        Ok(Self {
            entries: vec![initial],
            current: HistoryIndex(0),
            limits,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub const fn current_index(&self) -> HistoryIndex {
        self.current
    }

    #[must_use]
    pub fn current(&self) -> &HistoryEntry {
        &self.entries[self.current.0]
    }

    #[must_use]
    pub const fn can_go_back(&self) -> bool {
        self.current.0 > 0
    }

    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        self.current.0 + 1 < self.entries.len()
    }

    /// Appends after the active entry and discards any forward branch.
    ///
    /// # Errors
    ///
    /// Returns a resource error without modifying history if the resulting
    /// active branch would exceed a configured limit.
    pub fn push(&mut self, entry: HistoryEntry) -> Result<HistoryIndex, NavigationError> {
        enforce_url_limit(&entry.url, self.limits)?;
        let resulting_len = self.current.0 + 2;
        enforce_limit(
            ResourceKind::HistoryEntries,
            resulting_len,
            self.limits.max_history_entries,
        )?;
        self.entries.truncate(self.current.0 + 1);
        self.entries.push(entry);
        self.current = HistoryIndex(self.entries.len() - 1);
        Ok(self.current)
    }

    /// Replaces the active entry without changing list length or index.
    ///
    /// # Errors
    ///
    /// Returns an error if the replacement URL exceeds its limit.
    pub fn replace(&mut self, entry: HistoryEntry) -> Result<HistoryIndex, NavigationError> {
        enforce_url_limit(&entry.url, self.limits)?;
        self.entries[self.current.0] = entry;
        Ok(self.current)
    }

    pub fn back(&mut self) -> Option<&HistoryEntry> {
        self.go(-1)
    }

    pub fn forward(&mut self) -> Option<&HistoryEntry> {
        self.go(1)
    }

    pub fn go(&mut self, delta: isize) -> Option<&HistoryEntry> {
        let target = self.current.0.checked_add_signed(delta)?;
        if target >= self.entries.len() {
            return None;
        }
        self.current = HistoryIndex(target);
        Some(&self.entries[target])
    }

    #[must_use]
    pub fn reload(&self) -> &HistoryEntry {
        self.current()
    }

    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &HistoryEntry> {
        self.entries.iter()
    }
}

/// Whether a URL change is a same-document fragment navigation.
#[must_use]
pub fn is_same_document_fragment_navigation(current: &Url, destination: &Url) -> bool {
    if destination.fragment().is_none() || current == destination {
        return false;
    }
    let mut current_without_fragment = current.clone();
    current_without_fragment.set_fragment(None);
    let mut destination_without_fragment = destination.clone();
    destination_without_fragment.set_fragment(None);
    current_without_fragment == destination_without_fragment
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavigationError {
    EmptyAddress,
    ControlCharacter,
    InvalidUrl {
        input: String,
        reason: String,
    },
    InvalidFilePath(String),
    DangerousScheme(String),
    UnsupportedScheme(String),
    SearchUnavailable(String),
    InvalidSearchProvider,
    InvalidTargetName(String),
    ResourceLimit {
        resource: ResourceKind,
        limit: usize,
        actual: usize,
    },
    NavigationIdExhausted,
}

impl fmt::Display for NavigationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAddress => formatter.write_str("address input is empty"),
            Self::ControlCharacter => formatter.write_str("address contains a control character"),
            Self::InvalidUrl { input, reason } => {
                write!(formatter, "invalid URL '{input}': {reason}")
            }
            Self::InvalidFilePath(path) => write!(formatter, "invalid absolute file path '{path}'"),
            Self::DangerousScheme(scheme) => {
                write!(
                    formatter,
                    "dangerous address scheme '{scheme}' is not allowed"
                )
            }
            Self::UnsupportedScheme(scheme) => {
                write!(formatter, "unsupported address scheme '{scheme}'")
            }
            Self::SearchUnavailable(terms) => {
                write!(formatter, "no search provider is configured for '{terms}'")
            }
            Self::InvalidSearchProvider => formatter.write_str("invalid search provider"),
            Self::InvalidTargetName(name) => {
                write!(formatter, "invalid navigation target '{name}'")
            }
            Self::ResourceLimit {
                resource,
                limit,
                actual,
            } => write!(
                formatter,
                "{resource:?} resource limit exceeded: {actual} bytes/items, maximum {limit}"
            ),
            Self::NavigationIdExhausted => formatter.write_str("navigation ID space exhausted"),
        }
    }
}

impl Error for NavigationError {}

fn enforce_limit(
    resource: ResourceKind,
    actual: usize,
    limit: usize,
) -> Result<(), NavigationError> {
    if actual > limit {
        return Err(NavigationError::ResourceLimit {
            resource,
            limit,
            actual,
        });
    }
    Ok(())
}

fn enforce_url_limit(url: &Url, limits: NavigationLimits) -> Result<(), NavigationError> {
    enforce_limit(ResourceKind::Url, url.as_str().len(), limits.max_url_bytes)
}

fn has_explicit_scheme(value: &str) -> bool {
    let Some(colon) = value.find(':') else {
        return false;
    };
    let scheme = &value[..colon];
    !scheme.is_empty()
        && scheme
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

fn looks_like_host_with_numeric_port(value: &str) -> bool {
    let authority = value.split(['/', '?', '#']).next().unwrap_or_default();
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    if !is_port(port) || host.contains(':') {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok() {
        return true;
    }
    Url::parse(&format!("https://{host}"))
        .ok()
        .is_some_and(|url| matches!(url.host(), Some(Host::Domain(domain)) if domain.contains('.')))
}

fn looks_like_host_input(value: &str) -> bool {
    if value.chars().any(char::is_whitespace) {
        return false;
    }
    let authority = value.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.eq_ignore_ascii_case("localhost")
        || authority
            .split_once(':')
            .is_some_and(|(host, port)| host.eq_ignore_ascii_case("localhost") && is_port(port))
        || authority.parse::<IpAddr>().is_ok()
        || bracketed_ip_with_optional_port(authority)
    {
        return true;
    }
    let candidate = format!("https://{value}");
    Url::parse(&candidate)
        .ok()
        .is_some_and(|url| match url.host() {
            Some(Host::Domain(domain)) => domain.contains('.'),
            Some(Host::Ipv4(_) | Host::Ipv6(_)) => true,
            None => false,
        })
}

fn is_port(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn bracketed_ip_with_optional_port(authority: &str) -> bool {
    let Some(closing) = authority.find(']') else {
        return false;
    };
    if !authority.starts_with('[') {
        return false;
    }
    let Ok(ip) = authority[1..closing].parse::<IpAddr>() else {
        return false;
    };
    if !ip.is_ipv6() {
        return false;
    }
    let suffix = &authority[closing + 1..];
    suffix.is_empty() || suffix.strip_prefix(':').is_some_and(is_port)
}

fn is_absolute_local_path(value: &str) -> bool {
    Path::new(value).is_absolute() || is_windows_drive_path(value) || value.starts_with("\\\\")
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn file_url(value: &str) -> Result<Url, NavigationError> {
    if is_windows_drive_path(value) {
        let normalized = value.replace('\\', "/");
        let mut url = Url::parse("file:///").expect("the static file URL base is valid");
        url.set_path(&format!("/{normalized}"));
        return Ok(url);
    }
    if let Some(unc) = value.strip_prefix("\\\\") {
        let normalized = unc.replace('\\', "/");
        let Some((host, path)) = normalized.split_once('/') else {
            return Err(NavigationError::InvalidFilePath(value.to_owned()));
        };
        if host.is_empty() || path.is_empty() {
            return Err(NavigationError::InvalidFilePath(value.to_owned()));
        }
        let mut url = Url::parse("file:///").expect("the static file URL base is valid");
        url.set_host(Some(host))
            .map_err(|_| NavigationError::InvalidFilePath(value.to_owned()))?;
        url.set_path(&format!("/{path}"));
        return Ok(url);
    }
    Url::from_file_path(value).map_err(|()| NavigationError::InvalidFilePath(value.to_owned()))
}

fn validate_address_scheme(url: &Url) -> Result<(), NavigationError> {
    match url.scheme() {
        "http" | "https" | "file" | "about" | "data" => Ok(()),
        "javascript" | "vbscript" => Err(NavigationError::DangerousScheme(url.scheme().to_owned())),
        scheme => Err(NavigationError::UnsupportedScheme(scheme.to_owned())),
    }
}

fn sanitize_referrer(source: &Url) -> Option<Url> {
    if !matches!(source.scheme(), "http" | "https") {
        return None;
    }
    let mut sanitized = source.clone();
    sanitized.set_username("").ok()?;
    sanitized.set_password(None).ok()?;
    sanitized.set_fragment(None);
    Some(sanitized)
}

fn origin_referrer(source: &Url) -> Url {
    let mut origin = source.clone();
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    origin
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host() == right.host()
        && left.port_or_known_default() == right.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::{
        AddressInput, AddressInputConfig, HistoryEntry, NavigationError, NavigationLimits,
        NavigationRequest, NavigationTarget, ReferrerPolicy, SearchProvider, SessionHistory,
        StandardReferrerPolicy, is_same_document_fragment_navigation, resolve_url_reference,
    };
    use url::Url;

    fn config_with_search() -> AddressInputConfig {
        AddressInputConfig {
            search_provider: Some(
                SearchProvider::new(
                    Url::parse("https://search.example/find?source=bar").unwrap(),
                    "q",
                )
                .unwrap(),
            ),
            ..AddressInputConfig::default()
        }
    }

    fn parsed_url(input: &str) -> Url {
        AddressInput::parse(input, &config_with_search())
            .unwrap()
            .into_url()
    }

    #[test]
    fn bare_domains_are_https_urls_and_never_files() {
        let parsed =
            AddressInput::parse("google.com/search?q=rust#top", &config_with_search()).unwrap();
        assert!(matches!(parsed, AddressInput::Url(_)));
        assert_eq!(parsed.url().scheme(), "https");
        assert_eq!(parsed.url().host_str(), Some("google.com"));
        assert_eq!(parsed.url().query(), Some("q=rust"));
        assert_eq!(parsed.url().fragment(), Some("top"));
    }

    #[test]
    fn accepts_explicit_schemes_local_hosts_ip_ports_and_idn() {
        assert_eq!(parsed_url("http://example.com:8080/a").port(), Some(8080));
        assert_eq!(
            parsed_url("localhost:3000/a").as_str(),
            "https://localhost:3000/a"
        );
        assert_eq!(
            parsed_url("example.com:8443/a?q=1#part").as_str(),
            "https://example.com:8443/a?q=1#part"
        );
        assert_eq!(parsed_url("127.0.0.1:9000").host_str(), Some("127.0.0.1"));
        assert_eq!(parsed_url("[::1]:8000/a").host_str(), Some("[::1]"));
        assert_eq!(
            parsed_url("例子.测试/路径").host_str(),
            Some("xn--fsqu00a.xn--0zwm56d")
        );
        assert_eq!(
            parsed_url("data:text/html,%3Ch1%3EHello%3C%2Fh1%3E").as_str(),
            "data:text/html,%3Ch1%3EHello%3C%2Fh1%3E"
        );
    }

    #[test]
    fn absolute_windows_path_is_file_but_relative_text_is_search() {
        let file =
            AddressInput::parse(r"C:\Users\Elyar\index.html", &config_with_search()).unwrap();
        assert_eq!(file.url().scheme(), "file");
        assert_eq!(file.url().path(), "/C:/Users/Elyar/index.html");

        let unc = AddressInput::parse(r"\\server\share\index.html", &config_with_search()).unwrap();
        assert_eq!(unc.url().scheme(), "file");
        assert_eq!(unc.url().host_str(), Some("server"));
        assert_eq!(unc.url().path(), "/share/index.html");

        let search = AddressInput::parse("docs/index.html", &config_with_search()).unwrap();
        assert!(
            matches!(&search, AddressInput::Search { terms, .. } if terms == "docs/index.html")
        );
    }

    #[test]
    fn search_fallback_is_configured_and_url_encoded() {
        let result = AddressInput::parse("rust 浏览器", &config_with_search()).unwrap();
        assert!(matches!(&result, AddressInput::Search { terms, .. } if terms == "rust 浏览器"));
        assert_eq!(
            result.url().query_pairs().collect::<Vec<_>>(),
            [
                ("source".into(), "bar".into()),
                ("q".into(), "rust 浏览器".into())
            ]
        );
        assert!(matches!(
            AddressInput::parse("two words", &AddressInputConfig::default()),
            Err(NavigationError::SearchUnavailable(_))
        ));
    }

    #[test]
    fn rejects_dangerous_unsupported_and_malformed_schemes() {
        assert!(matches!(
            AddressInput::parse("javascript:alert(1)", &config_with_search()),
            Err(NavigationError::DangerousScheme(scheme)) if scheme == "javascript"
        ));
        assert!(matches!(
            AddressInput::parse("ftp://example.com", &config_with_search()),
            Err(NavigationError::UnsupportedScheme(scheme)) if scheme == "ftp"
        ));
        assert!(matches!(
            AddressInput::parse("http://[::1", &config_with_search()),
            Err(NavigationError::InvalidUrl { .. })
        ));
    }

    #[test]
    fn resolves_relative_query_fragment_and_parent_references() {
        let base = Url::parse("https://example.com/a/b?old=1").unwrap();
        assert_eq!(
            resolve_url_reference(&base, "../c?q=2#result", NavigationLimits::default())
                .unwrap()
                .as_str(),
            "https://example.com/c?q=2#result"
        );
        assert_eq!(
            resolve_url_reference(&base, "#new", NavigationLimits::default())
                .unwrap()
                .as_str(),
            "https://example.com/a/b?old=1#new"
        );
    }

    #[test]
    fn same_document_fragment_requires_only_fragment_to_change() {
        let current = Url::parse("https://example.com/a?q=1#old").unwrap();
        assert!(is_same_document_fragment_navigation(
            &current,
            &Url::parse("https://example.com/a?q=1#new").unwrap()
        ));
        assert!(!is_same_document_fragment_navigation(
            &current,
            &Url::parse("https://example.com/a?q=2#new").unwrap()
        ));
        assert!(!is_same_document_fragment_navigation(&current, &current));
    }

    #[test]
    fn history_traversal_reload_replace_and_branch_truncation_are_stable() {
        let limits = NavigationLimits::default();
        let mut history = SessionHistory::new(
            HistoryEntry::new(Url::parse("https://example.com/1").unwrap()),
            limits,
        )
        .unwrap();
        history
            .push(HistoryEntry::new(
                Url::parse("https://example.com/2").unwrap(),
            ))
            .unwrap();
        history
            .push(HistoryEntry::new(
                Url::parse("https://example.com/3").unwrap(),
            ))
            .unwrap();
        assert_eq!(history.back().unwrap().url.path(), "/2");
        assert_eq!(history.reload().url.path(), "/2");
        history
            .replace(HistoryEntry::new(
                Url::parse("https://example.com/replaced").unwrap(),
            ))
            .unwrap();
        history
            .push(HistoryEntry::new(
                Url::parse("https://example.com/branch").unwrap(),
            ))
            .unwrap();
        assert_eq!(history.len(), 3);
        assert!(!history.can_go_forward());
        assert_eq!(
            history
                .iter()
                .map(|entry| entry.url.path())
                .collect::<Vec<_>>(),
            ["/1", "/replaced", "/branch"]
        );
    }

    #[test]
    fn history_and_url_limits_are_explicit_and_transactional() {
        let limits = NavigationLimits {
            max_history_entries: 2,
            max_url_bytes: 128,
            ..NavigationLimits::default()
        };
        let mut history = SessionHistory::new(
            HistoryEntry::new(Url::parse("https://example.com/1").unwrap()),
            limits,
        )
        .unwrap();
        history
            .push(HistoryEntry::new(
                Url::parse("https://example.com/2").unwrap(),
            ))
            .unwrap();
        assert!(matches!(
            history.push(HistoryEntry::new(
                Url::parse("https://example.com/3").unwrap()
            )),
            Err(NavigationError::ResourceLimit { .. })
        ));
        assert_eq!(history.len(), 2);
        assert_eq!(history.current().url.path(), "/2");
    }

    #[test]
    fn target_request_ids_and_referrer_hook_are_typed() {
        let mut ids = super::NavigationIdGenerator::default();
        let target = NavigationTarget::parse("content-frame", NavigationLimits::default()).unwrap();
        let mut request = NavigationRequest::new(
            ids.next_id().unwrap(),
            Url::parse("https://other.example/page").unwrap(),
            target,
            NavigationLimits::default(),
        )
        .unwrap();
        request.referrer_source =
            Some(Url::parse("https://user:secret@source.example/private?q=1#fragment").unwrap());
        request.referrer_policy = ReferrerPolicy::StrictOriginWhenCrossOrigin;
        assert_eq!(request.id.get(), 1);
        assert_eq!(
            request
                .computed_referrer(&StandardReferrerPolicy)
                .unwrap()
                .as_str(),
            "https://source.example/"
        );
    }
}
