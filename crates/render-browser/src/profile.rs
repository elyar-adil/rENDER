//! The browser profile: state that outlives one page and, for persistent
//! cookies, one run of the browser.
//!
//! Cookies are the first profile store. There is one jar for the whole browser,
//! so a sign-in in one tab applies in every other tab. Persistent cookies (those
//! with `Expires` or `Max-Age`) are written to `cookies.txt` in the profile
//! directory, so they survive a restart. Session cookies stay in memory, because
//! they end with the browser session.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use render_net::{CookieIssue, CookieJar, FetchRequest, FetchResponse};

const COOKIE_FILE: &str = "cookies.txt";
const STORAGE_FILE: &str = "local-storage.txt";
const STORAGE_HEADER: &str = "# rENDER local storage v1";

/// The largest `localStorage` area the browser persists for one origin. The
/// engine does not enforce a quota yet, so the browser refuses a change that
/// would grow an area past this, rather than writing unbounded data to disk.
const MAX_ORIGIN_STORAGE_BYTES: usize = 5 * 1024 * 1024;

/// The browser-wide cookie jar and its on-disk copy.
pub(super) struct ProfileCookies {
    jar: CookieJar,
    /// `None` keeps the jar in memory only: a browser without a usable profile
    /// directory, and tests, must not write to the user's real profile.
    path: Option<PathBuf>,
    dirty: bool,
}

impl ProfileCookies {
    /// Open the cookie store in `directory`, loading the persistent cookies that
    /// are still live. A missing file is a first run. A damaged one starts empty
    /// and says so, instead of blocking the browser from starting.
    pub(super) fn open(directory: Option<PathBuf>) -> Self {
        let path = directory.map(|directory| directory.join(COOKIE_FILE));
        let mut jar = CookieJar::default();
        if let Some(path) = &path {
            match fs::read_to_string(path) {
                Ok(text) => {
                    jar.load_store_text(&text, unix_now());
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => eprintln!("render-browser cookies not loaded: {error}"),
            }
        }
        Self {
            jar,
            path,
            dirty: false,
        }
    }

    pub(super) fn decorate_request(&self, request: FetchRequest) -> FetchRequest {
        self.jar.decorate_request(request)
    }

    /// Absorb the `Set-Cookie` fields of a response, marking the store dirty only
    /// when the response actually carried one.
    pub(super) fn absorb_response(&mut self, response: &FetchResponse) -> Vec<CookieIssue> {
        let carries_cookies = response
            .headers
            .iter()
            .chain(
                response
                    .redirects
                    .iter()
                    .flat_map(|redirect| &redirect.headers),
            )
            .any(|header| header.name.eq_ignore_ascii_case("set-cookie"));
        if carries_cookies {
            self.dirty = true;
        }
        self.jar.absorb_response(response)
    }

    /// Write the store if anything changed since the last write. The event loop
    /// calls this once per turn, so a burst of responses costs one write.
    pub(super) fn flush_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(path) = &self.path else {
            return;
        };
        let text = self.jar.to_store_text(unix_now());
        if let Err(error) = write_store(path, &text) {
            eprintln!("render-browser cookies not saved: {error}");
        }
    }
}

impl Drop for ProfileCookies {
    /// Last chance to keep sign-ins made since the previous turn.
    fn drop(&mut self) {
        self.flush_if_dirty();
    }
}

/// The `localStorage` areas of persistent origins, keyed by serialized origin.
///
/// Each document keeps its own copy of its origin's area and reports its
/// changes as a difference from the copy it started with. The store applies
/// only the keys that changed, so two tabs of one origin merge their writes
/// rather than overwriting each other with stale copies.
pub(super) struct ProfileStorage {
    origins: BTreeMap<String, BTreeMap<String, String>>,
    path: Option<PathBuf>,
    dirty: bool,
}

impl ProfileStorage {
    /// Open the store in `directory`, loading every persisted area. A missing
    /// file is a first run. A damaged file starts empty and says so.
    pub(super) fn open(directory: Option<PathBuf>) -> Self {
        let path = directory.map(|directory| directory.join(STORAGE_FILE));
        let mut origins: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        if let Some(path) = &path {
            match fs::read_to_string(path) {
                Ok(text) => {
                    for (origin, key, value) in parse_storage_text(&text) {
                        origins.entry(origin).or_default().insert(key, value);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => eprintln!("render-browser local storage not loaded: {error}"),
            }
        }
        Self {
            origins,
            path,
            dirty: false,
        }
    }

    /// The persisted entries of `origin`, in key order.
    pub(super) fn area(&self, origin: &str) -> Vec<(String, String)> {
        self.origins.get(origin).map_or_else(Vec::new, |area| {
            area.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
    }

    /// Apply the difference between a document's `before` and `after` copies of
    /// `origin`'s area. Keys the document did not touch keep whatever the store
    /// holds now, which is what lets concurrent documents of one origin merge.
    pub(super) fn apply_changes(
        &mut self,
        origin: &str,
        before: &[(String, String)],
        after: &[(String, String)],
    ) {
        let before: BTreeMap<&str, &str> = before
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let after: BTreeMap<&str, &str> = after
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let mut area = self.origins.get(origin).cloned().unwrap_or_default();
        let mut changed = false;
        for key in before.keys() {
            if !after.contains_key(key) {
                changed |= area.remove(*key).is_some();
            }
        }
        for (key, value) in &after {
            if before.get(key) != Some(value) {
                area.insert((*key).to_owned(), (*value).to_owned());
                changed = true;
            }
        }
        if !changed {
            return;
        }
        let bytes: usize = area
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum();
        if bytes > MAX_ORIGIN_STORAGE_BYTES {
            eprintln!(
                "render-browser local storage for {origin} is over {MAX_ORIGIN_STORAGE_BYTES} bytes; this change is not persisted"
            );
            return;
        }
        if area.is_empty() {
            self.origins.remove(origin);
        } else {
            self.origins.insert(origin.to_owned(), area);
        }
        self.dirty = true;
    }

    /// Write the store if anything changed since the last write.
    pub(super) fn flush_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(path) = &self.path else {
            return;
        };
        let text = self.serialize();
        if let Err(error) = write_store(path, &text) {
            eprintln!("render-browser local storage not saved: {error}");
        }
    }

    fn serialize(&self) -> String {
        let mut text = String::from(STORAGE_HEADER);
        text.push('\n');
        for (origin, area) in &self.origins {
            for (key, value) in area {
                text.push_str(&escape_field(origin));
                text.push('\t');
                text.push_str(&escape_field(key));
                text.push('\t');
                text.push_str(&escape_field(value));
                text.push('\n');
            }
        }
        text
    }
}

impl Drop for ProfileStorage {
    fn drop(&mut self) {
        self.flush_if_dirty();
    }
}

/// Parse a store written by [`ProfileStorage::serialize`]. A malformed line is
/// skipped, so one damaged record cannot erase the rest. A file without the
/// header is not a store this version wrote, and yields nothing.
fn parse_storage_text(text: &str) -> Vec<(String, String, String)> {
    let mut lines = text.lines();
    if lines.next() != Some(STORAGE_HEADER) {
        return Vec::new();
    }
    lines
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            match fields.as_slice() {
                [origin, key, value] => Some((
                    unescape_field(origin)?,
                    unescape_field(key)?,
                    unescape_field(value)?,
                )),
                _ => None,
            }
        })
        .collect()
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

/// The directory that holds the profile. `RENDER_PROFILE_DIR` names it
/// directly; otherwise it is `rENDER` under the platform data directory.
pub(super) fn profile_directory() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("RENDER_PROFILE_DIR") {
        let root = PathBuf::from(root);
        return root.is_absolute().then_some(root);
    }
    platform_data_directory().map(|base| base.join("rENDER"))
}

#[cfg(target_os = "macos")]
fn platform_data_directory() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
}

#[cfg(target_os = "windows")]
fn platform_data_directory() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_data_directory() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

#[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
fn platform_data_directory() -> Option<PathBuf> {
    None
}

/// Replace the store atomically: write a sibling temporary file, flush it to
/// disk, then rename it over the old one, so a crash mid-write leaves either the
/// previous store or the new one, never a torn file. On Unix the file is
/// readable only by its owner, because it holds session credentials.
fn write_store(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("txt.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        COOKIE_FILE, MAX_ORIGIN_STORAGE_BYTES, ProfileCookies, ProfileStorage, STORAGE_FILE,
    };
    use render_net::{
        CancelToken, FetchConfig, FetchRequest, FetchResponse, Header, HttpTransport, Url,
    };
    use std::path::PathBuf;

    fn scratch_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("render-profile-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    /// A response from `url` that sets one cookie. The transport is used only
    /// to obtain a well-formed response value, as the cache tests do.
    fn response_setting(url: &str, set_cookie: &str) -> FetchResponse {
        let seed = Url::parse("data:text/plain,profile-seed").expect("data URL");
        let mut response = HttpTransport::new(FetchConfig::default())
            .fetch(&FetchRequest::get(seed), &CancelToken::default())
            .expect("data URL response");
        let url = Url::parse(url).expect("response URL");
        response.requested_url = url.clone();
        response.final_url = url.clone();
        response.redirect_chain = vec![url];
        response.headers = vec![Header {
            name: "Set-Cookie".to_owned(),
            value: set_cookie.as_bytes().to_vec(),
        }];
        response
    }

    fn cookie_for(cookies: &ProfileCookies, url: &str) -> Option<String> {
        cookies
            .decorate_request(FetchRequest::get(Url::parse(url).expect("request URL")))
            .cookie
    }

    #[test]
    fn a_persistent_sign_in_survives_reopening_the_store() {
        let directory = scratch_directory("reopen");
        let mut first = ProfileCookies::open(Some(directory.clone()));
        let issues = first.absorb_response(&response_setting(
            "https://example.test/login",
            "session=abc; Path=/; Max-Age=86400; Secure",
        ));
        assert!(issues.is_empty());
        first.flush_if_dirty();
        drop(first);

        let reopened = ProfileCookies::open(Some(directory.clone()));
        assert_eq!(
            cookie_for(&reopened, "https://example.test/account").as_deref(),
            Some("session=abc")
        );
        assert!(directory.join(COOKIE_FILE).is_file());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn session_cookies_do_not_survive_a_restart() {
        let directory = scratch_directory("session");
        let mut first = ProfileCookies::open(Some(directory.clone()));
        first.absorb_response(&response_setting(
            "https://example.test/",
            "transient=1; Path=/",
        ));
        assert_eq!(
            cookie_for(&first, "https://example.test/").as_deref(),
            Some("transient=1"),
            "the session cookie is usable within the session"
        );
        drop(first);

        let reopened = ProfileCookies::open(Some(directory.clone()));
        assert_eq!(cookie_for(&reopened, "https://example.test/"), None);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_store_without_a_directory_stays_in_memory() {
        let mut cookies = ProfileCookies::open(None);
        cookies.absorb_response(&response_setting(
            "https://example.test/",
            "kept=1; Path=/; Max-Age=600",
        ));
        assert!(cookies.dirty, "the change is pending");
        cookies.flush_if_dirty();
        assert!(!cookies.dirty, "a flush clears the pending change");
        assert_eq!(
            cookie_for(&cookies, "https://example.test/").as_deref(),
            Some("kept=1")
        );
    }

    #[test]
    fn a_response_without_set_cookie_does_not_mark_the_store_dirty() {
        let mut cookies = ProfileCookies::open(None);
        let mut response = response_setting("https://example.test/", "x=1");
        response.headers.clear();
        cookies.absorb_response(&response);
        assert!(!cookies.dirty);
    }

    const ORIGIN: &str = "https://example.test";

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn documents_that_started_from_one_copy_merge_instead_of_overwriting() {
        let mut store = ProfileStorage::open(None);
        let seeded = entries(&[("x", "1")]);
        store.apply_changes(ORIGIN, &[], &seeded);

        // Two documents start from the same copy. The first changes x.
        store.apply_changes(ORIGIN, &seeded, &entries(&[("x", "2")]));
        // The second still holds the old x and adds y. Its stale x must not win.
        store.apply_changes(ORIGIN, &seeded, &entries(&[("x", "1"), ("y", "3")]));

        assert_eq!(store.area(ORIGIN), entries(&[("x", "2"), ("y", "3")]));
    }

    #[test]
    fn a_key_a_document_removed_leaves_the_store() {
        let mut store = ProfileStorage::open(None);
        store.apply_changes(ORIGIN, &[], &entries(&[("x", "1"), ("keep", "k")]));
        store.apply_changes(
            ORIGIN,
            &entries(&[("x", "1"), ("keep", "k")]),
            &entries(&[("keep", "k")]),
        );
        assert_eq!(store.area(ORIGIN), entries(&[("keep", "k")]));

        store.apply_changes(ORIGIN, &entries(&[("keep", "k")]), &[]);
        assert!(store.area(ORIGIN).is_empty());
        assert!(store.origins.is_empty(), "an empty area is not kept");
    }

    #[test]
    fn a_change_past_the_origin_limit_is_refused_and_leaves_the_store_as_it_was() {
        let mut store = ProfileStorage::open(None);
        let small = entries(&[("kept", "yes")]);
        store.apply_changes(ORIGIN, &[], &small);

        let oversized = "x".repeat(MAX_ORIGIN_STORAGE_BYTES);
        let mut after = small.clone();
        after.push(("huge".to_owned(), oversized));
        store.apply_changes(ORIGIN, &small, &after);

        assert_eq!(store.area(ORIGIN), small);
    }

    #[test]
    fn persisted_areas_survive_reopening_and_escape_awkward_text() {
        let directory = scratch_directory("storage");
        let mut first = ProfileStorage::open(Some(directory.clone()));
        let awkward = "line one\nline\ttwo \\ back";
        first.apply_changes(ORIGIN, &[], &entries(&[("draft", awkward)]));
        first.flush_if_dirty();
        drop(first);

        let reopened = ProfileStorage::open(Some(directory.clone()));
        assert_eq!(reopened.area(ORIGIN), entries(&[("draft", awkward)]));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_file_without_the_store_header_is_not_loaded() {
        let directory = scratch_directory("foreign");
        std::fs::create_dir_all(&directory).expect("scratch directory");
        std::fs::write(directory.join(STORAGE_FILE), "https://example.test\tk\tv\n")
            .expect("foreign file");

        let store = ProfileStorage::open(Some(directory.clone()));
        assert!(store.area(ORIGIN).is_empty());
        let _ = std::fs::remove_dir_all(&directory);
    }
}
