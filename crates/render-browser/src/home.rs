//! Built-in new-tab page shown for home navigations.

/// The title used by the browser chrome for the built-in home page.
pub const HOME_TITLE: &str = "新标签页";

/// A self-contained, network-independent start page.
///
/// Favorites remain ordinary HTTPS links so they use the same navigation path
/// as links in any other document. Search intentionally belongs to the browser
/// address bar instead of being duplicated here.
pub const HOME_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>新标签页</title>
  <style>
    :root {
      color-scheme: light;
      --page: #f4f6fa;
      --surface: #ffffff;
      --text: #1d2129;
      --muted: #6b7280;
      --line: #e3e8ef;
      --focus: #2563eb;
      --shadow: 0 1px 2px rgba(16, 24, 40, 0.06), 0 2px 6px rgba(16, 24, 40, 0.06);
      --shadow-hover: 0 4px 12px rgba(16, 24, 40, 0.10), 0 10px 24px rgba(16, 24, 40, 0.08);
      font-family: system-ui, -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
    }

    * {
      box-sizing: border-box;
    }

    html,
    body {
      min-height: 100%;
      margin: 0;
      background-color: var(--page);
      color: var(--text);
    }

    body {
      min-height: 100vh;
    }

    .start-page {
      width: calc(100% - 48px);
      max-width: 880px;
      margin-left: auto;
      margin-right: auto;
      padding-top: clamp(48px, 10vh, 96px);
      padding-bottom: 64px;
    }

    .section-heading {
      margin: 0 0 6px 0;
      font-size: 22px;
      font-weight: 650;
      line-height: 1.3;
      letter-spacing: -0.01em;
    }

    .section-caption {
      margin: 0 0 24px 0;
      color: var(--muted);
      font-size: 14px;
      line-height: 1.5;
    }

    .favorite-list {
      margin: 0;
      padding: 0;
      display: grid;
      grid-template-columns: repeat(4, minmax(0, 1fr));
      gap: 14px;
      list-style: none;
    }

    .favorite-item {
      display: block;
      margin: 0;
      padding: 0;
      list-style: none;
    }

    .favorite-link {
      height: 100%;
      display: flex;
      flex-direction: column;
      align-items: center;
      row-gap: 12px;
      padding: 20px 10px 16px 10px;
      border: 1px solid var(--line);
      border-radius: 16px;
      background-color: var(--surface);
      box-shadow: var(--shadow);
      color: var(--text);
      text-align: center;
      text-decoration: none;
      transition: transform 140ms ease, box-shadow 140ms ease;
    }

    .favorite-icon {
      width: 52px;
      height: 52px;
      display: flex;
      align-items: center;
      justify-content: center;
      border-radius: 14px;
      background-color: var(--brand, #4b5563);
      color: #ffffff;
      font-size: 22px;
      font-weight: 700;
      line-height: 1;
    }

    .favorite-name {
      display: block;
      max-width: 100%;
      overflow: hidden;
      color: var(--text);
      font-size: 14px;
      font-weight: 500;
      line-height: 1.35;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .favorite-link:hover {
      transform: translateY(-2px);
      box-shadow: var(--shadow-hover);
    }

    .favorite-link:focus-visible {
      outline: 2px solid var(--focus);
      outline-offset: 3px;
    }

    .baidu { --brand: #2932e1; }
    .hao123 { --brand: #0f9d58; }
    .bilibili { --brand: #fb7299; }
    .zhihu { --brand: #0084ff; }
    .weibo { --brand: #e6162d; }
    .taobao { --brand: #ff5000; }
    .jd { --brand: #e1251b; }
    .netease { --brand: #c20c0c; }

    .start-page-details {
      margin-top: 40px;
      display: flex;
      flex-wrap: wrap;
      align-items: stretch;
      column-gap: 14px;
      row-gap: 14px;
    }

    .empty-section {
      min-width: 240px;
      flex-grow: 1;
      flex-shrink: 1;
      flex-basis: 320px;
      padding: 16px 18px;
      border: 1px solid var(--line);
      border-radius: 14px;
      background-color: var(--surface);
    }

    .empty-section h2 {
      margin: 0 0 4px 0;
      color: var(--muted);
      font-size: 13px;
      font-weight: 600;
      line-height: 1.3;
    }

    .empty-section p {
      margin: 0;
      color: var(--muted);
      font-size: 13px;
      line-height: 1.55;
    }

    @media (max-width: 680px) {
      .start-page {
        width: calc(100% - 32px);
        padding-top: 40px;
        padding-bottom: 40px;
      }

      .favorite-list {
        grid-template-columns: repeat(3, minmax(0, 1fr));
        gap: 12px;
      }

      .start-page-details {
        margin-top: 32px;
      }
    }

    @media (prefers-reduced-motion: reduce) {
      *, *::before, *::after {
        transition-duration: 0.01ms !important;
      }
    }

    @media (prefers-color-scheme: dark) {
      :root {
        color-scheme: dark;
        --page: #121417;
        --surface: #1c1f24;
        --text: #eceff4;
        --muted: #9aa3b2;
        --line: #2c3139;
        --focus: #7aa2ff;
        --shadow: 0 1px 2px rgba(0, 0, 0, 0.30);
        --shadow-hover: 0 6px 16px rgba(0, 0, 0, 0.40);
      }
    }
  </style>
</head>
<body>
  <main class="start-page">
    <section id="favorites" class="favorites" aria-labelledby="favorites-title" data-start-page-primary="favorites">
      <h1 id="favorites-title" class="section-heading">常用网站</h1>
      <p class="section-caption">点击网站名称或图标即可访问</p>
      <nav aria-label="常用网站">
        <ul class="favorite-list">
          <li class="favorite-item"><a class="favorite-link" href="https://www.baidu.com/"><span class="favorite-icon baidu" aria-hidden="true">百</span><span class="favorite-name">百度</span></a></li>
          <li class="favorite-item"><a class="favorite-link" href="https://www.hao123.com/"><span class="favorite-icon hao123" aria-hidden="true">好</span><span class="favorite-name">hao123</span></a></li>
          <li class="favorite-item"><a class="favorite-link" href="https://www.bilibili.com/"><span class="favorite-icon bilibili" aria-hidden="true">哔</span><span class="favorite-name">哔哩哔哩</span></a></li>
          <li class="favorite-item"><a class="favorite-link" href="https://www.zhihu.com/"><span class="favorite-icon zhihu" aria-hidden="true">知</span><span class="favorite-name">知乎</span></a></li>
          <li class="favorite-item"><a class="favorite-link" href="https://weibo.com/"><span class="favorite-icon weibo" aria-hidden="true">微</span><span class="favorite-name">微博</span></a></li>
          <li class="favorite-item"><a class="favorite-link" href="https://www.taobao.com/"><span class="favorite-icon taobao" aria-hidden="true">淘</span><span class="favorite-name">淘宝</span></a></li>
          <li class="favorite-item"><a class="favorite-link" href="https://www.jd.com/"><span class="favorite-icon jd" aria-hidden="true">京</span><span class="favorite-name">京东</span></a></li>
          <li class="favorite-item"><a class="favorite-link" href="https://www.163.com/"><span class="favorite-icon netease" aria-hidden="true">易</span><span class="favorite-name">网易</span></a></li>
        </ul>
      </nav>
    </section>

    <div class="start-page-details" aria-label="起始页信息">
      <section class="empty-section" aria-labelledby="recent-title" data-dynamic-section="recently-visited">
        <h2 id="recent-title">最近访问</h2>
        <p>浏览过的网站会显示在这里。</p>
      </section>
      <section class="empty-section" aria-labelledby="privacy-title" data-dynamic-section="privacy-report">
        <h2 id="privacy-title">隐私报告</h2>
        <p>有可用的隐私信息时会显示在这里。</p>
      </section>
    </div>
  </main>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use render_core::document::Document;
    use render_core::dom::{Dom, ElementData, NodeId, NodeKind};

    use super::{HOME_HTML, HOME_TITLE};

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
    fn home_document_parses_in_standards_mode_without_errors() {
        let document = Document::parse(HOME_HTML);

        assert!(
            document.html_errors().is_empty(),
            "{:?}",
            document.html_errors()
        );
        assert_eq!(document.quirks_mode().as_str(), "no-quirks");
        assert_eq!(elements_named(document.dom(), "title").count(), 1);
        assert_eq!(HOME_TITLE, "新标签页");
    }

    #[test]
    fn address_bar_remains_the_only_search_surface() {
        let document = Document::parse(HOME_HTML);
        let dom = document.dom();

        assert_eq!(elements_named(dom, "form").count(), 0);
        assert_eq!(
            elements_named(dom, "input")
                .filter(|(_, input)| attribute(input, "type") == Some("search"))
                .count(),
            0
        );
        assert!(elements(dom).all(|(_, element)| attribute(element, "role") != Some("search")));
    }

    #[test]
    fn favorite_links_are_unique_https_destinations() {
        let document = Document::parse(HOME_HTML);
        let hrefs = elements_named(document.dom(), "a")
            .map(|(_, link)| attribute(link, "href").expect("every favorite has an href"))
            .collect::<Vec<_>>();

        assert!(hrefs.len() >= 8, "expected a useful set of favorites");
        assert!(hrefs.iter().all(|href| href.starts_with("https://")));
        assert_eq!(
            hrefs.iter().copied().collect::<HashSet<_>>().len(),
            hrefs.len()
        );
        assert!(hrefs.contains(&"https://www.hao123.com/"));
    }

    #[test]
    fn favorites_are_the_primary_start_page_content() {
        let document = Document::parse(HOME_HTML);
        let dom = document.dom();
        let main = elements_named(dom, "main")
            .next()
            .expect("one main landmark");
        let first_element_child = dom
            .node(main.0)
            .expect("main node")
            .children()
            .iter()
            .filter_map(|child| dom.node(*child))
            .find_map(|node| match node.kind() {
                NodeKind::Element(element) => Some(element),
                _ => None,
            })
            .expect("main has element content");

        assert_eq!(first_element_child.local_name, "section");
        assert_eq!(attribute(first_element_child, "id"), Some("favorites"));
        assert_eq!(
            attribute(first_element_child, "data-start-page-primary"),
            Some("favorites")
        );
        assert_eq!(elements_named(dom, "nav").count(), 1);
        assert_eq!(elements_named(dom, "h1").count(), 1);
    }

    #[test]
    fn home_does_not_regress_into_a_marketing_landing_page() {
        let document = Document::parse(HOME_HTML);
        let dom = document.dom();

        assert_eq!(elements_named(dom, "header").count(), 0);
        assert_eq!(elements_named(dom, "footer").count(), 0);
        assert_eq!(elements_named(dom, "script").count(), 0);
        assert_eq!(elements_named(dom, "img").count(), 0);
        assert!(!HOME_HTML.contains("class=\"hero\""));
        assert!(!HOME_HTML.contains("class=\"brand\""));
        assert!(!HOME_HTML.contains("宣传"));
        assert!(!HOME_HTML.contains("天气"));
        assert!(!HOME_HTML.contains("新闻"));
    }

    #[test]
    fn dynamic_sections_are_explicit_empty_states() {
        let document = Document::parse(HOME_HTML);
        let sections = elements_named(document.dom(), "section")
            .filter_map(|(_, section)| attribute(section, "data-dynamic-section"))
            .collect::<HashSet<_>>();

        assert_eq!(
            sections,
            HashSet::from(["recently-visited", "privacy-report"])
        );
    }
}
