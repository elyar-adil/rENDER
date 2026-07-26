//! Typed navigation requests emitted by browser chrome.

use render_core::navigation::{AddressInput, AddressInputConfig, NavigationError, SearchProvider};
use render_net::Url;

const HOME_ADDRESS: &str = "render://home";
const SEARCH_ENDPOINT: &str = "https://www.google.com/search";

/// A destination understood by the shell without conflating URLs and files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavigationTarget {
    Home,
    Url(Url),
}

impl NavigationTarget {
    #[must_use]
    pub fn from_url(url: Url) -> Self {
        if is_home_url(&url) {
            Self::Home
        } else {
            Self::Url(url)
        }
    }

    #[must_use]
    pub fn from_address_input(input: AddressInput) -> Self {
        Self::from_url(input.into_url())
    }

    #[must_use]
    pub fn display_address(&self) -> String {
        match self {
            Self::Home => HOME_ADDRESS.to_owned(),
            Self::Url(url) => url.as_str().to_owned(),
        }
    }

    #[must_use]
    pub fn history_url(&self) -> Url {
        match self {
            Self::Home => home_url(),
            Self::Url(url) => url.clone(),
        }
    }
}

/// A user intent from a toolbar, keyboard shortcut, or address submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavigationIntent {
    Navigate(AddressInput),
    Back,
    Forward,
    Reload,
    Home,
}

/// Parse one address-bar submission through the shared URL Standard adapter.
/// Internal home-page aliases remain browser-chrome commands and never enter
/// network or file URL classification.
///
/// # Errors
///
/// Returns the typed core navigation error for malformed, dangerous,
/// unsupported, or over-limit input.
pub fn intent_from_address(input: &str) -> Result<NavigationIntent, NavigationError> {
    let value = input.trim();
    if value.eq_ignore_ascii_case(HOME_ADDRESS) || value.eq_ignore_ascii_case("about:home") {
        return Ok(NavigationIntent::Home);
    }
    AddressInput::parse(value, &address_input_config()).map(NavigationIntent::Navigate)
}

fn address_input_config() -> AddressInputConfig {
    let search_provider = Url::parse(SEARCH_ENDPOINT)
        .ok()
        .and_then(|endpoint| SearchProvider::new(endpoint, "q").ok());
    AddressInputConfig {
        search_provider,
        ..AddressInputConfig::default()
    }
}

fn home_url() -> Url {
    Url::parse(HOME_ADDRESS).expect("the built-in home URL is valid")
}

fn is_home_url(url: &Url) -> bool {
    url.scheme() == "render" && url.host_str() == Some("home")
}

#[cfg(test)]
mod tests {
    use render_core::navigation::AddressInput;

    use super::{NavigationIntent, NavigationTarget, intent_from_address};

    #[test]
    fn host_name_becomes_https_navigation() {
        let intent = intent_from_address("google.com").expect("valid host input");
        let NavigationIntent::Navigate(AddressInput::Url(url)) = intent else {
            panic!("host input should be a URL navigation");
        };
        assert_eq!(url.as_str(), "https://google.com/");
    }

    #[test]
    fn network_url_is_not_a_local_path() {
        let intent = intent_from_address("http://example.test/a").expect("valid URL input");
        let NavigationIntent::Navigate(input) = intent else {
            panic!("network input should navigate");
        };
        assert_eq!(
            NavigationTarget::from_address_input(input).display_address(),
            "http://example.test/a"
        );
    }

    #[test]
    fn whitespace_input_uses_the_configured_search_provider() {
        let intent = intent_from_address("rust browser engine").expect("search input");
        let NavigationIntent::Navigate(AddressInput::Search { terms, url }) = intent else {
            panic!("whitespace input should be a search");
        };
        assert_eq!(terms, "rust browser engine");
        assert_eq!(url.host_str(), Some("www.google.com"));
        assert_eq!(url.query(), Some("q=rust+browser+engine"));
    }

    #[test]
    fn home_alias_is_a_chrome_command() {
        assert_eq!(
            intent_from_address("about:home").expect("home alias"),
            NavigationIntent::Home
        );
    }
}
