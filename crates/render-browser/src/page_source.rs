//! Native browser shell for the self-owned Rust rendering pipeline.
#![allow(clippy::cast_precision_loss)]
use render_browser::home::HOME_HTML;
use render_browser::home::HOME_TITLE;
use render_browser::navigation::NavigationTarget;
use render_browser::settings::CacheClearUiState;
use render_browser::settings::ProxyUiState;
use render_browser::settings::SETTINGS_TITLE;
use render_browser::settings::settings_html;
use render_core::html::HtmlDecodeOptions;
use render_core::html::decode_html_bytes;
use render_net::FetchResponse;
use render_net::Url;
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub(super) struct PageSource {
    pub(super) html: String,
    pub(super) title: String,
    pub(super) target: NavigationTarget,
}

/// What the command line asked the process to do.
pub(super) struct Launch {
    /// The page to open, or `None` when `--help` ended the launch.
    pub(super) page: Option<PageSource>,
    /// `--screenshot`: where to write the composed frame and how long to let
    /// the page settle first. The process exits after writing it.
    pub(super) screenshot: Option<(PathBuf, Duration)>,
}

pub(super) fn load_initial_page() -> Result<Launch, Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let mut argument = None;
    let mut screenshot = None;
    let mut screenshot_delay = Duration::from_secs(4);
    while let Some(next) = arguments.next() {
        match next.to_str() {
            Some("-h" | "--help") => {
                println!(
                    "Usage: render-browser [--screenshot PATH] [--screenshot-delay MILLIS] [URL_OR_LOCAL_HTML_PATH]\n\nNo argument opens the built-in home page. HTTP, HTTPS, and data: URLs use the browser's normal navigation pipeline. With --screenshot the process writes the composed frame to PATH once the page has settled, then exits."
                );
                return Ok(Launch {
                    page: None,
                    screenshot: None,
                });
            }
            Some("--screenshot") => {
                screenshot = Some(PathBuf::from(arguments.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--screenshot requires a path")
                })?));
            }
            Some("--screenshot-delay") => {
                let value = arguments.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--screenshot-delay requires milliseconds",
                    )
                })?;
                let millis = value
                    .to_str()
                    .and_then(|text| text.parse::<u64>().ok())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--screenshot-delay takes a whole number of milliseconds",
                        )
                    })?;
                screenshot_delay = Duration::from_millis(millis);
            }
            _ => {
                if argument.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "expected at most one URL or local HTML path",
                    )
                    .into());
                }
                argument = Some(next);
            }
        }
    }
    let Some(argument) = argument else {
        return Ok(Launch {
            page: Some(home_source()),
            screenshot: screenshot.map(|path| (path, screenshot_delay)),
        });
    };
    if let Some(value) = argument.to_str()
        && let Ok(url) = Url::parse(value)
        && matches!(url.scheme(), "http" | "https" | "data")
    {
        return Ok(Launch {
            page: Some(network_start_source(url)),
            screenshot: screenshot.map(|path| (path, screenshot_delay)),
        });
    }
    let path = PathBuf::from(argument);
    if path.to_string_lossy().contains("://") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the command-line URL must use HTTP, HTTPS, or data",
        )
        .into());
    }
    Ok(Launch {
        page: Some(source_from_local_file(path)?),
        screenshot: screenshot.map(|path| (path, screenshot_delay)),
    })
}

pub(super) fn home_source() -> PageSource {
    PageSource {
        html: HOME_HTML.to_owned(),
        title: HOME_TITLE.to_owned(),
        target: NavigationTarget::Home,
    }
}

pub(super) fn settings_source(state: CacheClearUiState, proxy: &ProxyUiState) -> PageSource {
    PageSource {
        html: settings_html(state, proxy),
        title: SETTINGS_TITLE.to_owned(),
        target: NavigationTarget::Settings,
    }
}

pub(super) fn network_start_source(url: Url) -> PageSource {
    PageSource {
        html: String::new(),
        title: "Loading".to_owned(),
        target: NavigationTarget::Url(url),
    }
}

pub(super) fn source_from_local_file(path: PathBuf) -> Result<PageSource, Box<dyn Error>> {
    if !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("local HTML file does not exist: {}", path.display()),
        )
        .into());
    }
    let path = fs::canonicalize(path)?;
    let bytes = fs::read(&path)?;
    let html = decode_html_bytes(&bytes, &HtmlDecodeOptions::default())?.text;
    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Local page")
        .to_owned();
    Ok(PageSource {
        html,
        title,
        target: NavigationTarget::Url(Url::from_file_path(&path).map_err(|()| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "local path cannot be represented as a file URL: {}",
                    path.display()
                ),
            )
        })?),
    })
}

pub(super) fn status_source(
    target: NavigationTarget,
    title: &str,
    heading: &str,
    message: &str,
) -> PageSource {
    let address = escape_html(&target.display_address());
    let title_html = escape_html(title);
    let heading = escape_html(heading);
    let message = escape_html(message);
    let html = format!(
        r#"<!doctype html><html><head><title>{title_html}</title><style>
html {{ background-color: #f5f7fb; color: #172033; }} body {{ display: block; margin-top: 0px; margin-right: 0px; margin-bottom: 0px; margin-left: 0px; }}
main {{ display: block; width: 720px; margin-top: 72px; margin-right: auto; margin-bottom: 40px; margin-left: auto; background-color: white; padding-top: 36px; padding-right: 40px; padding-bottom: 36px; padding-left: 40px; }}
h1, p {{ display: block; }} h1 {{ color: #1859a9; margin-top: 0px; }} .address {{ display: block; background-color: #eef3f9; padding-top: 14px; padding-right: 16px; padding-bottom: 14px; padding-left: 16px; margin-top: 22px; }}
</style></head><body><main><h1>{heading}</h1><p>{message}</p><div class="address">{address}</div></main></body></html>"#
    );
    PageSource {
        html,
        title: title.to_owned(),
        target,
    }
}

pub(super) fn error_source(target: NavigationTarget, message: &str) -> PageSource {
    status_source(
        target,
        "Load error",
        "This page could not be loaded",
        message,
    )
}

pub(super) fn source_from_network_response(response: &FetchResponse) -> Result<PageSource, String> {
    let target = NavigationTarget::Url(response.final_url.clone());
    let status = response.status.as_u16();
    let media_type = response
        .content_type
        .as_ref()
        .map(|content_type| content_type.media_type.as_str());
    // The status code does not stop rendering: a 4xx or 5xx response with an
    // HTML body is the page the server sent. Only a body the engine cannot
    // render is replaced by a status message.
    if !matches!(media_type, None | Some("text/html" | "text/plain")) {
        return Err(if response.status.is_success() {
            format!(
                "The response content type '{}' is not renderable as a document yet.",
                media_type.unwrap_or_default()
            )
        } else {
            format!("The server returned HTTP status {status}.")
        });
    }
    if !response.status.is_success() && response.body.is_empty() {
        return Err(format!("The server returned HTTP status {status}."));
    }

    let decoded = decode_html_bytes(
        &response.body,
        &HtmlDecodeOptions {
            transport_encoding_label: response
                .content_type
                .as_ref()
                .and_then(|content_type| content_type.charset.clone()),
            ..HtmlDecodeOptions::default()
        },
    )
    .map_err(|error| format!("HTML decoding failed: {error}"))?;
    let html = if media_type == Some("text/plain") {
        format!(
            "<!doctype html><html><head><title>Plain text</title></head><body><pre>{}</pre></body></html>",
            escape_html(&decoded.text)
        )
    } else {
        decoded.text
    };
    let title = response
        .final_url
        .host_str()
        .unwrap_or(response.final_url.as_str())
        .to_owned();
    Ok(PageSource {
        html,
        title,
        target,
    })
}

pub(super) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
