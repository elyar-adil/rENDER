//! Conservative, in-memory HTTP response caching for the browser shell.
//!
//! The cache intentionally accepts only anonymous, direct HTTP(S) `200`
//! responses with an explicit `Cache-Control: max-age` lifetime. Responses
//! selected by a `Vary` header retain the request-header values that selected
//! them and are only served to requests carrying the same values; `Vary: *`
//! responses are never stored. Expired entries remain available as validator
//! metadata until they are replaced or evicted, allowing callers to perform
//! conditional revalidation without blocking the UI thread.

pub mod disk;
pub mod payload;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use render_net::{
    CacheValidators, FetchRequest, FetchResponse, Header, HttpMethod, HttpStatus, Url,
};

/// Limits for the in-memory HTTP cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HttpCacheConfig {
    /// Maximum number of retained responses. Zero disables caching.
    pub max_entries: usize,
    /// Maximum retained response-body bytes. Zero disables caching.
    pub max_bytes: usize,
    /// Maximum response-body bytes for one cache entry.
    pub max_entry_bytes: usize,
}

impl Default for HttpCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 256,
            max_bytes: 32 * 1024 * 1024,
            max_entry_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Monotonically increasing generation for cache-aware requests.
///
/// Callers capture this when submitting a network request and pass it to
/// [`HttpCache::store`]. A response completed after a clear can still be used
/// by its original navigation, but it cannot refill the cleared cache.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CacheEpoch(u64);

impl CacheEpoch {
    /// Returns the numeric cache generation for diagnostics and tests.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Result of clearing the in-memory HTTP cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheClearResult {
    /// Number of in-memory response entries removed.
    pub memory_entries: usize,
    /// Number of response-body bytes removed from memory.
    pub memory_bytes: usize,
    /// Cache generation after the clear.
    pub epoch: CacheEpoch,
}

/// Policy decision for a network response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CachePolicy {
    /// The response may be retained for the supplied freshness lifetime.
    Store { freshness: Duration },
    /// The response must not enter this cache.
    Skip(CacheSkipReason),
}

/// Why a request or response is not eligible for this conservative cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheSkipReason {
    UnsupportedScheme,
    /// Only GET responses are cached. The cache key has no method, so a POST
    /// would otherwise read a cached GET body and a POST body could be served
    /// to a later GET.
    NonGetMethod,
    RequestHasCookie,
    RequestHasByteRange,
    ResponseStatus(u16),
    Redirected,
    ResponseSetsCookie,
    /// The response cannot be represented as one or more variants, either
    /// because it declares `Vary: *` or because its `Vary` field names are
    /// malformed.
    Vary,
    NoStore,
    Private,
    NoCache,
    NoExplicitFreshness,
    InvalidMaxAge,
    NotFresh,
    EntryTooLarge,
    CapacityDisabled,
    StaleEpoch,
    TimeOverflow,
}

/// Result of looking up a fresh cached response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CacheLookup {
    Hit(Box<FetchResponse>),
    Miss,
}

/// Result of attempting to retain a network response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheStoreOutcome {
    Stored,
    Skipped(CacheSkipReason),
}

/// Small LRU response cache owned by the browser application.
#[derive(Debug)]
pub struct HttpCache {
    config: HttpCacheConfig,
    /// One bucket per primary cache key, holding one entry per selected
    /// response variant. Most responses have a single, no-`Vary` variant.
    entries: HashMap<CacheKey, Vec<CacheEntry>>,
    memory_bytes: usize,
    access_clock: u64,
    epoch: CacheEpoch,
}

impl Default for HttpCache {
    fn default() -> Self {
        Self::new(HttpCacheConfig::default())
    }
}

impl HttpCache {
    /// Creates an empty cache with explicit memory limits.
    #[must_use]
    pub fn new(config: HttpCacheConfig) -> Self {
        Self {
            config,
            entries: HashMap::new(),
            memory_bytes: 0,
            access_clock: 0,
            epoch: CacheEpoch::default(),
        }
    }

    /// Returns the cache configuration.
    #[must_use]
    pub const fn config(&self) -> HttpCacheConfig {
        self.config
    }

    /// Returns the epoch to capture before submitting a cache-aware request.
    #[must_use]
    pub const fn epoch(&self) -> CacheEpoch {
        self.epoch
    }

    /// Returns the number of retained in-memory responses.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }

    /// Returns the number of retained response-body bytes.
    #[must_use]
    pub const fn memory_bytes(&self) -> usize {
        self.memory_bytes
    }

    /// Looks up a currently fresh cached response.
    #[must_use]
    pub fn lookup(&mut self, request: &FetchRequest, now: Instant) -> CacheLookup {
        if request_skip_reason(request).is_some() || request.cache_validators.is_some() {
            return CacheLookup::Miss;
        }

        let key = CacheKey::from_request(request);
        let Some(index) = self
            .entries
            .get(&key)
            .and_then(|entries| fresh_entry_index(entries, request, now))
        else {
            return CacheLookup::Miss;
        };

        let last_used = self.next_access();
        let Some(entry) = self
            .entries
            .get_mut(&key)
            .and_then(|entries| entries.get_mut(index))
        else {
            return CacheLookup::Miss;
        };
        entry.last_used = last_used;
        CacheLookup::Hit(Box::new(entry.response.clone()))
    }

    /// Returns an expired response that can be used to construct a
    /// conditional request. Fresh entries are intentionally not returned;
    /// callers should serve those through [`Self::lookup`].
    #[must_use]
    pub fn stale_response(
        &mut self,
        request: &FetchRequest,
        now: Instant,
    ) -> Option<Box<FetchResponse>> {
        if request_skip_reason(request).is_some() {
            return None;
        }
        let key = CacheKey::from_request(request);
        let index = self
            .entries
            .get(&key)
            .and_then(|entries| stale_entry_index(entries, request, now))?;
        let last_used = self.next_access();
        self.entries
            .get_mut(&key)
            .and_then(|entries| entries.get_mut(index))
            .map(|entry| {
                entry.last_used = last_used;
                Box::new(entry.response.clone())
            })
    }

    /// Builds a conditional request from an expired cached response.
    #[must_use]
    pub fn revalidation_request(
        &mut self,
        request: &FetchRequest,
        now: Instant,
    ) -> Option<FetchRequest> {
        let stale = self.stale_response(request, now)?;
        let validators = CacheValidators::from_headers(&stale.headers);
        if validators.is_empty() {
            return None;
        }
        Some(request.clone().with_cache_validators(validators))
    }

    /// Merges a `304 Not Modified` response with its expired cached body and
    /// refreshes the entry's lifetime. Returns the complete `200` response for
    /// the caller's normal resource pipeline, or `None` when no matching
    /// current-epoch entry exists.
    ///
    /// Returns `None` for a mismatched response, an absent/still-fresh entry,
    /// an invalidated epoch, or a response whose refreshed directives make it
    /// unsafe to retain.
    #[must_use]
    pub fn merge_not_modified(
        &mut self,
        request: &FetchRequest,
        response: &FetchResponse,
        now: Instant,
        submitted_epoch: CacheEpoch,
    ) -> Option<FetchResponse> {
        if submitted_epoch != self.epoch || response.status.as_u16() != 304 {
            return None;
        }
        if request_skip_reason(request).is_some() {
            return None;
        }
        if !same_fetch_url(&request.url, &response.requested_url)
            || !same_fetch_url(&request.url, &response.final_url)
            || !response.redirects.is_empty()
            || response.redirect_chain.len() > 1
        {
            return None;
        }
        let key = CacheKey::from_request(request);
        let index = self
            .entries
            .get(&key)
            .and_then(|entries| stale_entry_index(entries, request, now))?;
        let previous = self.entries.get(&key)?.get(index)?.response.clone();
        let headers = merge_headers(&previous.headers, &response.headers);
        let merged = FetchResponse {
            requested_url: previous.requested_url,
            final_url: previous.final_url,
            redirect_chain: previous.redirect_chain,
            redirects: previous.redirects,
            status: HttpStatus::from_u16(200),
            content_type: header_text(&headers, "content-type").and_then(parse_content_type),
            headers,
            body: previous.body,
        };
        // The 304 either preserved the stored `Vary` field (merge_headers
        // keeps it when the update omits one) or replaced it. Recompute the
        // selecting values from this request so a changed field list is
        // represented accurately. A `Vary: *` update makes this variant
        // unusable; evict only that variant and still return the merged body.
        let Some(variant) = VaryVariant::from_request(request, &merged.headers) else {
            self.remove_bucket_entry(&key, index);
            return Some(merged);
        };
        let freshness = match cache_policy(request, &merged) {
            CachePolicy::Store { freshness } => freshness,
            // A 304 still supplies a usable representation. If its updated
            // directives make it uncacheable, evict that variant but return
            // the merged body to the resource pipeline.
            CachePolicy::Skip(_) => {
                self.remove_bucket_entry(&key, index);
                return Some(merged);
            }
        };
        let fresh_until = now.checked_add(freshness)?;
        let response_bytes = merged.body.len();
        let last_used = self.next_access();
        let entry = CacheEntry {
            response: merged.clone(),
            variant,
            fresh_until,
            response_bytes,
            last_used,
        };
        // Only the selected variant is refreshed. Other variants stored under
        // the same primary key keep their previous freshness.
        self.remove_bucket_entry(&key, index);
        self.insert_entry(key, entry);
        Some(merged)
    }

    /// Stores an eligible response if it belongs to the current cache epoch.
    ///
    /// `submitted_epoch` must be captured before its network request begins.
    /// A stale write is deliberately ignored after [`Self::clear`].
    #[must_use]
    pub fn store(
        &mut self,
        request: &FetchRequest,
        response: &FetchResponse,
        now: Instant,
        submitted_epoch: CacheEpoch,
    ) -> CacheStoreOutcome {
        if submitted_epoch != self.epoch {
            return CacheStoreOutcome::Skipped(CacheSkipReason::StaleEpoch);
        }

        let freshness = match cache_policy(request, response) {
            CachePolicy::Store { freshness } => freshness,
            CachePolicy::Skip(reason) => return CacheStoreOutcome::Skipped(reason),
        };
        let Some(variant) = VaryVariant::from_request(request, &response.headers) else {
            return CacheStoreOutcome::Skipped(CacheSkipReason::Vary);
        };

        if self.config.max_entries == 0 || self.config.max_bytes == 0 {
            return CacheStoreOutcome::Skipped(CacheSkipReason::CapacityDisabled);
        }

        let response_bytes = response.body.len();
        if response_bytes > self.config.max_entry_bytes || response_bytes > self.config.max_bytes {
            return CacheStoreOutcome::Skipped(CacheSkipReason::EntryTooLarge);
        }

        let Some(fresh_until) = now.checked_add(freshness) else {
            return CacheStoreOutcome::Skipped(CacheSkipReason::TimeOverflow);
        };

        let key = CacheKey::from_request(request);
        let entry = CacheEntry {
            response: response.clone(),
            variant,
            fresh_until,
            response_bytes,
            last_used: self.next_access(),
        };
        self.insert_entry(key, entry);

        CacheStoreOutcome::Stored
    }

    /// Removes all in-memory entries and advances the cache epoch first.
    #[must_use]
    pub fn clear(&mut self) -> CacheClearResult {
        self.epoch = CacheEpoch(self.epoch.0.saturating_add(1));
        let result = CacheClearResult {
            memory_entries: self.entry_count(),
            memory_bytes: self.memory_bytes,
            epoch: self.epoch,
        };
        self.entries.clear();
        self.memory_bytes = 0;
        result
    }

    fn next_access(&mut self) -> u64 {
        self.access_clock = self.access_clock.saturating_add(1);
        self.access_clock
    }

    fn insert_entry(&mut self, key: CacheKey, entry: CacheEntry) {
        let response_bytes = entry.response_bytes;
        if let Some(entries) = self.entries.get_mut(&key) {
            // A newer response for the same variant replaces the old one; a
            // different variant is retained alongside it.
            if let Some(index) = entries
                .iter()
                .position(|existing| existing.variant == entry.variant)
            {
                let previous = entries.remove(index);
                self.memory_bytes = self.memory_bytes.saturating_sub(previous.response_bytes);
            }
            entries.push(entry);
        } else {
            self.entries.insert(key, vec![entry]);
        }
        self.memory_bytes = self.memory_bytes.saturating_add(response_bytes);
        self.evict_to_limits();
    }

    fn remove_bucket_entry(&mut self, key: &CacheKey, index: usize) {
        let Some(entries) = self.entries.get_mut(key) else {
            return;
        };
        if index >= entries.len() {
            return;
        }
        let removed = entries.remove(index);
        let empty = entries.is_empty();
        if empty {
            self.entries.remove(key);
        }
        self.memory_bytes = self.memory_bytes.saturating_sub(removed.response_bytes);
    }

    fn evict_to_limits(&mut self) {
        while self.entry_count() > self.config.max_entries
            || self.memory_bytes > self.config.max_bytes
        {
            let Some((key, index)) = self
                .entries
                .iter()
                .flat_map(|(key, entries)| {
                    entries
                        .iter()
                        .enumerate()
                        .map(move |(index, entry)| (entry.last_used, key, index))
                })
                .min_by_key(|(last_used, _, _)| *last_used)
                .map(|(_, key, index)| (key.clone(), index))
            else {
                return;
            };
            self.remove_bucket_entry(&key, index);
        }
    }
}

/// Decides whether a response can safely enter the cache.
#[must_use]
pub fn cache_policy(request: &FetchRequest, response: &FetchResponse) -> CachePolicy {
    if let Some(reason) = request_skip_reason(request) {
        return CachePolicy::Skip(reason);
    }
    if response.status.as_u16() != 200 {
        return CachePolicy::Skip(CacheSkipReason::ResponseStatus(response.status.as_u16()));
    }
    if !same_fetch_url(&request.url, &response.requested_url)
        || !same_fetch_url(&request.url, &response.final_url)
        || !response.redirects.is_empty()
        || response.redirect_chain.len() > 1
    {
        return CachePolicy::Skip(CacheSkipReason::Redirected);
    }
    if has_header(&response.headers, "set-cookie") {
        return CachePolicy::Skip(CacheSkipReason::ResponseSetsCookie);
    }
    if vary_selectors(&response.headers).is_none() {
        return CachePolicy::Skip(CacheSkipReason::Vary);
    }

    cache_control_policy(&response.headers)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    fetch_url: String,
    accept: Option<String>,
}

impl CacheKey {
    fn from_request(request: &FetchRequest) -> Self {
        Self {
            fetch_url: normalized_fetch_url(&request.url),
            accept: request.accept.clone(),
        }
    }
}

#[derive(Debug)]
struct CacheEntry {
    response: FetchResponse,
    /// Request-header values that selected this response, captured when the
    /// response was stored (or last revalidated).
    variant: VaryVariant,
    fresh_until: Instant,
    response_bytes: usize,
    last_used: u64,
}

/// One stored response's `Vary` selecting fields and the request values that
/// selected it.
///
/// Field names are lowercase and sorted so equivalent responses compare
/// equal regardless of spelling or order. Each value is the normalized
/// combination of every request field line with that name. `None` means the
/// request did not carry the field at all, while `Some("")` means it carried
/// the field with an empty value; they are deliberately distinct because
/// RFC 9111 §4.1 requires an absent field to match only another absent field.
/// Being strict here can cause a cache miss, but never a wrong variant hit.
#[derive(Clone, Debug, Eq, PartialEq)]
struct VaryVariant {
    selectors: Vec<(String, Option<String>)>,
}

impl VaryVariant {
    /// Captures the selecting values for a response, or returns `None` when
    /// its `Vary` metadata cannot be represented (`Vary: *` or malformed
    /// field names).
    fn from_request(request: &FetchRequest, response_headers: &[Header]) -> Option<Self> {
        let names = vary_selectors(response_headers)?;
        Some(Self {
            selectors: names
                .into_iter()
                .map(|name| {
                    let value = request_header_value(request, &name);
                    (name, value)
                })
                .collect(),
        })
    }

    /// Whether this stored variant was selected by the same request field
    /// values as `request`.
    fn matches(&self, request: &FetchRequest) -> bool {
        self.selectors
            .iter()
            .all(|(name, stored)| request_header_value(request, name).as_ref() == stored.as_ref())
    }
}

/// Returns the index of the freshest entry selected by `request`.
fn fresh_entry_index(
    entries: &[CacheEntry],
    request: &FetchRequest,
    now: Instant,
) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| now < entry.fresh_until && entry.variant.matches(request))
        .max_by_key(|(_, entry)| (entry.fresh_until, entry.last_used))
        .map(|(index, _)| index)
}

/// Returns the index of the most recently freshened stale entry selected by
/// `request`.
fn stale_entry_index(
    entries: &[CacheEntry],
    request: &FetchRequest,
    now: Instant,
) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| now >= entry.fresh_until && entry.variant.matches(request))
        .max_by_key(|(_, entry)| (entry.fresh_until, entry.last_used))
        .map(|(index, _)| index)
}

/// Parses all `Vary` field lines into a sorted, deduplicated list of lowercase
/// field names. Returns `None` for `Vary: *` or a field name that is not an
/// HTTP token, both of which make the response unusable by this cache.
fn vary_selectors(headers: &[Header]) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for header in headers {
        if !header.name.eq_ignore_ascii_case("vary") {
            continue;
        }
        let value = std::str::from_utf8(&header.value).ok()?;
        for field in value.split(',') {
            let field = field.trim_matches([' ', '\t']);
            if field.is_empty() {
                continue;
            }
            if field == "*" {
                return None;
            }
            if !is_http_token(field) {
                return None;
            }
            let name = field.to_ascii_lowercase();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.sort_unstable();
    Some(names)
}

/// Returns the normalized value(s) of a request field used for `Vary`
/// selection.
///
/// Repeated field lines are combined in order with commas, as required when a
/// recipient treats repeated fields as a list. `FetchRequest::accept` is a
/// separate transport field but goes on the wire as `Accept`, so it is used
/// when no explicit `Accept` field was supplied. Other fields injected by the
/// transport with one fixed value for every request (`Accept-Language`,
/// `Accept-Encoding`, client hints) are intentionally not synthesized here:
/// they cannot select different variants while constant, and an explicitly
/// supplied value is present in `headers` and compared normally.
fn request_header_value(request: &FetchRequest, name: &str) -> Option<String> {
    let mut values: Vec<&str> = request
        .headers
        .iter()
        .filter(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect();
    if values.is_empty()
        && name.eq_ignore_ascii_case("accept")
        && let Some(accept) = request.accept.as_deref()
    {
        values.push(accept);
    }
    if values.is_empty() {
        return None;
    }
    Some(
        values
            .into_iter()
            .map(normalize_field_value)
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Trims optional whitespace from a field value and around list commas.
///
/// OWS inside quoted strings is preserved, so the normalization only removes
/// whitespace that HTTP list syntax allows to vary (RFC 9111 §4.1).
fn normalize_field_value(value: &str) -> String {
    let value = value.trim_matches([' ', '\t']);
    let mut normalized = String::with_capacity(value.len());
    let mut in_quotes = false;
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '"' => {
                in_quotes = !in_quotes;
                normalized.push(character);
            }
            '\\' if in_quotes => {
                normalized.push(character);
                if let Some(escaped) = chars.next() {
                    normalized.push(escaped);
                }
            }
            ',' if !in_quotes => {
                while normalized.ends_with(' ') || normalized.ends_with('\t') {
                    normalized.pop();
                }
                normalized.push(',');
                while chars
                    .peek()
                    .is_some_and(|next| *next == ' ' || *next == '\t')
                {
                    chars.next();
                }
            }
            _ => normalized.push(character),
        }
    }
    normalized
}

/// Whether `value` is an RFC 9110 `token`, the only valid shape for a header
/// field name.
fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn request_skip_reason(request: &FetchRequest) -> Option<CacheSkipReason> {
    if !matches!(request.url.scheme(), "http" | "https") {
        return Some(CacheSkipReason::UnsupportedScheme);
    }
    if request.method != HttpMethod::Get {
        return Some(CacheSkipReason::NonGetMethod);
    }
    if request.cookie.is_some() {
        return Some(CacheSkipReason::RequestHasCookie);
    }
    if request.byte_range.is_some() {
        return Some(CacheSkipReason::RequestHasByteRange);
    }
    None
}

fn same_fetch_url(left: &Url, right: &Url) -> bool {
    normalized_fetch_url(left) == normalized_fetch_url(right)
}

fn normalized_fetch_url(url: &Url) -> String {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    normalized.to_string()
}

fn has_header(headers: &[Header], name: &str) -> bool {
    headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case(name))
}

fn merge_headers(previous: &[Header], update: &[Header]) -> Vec<Header> {
    let mut merged = previous.to_vec();
    for replacement in update {
        if matches!(
            replacement.name.to_ascii_lowercase().as_str(),
            "content-length"
                | "content-type"
                | "content-encoding"
                | "content-range"
                | "transfer-encoding"
        ) {
            continue;
        }
        // A 304 update replaces every stored field line with the same name,
        // not just the first one (RFC 9111 §3.2). This matters for list-valued
        // fields such as `Vary`, whose selecting names must not keep stale
        // duplicates from the previous response.
        let insert_at = merged
            .iter()
            .position(|header| header.name.eq_ignore_ascii_case(&replacement.name));
        merged.retain(|header| !header.name.eq_ignore_ascii_case(&replacement.name));
        if let Some(index) = insert_at {
            merged.insert(index, replacement.clone());
        } else {
            merged.push(replacement.clone());
        }
    }
    merged
}

fn header_text<'a>(headers: &'a [Header], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .and_then(|header| std::str::from_utf8(&header.value).ok())
}

fn parse_content_type(value: &str) -> Option<render_net::ContentType> {
    let mut parts = value.split(';');
    let media_type = parts.next()?.trim().to_ascii_lowercase();
    if media_type.is_empty() || !media_type.contains('/') {
        return None;
    }
    let charset = parts.find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"').to_ascii_lowercase())
    });
    Some(render_net::ContentType {
        media_type,
        charset,
    })
}

fn cache_control_policy(headers: &[Header]) -> CachePolicy {
    let mut max_age = None;

    for header in headers {
        if !header.name.eq_ignore_ascii_case("cache-control") {
            continue;
        }
        let Ok(value) = std::str::from_utf8(&header.value) else {
            return CachePolicy::Skip(CacheSkipReason::InvalidMaxAge);
        };

        for directive in value.split(',') {
            let directive = directive.trim();
            if directive.is_empty() {
                continue;
            }
            let (name, value) = directive
                .split_once('=')
                .map_or((directive, None), |(name, value)| {
                    (name.trim(), Some(value.trim()))
                });

            if name.eq_ignore_ascii_case("no-store") {
                return CachePolicy::Skip(CacheSkipReason::NoStore);
            }
            if name.eq_ignore_ascii_case("private") {
                return CachePolicy::Skip(CacheSkipReason::Private);
            }
            if name.eq_ignore_ascii_case("no-cache") {
                return CachePolicy::Skip(CacheSkipReason::NoCache);
            }
            if !name.eq_ignore_ascii_case("max-age") {
                continue;
            }

            let Some(value) = value else {
                return CachePolicy::Skip(CacheSkipReason::InvalidMaxAge);
            };
            let value = value.trim_matches('"');
            let Ok(seconds) = value.parse::<u64>() else {
                return CachePolicy::Skip(CacheSkipReason::InvalidMaxAge);
            };
            let freshness = Duration::from_secs(seconds);
            if max_age.is_some_and(|existing| existing != freshness) {
                return CachePolicy::Skip(CacheSkipReason::InvalidMaxAge);
            }
            max_age = Some(freshness);
        }
    }

    match max_age {
        Some(freshness) if freshness.is_zero() => CachePolicy::Skip(CacheSkipReason::NotFresh),
        Some(freshness) => CachePolicy::Store { freshness },
        None => CachePolicy::Skip(CacheSkipReason::NoExplicitFreshness),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use render_net::{
        ByteRange, CacheValidators, CancelToken, FetchConfig, FetchRequest, FetchResponse, Header,
        HttpMethod, HttpStatus, HttpTransport, Url,
    };

    use super::{
        CacheLookup, CachePolicy, CacheSkipReason, CacheStoreOutcome, HttpCache, HttpCacheConfig,
        cache_policy,
    };

    fn request(url: &str) -> FetchRequest {
        FetchRequest::get(Url::parse(url).expect("test URL"))
    }

    fn response(url: &str, headers: Vec<Header>, body: &[u8]) -> FetchResponse {
        let seed_url = Url::parse("data:text/plain,cache-seed").expect("valid data URL");
        let mut response = HttpTransport::new(FetchConfig::default())
            .fetch(&FetchRequest::get(seed_url), &CancelToken::default())
            .expect("data URL response");
        let url = Url::parse(url).expect("test URL");
        response.requested_url = url.clone();
        response.final_url = url.clone();
        response.redirect_chain = vec![url];
        response.headers = headers;
        response.body = body.to_vec();
        response
    }

    fn header(name: &str, value: &str) -> Header {
        Header {
            name: name.to_owned(),
            value: value.as_bytes().to_vec(),
        }
    }

    fn cacheable_response(url: &str, body: &[u8]) -> FetchResponse {
        response(url, vec![header("Cache-Control", "max-age=60")], body)
    }

    fn request_with_headers(url: &str, headers: &[(&str, &str)]) -> FetchRequest {
        headers.iter().fold(request(url), |request, (name, value)| {
            request.with_header(*name, *value)
        })
    }

    fn vary_response(url: &str, vary: &str, body: &[u8]) -> FetchResponse {
        response(
            url,
            vec![header("Cache-Control", "max-age=60"), header("Vary", vary)],
            body,
        )
    }

    #[test]
    fn a_post_neither_reads_nor_writes_the_cache() {
        let url = "https://example.test/login";
        let get = request(url);
        let post = FetchRequest::new(HttpMethod::Post, Url::parse(url).expect("test URL"))
            .with_body(b"user=a".to_vec());
        let cacheable = cacheable_response(url, b"get body");
        let now = Instant::now();
        let mut cache = HttpCache::default();

        // A GET response is cached, but a POST to the same URL must not read it.
        let epoch = cache.epoch();
        assert_eq!(
            cache.store(&get, &cacheable, now, epoch),
            CacheStoreOutcome::Stored
        );
        assert!(matches!(cache.lookup(&get, now), CacheLookup::Hit(_)));
        assert_eq!(cache.lookup(&post, now), CacheLookup::Miss);

        // A POST response is never stored, so a later GET cannot receive it.
        let post_response = cacheable_response(url, b"post result");
        assert_eq!(
            cache.store(&post, &post_response, now, epoch),
            CacheStoreOutcome::Skipped(CacheSkipReason::NonGetMethod)
        );
        assert_eq!(
            cache_policy(&post, &post_response),
            CachePolicy::Skip(CacheSkipReason::NonGetMethod)
        );
    }

    #[test]
    fn policy_rejects_personalized_and_ambiguous_responses() {
        let request = request("https://example.test/document");
        let cacheable = cacheable_response("https://example.test/document", b"body");

        assert_eq!(
            cache_policy(&request.clone().with_cookie("session=1"), &cacheable),
            CachePolicy::Skip(CacheSkipReason::RequestHasCookie)
        );
        assert_eq!(
            cache_policy(
                &request
                    .clone()
                    .with_byte_range(ByteRange::From { start: 0 }),
                &cacheable
            ),
            CachePolicy::Skip(CacheSkipReason::RequestHasByteRange)
        );
        assert_eq!(
            cache_policy(
                &request,
                &response(
                    "https://example.test/document",
                    vec![
                        header("Set-Cookie", "session=1"),
                        header("Cache-Control", "max-age=60")
                    ],
                    b"body"
                )
            ),
            CachePolicy::Skip(CacheSkipReason::ResponseSetsCookie)
        );
        assert_eq!(
            cache_policy(
                &request,
                &response(
                    "https://example.test/document",
                    vec![header("Cache-Control", "no-store, max-age=60")],
                    b"body"
                )
            ),
            CachePolicy::Skip(CacheSkipReason::NoStore)
        );
    }

    #[test]
    fn stores_and_serves_a_fresh_response() {
        let request = request("https://example.test/document");
        let response = cacheable_response("https://example.test/document", b"cached body");
        let now = Instant::now();
        let mut cache = HttpCache::default();

        assert_eq!(
            cache.store(&request, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        assert_eq!(cache.entry_count(), 1);
        assert_eq!(cache.memory_bytes(), b"cached body".len());

        let CacheLookup::Hit(cached) = cache.lookup(&request, now + Duration::from_secs(59)) else {
            panic!("fresh response should be cached");
        };
        assert_eq!(cached.body, b"cached body");
    }

    #[test]
    fn vary_accept_language_keeps_variants_separate() {
        let url = "https://example.test/page";
        let english = request_with_headers(url, &[("Accept-Language", "en-US")]);
        let french = request_with_headers(url, &[("Accept-Language", "fr-FR")]);
        let english_response = vary_response(url, "Accept-Language", b"hello");
        let french_response = vary_response(url, "Accept-Language", b"bonjour");
        let now = Instant::now();
        let mut cache = HttpCache::default();

        assert_eq!(
            cache.store(&english, &english_response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        assert_eq!(cache.lookup(&french, now), CacheLookup::Miss);
        let CacheLookup::Hit(cached) = cache.lookup(&english, now) else {
            panic!("the English variant should be cached");
        };
        assert_eq!(cached.body, b"hello");
        assert_eq!(cache.entry_count(), 1);

        // A second variant is retained alongside the first instead of
        // replacing it.
        assert_eq!(
            cache.store(&french, &french_response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        assert_eq!(cache.entry_count(), 2);
        let CacheLookup::Hit(cached) = cache.lookup(&french, now) else {
            panic!("the French variant should be cached");
        };
        assert_eq!(cached.body, b"bonjour");
        let CacheLookup::Hit(cached) = cache.lookup(&english, now) else {
            panic!("the English variant should still be cached");
        };
        assert_eq!(cached.body, b"hello");
    }

    #[test]
    fn vary_matching_values_share_a_cached_response() {
        let url = "https://example.test/page";
        let stored = request_with_headers(url, &[("Accept-Language", " en-US ")]);
        let matching = request_with_headers(url, &[("accept-language", "en-US")]);
        let response = vary_response(url, "Accept-Language", b"hello");
        let now = Instant::now();
        let mut cache = HttpCache::default();

        assert_eq!(
            cache.store(&stored, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        let CacheLookup::Hit(cached) = cache.lookup(&matching, now) else {
            panic!("normalized selecting values should match");
        };
        assert_eq!(cached.body, b"hello");
    }

    #[test]
    fn vary_names_and_values_are_normalized() {
        let url = "https://example.test/page";
        let stored = request_with_headers(
            url,
            &[
                ("ACCEPT-LANGUAGE", " en-US "),
                ("accept-language", "\tfr "),
                ("Accept-Encoding", " gzip "),
            ],
        );
        let matching = request_with_headers(
            url,
            &[
                ("accept-language", "en-US,\tfr"),
                ("ACCEPT-ENCODING", "gzip"),
            ],
        );
        let different = request_with_headers(
            url,
            &[
                ("Accept-Language", "en-US, fr;q=0.5"),
                ("Accept-Encoding", "gzip"),
            ],
        );
        let response = vary_response(url, " accept-language ,  ACCEPT-ENCODING ", b"body");
        let now = Instant::now();
        let mut cache = HttpCache::default();

        assert_eq!(
            cache.store(&stored, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        assert!(matches!(cache.lookup(&matching, now), CacheLookup::Hit(_)));
        assert_eq!(cache.lookup(&different, now), CacheLookup::Miss);
        assert_eq!(cache.entry_count(), 1);
    }

    #[test]
    fn vary_treats_absent_and_empty_request_fields_as_distinct() {
        let url = "https://example.test/page";
        let absent = request(url);
        let empty = request_with_headers(url, &[("Accept-Language", "")]);
        let response = vary_response(url, "Accept-Language", b"body");
        let now = Instant::now();

        let mut cache = HttpCache::default();
        assert_eq!(
            cache.store(&absent, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        assert!(matches!(cache.lookup(&absent, now), CacheLookup::Hit(_)));
        assert_eq!(cache.lookup(&empty, now), CacheLookup::Miss);

        let mut cache = HttpCache::default();
        assert_eq!(
            cache.store(&empty, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        assert_eq!(cache.lookup(&absent, now), CacheLookup::Miss);
        assert!(matches!(cache.lookup(&empty, now), CacheLookup::Hit(_)));
    }

    #[test]
    fn vary_star_is_never_stored_or_reused() {
        let url = "https://example.test/page";
        let request = request(url);
        let response = vary_response(url, "*", b"body");
        let now = Instant::now();
        let mut cache = HttpCache::default();

        assert_eq!(
            cache_policy(&request, &response),
            CachePolicy::Skip(CacheSkipReason::Vary)
        );
        assert_eq!(
            cache.store(&request, &response, now, cache.epoch()),
            CacheStoreOutcome::Skipped(CacheSkipReason::Vary)
        );
        assert_eq!(cache.entry_count(), 0);
        assert_eq!(cache.lookup(&request, now), CacheLookup::Miss);
    }

    #[test]
    fn response_without_vary_ignores_unlisted_request_headers() {
        let url = "https://example.test/page";
        let stored = request(url);
        let different = request_with_headers(
            url,
            &[("Accept-Language", "fr-FR"), ("Accept-Encoding", "br")],
        );
        let response = cacheable_response(url, b"body");
        let now = Instant::now();
        let mut cache = HttpCache::default();

        assert_eq!(
            cache.store(&stored, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        let CacheLookup::Hit(cached) = cache.lookup(&different, now) else {
            panic!("a response without Vary is reusable for any request headers");
        };
        assert_eq!(cached.body, b"body");
    }

    #[test]
    fn stale_entries_remain_available_for_revalidation() {
        let request = request("https://example.test/document");
        let response = cacheable_response("https://example.test/document", b"cached body");
        let now = Instant::now();
        let mut cache = HttpCache::default();

        assert_eq!(
            cache.store(&request, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        assert_eq!(
            cache.lookup(&request, now + Duration::from_secs(60)),
            CacheLookup::Miss
        );
        assert_eq!(cache.entry_count(), 1);
        assert_eq!(cache.memory_bytes(), b"cached body".len());
        assert!(
            cache
                .stale_response(&request, now + Duration::from_secs(60))
                .is_some()
        );
    }

    #[test]
    fn conditional_request_uses_expired_response_validators() {
        let request = request("https://example.test/document");
        let response = response(
            "https://example.test/document",
            vec![
                header("Cache-Control", "max-age=1"),
                header("ETag", "\"v1\""),
                header("Last-Modified", "Wed, 21 Oct 2015 07:28:00 GMT"),
            ],
            b"cached body",
        );
        let now = Instant::now();
        let mut cache = HttpCache::default();
        assert_eq!(
            cache.store(&request, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        let conditional = cache
            .revalidation_request(&request, now + Duration::from_secs(1))
            .expect("stale validators");
        assert_eq!(
            conditional.cache_validators,
            Some(CacheValidators {
                etag: Some("\"v1\"".to_owned()),
                last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_owned()),
            })
        );
    }

    #[test]
    fn not_modified_response_reuses_body_and_refreshes_freshness() {
        let request = request("https://example.test/document");
        let cached = response(
            "https://example.test/document",
            vec![
                header("Cache-Control", "max-age=1"),
                header("ETag", "\"v1\""),
                header("Content-Type", "text/plain"),
            ],
            b"cached body",
        );
        let not_modified = response(
            "https://example.test/document",
            vec![
                header("Cache-Control", "max-age=60"),
                header("ETag", "\"v1\""),
            ],
            b"",
        );
        let not_modified = FetchResponse {
            status: HttpStatus::from_u16(304),
            ..not_modified
        };
        let now = Instant::now();
        let mut cache = HttpCache::default();
        assert_eq!(
            cache.store(&request, &cached, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        let merged = cache
            .merge_not_modified(
                &request,
                &not_modified,
                now + Duration::from_secs(1),
                cache.epoch(),
            )
            .expect("304 should merge");
        assert_eq!(merged.status.as_u16(), 200);
        assert_eq!(merged.body, b"cached body");
        let CacheLookup::Hit(hit) = cache.lookup(&request, now + Duration::from_secs(2)) else {
            panic!("merged response should be fresh");
        };
        assert_eq!(hit.body, b"cached body");
    }

    #[test]
    fn not_modified_preserves_vary_and_refreshes_only_the_selected_variant() {
        let url = "https://example.test/page";
        let english = request_with_headers(url, &[("Accept-Language", "en-US")]);
        let french = request_with_headers(url, &[("Accept-Language", "fr-FR")]);
        let english_response = response(
            url,
            vec![
                header("Cache-Control", "max-age=1"),
                header("ETag", "\"en\""),
                header("Vary", "Accept-Language"),
            ],
            b"hello",
        );
        let french_response = response(
            url,
            vec![
                header("Cache-Control", "max-age=1"),
                header("ETag", "\"fr\""),
                header("Vary", "Accept-Language"),
            ],
            b"bonjour",
        );
        let not_modified = response(
            url,
            vec![
                header("Cache-Control", "max-age=60"),
                header("ETag", "\"en\""),
            ],
            b"",
        );
        let not_modified = FetchResponse {
            status: HttpStatus::from_u16(304),
            ..not_modified
        };
        let now = Instant::now();
        let mut cache = HttpCache::default();
        assert_eq!(
            cache.store(&english, &english_response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        assert_eq!(
            cache.store(&french, &french_response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );

        let merged = cache
            .merge_not_modified(
                &english,
                &not_modified,
                now + Duration::from_secs(1),
                cache.epoch(),
            )
            .expect("304 should merge");
        assert_eq!(merged.body, b"hello");
        assert!(
            merged.headers.iter().any(|header| {
                header.name.eq_ignore_ascii_case("vary") && header.value == b"Accept-Language"
            }),
            "a 304 without Vary preserves the stored selecting fields"
        );

        // Only the English variant was refreshed. French remains stale and
        // still matches only French requests.
        let CacheLookup::Hit(cached) = cache.lookup(&english, now + Duration::from_secs(2)) else {
            panic!("the refreshed English variant should be fresh");
        };
        assert_eq!(cached.body, b"hello");
        assert_eq!(
            cache.lookup(&french, now + Duration::from_secs(2)),
            CacheLookup::Miss
        );
        assert!(
            cache
                .stale_response(&french, now + Duration::from_secs(2))
                .is_some()
        );
        assert_eq!(cache.entry_count(), 2);
    }

    #[test]
    fn not_modified_can_update_vary_selecting_fields() {
        let url = "https://example.test/page";
        let request = request_with_headers(
            url,
            &[("Accept-Language", "en-US"), ("Accept-Encoding", "gzip")],
        );
        let cached = response(
            url,
            vec![
                header("Cache-Control", "max-age=1"),
                header("ETag", "\"v1\""),
                header("Vary", "Accept-Language"),
            ],
            b"body",
        );
        let not_modified = response(
            url,
            vec![
                header("Cache-Control", "max-age=60"),
                header("ETag", "\"v1\""),
                header("Vary", "accept-language, ACCEPT-ENCODING"),
            ],
            b"",
        );
        let not_modified = FetchResponse {
            status: HttpStatus::from_u16(304),
            ..not_modified
        };
        let now = Instant::now();
        let mut cache = HttpCache::default();
        assert_eq!(
            cache.store(&request, &cached, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        cache
            .merge_not_modified(
                &request,
                &not_modified,
                now + Duration::from_secs(1),
                cache.epoch(),
            )
            .expect("304 should merge");

        // The updated Vary list now also selects on Accept-Encoding.
        let different_encoding = request_with_headers(
            url,
            &[("Accept-Language", "en-US"), ("Accept-Encoding", "br")],
        );
        assert_eq!(
            cache.lookup(&different_encoding, now + Duration::from_secs(2)),
            CacheLookup::Miss
        );
        assert!(matches!(
            cache.lookup(&request, now + Duration::from_secs(2)),
            CacheLookup::Hit(_)
        ));
    }

    #[test]
    fn clear_advances_epoch_before_late_response_can_store() {
        let request = request("https://example.test/document");
        let response = cacheable_response("https://example.test/document", b"cached body");
        let now = Instant::now();
        let mut cache = HttpCache::default();
        let submitted_epoch = cache.epoch();

        assert_eq!(
            cache.store(&request, &response, now, submitted_epoch),
            CacheStoreOutcome::Stored
        );
        let cleared = cache.clear();
        assert_eq!(cleared.memory_entries, 1);
        assert_eq!(cleared.memory_bytes, b"cached body".len());
        assert!(cleared.epoch > submitted_epoch);
        assert_eq!(cache.entry_count(), 0);
        assert_eq!(
            cache.store(&request, &response, now, submitted_epoch),
            CacheStoreOutcome::Skipped(CacheSkipReason::StaleEpoch)
        );
        assert_eq!(
            cache.store(&request, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
    }

    #[test]
    fn least_recently_used_entry_is_evicted_at_capacity() {
        let config = HttpCacheConfig {
            max_entries: 2,
            max_bytes: 100,
            max_entry_bytes: 100,
        };
        let now = Instant::now();
        let first = request("https://example.test/first");
        let second = request("https://example.test/second");
        let third = request("https://example.test/third");
        let mut cache = HttpCache::new(config);

        for request in [&first, &second] {
            let response = cacheable_response(request.url.as_str(), b"body");
            assert_eq!(
                cache.store(request, &response, now, cache.epoch()),
                CacheStoreOutcome::Stored
            );
        }
        assert!(matches!(cache.lookup(&first, now), CacheLookup::Hit(_)));

        let response = cacheable_response(third.url.as_str(), b"body");
        assert_eq!(
            cache.store(&third, &response, now, cache.epoch()),
            CacheStoreOutcome::Stored
        );
        assert!(matches!(cache.lookup(&first, now), CacheLookup::Hit(_)));
        assert_eq!(cache.lookup(&second, now), CacheLookup::Miss);
        assert!(matches!(cache.lookup(&third, now), CacheLookup::Hit(_)));
    }

    #[test]
    fn oversized_entries_do_not_displace_existing_content() {
        let config = HttpCacheConfig {
            max_entries: 2,
            max_bytes: 8,
            max_entry_bytes: 8,
        };
        let request = request("https://example.test/document");
        let response = cacheable_response("https://example.test/document", b"too large");
        let mut cache = HttpCache::new(config);

        assert_eq!(
            cache.store(&request, &response, Instant::now(), cache.epoch()),
            CacheStoreOutcome::Skipped(CacheSkipReason::EntryTooLarge)
        );
        assert_eq!(cache.entry_count(), 0);
    }
}
