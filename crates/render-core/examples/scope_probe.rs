//! Demo probe for CSS Cascade 6 `@scope`: prints the computed `color` of each
//! target element so scoping, limits, nesting and implicit roots are visible.
//!
//! `cargo run -p render-core --example scope_probe`

use render_core::css::computed::ComputedValue;
use render_core::css::selector::{MatchContext, parse_selector_list, select_all};
use render_core::document::{Document, DocumentRenderOptions};
use render_core::dom::NodeId;

const DOCUMENT: &str = "<!doctype html>\
<style>\
  @scope (.card) to (.limit) {\
    p { background-color: rgb(200, 0, 0) }\
    :scope { color: rgb(0, 120, 0) }\
  }\
  @scope (.outer) {\
    p { background-color: rgb(0, 0, 220) }\
    @scope (.inner) {\
      p { background-color: rgb(180, 0, 180) }\
    }\
  }\
</style>\
<div class=card id=card>\
  <p id=in-scope>inside the card</p>\
  <div class=limit><p id=under-limit>under the limit</p></div>\
</div>\
<p id=outside>outside every scope</p>\
<div class=outer id=outer>\
  <p id=outer-p>outer paragraph</p>\
  <div class=inner id=inner><p id=inner-p>inner paragraph</p></div>\
</div>";

const IMPLICIT: &str = "<!doctype html>\
<div id=host>\
  <style>@scope { p { background-color: rgb(1, 2, 3) } }</style>\
  <p id=implicit-in>inside the host</p>\
</div>\
<p id=implicit-out>outside the host</p>";

fn node(dom: &render_core::dom::Dom, id: &str) -> NodeId {
    let selector = parse_selector_list(&format!("#{id}")).expect("valid selector");
    select_all(dom, dom.document(), &selector, &MatchContext::default())[0]
}

fn main() {
    for (title, html, ids) in [
        (
            "@scope (.card) to (.limit) + nested @scope (.outer) / @scope (.inner)",
            DOCUMENT,
            &[
                "card",
                "in-scope",
                "under-limit",
                "outside",
                "outer",
                "outer-p",
                "inner",
                "inner-p",
            ][..],
        ),
        (
            "implicit @scope inside <div id=host>",
            IMPLICIT,
            &["host", "implicit-in", "implicit-out"][..],
        ),
    ] {
        let document = Document::parse(html);
        let render = document.render_reference(DocumentRenderOptions::default());
        println!();
        println!("{title}");
        println!("{:-<78}", "");
        println!("{:<16} {:<24} background-color", "element", "color");
        for id in ids {
            let style = &render.styles[&node(document.dom(), id)];
            let color = style.get("color").map_or("<none>", ComputedValue::css_text);
            let background = style
                .get("background-color")
                .map_or("<none>", ComputedValue::css_text);
            println!("{:<16} {:<24} {background}", format!("#{id}"), color);
        }
    }
}
