//! Conservative, in-memory HTTP response caching for the browser shell.
//!
//! The cache intentionally accepts only anonymous, direct HTTP(S) `200`
//! responses with an explicit `Cache-Control: max-age` lifetime. Expired
//! entries remain available as validator metadata until they are replaced or
//! evicted, allowing callers to perform conditional revalidation without
//! blocking the UI thread.

pub mod disk;
pub mod payload;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use render_net::{CacheValidators, FetchRequest, FetchResponse, Header, HttpStatus, Url};

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
    RequestHasCookie,
    RequestHasByteRange,
    ResponseStatus(u16),
    Redirected,
    ResponseSetsCookie,
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
    entries: HashMap<CacheKey, CacheEntry>,
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
        self.entries.len()
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
        let Some(fresh_until) = self.entries.get(&key).map(|entry| entry.fresh_until) else {
            return CacheLookup::Miss;
        };

        if now >= fresh_until {
            return CacheLookup::Miss;
        }

        let last_used = self.next_access();
        let Some(entry) = self.entries.get_mut(&key) else {
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
        let is_stale = self
            .entries
            .get(&key)
            .is_some_and(|entry| now >= entry.fresh_until);
        if !is_stale {
            return None;
        }
        let last_used = self.next_access();
        self.entries.get_mut(&key).map(|entry| {
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
        let previous_entry = self.entries.get(&key)?;
        if now < previous_entry.fresh_until {
            return None;
        }
        let previous = previous_entry.response.clone();
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
        let freshness = match cache_policy(request, &merged) {
            CachePolicy::Store { freshness } => Some(freshness),
            // A 304 still supplies a usable representation. If its updated
            // directives make it uncacheable, evict the old entry but return
            // the merged body to the resource pipeline.
            CachePolicy::Skip(_) => {
                self.remove_entry(&key);
                return Some(merged);
            }
        };
        let fresh_until = now.checked_add(freshness?)?;
        let response_bytes = merged.body.len();
        let last_used = self.next_access();
        let entry = CacheEntry {
            response: merged.clone(),
            fresh_until,
            response_bytes,
            last_used,
        };
        if let Some(previous) = self.entries.insert(key, entry) {
            self.memory_bytes = self.memory_bytes.saturating_sub(previous.response_bytes);
        }
        self.memory_bytes = self.memory_bytes.saturating_add(response_bytes);
        self.evict_to_limits();
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
            fresh_until,
            response_bytes,
            last_used: self.next_access(),
        };

        if let Some(previous) = self.entries.insert(key, entry) {
            self.memory_bytes = self.memory_bytes.saturating_sub(previous.response_bytes);
        }
        self.memory_bytes = self.memory_bytes.saturating_add(response_bytes);
        self.evict_to_limits();

        CacheStoreOutcome::Stored
    }

    /// Removes all in-memory entries and advances the cache epoch first.
    #[must_use]
    pub fn clear(&mut self) -> CacheClearResult {
        self.epoch = CacheEpoch(self.epoch.0.saturating_add(1));
        let result = CacheClearResult {
            memory_entries: self.entries.len(),
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

    fn remove_entry(&mut self, key: &CacheKey) {
        if let Some(entry) = self.entries.remove(key) {
            self.memory_bytes = self.memory_bytes.saturating_sub(entry.response_bytes);
        }
    }

    fn evict_to_limits(&mut self) {
        while self.entries.len() > self.config.max_entries
            || self.memory_bytes > self.config.max_bytes
        {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            self.remove_entry(&key);
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
    if has_nonempty_header(&response.headers, "vary") {
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
    fresh_until: Instant,
    response_bytes: usize,
    last_used: u64,
}

fn request_skip_reason(request: &FetchRequest) -> Option<CacheSkipReason> {
    if !matches!(request.url.scheme(), "http" | "https") {
        return Some(CacheSkipReason::UnsupportedScheme);
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

fn has_nonempty_header(headers: &[Header], name: &str) -> bool {
    headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case(name)
            && match std::str::from_utf8(&header.value) {
                Ok(value) => !value.trim().is_empty(),
                Err(_) => true,
            }
    })
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
        if let Some(existing) = merged
            .iter_mut()
            .find(|header| header.name.eq_ignore_ascii_case(&replacement.name))
        {
            *existing = replacement.clone();
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
        HttpStatus, HttpTransport, Url,
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
