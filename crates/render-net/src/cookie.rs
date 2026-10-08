//! Browser-context cookie storage and request matching.
//!
//! The HTTP transport remains stateless. A caller absorbs `Set-Cookie` response
//! fields into this bounded jar and decorates later requests with the serialized
//! Cookie header returned by [`CookieJar::cookie_header`].

use std::collections::BTreeMap;

use crate::{FetchRequest, FetchResponse, Url};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CookieLimits {
    pub max_cookies: usize,
    pub max_cookie_bytes: usize,
}

impl Default for CookieLimits {
    fn default() -> Self {
        Self {
            max_cookies: 4_096,
            max_cookie_bytes: 4_096,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub host_only: bool,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<SameSite>,
    /// Unix time in seconds at which a persistent cookie expires. `None` marks
    /// a session cookie, which lasts for the life of the browser.
    pub expires_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CookieRejection {
    InvalidOrigin,
    InvalidName,
    InvalidDomain,
    PublicSuffixLikeDomain,
    InsecureSameSiteNone,
    SizeLimit,
    CapacityLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookieIssue {
    pub rejection: CookieRejection,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct CookieJar {
    cookies: BTreeMap<(String, String, String), Cookie>,
    limits: CookieLimits,
}

impl Default for CookieJar {
    fn default() -> Self {
        Self::with_limits(CookieLimits::default())
    }
}

impl CookieJar {
    #[must_use]
    pub const fn with_limits(limits: CookieLimits) -> Self {
        Self {
            cookies: BTreeMap::new(),
            limits,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// Absorb every `Set-Cookie` response field against each response URL in
    /// the redirect chain and the final response URL.
    /// Invalid individual cookies are reported without hiding valid siblings.
    pub fn absorb_response(&mut self, response: &FetchResponse) -> Vec<CookieIssue> {
        let mut issues = Vec::new();
        for redirect in &response.redirects {
            issues.extend(self.absorb_headers(&redirect.url, &redirect.headers));
        }
        issues.extend(self.absorb_headers(&response.final_url, &response.headers));
        issues
    }

    /// Parse and store one Set-Cookie field against the wall clock.
    ///
    /// # Errors
    ///
    /// Returns a typed rejection when the origin, name, domain, attributes, or
    /// configured size/capacity limits make the cookie unsafe to retain.
    pub fn set_cookie(&mut self, origin: &Url, field: &str) -> Result<(), CookieIssue> {
        self.set_cookie_at(origin, field, unix_now())
    }

    /// Parse and store one Set-Cookie field as of `now` (Unix seconds).
    ///
    /// `Max-Age` takes precedence over `Expires` (RFC 6265 §5.3 step 3). A
    /// cookie whose expiry is not after `now` removes any stored cookie with
    /// the same domain, path and name, which is how a server deletes one.
    ///
    /// # Errors
    ///
    /// Returns a typed rejection when the origin, name, domain, attributes, or
    /// configured size/capacity limits make the cookie unsafe to retain.
    pub fn set_cookie_at(
        &mut self,
        origin: &Url,
        field: &str,
        now: i64,
    ) -> Result<(), CookieIssue> {
        let parsed = ParsedCookie::parse(origin, field, self.limits.max_cookie_bytes, now)?;
        let key = (
            parsed.cookie.domain.clone(),
            parsed.cookie.path.clone(),
            parsed.cookie.name.clone(),
        );
        if parsed.remove {
            self.cookies.remove(&key);
            return Ok(());
        }
        if !self.cookies.contains_key(&key) && self.cookies.len() >= self.limits.max_cookies {
            return Err(issue(
                CookieRejection::CapacityLimit,
                "cookie jar capacity reached",
            ));
        }
        self.cookies.insert(key, parsed.cookie);
        Ok(())
    }

    #[must_use]
    pub fn cookie_header(&self, url: &Url) -> Option<String> {
        self.cookie_header_at(url, unix_now())
    }

    /// The Cookie header for `url` as of `now` (Unix seconds). Expired cookies
    /// are never sent, even before they are purged.
    #[must_use]
    pub fn cookie_header_at(&self, url: &Url, now: i64) -> Option<String> {
        let host = url.host_str()?.to_ascii_lowercase();
        let secure_transport = url.scheme() == "https";
        let request_path = normalized_request_path(url.path());
        let mut matches = self
            .cookies
            .values()
            .filter(|cookie| {
                cookie.expires_at.is_none_or(|expiry| expiry > now)
                    && (!cookie.secure || secure_transport)
                    && if cookie.host_only {
                        cookie.domain == host
                    } else {
                        domain_matches(&host, &cookie.domain)
                    }
                    && path_matches(request_path, &cookie.path)
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .path
                .len()
                .cmp(&left.path.len())
                .then_with(|| left.name.cmp(&right.name))
        });
        (!matches.is_empty()).then(|| {
            matches
                .into_iter()
                .map(|cookie| format!("{}={}", cookie.name, cookie.value))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    #[must_use]
    pub fn decorate_request(&self, mut request: FetchRequest) -> FetchRequest {
        request.cookie = self.cookie_header(&request.url);
        request
    }

    /// Drop every cookie whose expiry is not after `now`.
    pub fn purge_expired(&mut self, now: i64) {
        self.cookies
            .retain(|_, cookie| cookie.expires_at.is_none_or(|expiry| expiry > now));
    }

    /// Serialize the persistent cookies that are still live at `now`, for a
    /// profile store. Session cookies are omitted because they must not outlive
    /// the browser session. One cookie per line, fields separated by tabs, with
    /// `\\`, `\t`, `\n` and `\r` escaped inside each field.
    #[must_use]
    pub fn to_store_text(&self, now: i64) -> String {
        let mut text = String::from(STORE_HEADER);
        text.push('\n');
        for cookie in self.cookies.values() {
            let Some(expiry) = cookie.expires_at.filter(|expiry| *expiry > now) else {
                continue;
            };
            let same_site = match cookie.same_site {
                None => "-",
                Some(SameSite::Strict) => "strict",
                Some(SameSite::Lax) => "lax",
                Some(SameSite::None) => "none",
            };
            let fields = [
                escape_field(&cookie.name),
                escape_field(&cookie.value),
                escape_field(&cookie.domain),
                escape_field(&cookie.path),
                flag(cookie.host_only).to_owned(),
                flag(cookie.secure).to_owned(),
                flag(cookie.http_only).to_owned(),
                same_site.to_owned(),
                expiry.to_string(),
            ];
            text.push_str(&fields.join("\t"));
            text.push('\n');
        }
        text
    }

    /// Load cookies written by [`CookieJar::to_store_text`], keeping only the
    /// ones still live at `now`. A malformed line is skipped rather than failing
    /// the whole store, so one damaged record cannot erase the rest. Loading
    /// obeys the same capacity limit as live absorption.
    ///
    /// Returns the number of cookies loaded.
    pub fn load_store_text(&mut self, text: &str, now: i64) -> usize {
        let mut lines = text.lines();
        if lines.next() != Some(STORE_HEADER) {
            // Not a store this version wrote: loading nothing is safer than
            // guessing at its layout.
            return 0;
        }
        let mut loaded = 0;
        for line in lines {
            let Some(cookie) = parse_store_line(line) else {
                continue;
            };
            if cookie.expires_at.is_none_or(|expiry| expiry <= now) {
                continue;
            }
            let key = (
                cookie.domain.clone(),
                cookie.path.clone(),
                cookie.name.clone(),
            );
            if !self.cookies.contains_key(&key) && self.cookies.len() >= self.limits.max_cookies {
                break;
            }
            self.cookies.insert(key, cookie);
            loaded += 1;
        }
        loaded
    }

    fn absorb_headers(&mut self, origin: &Url, headers: &[crate::Header]) -> Vec<CookieIssue> {
        headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("set-cookie"))
            .filter_map(|header| std::str::from_utf8(&header.value).ok())
            .filter_map(|value| self.set_cookie(origin, value).err())
            .collect()
    }
}

struct ParsedCookie {
    cookie: Cookie,
    remove: bool,
    max_age: Option<i64>,
    expires: Option<i64>,
}

impl ParsedCookie {
    fn parse(origin: &Url, field: &str, byte_limit: usize, now: i64) -> Result<Self, CookieIssue> {
        let Some(host) = origin.host_str().map(str::to_ascii_lowercase) else {
            return Err(issue(
                CookieRejection::InvalidOrigin,
                "cookie origin has no host",
            ));
        };
        let mut parts = field.split(';');
        let pair = parts.next().unwrap_or_default();
        if pair.len() > byte_limit {
            return Err(issue(
                CookieRejection::SizeLimit,
                "cookie exceeds byte limit",
            ));
        }
        let Some((name, value)) = pair.split_once('=') else {
            return Err(issue(
                CookieRejection::InvalidName,
                "cookie has no name/value separator",
            ));
        };
        let name = name.trim();
        if !valid_cookie_name(name) {
            return Err(issue(
                CookieRejection::InvalidName,
                "cookie name is invalid",
            ));
        }

        let mut parsed = Self {
            cookie: Cookie {
                name: name.to_owned(),
                value: value.trim().to_owned(),
                domain: host.clone(),
                path: default_path(origin.path()),
                host_only: true,
                secure: false,
                http_only: false,
                same_site: None,
                expires_at: None,
            },
            remove: false,
            max_age: None,
            expires: None,
        };
        for attribute in parts {
            parsed.apply_attribute(&host, attribute)?;
        }
        if parsed.cookie.same_site == Some(SameSite::None) && !parsed.cookie.secure {
            return Err(issue(
                CookieRejection::InsecureSameSiteNone,
                "SameSite=None requires Secure",
            ));
        }
        // RFC 6265 §5.3 step 3: Max-Age wins over Expires. A non-positive
        // Max-Age expires the cookie at once, so it deletes the stored one.
        let expiry = match (parsed.max_age, parsed.expires) {
            (Some(seconds), _) if seconds <= 0 => Some(i64::MIN),
            (Some(seconds), _) => Some(now.saturating_add(seconds)),
            (None, at) => at,
        };
        match expiry {
            Some(at) if at <= now => parsed.remove = true,
            _ => parsed.cookie.expires_at = expiry,
        }
        Ok(parsed)
    }

    fn apply_attribute(&mut self, host: &str, attribute: &str) -> Result<(), CookieIssue> {
        let attribute = attribute.trim();
        let (key, value) = attribute.split_once('=').unwrap_or((attribute, ""));
        if key.eq_ignore_ascii_case("domain") {
            self.apply_domain(host, value)?;
        } else if key.eq_ignore_ascii_case("path") && value.starts_with('/') {
            value.clone_into(&mut self.cookie.path);
        } else if key.eq_ignore_ascii_case("secure") {
            self.cookie.secure = true;
        } else if key.eq_ignore_ascii_case("httponly") {
            self.cookie.http_only = true;
        } else if key.eq_ignore_ascii_case("samesite") {
            self.cookie.same_site = parse_same_site(value).or(self.cookie.same_site);
        } else if key.eq_ignore_ascii_case("max-age") {
            // An attribute that does not parse is ignored (RFC 6265 §5.2.2).
            if let Ok(seconds) = value.trim().parse::<i64>() {
                self.max_age = Some(seconds);
            }
        } else if key.eq_ignore_ascii_case("expires") {
            self.expires = parse_cookie_date(value);
        }
        Ok(())
    }

    fn apply_domain(&mut self, host: &str, value: &str) -> Result<(), CookieIssue> {
        let candidate = value.trim().trim_start_matches('.').to_ascii_lowercase();
        if candidate.is_empty() || !domain_matches(host, &candidate) {
            return Err(issue(
                CookieRejection::InvalidDomain,
                "cookie Domain does not match origin",
            ));
        }
        if !candidate.contains('.') && candidate != host {
            return Err(issue(
                CookieRejection::PublicSuffixLikeDomain,
                "cookie Domain is too broad",
            ));
        }
        self.cookie.domain = candidate;
        self.cookie.host_only = false;
        Ok(())
    }
}

fn parse_same_site(value: &str) -> Option<SameSite> {
    if value.eq_ignore_ascii_case("strict") {
        Some(SameSite::Strict)
    } else if value.eq_ignore_ascii_case("lax") {
        Some(SameSite::Lax)
    } else if value.eq_ignore_ascii_case("none") {
        Some(SameSite::None)
    } else {
        None
    }
}

fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte > 0x20
                && byte < 0x7f
                && !matches!(
                    byte,
                    b'(' | b')'
                        | b'<'
                        | b'>'
                        | b'@'
                        | b','
                        | b';'
                        | b':'
                        | b'\\'
                        | b'"'
                        | b'/'
                        | b'['
                        | b']'
                        | b'?'
                        | b'='
                        | b'{'
                        | b'}'
                )
        })
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn default_path(path: &str) -> String {
    if !path.starts_with('/') || path == "/" {
        return "/".to_owned();
    }
    let Some(index) = path.rfind('/') else {
        return "/".to_owned();
    };
    if index == 0 {
        "/".to_owned()
    } else {
        path[..index].to_owned()
    }
}

fn normalized_request_path(path: &str) -> &str {
    if path.is_empty() { "/" } else { path }
}

fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie
        || request
            .strip_prefix(cookie)
            .is_some_and(|suffix| cookie.ends_with('/') || suffix.as_bytes().first() == Some(&b'/'))
}

fn issue(rejection: CookieRejection, message: impl Into<String>) -> CookieIssue {
    CookieIssue {
        rejection,
        message: message.into(),
    }
}

/// The wall clock as Unix seconds. A clock set before 1970 reads as 0, which
/// keeps every persistent cookie with a future expiry live.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Parse a cookie date as RFC 6265 §5.1.1 specifies. It accepts the IMF-fixdate
/// and the obsolete RFC 850 and asctime forms that servers still send, and
/// returns Unix seconds. An unparseable date is `None`, and the caller ignores
/// the attribute.
fn parse_cookie_date(value: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let is_delimiter = |character: char| {
        character == '\t'
            || (' '..='/').contains(&character)
            || (';'..='@').contains(&character)
            || ('['..='`').contains(&character)
            || ('{'..='~').contains(&character)
    };
    let mut time = None;
    let mut day = None;
    let mut month = None;
    let mut year = None;
    // Each token fills the first field of the four that it matches and that is
    // still empty, in the order RFC 6265 §5.1.1 gives.
    for token in value.split(is_delimiter).filter(|token| !token.is_empty()) {
        if time.is_none() {
            if let Some(parsed) = parse_time_token(token) {
                time = Some(parsed);
                continue;
            }
        }
        if day.is_none() {
            if let Some((number, _)) = leading_number(token, 1, 2) {
                day = Some(number);
                continue;
            }
        }
        if month.is_none() {
            if let Some(index) = parse_month_token(token, &MONTHS) {
                month = Some(index);
                continue;
            }
        }
        if year.is_none() {
            if let Some((number, _)) = leading_number(token, 2, 4) {
                year = Some(number);
            }
        }
    }
    let (hour, minute, second) = time?;
    let day = day?;
    let month = month?;
    // Two-digit years take the century the RFC assigns (70-99 is 19xx, 0-69 is 20xx).
    let year = match year? {
        value @ 70..=99 => value + 1900,
        value @ 0..=69 => value + 2000,
        value => value,
    };
    if !(1..=31).contains(&day) || year < 1601 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// `hh:mm:ss`, where the last field may be followed by non-digit text.
fn parse_time_token(token: &str) -> Option<(i64, i64, i64)> {
    let (hour, rest) = leading_number(token, 1, 2)?;
    let rest = rest.strip_prefix(':')?;
    let (minute, rest) = leading_number(rest, 1, 2)?;
    let rest = rest.strip_prefix(':')?;
    let (second, _) = leading_number(rest, 1, 2)?;
    Some((hour, minute, second))
}

/// The month's first three letters, which must not be followed by another
/// letter (RFC 6265 §5.1.1 `month`).
fn parse_month_token(token: &str, months: &[&str; 12]) -> Option<i64> {
    let prefix = token.get(..3)?;
    let index = months
        .iter()
        .position(|name| prefix.eq_ignore_ascii_case(name))?;
    if token
        .get(3..)?
        .starts_with(|character: char| character.is_ascii_alphabetic())
    {
        return None;
    }
    i64::try_from(index + 1).ok()
}

/// The leading run of ASCII digits, when its length is within `min..=max`,
/// together with the text that follows it.
fn leading_number(text: &str, min: usize, max: usize) -> Option<(i64, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits < min || digits > max {
        return None;
    }
    let value = text.get(..digits)?.parse::<i64>().ok()?;
    Some((value, text.get(digits..)?))
}

/// Days from 1970-01-01 to the given proleptic Gregorian date, by the standard
/// era-based civil calendar algorithm.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

const STORE_HEADER: &str = "# rENDER cookie store v1";

fn flag(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

fn parse_flag(text: &str) -> Option<bool> {
    match text {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn escape_field(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn unescape_field(text: &str) -> Option<String> {
    let mut plain = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            plain.push(character);
            continue;
        }
        match characters.next()? {
            '\\' => plain.push('\\'),
            't' => plain.push('\t'),
            'n' => plain.push('\n'),
            'r' => plain.push('\r'),
            _ => return None,
        }
    }
    Some(plain)
}

fn parse_store_line(line: &str) -> Option<Cookie> {
    let fields = line.split('\t').collect::<Vec<_>>();
    let [
        name,
        value,
        domain,
        path,
        host_only,
        secure,
        http_only,
        same_site,
        expires_at,
    ] = fields.as_slice()
    else {
        return None;
    };
    let name = unescape_field(name)?;
    if !valid_cookie_name(&name) {
        return None;
    }
    Some(Cookie {
        name,
        value: unescape_field(value)?,
        domain: unescape_field(domain)?,
        path: unescape_field(path)?,
        host_only: parse_flag(host_only)?,
        secure: parse_flag(secure)?,
        http_only: parse_flag(http_only)?,
        same_site: match *same_site {
            "-" => None,
            "strict" => Some(SameSite::Strict),
            "lax" => Some(SameSite::Lax),
            "none" => Some(SameSite::None),
            _ => return None,
        },
        expires_at: Some(expires_at.parse::<i64>().ok()?),
    })
}

#[cfg(test)]
mod tests {
    use super::{CookieJar, CookieRejection, parse_cookie_date};
    use crate::{FetchRequest, Url};

    #[test]
    fn host_domain_path_secure_and_deletion_rules_are_enforced() {
        let https = Url::parse("https://login.example.test/account/start").expect("URL");
        let mut jar = CookieJar::default();
        jar.set_cookie(&https, "host=H; Path=/account; HttpOnly")
            .expect("host cookie");
        jar.set_cookie(
            &https,
            "domain=D; Domain=example.test; Path=/; Secure; SameSite=None",
        )
        .expect("domain cookie");

        assert_eq!(
            jar.cookie_header(&Url::parse("https://login.example.test/account/profile").unwrap()),
            Some("host=H; domain=D".to_owned())
        );
        assert_eq!(
            jar.cookie_header(&Url::parse("https://www.example.test/").unwrap()),
            Some("domain=D".to_owned())
        );
        assert_eq!(
            jar.cookie_header(&Url::parse("http://www.example.test/").unwrap()),
            None
        );

        jar.set_cookie(&https, "host=gone; Path=/account; Max-Age=0")
            .expect("deletion");
        assert_eq!(
            jar.cookie_header(&Url::parse("https://login.example.test/account/profile").unwrap()),
            Some("domain=D".to_owned())
        );
    }

    #[test]
    fn invalid_domains_and_insecure_samesite_none_fail_closed() {
        let origin = Url::parse("https://example.test/").expect("URL");
        let mut jar = CookieJar::default();
        assert_eq!(
            jar.set_cookie(&origin, "bad=1; Domain=attacker.test")
                .expect_err("foreign domain")
                .rejection,
            CookieRejection::InvalidDomain
        );
        assert_eq!(
            jar.set_cookie(&origin, "bad=1; SameSite=None")
                .expect_err("SameSite None without Secure")
                .rejection,
            CookieRejection::InsecureSameSiteNone
        );
        assert!(jar.is_empty());
    }

    #[test]
    fn request_decoration_is_owned_by_the_browser_context() {
        let origin = Url::parse("https://example.test/login").expect("URL");
        let mut jar = CookieJar::default();
        jar.set_cookie(&origin, "session=abc; Path=/; Secure")
            .expect("session cookie");

        let request = jar.decorate_request(FetchRequest::get(
            Url::parse("https://example.test/profile").expect("request URL"),
        ));
        assert_eq!(request.cookie.as_deref(), Some("session=abc"));
    }

    /// 1994-11-06 08:49:37 UTC, the RFC 6265 example date.
    const RFC_EXAMPLE_INSTANT: i64 = 784_111_777;

    #[test]
    fn every_cookie_date_format_names_the_same_instant() {
        for field in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(
                parse_cookie_date(field),
                Some(RFC_EXAMPLE_INSTANT),
                "{field}"
            );
        }
        assert_eq!(
            parse_cookie_date("Thu, 29 Feb 2024 12:00:00 GMT"),
            Some(1_709_208_000)
        );
        assert_eq!(parse_cookie_date("Thu, 01 Jan 70 00:00:00 GMT"), Some(0));
    }

    #[test]
    fn malformed_cookie_dates_are_rejected_rather_than_guessed() {
        assert_eq!(parse_cookie_date("yesterday"), None);
        assert_eq!(parse_cookie_date("Sun, 32 Nov 1994 08:49:37 GMT"), None);
        assert_eq!(parse_cookie_date("Sun, 06 Nov 1994 24:00:00 GMT"), None);
        assert_eq!(parse_cookie_date("Sun, 06 Nov 1600 08:49:37 GMT"), None);
        assert_eq!(parse_cookie_date("Sun, 06 Nov 1994 08:49 GMT"), None);
    }

    #[test]
    fn max_age_bounds_a_cookie_and_expired_cookies_are_not_sent() {
        let origin = Url::parse("https://example.test/").expect("URL");
        let url = Url::parse("https://example.test/").expect("request URL");
        let now = 1_000_000;
        let mut jar = CookieJar::default();
        jar.set_cookie_at(&origin, "session=abc; Path=/; Max-Age=3600", now)
            .expect("persistent cookie");

        assert_eq!(
            jar.cookie_header_at(&url, now + 3_599).as_deref(),
            Some("session=abc")
        );
        assert_eq!(jar.cookie_header_at(&url, now + 3_600), None);
    }

    #[test]
    fn max_age_takes_precedence_over_expires() {
        let origin = Url::parse("https://example.test/").expect("URL");
        let mut jar = CookieJar::default();
        // The Expires date is in the past, so only Max-Age keeps the cookie.
        jar.set_cookie_at(
            &origin,
            "kept=1; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=60",
            1_000,
        )
        .expect("Max-Age wins");
        let stored = jar.cookies.values().next().expect("stored cookie");
        assert_eq!(stored.expires_at, Some(1_060));
    }

    #[test]
    fn a_past_expiry_deletes_the_stored_cookie() {
        let origin = Url::parse("https://example.test/").expect("URL");
        let url = Url::parse("https://example.test/").expect("request URL");
        let mut jar = CookieJar::default();
        jar.set_cookie_at(&origin, "token=x; Path=/; Max-Age=600", 1_000)
            .expect("set");
        jar.set_cookie_at(
            &origin,
            "token=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
            1_000,
        )
        .expect("delete");
        assert_eq!(jar.cookie_header_at(&url, 1_001), None);
        assert!(jar.is_empty());
    }

    #[test]
    fn an_unparseable_expires_leaves_a_session_cookie() {
        let origin = Url::parse("https://example.test/").expect("URL");
        let mut jar = CookieJar::default();
        jar.set_cookie_at(&origin, "s=1; Expires=yesterday", 1_000)
            .expect("session cookie");
        assert_eq!(
            jar.cookies.values().next().expect("cookie").expires_at,
            None
        );
    }

    #[test]
    fn store_round_trip_keeps_live_persistent_cookies_and_drops_session_ones() {
        let origin = Url::parse("https://example.test/").expect("URL");
        let url = Url::parse("https://example.test/").expect("request URL");
        let now = 1_000;
        let mut jar = CookieJar::default();
        jar.set_cookie_at(&origin, "session=gone; Path=/", now)
            .expect("session cookie");
        // The tab is inside the value, so the escaping has to survive a round trip.
        jar.set_cookie_at(&origin, "account=a\tb\\c; Path=/; Max-Age=600; Secure", now)
            .expect("persistent cookie");

        let text = jar.to_store_text(now);
        assert!(!text.contains("session=gone"));

        let mut restored = CookieJar::default();
        assert_eq!(restored.load_store_text(&text, now + 1), 1);
        let plain_http = Url::parse("http://example.test/").expect("URL");
        assert_eq!(
            restored.cookie_header_at(&plain_http, now + 1).as_deref(),
            None,
            "the cookie is Secure, so plain HTTP must not receive it"
        );
        assert_eq!(
            restored.cookie_header_at(&url, now + 1).as_deref(),
            Some("account=a\tb\\c")
        );
    }

    #[test]
    fn loading_skips_malformed_and_expired_lines_without_losing_the_rest() {
        let text = "# rENDER cookie store v1\n\
            not a cookie record\n\
            old\tgone\texample.test\t/\t1\t0\t0\t-\t500\n\
            kept\tyes\texample.test\t/\t1\t0\t0\t-\t9000\n\
            bad\tflag\texample.test\t/\tmaybe\t0\t0\t-\t9000\n";
        let mut jar = CookieJar::default();
        assert_eq!(jar.load_store_text(text, 1_000), 1);
        let url = Url::parse("https://example.test/").expect("request URL");
        assert_eq!(
            jar.cookie_header_at(&url, 1_000).as_deref(),
            Some("kept=yes")
        );
    }
}
