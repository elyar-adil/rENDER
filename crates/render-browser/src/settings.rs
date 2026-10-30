//! Trusted source for the browser's built-in settings document.
//!
//! This module generates presentation only. The shell must verify that a click
//! came from the built-in settings target before acting on the cache-clear
//! marker; a remote document must never gain that privilege by copying these
//! attributes.

/// Browser-chrome title for the internal settings document.
pub const SETTINGS_TITLE: &str = "设置";

/// Stable DOM id for the HTTP-cache clear control.
pub const CLEAR_HTTP_CACHE_BUTTON_ID: &str = "clear-http-cache";

/// Stable action marker consumed by the trusted browser shell.
pub const CLEAR_HTTP_CACHE_ACTION: &str = "clear-http-cache";

/// Stable DOM id for the HTTP proxy address field.
pub const HTTP_PROXY_INPUT_ID: &str = "http-proxy-address";

/// Stable DOM id for the HTTP proxy save control.
pub const SAVE_HTTP_PROXY_BUTTON_ID: &str = "save-http-proxy";

/// Stable action marker consumed by the trusted browser shell.
pub const SAVE_HTTP_PROXY_ACTION: &str = "save-http-proxy";

/// Presentation state for the HTTP-cache clear control.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CacheClearUiState {
    /// No clear operation is currently in progress.
    #[default]
    Ready,
    /// Memory is clear and best-effort disk cleanup is still pending.
    ClearingDisk {
        memory_entries: usize,
        memory_bytes: usize,
    },
    /// Both memory and disk cleanup completed.
    Cleared {
        memory_entries: usize,
        memory_bytes: usize,
    },
    /// Memory cleanup completed, while disk cleanup could not finish.
    DiskClearFailed {
        memory_entries: usize,
        memory_bytes: usize,
    },
}

impl CacheClearUiState {
    /// Whether the clear control should be disabled while work is pending.
    #[must_use]
    pub const fn is_busy(self) -> bool {
        matches!(self, Self::ClearingDisk { .. })
    }
}

/// Returns whether a clicked element is the privileged clear action.
///
/// `is_settings_document` must be true only for the browser-generated
/// settings target, never merely for a URL string supplied by page content.
#[must_use]
pub fn is_trusted_clear_http_cache_action(
    is_settings_document: bool,
    element_id: Option<&str>,
    action: Option<&str>,
) -> bool {
    is_settings_document
        && element_id == Some(CLEAR_HTTP_CACHE_BUTTON_ID)
        && action == Some(CLEAR_HTTP_CACHE_ACTION)
}

/// Returns whether a clicked element is the privileged proxy-save action.
#[must_use]
pub fn is_trusted_save_http_proxy_action(
    is_settings_document: bool,
    element_id: Option<&str>,
    action: Option<&str>,
) -> bool {
    is_settings_document
        && element_id == Some(SAVE_HTTP_PROXY_BUTTON_ID)
        && action == Some(SAVE_HTTP_PROXY_ACTION)
}

/// Feedback from the last proxy save attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyFeedbackKind {
    Success,
    Error,
}

/// One line of feedback shown under the proxy field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyFeedback {
    pub kind: ProxyFeedbackKind,
    pub message: String,
}

/// Presentation state of the HTTP proxy section.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProxyUiState {
    /// The canonical address currently applied, if any.
    pub address: Option<String>,
    /// Whether the user explicitly chose direct connections (as opposed to
    /// having no saved choice yet).
    pub direct: bool,
    /// Text shown in the field. After a rejected save this is the draft the
    /// user typed, so it can be corrected instead of retyped.
    pub input: String,
    /// Result of the last save; `None` shows the current state.
    pub feedback: Option<ProxyFeedback>,
}

impl ProxyUiState {
    /// Builds the idle state for a persisted proxy address.
    #[must_use]
    pub fn new(address: Option<String>) -> Self {
        Self {
            input: address.clone().unwrap_or_default(),
            address,
            direct: false,
            feedback: None,
        }
    }

    /// Builds the idle state after the user explicitly disabled the proxy.
    #[must_use]
    pub fn direct() -> Self {
        Self {
            address: None,
            direct: true,
            input: String::new(),
            feedback: None,
        }
    }

    /// Builds the state after a successful save.
    #[must_use]
    pub fn saved(address: Option<String>) -> Self {
        let message = match &address {
            Some(address) => format!("代理已保存，后续请求立即使用：{address}"),
            None => "已关闭代理，后续请求将直接连接。".to_owned(),
        };
        Self {
            input: address.clone().unwrap_or_default(),
            direct: address.is_none(),
            address,
            feedback: Some(ProxyFeedback {
                kind: ProxyFeedbackKind::Success,
                message,
            }),
        }
    }

    /// Builds the state after a rejected save, keeping the draft.
    #[must_use]
    pub fn invalid(input: String, message: String) -> Self {
        Self {
            input,
            direct: false,
            address: None,
            feedback: Some(ProxyFeedback {
                kind: ProxyFeedbackKind::Error,
                message,
            }),
        }
    }

    /// The line shown below the field.
    #[must_use]
    pub fn status_text(&self) -> String {
        if let Some(feedback) = &self.feedback {
            return feedback.message.clone();
        }
        if let Some(address) = &self.address {
            return format!("当前代理：{address}");
        }
        if self.direct {
            return "代理已关闭，后续请求将直接连接。".to_owned();
        }
        "未保存代理设置；保存后会覆盖系统或环境代理。".to_owned()
    }

    fn status_state(&self) -> &'static str {
        match self.feedback.as_ref().map(|feedback| feedback.kind) {
            Some(ProxyFeedbackKind::Success) => "success",
            Some(ProxyFeedbackKind::Error) => "error",
            None => "idle",
        }
    }
}

/// Escapes text inserted into the self-contained settings document.
///
/// The proxy field carries user input, so every interpolation into HTML or a
/// quoted attribute goes through this function.
#[must_use]
pub fn html_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Builds the self-contained internal settings HTML for the current state.
#[must_use]
pub fn settings_html(state: CacheClearUiState, proxy: &ProxyUiState) -> String {
    let (state_name, status, button_label, disabled) = match state {
        CacheClearUiState::Ready => (
            "ready",
            "缓存会加快重复访问。清除后，下一次加载可能会更慢。".to_owned(),
            "清除 HTTP 缓存",
            "",
        ),
        CacheClearUiState::ClearingDisk {
            memory_entries,
            memory_bytes,
        } => (
            "clearing-disk",
            clear_summary(
                memory_entries,
                memory_bytes,
                "内存缓存已清除，正在清理磁盘缓存。",
            ),
            "正在清理…",
            " disabled",
        ),
        CacheClearUiState::Cleared {
            memory_entries,
            memory_bytes,
        } => (
            "cleared",
            clear_summary(memory_entries, memory_bytes, "内存和磁盘缓存已清除。"),
            "清除 HTTP 缓存",
            "",
        ),
        CacheClearUiState::DiskClearFailed {
            memory_entries,
            memory_bytes,
        } => (
            "disk-clear-failed",
            clear_summary(
                memory_entries,
                memory_bytes,
                "磁盘缓存未能清除，可稍后重试。",
            ),
            "重试清除 HTTP 缓存",
            "",
        ),
    };

    let proxy_input = html_escape(&proxy.input);
    let proxy_status = html_escape(&proxy.status_text());
    let proxy_status_state = proxy.status_state();

    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{SETTINGS_TITLE}</title>
  <style>
    :root {{ color-scheme: light; font-family: system-ui, -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif; }}
    html, body {{ min-height: 100%; margin: 0; background: #f3f5f8; color: #20242c; }}
    main {{ width: calc(100% - 48px); max-width: 720px; margin: 0 auto; padding: 72px 0; }}
    h1 {{ margin: 0 0 28px; font-size: 28px; line-height: 1.2; }}
    section {{ padding: 24px; border: 1px solid #dce1e8; border-radius: 14px; background: #ffffff; }}
    section + section {{ margin-top: 18px; }}
    h2 {{ margin: 0 0 10px; font-size: 18px; }}
    p {{ margin: 0; color: #68707d; font-size: 14px; line-height: 1.6; }}
    label {{ display: block; margin: 18px 0 6px; font-size: 14px; font-weight: 600; }}
    input {{ box-sizing: border-box; width: 100%; min-height: 40px; padding: 8px 10px; border: 1px solid #b7c0cc; border-radius: 8px; background: #ffffff; color: inherit; font: inherit; }}
    input:focus {{ border-color: #2864dc; outline: 2px solid rgba(40, 100, 220, 0.2); }}
    .hint {{ margin-top: 6px; font-size: 13px; }}
    .cache-actions, .proxy-actions {{ margin-top: 20px; }}
    button {{ min-height: 38px; padding: 0 14px; border: 1px solid #2864dc; border-radius: 8px; background: #2864dc; color: #ffffff; font: inherit; white-space: nowrap; }}
    button:disabled {{ border-color: #9aa2ad; background: #9aa2ad; }}
    #cache-clear-status, #http-proxy-status {{ margin-top: 12px; }}
    #http-proxy-status[data-proxy-state="error"] {{ color: #b3261e; }}
    #http-proxy-status[data-proxy-state="success"] {{ color: #1a6b32; }}
  </style>
</head>
<body>
  <main>
    <h1>{SETTINGS_TITLE}</h1>
    <section aria-labelledby="http-proxy-title" data-settings-section="http-proxy">
      <h2 id="http-proxy-title">HTTP 代理</h2>
      <p>代理设置对所有标签页立即生效。留空并保存将关闭代理、改为直接连接。</p>
      <label for="{HTTP_PROXY_INPUT_ID}">代理地址</label>
      <input id="{HTTP_PROXY_INPUT_ID}" type="text" name="http-proxy" value="{proxy_input}" placeholder="http://127.0.0.1:8080" autocomplete="off" spellcheck="false" aria-describedby="http-proxy-hint http-proxy-status">
      <p id="http-proxy-hint" class="hint">支持 host:port 或 http://host:port；暂不支持 SOCKS 或 HTTPS 代理。</p>
      <div class="proxy-actions">
        <button id="{SAVE_HTTP_PROXY_BUTTON_ID}" type="button" data-render-action="{SAVE_HTTP_PROXY_ACTION}">保存代理设置</button>
        <p id="http-proxy-status" role="status" data-proxy-state="{proxy_status_state}">{proxy_status}</p>
      </div>
    </section>
    <section aria-labelledby="http-cache-title" data-settings-section="http-cache">
      <h2 id="http-cache-title">HTTP 缓存</h2>
      <p>缓存会保存可安全复用的网页资源，以减少重复下载。</p>
      <div class="cache-actions">
        <button id="{CLEAR_HTTP_CACHE_BUTTON_ID}" type="button" data-render-action="{CLEAR_HTTP_CACHE_ACTION}"{disabled}>{button_label}</button>
        <p id="cache-clear-status" role="status" data-cache-clear-state="{state_name}">{status}</p>
      </div>
    </section>
  </main>
</body>
</html>"#
    )
}

fn clear_summary(memory_entries: usize, memory_bytes: usize, suffix: &str) -> String {
    format!("已清除 {memory_entries} 个内存缓存条目（{memory_bytes} 字节）；{suffix}")
}

#[cfg(test)]
mod tests {
    use render_core::document::Document;
    use render_core::dom::{Dom, ElementData, NodeId, NodeKind};

    use super::{
        CLEAR_HTTP_CACHE_ACTION, CLEAR_HTTP_CACHE_BUTTON_ID, CacheClearUiState,
        HTTP_PROXY_INPUT_ID, ProxyFeedbackKind, ProxyUiState, SAVE_HTTP_PROXY_ACTION,
        SAVE_HTTP_PROXY_BUTTON_ID, SETTINGS_TITLE, html_escape, is_trusted_clear_http_cache_action,
        is_trusted_save_http_proxy_action, settings_html,
    };

    fn elements(dom: &Dom) -> impl Iterator<Item = (NodeId, &ElementData)> {
        let mut stack = vec![dom.document()];
        let mut matches = Vec::new();

        while let Some(node_id) = stack.pop() {
            let Some(node) = dom.node(node_id) else {
                continue;
            };
            if let NodeKind::Element(element) = node.kind() {
                matches.push((node_id, element));
            }
            stack.extend(node.children().iter().rev().copied());
        }

        matches.into_iter()
    }

    fn elements_named<'a>(
        dom: &'a Dom,
        name: &'a str,
    ) -> impl Iterator<Item = (NodeId, &'a ElementData)> {
        elements(dom).filter(move |(_, element)| element.local_name == name)
    }

    fn element_with_id<'a>(dom: &'a Dom, id: &str) -> &'a ElementData {
        elements(dom)
            .find(|(_, element)| attribute(element, "id") == Some(id))
            .map_or_else(
                || panic!("element {id} should exist"),
                |(_, element)| element,
            )
    }

    fn attribute<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
        element
            .attributes
            .iter()
            .find(|attribute| attribute.local_name == name)
            .map(|attribute| attribute.value.as_str())
    }

    #[test]
    fn settings_document_is_static_and_parseable() {
        let html = settings_html(
            CacheClearUiState::Ready,
            &ProxyUiState::new(Some("http://127.0.0.1:8080".to_owned())),
        );
        let document = Document::parse(&html);
        let dom = document.dom();

        assert!(
            document.html_errors().is_empty(),
            "{:?}",
            document.html_errors()
        );
        assert_eq!(document.quirks_mode().as_str(), "no-quirks");
        assert_eq!(SETTINGS_TITLE, "设置");
        assert_eq!(elements_named(dom, "script").count(), 0);
        assert_eq!(elements_named(dom, "form").count(), 0);
        assert_eq!(elements_named(dom, "input").count(), 1);
        assert_eq!(elements_named(dom, "img").count(), 0);
        assert_eq!(elements_named(dom, "a").count(), 0);
    }

    #[test]
    fn ready_settings_document_has_exact_clear_and_proxy_controls() {
        let html = settings_html(CacheClearUiState::Ready, &ProxyUiState::default());
        let document = Document::parse(&html);
        let dom = document.dom();

        assert_eq!(elements_named(dom, "button").count(), 2);

        let clear = element_with_id(dom, CLEAR_HTTP_CACHE_BUTTON_ID);
        assert_eq!(
            attribute(clear, "data-render-action"),
            Some(CLEAR_HTTP_CACHE_ACTION)
        );
        assert_eq!(attribute(clear, "disabled"), None);

        let save = element_with_id(dom, SAVE_HTTP_PROXY_BUTTON_ID);
        assert_eq!(
            attribute(save, "data-render-action"),
            Some(SAVE_HTTP_PROXY_ACTION)
        );
    }

    #[test]
    fn proxy_field_round_trips_the_saved_address() {
        let state = ProxyUiState::new(Some("http://proxy.example:3128".to_owned()));
        let html = settings_html(CacheClearUiState::Ready, &state);
        let document = Document::parse(&html);
        let input = element_with_id(document.dom(), HTTP_PROXY_INPUT_ID);

        assert_eq!(attribute(input, "type"), Some("text"));
        assert_eq!(attribute(input, "value"), Some("http://proxy.example:3128"));
        assert!(html.contains("当前代理：http://proxy.example:3128"));
        assert_eq!(state.status_state(), "idle");
    }

    #[test]
    fn proxy_feedback_and_draft_are_preserved_and_escaped() {
        let state = ProxyUiState::invalid(
            "\" onfocus=\"alert(1)".to_owned(),
            "代理地址格式无效：<bad> & worse".to_owned(),
        );
        let html = settings_html(CacheClearUiState::Ready, &state);
        let document = Document::parse(&html);
        let dom = document.dom();
        let input = element_with_id(dom, HTTP_PROXY_INPUT_ID);

        assert_eq!(
            attribute(input, "value"),
            Some("\" onfocus=\"alert(1)"),
            "the draft survives a rejected save"
        );
        assert!(!html.contains("<bad>"));
        assert!(html.contains("&lt;bad&gt; &amp; worse"));
        assert_eq!(state.status_state(), "error");
        assert_eq!(
            state.feedback.as_ref().map(|feedback| feedback.kind),
            Some(ProxyFeedbackKind::Error)
        );
        assert_eq!(html_escape("a&b<c>\"d'"), "a&amp;b&lt;c&gt;&quot;d&#39;");
    }

    #[test]
    fn saved_proxy_state_reports_success_both_ways() {
        let enabled = ProxyUiState::saved(Some("http://127.0.0.1:8080".to_owned()));
        assert_eq!(enabled.status_state(), "success");
        assert!(enabled.status_text().contains("立即使用"));
        assert_eq!(enabled.input, "http://127.0.0.1:8080");
        assert!(!enabled.direct);

        let disabled = ProxyUiState::saved(None);
        assert_eq!(disabled.status_state(), "success");
        assert!(disabled.status_text().contains("直接连接"));
        assert!(disabled.input.is_empty());
        assert!(disabled.direct);
        assert!(ProxyUiState::direct().status_text().contains("直接连接"));
    }

    #[test]
    fn pending_disk_cleanup_disables_the_clear_control() {
        let html = settings_html(
            CacheClearUiState::ClearingDisk {
                memory_entries: 3,
                memory_bytes: 128,
            },
            &ProxyUiState::default(),
        );
        let document = Document::parse(&html);
        let button = element_with_id(document.dom(), CLEAR_HTTP_CACHE_BUTTON_ID);

        assert_eq!(attribute(button, "disabled"), Some(""));
        assert!(html.contains("正在清理磁盘缓存"));
        assert!(
            CacheClearUiState::ClearingDisk {
                memory_entries: 0,
                memory_bytes: 0,
            }
            .is_busy()
        );
    }

    #[test]
    fn privileged_actions_require_the_trusted_settings_document() {
        assert!(is_trusted_clear_http_cache_action(
            true,
            Some(CLEAR_HTTP_CACHE_BUTTON_ID),
            Some(CLEAR_HTTP_CACHE_ACTION),
        ));
        assert!(!is_trusted_clear_http_cache_action(
            false,
            Some(CLEAR_HTTP_CACHE_BUTTON_ID),
            Some(CLEAR_HTTP_CACHE_ACTION),
        ));
        assert!(!is_trusted_clear_http_cache_action(
            true,
            Some(CLEAR_HTTP_CACHE_BUTTON_ID),
            Some("anything-else"),
        ));

        assert!(is_trusted_save_http_proxy_action(
            true,
            Some(SAVE_HTTP_PROXY_BUTTON_ID),
            Some(SAVE_HTTP_PROXY_ACTION),
        ));
        assert!(!is_trusted_save_http_proxy_action(
            false,
            Some(SAVE_HTTP_PROXY_BUTTON_ID),
            Some(SAVE_HTTP_PROXY_ACTION),
        ));
        assert!(!is_trusted_save_http_proxy_action(
            true,
            Some(CLEAR_HTTP_CACHE_BUTTON_ID),
            Some(SAVE_HTTP_PROXY_ACTION),
        ));
    }
}
