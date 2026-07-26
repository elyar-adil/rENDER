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
      --page: #f3f5f8;
      --surface: #ffffff;
      --surface-muted: #e8ecf2;
      --text: #20242c;
      --muted: #68707d;
      --line: #dce1e8;
      --focus: #2864dc;
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
      max-width: 960px;
      margin-left: auto;
      margin-right: auto;
      padding-top: clamp(64px, 14vh, 132px);
      padding-bottom: 72px;
    }

    .section-heading {
      margin-top: 0;
      margin-right: 0;
      margin-bottom: 24px;
      margin-left: 0;
      font-size: 20px;
      font-weight: 650;
      line-height: 1.25;
      letter-spacing: -0.01em;
    }

    .favorite-list {
      margin-top: 0;
      margin-right: 0;
      margin-bottom: 0;
      margin-left: 0;
      padding-top: 0;
      padding-right: 0;
      padding-bottom: 0;
      padding-left: 0;
      display: flex;
      flex-wrap: wrap;
      justify-content: flex-start;
      align-items: flex-start;
      column-gap: 20px;
      row-gap: 24px;
      list-style: none;
    }

    .favorite-item {
      display: block;
      flex-grow: 1;
      flex-shrink: 1;
      flex-basis: 96px;
      min-width: 76px;
      max-width: 100px;
    }

    .favorite-link {
      width: 100%;
      display: flex;
      flex-direction: column;
      align-items: center;
      row-gap: 10px;
      color: var(--text);
      text-align: center;
      text-decoration: none;
    }

    .favorite-icon {
      width: 56px;
      height: 56px;
      display: flex;
      align-items: center;
      justify-content: center;
      border-top-width: 1px;
      border-right-width: 1px;
      border-bottom-width: 1px;
      border-left-width: 1px;
      border-top-style: solid;
      border-right-style: solid;
      border-bottom-style: solid;
      border-left-style: solid;
      border-top-color: var(--line);
      border-right-color: var(--line);
      border-bottom-color: var(--line);
      border-left-color: var(--line);
      border-radius: 18px;
      background-color: var(--surface);
      color: var(--icon-color, #394150);
      box-shadow: 0 4px 14px rgba(22, 30, 45, 0.08);
      font-size: 18px;
      font-weight: 700;
      line-height: 1;
      transition: transform 140ms ease, box-shadow 140ms ease, background-color 140ms ease;
    }

    .favorite-name {
      display: block;
      max-width: 100%;
      overflow: hidden;
      color: var(--text);
      font-size: 13px;
      line-height: 1.35;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .favorite-link:hover .favorite-icon {
      background-color: #f9fafc;
      box-shadow: 0 7px 20px rgba(22, 30, 45, 0.12);
      transform: translateY(-2px);
    }

    .favorite-link:focus-visible {
      outline: 3px solid var(--focus);
      outline-offset: 5px;
      border-radius: 20px;
    }

    .baidu { --icon-color: #315efb; background-color: #eef2ff; }
    .hao123 { --icon-color: #16875b; background-color: #edf8f2; }
    .bilibili { --icon-color: #e65d87; background-color: #fff0f5; }
    .zhihu { --icon-color: #1769e0; background-color: #edf5ff; }
    .weibo { --icon-color: #db4b36; background-color: #fff1ee; }
    .taobao { --icon-color: #e85d1a; background-color: #fff2e9; }
    .jd { --icon-color: #d9363e; background-color: #fff0f1; }
    .netease { --icon-color: #c83e42; background-color: #fff1f1; }

    .start-page-details {
      margin-top: 52px;
      display: flex;
      flex-wrap: wrap;
      align-items: stretch;
      column-gap: 16px;
      row-gap: 16px;
    }

    .empty-section {
      min-width: 240px;
      min-height: 104px;
      flex-grow: 1;
      flex-shrink: 1;
      flex-basis: 360px;
      padding-top: 18px;
      padding-right: 20px;
      padding-bottom: 18px;
      padding-left: 20px;
      border-top-width: 1px;
      border-right-width: 1px;
      border-bottom-width: 1px;
      border-left-width: 1px;
      border-top-style: solid;
      border-right-style: solid;
      border-bottom-style: solid;
      border-left-style: solid;
      border-top-color: var(--line);
      border-right-color: var(--line);
      border-bottom-color: var(--line);
      border-left-color: var(--line);
      border-radius: 14px;
      background-color: var(--surface);
    }

    .empty-section h2 {
      margin-top: 0;
      margin-right: 0;
      margin-bottom: 8px;
      margin-left: 0;
      font-size: 15px;
      font-weight: 650;
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
        padding-top: 48px;
        padding-bottom: 40px;
      }

      .favorite-list {
        column-gap: 12px;
      }

      .favorite-item {
        flex-basis: calc(25% - 12px);
        min-width: 64px;
      }

      .start-page-details {
        margin-top: 40px;
      }
    }

    @media (max-width: 400px) {
      .favorite-item {
        flex-basis: calc(33.333% - 12px);
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
        --page: #17191d;
        --surface: #24272d;
        --surface-muted: #2d3138;
        --text: #f1f3f6;
        --muted: #a7aeb9;
        --line: #393e47;
        --focus: #83aaff;
      }

      .favorite-link:hover .favorite-icon {
        background-color: #2b2f36;
        box-shadow: 0 7px 20px rgba(0, 0, 0, 0.28);
      }

      .baidu { background-color: #202c50; }
      .hao123 { background-color: #1d352d; }
      .bilibili { background-color: #432832; }
      .zhihu { background-color: #202f45; }
      .weibo, .taobao { background-color: #402a27; }
      .jd, .netease { background-color: #3e282b; }
    }
  </style>
</head>
<body>
  <main class="start-page">
    <section id="favorites" class="favorites" aria-labelledby="favorites-title" data-start-page-primary="favorites">
      <h1 id="favorites-title" class="section-heading">常用网站</h1>
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
