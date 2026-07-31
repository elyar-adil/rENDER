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

    /// Absorb every `Set-Cookie` response field against the final response URL.
    /// Invalid individual cookies are reported without hiding valid siblings.
    pub fn absorb_response(&mut self, response: &FetchResponse) -> Vec<CookieIssue> {
        response
            .headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("set-cookie"))
            .filter_map(|header| std::str::from_utf8(&header.value).ok())
            .filter_map(|value| self.set_cookie(&response.final_url, value).err())
            .collect()
    }

    /// Parse and store one Set-Cookie field. `Max-Age<=0` removes the matching
    /// cookie. Absolute `Expires` persistence is intentionally deferred until a
    /// wall-clock service is introduced; session cookies remain in memory.
    ///
    /// # Errors
    ///
    /// Returns a typed rejection when the origin, name, domain, attributes, or
    /// configured size/capacity limits make the cookie unsafe to retain.
    pub fn set_cookie(&mut self, origin: &Url, field: &str) -> Result<(), CookieIssue> {
        let parsed = ParsedCookie::parse(origin, field, self.limits.max_cookie_bytes)?;
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
        let host = url.host_str()?.to_ascii_lowercase();
        let secure_transport = url.scheme() == "https";
        let request_path = normalized_request_path(url.path());
        let mut matches = self
            .cookies
            .values()
            .filter(|cookie| {
                (!cookie.secure || secure_transport)
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
}

struct ParsedCookie {
    cookie: Cookie,
    remove: bool,
}

impl ParsedCookie {
    fn parse(origin: &Url, field: &str, byte_limit: usize) -> Result<Self, CookieIssue> {
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
            },
            remove: false,
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
            self.remove = value
                .trim()
                .parse::<i64>()
                .is_ok_and(|seconds| seconds <= 0);
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

#[cfg(test)]
mod tests {
    use super::{CookieJar, CookieRejection};
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
}
