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

/// Builds the self-contained internal settings HTML for the current state.
#[must_use]
pub fn settings_html(state: CacheClearUiState) -> String {
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
    h2 {{ margin: 0 0 10px; font-size: 18px; }}
    p {{ margin: 0; color: #68707d; font-size: 14px; line-height: 1.6; }}
    .cache-actions {{ margin-top: 20px; }}
    button {{ min-height: 38px; padding: 0 14px; border: 1px solid #2864dc; border-radius: 8px; background: #2864dc; color: #ffffff; font: inherit; }}
    button:disabled {{ border-color: #9aa2ad; background: #9aa2ad; }}
    #cache-clear-status {{ margin-top: 12px; }}
  </style>
</head>
<body>
  <main>
    <h1>{SETTINGS_TITLE}</h1>
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
        CLEAR_HTTP_CACHE_ACTION, CLEAR_HTTP_CACHE_BUTTON_ID, CacheClearUiState, SETTINGS_TITLE,
        is_trusted_clear_http_cache_action, settings_html,
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

    fn attribute<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
        element
            .attributes
            .iter()
            .find(|attribute| attribute.local_name == name)
            .map(|attribute| attribute.value.as_str())
    }

    #[test]
    fn settings_document_is_static_and_parseable() {
        let html = settings_html(CacheClearUiState::Ready);
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
        assert_eq!(elements_named(dom, "input").count(), 0);
        assert_eq!(elements_named(dom, "img").count(), 0);
        assert_eq!(elements_named(dom, "a").count(), 0);
    }

    #[test]
    fn ready_settings_document_has_one_exact_clear_control() {
        let html = settings_html(CacheClearUiState::Ready);
        let document = Document::parse(&html);
        let buttons = elements_named(document.dom(), "button").collect::<Vec<_>>();

        assert_eq!(buttons.len(), 1);
        let (_, button) = buttons[0];
        assert_eq!(attribute(button, "id"), Some(CLEAR_HTTP_CACHE_BUTTON_ID));
        assert_eq!(
            attribute(button, "data-render-action"),
            Some(CLEAR_HTTP_CACHE_ACTION)
        );
        assert_eq!(attribute(button, "disabled"), None);
    }

    #[test]
    fn pending_disk_cleanup_disables_the_clear_control() {
        let html = settings_html(CacheClearUiState::ClearingDisk {
            memory_entries: 3,
            memory_bytes: 128,
        });
        let document = Document::parse(&html);
        let (_, button) = elements_named(document.dom(), "button")
            .next()
            .expect("clear button");

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
    fn privileged_action_requires_the_trusted_settings_document() {
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
    }
}
