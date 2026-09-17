use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use render_core::dom::Dom;
use render_core::dom::NodeId;
use render_core::dom::NodeKind;
use render_core::html::parse_document;
use render_core::js::RuntimeLimits;
use render_core::layout::{PhysicalPoint, PhysicalRect};
use render_core::navigation::HistoryEntry;
use render_core::paint::PaintCoordinateSpace;
use render_core::paint::{ClipShape, Color, DisplayCommand, Surface, Transform2D};
use render_core::script::ScriptDiscoveryLimits;
use render_net::NetworkWorker;
use render_net::{FetchConfig, FetchRequest, HttpTransport, Url};
use winit::dpi::{PhysicalPosition, PhysicalSize as WindowSize};
use winit::event::ElementState;
use winit::event::MouseScrollDelta;
use winit::keyboard::Key;
use winit::keyboard::NamedKey;

use crate::app::BrowserApp;
use crate::app::HostPlatform;
use crate::app::RawKeyInput;
use crate::app::address_shortcut;
use crate::app::primary_modifier_for;
use crate::app::wheel_document_delta_y;
use crate::content_interaction;
use crate::content_interaction::{
    ContentHitRegion, associated_form_for_node, content_text_input_value, content_wrapper_control,
    get_content_navigation_target, hit_test_content_regions, submit_form_for_node,
};
use crate::frame::FrameDamage;
use crate::frame::FrameRect;
use crate::frame::blit_page;
use crate::frame::surface_to_softbuffer;
use crate::page_source::PageSource;
use crate::page_source::home_source;
use crate::page_source::network_start_source;
use crate::page_source::source_from_network_response;
use crate::page_state::PageNavigation;
use crate::page_state::PageState;
use crate::render_worker::PageRenderFrame;
use crate::render_worker::PageRenderPayload;
use render_browser::worker::RenderJob;
use render_browser::chrome::ChromeLayout;
use render_browser::chrome::HitTarget;
use render_browser::chrome::Point;
use render_browser::editor::AddressCommand;
use render_browser::editor::AddressEditor;
use render_browser::font_backend::SystemFontBackend;
use render_browser::home::HOME_TITLE;
use render_browser::model::TabModel;
use render_browser::model::TabId;
use render_browser::navigation::NavigationTarget;
use render_browser::scripts::{
    plan_classic_scripts, plan_unstarted_classic_scripts, prepare_script_batch,
};
use render_browser::settings::CacheClearUiState;
use render_browser::worker::RenderCancellation;
use render_browser::worker::RenderFailure;
use render_browser::worker::RenderWorkerOptions;
use render_core::js::ElementRect;

#[test]
fn converts_core_surface_to_softbuffer_rgb_words() {
    let surface = Surface::new(1, 1, Color::rgb(0x12, 0x34, 0x56));
    assert_eq!(surface_to_softbuffer(&surface), [0x0012_3456]);
}

#[test]
fn frame_damage_clips_and_merges_touching_regions() {
    let mut damage = FrameDamage::default();
    damage.mark_rect(
        FrameRect {
            x: 8,
            y: 8,
            width: 10,
            height: 10,
        },
        100,
        100,
    );
    damage.mark_rect(
        FrameRect {
            x: 18,
            y: 8,
            width: 10,
            height: 10,
        },
        100,
        100,
    );
    assert_eq!(
        damage.rects,
        [FrameRect {
            x: 8,
            y: 8,
            width: 20,
            height: 10,
        }]
    );
    damage.mark_rect(
        FrameRect {
            x: 95,
            y: 95,
            width: 20,
            height: 20,
        },
        100,
        100,
    );
    assert!(damage.rects.contains(&FrameRect {
        x: 95,
        y: 95,
        width: 5,
        height: 5,
    }));
}

#[test]
fn frame_damage_switches_to_full_for_large_updates() {
    let mut damage = FrameDamage::default();
    damage.mark_rect(
        FrameRect {
            x: 0,
            y: 0,
            width: 80,
            height: 100,
        },
        100,
        100,
    );
    assert!(damage.full);
    assert!(damage.rects.is_empty());
}

#[test]
fn network_start_source_preserves_the_requested_url() {
    let url = Url::parse("https://www.baidu.com/").expect("valid URL");
    let source = network_start_source(url.clone());

    assert_eq!(
        source.target,
        render_browser::navigation::NavigationTarget::Url(url)
    );
    assert!(source.html.is_empty());
}

#[test]
fn data_document_response_becomes_a_renderable_html_page() {
    let url = Url::parse("data:text/html,%3Ctitle%3EData%3C%2Ftitle%3E%3Ch1%3EHello%3C%2Fh1%3E")
        .expect("valid data URL");
    let response = HttpTransport::new(FetchConfig::default())
        .fetch(
            &FetchRequest::get(url.clone()),
            &render_net::CancelToken::default(),
        )
        .expect("data response");
    let source = source_from_network_response(&response).expect("renderable data document");

    assert_eq!(source.target, NavigationTarget::Url(url));
    assert!(source.html.contains("<h1>Hello</h1>"));
}

#[test]
fn address_control_shortcuts_map_to_shared_edit_commands() {
    assert_eq!(
        address_shortcut(&Key::Character("z".into()), false),
        Some(AddressCommand::Undo)
    );
    assert_eq!(
        address_shortcut(&Key::Character("Z".into()), true),
        Some(AddressCommand::Redo)
    );
    for (key, command) in [
        ("c", AddressCommand::Copy),
        ("x", AddressCommand::Cut),
        ("v", AddressCommand::Paste),
        ("a", AddressCommand::SelectAll),
    ] {
        assert_eq!(
            address_shortcut(&Key::Character(key.into()), false),
            Some(command)
        );
    }
}

#[test]
fn primary_modifier_matches_macos_and_windows_conventions() {
    assert!(primary_modifier_for(HostPlatform::MacOs, false, true));
    assert!(!primary_modifier_for(HostPlatform::MacOs, true, false));
    assert!(primary_modifier_for(HostPlatform::Other, true, false));
    assert!(!primary_modifier_for(HostPlatform::Other, false, true));
}

#[test]
fn page_blit_starts_below_chrome_and_clips() {
    let mut destination = vec![0; 4 * 4];
    let source = vec![7; 4 * 4];
    blit_page(
        &mut destination,
        WindowSize::new(4, 4),
        &source,
        WindowSize::new(4, 4),
        2,
    );
    assert_eq!(&destination[..8], &[0; 8]);
    assert_eq!(&destination[8..], &[7; 8]);
}

#[test]
fn content_hit_test_accounts_for_chrome_scroll_and_paint_order() {
    let mut dom = render_core::dom::Dom::new();
    let document_source = dom.create_element("div");
    let top_source = dom.create_element("button");
    let regions = [
        ContentHitRegion {
            bounds: PhysicalRect::new(10.0, 140.0, 100.0, 30.0),
            source: Some(document_source),
            coordinate_space: PaintCoordinateSpace::Document,
            hit_testable: true,
        },
        ContentHitRegion {
            bounds: PhysicalRect::new(10.0, 140.0, 100.0, 30.0),
            source: Some(top_source),
            coordinate_space: PaintCoordinateSpace::Document,
            hit_testable: true,
        },
    ];

    assert_eq!(
        content_interaction::hit_test_content_regions(
            regions.into_iter(),
            Point { x: 20.0, y: 90.0 },
            60,
            PhysicalPoint { x: 0.0, y: 120.0 },
        ),
        Some(top_source)
    );
    assert_eq!(
        hit_test_content_regions(
            regions.into_iter(),
            Point { x: 20.0, y: 59.0 },
            60,
            PhysicalPoint { x: 0.0, y: 120.0 },
        ),
        None
    );
}

#[test]
fn structural_paint_commands_do_not_participate_in_content_hits() {
    let bounds = PhysicalRect::new(0.0, 0.0, 100.0, 50.0);
    assert!(content_interaction::is_content_hit_command(
        &DisplayCommand::SolidRect {
            rect: bounds,
            color: Color::rgb(0xff, 0xff, 0xff),
        }
    ));
    assert!(!content_interaction::is_content_hit_command(
        &DisplayCommand::PushClip(ClipShape::Rect(bounds))
    ));
    assert!(!content_interaction::is_content_hit_command(
        &DisplayCommand::PopClip
    ));
    assert!(!content_interaction::is_content_hit_command(
        &DisplayCommand::PushTransform(Transform2D::default())
    ));
    assert!(!content_interaction::is_content_hit_command(
        &DisplayCommand::PopTransform
    ));
    assert!(!content_interaction::is_content_hit_command(
        &DisplayCommand::PopStackingContext
    ));
}

#[test]
fn clip_items_do_not_shadow_painted_content_during_hit_testing() {
    let mut dom = render_core::dom::Dom::new();
    let root = dom.create_element("div");
    let link = dom.create_element("a");
    // A root-sized clip pair surrounds every content item in paint order;
    // the reverse scan must land on the link content, not on the clips.
    let regions = [
        ContentHitRegion {
            bounds: PhysicalRect::new(0.0, 0.0, 1_770.0, 1_026.0),
            source: Some(root),
            coordinate_space: PaintCoordinateSpace::Document,
            hit_testable: false,
        },
        ContentHitRegion {
            bounds: PhysicalRect::new(36.0, 30.0, 24.0, 20.0),
            source: Some(link),
            coordinate_space: PaintCoordinateSpace::Document,
            hit_testable: true,
        },
        ContentHitRegion {
            bounds: PhysicalRect::new(0.0, 0.0, 1_770.0, 1_026.0),
            source: Some(root),
            coordinate_space: PaintCoordinateSpace::Document,
            hit_testable: false,
        },
    ];
    assert_eq!(
        hit_test_content_regions(
            regions.into_iter(),
            Point { x: 48.0, y: 100.0 },
            60,
            PhysicalPoint { x: 0.0, y: 0.0 },
        ),
        Some(link)
    );
}

#[test]
fn text_input_value_defaults_missing_and_empty_type_to_text() {
    let document = parse_document(
        "<input id='kw' name='wd' value=''><input id='blank' type='' value='x'>\
             <input id='hidden' type='hidden' value='h'><input id='search' type='SEARCH' value='s'>\
             <textarea id='ta'>hi</textarea><div id='plain'>text</div>",
    );
    let dom = &document.dom;
    let find = |id: &str| {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            if dom.attribute(node, "id").ok().flatten() == Some(id) {
                return node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().copied());
        }
        panic!("element {id} should exist");
    };
    assert_eq!(
        content_text_input_value(dom, find("kw")),
        Some(String::new())
    );
    assert_eq!(
        content_text_input_value(dom, find("blank")),
        Some("x".to_owned())
    );
    assert_eq!(content_text_input_value(dom, find("hidden")), None);
    assert_eq!(
        content_text_input_value(dom, find("search")),
        Some("s".to_owned())
    );
    assert_eq!(
        content_text_input_value(dom, find("ta")),
        Some("hi".to_owned())
    );
    assert_eq!(content_text_input_value(dom, find("plain")), None);
}

#[test]
fn wrapper_click_routes_to_dominant_embedded_text_control() {
    let document =
        parse_document("<div id='wrap'><textarea id='ta'></textarea><button>go</button></div>");
    let dom = &document.dom;
    let find = |id: &str| {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            if dom.attribute(node, "id").ok().flatten() == Some(id) {
                return node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().copied());
        }
        panic!("element {id} should exist");
    };
    let wrap = find("wrap");
    let ta = find("ta");
    let mut geometry = BTreeMap::new();
    geometry.insert(
        wrap.as_u64(),
        ElementRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        },
    );
    geometry.insert(
        ta.as_u64(),
        ElementRect {
            x: 5.0,
            y: 5.0,
            width: 90.0,
            height: 28.0,
        },
    );
    assert_eq!(content_wrapper_control(dom, &geometry, wrap), Some(ta));

    // A control covering only a sliver of the wrapper does not capture it.
    geometry.insert(
        ta.as_u64(),
        ElementRect {
            x: 5.0,
            y: 5.0,
            width: 10.0,
            height: 10.0,
        },
    );
    assert_eq!(content_wrapper_control(dom, &geometry, wrap), None);
}

#[test]
fn wrapper_without_geometry_or_control_stays_unrouted() {
    let document = parse_document(
        "<div id='bare'><p>nothing interactive</p></div><div id='hidden-wrap'><textarea id='ta'></textarea></div>",
    );
    let dom = &document.dom;
    let find = |id: &str| {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            if dom.attribute(node, "id").ok().flatten() == Some(id) {
                return node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().copied());
        }
        panic!("element {id} should exist");
    };
    let bare = find("bare");
    let hidden_wrap = find("hidden-wrap");
    let mut geometry = BTreeMap::new();
    geometry.insert(
        bare.as_u64(),
        ElementRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 0.0,
        },
    );
    assert_eq!(content_wrapper_control(dom, &geometry, bare), None);
    // No geometry entry for the wrapper at all.
    assert_eq!(content_wrapper_control(dom, &geometry, hidden_wrap), None);
}

#[test]
fn submit_descendant_builds_get_navigation_target() {
    let document = parse_document(
        "<form action='/s'><input name='wd' value='small browser'><button id='go' name='from' value='render'><span>Search</span></button></form>",
    );
    let dom = &document.dom;
    let mut pending = vec![dom.document()];
    let hit_node = loop {
        let node = pending.pop().expect("submit text exists");
        if matches!(dom.node(node).map(render_core::dom::Node::kind), Some(NodeKind::Text(text)) if text == "Search")
        {
            break node;
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev());
    };
    let target = get_content_navigation_target(
        dom,
        hit_node,
        &Url::parse("https://www.baidu.com/").expect("valid base URL"),
    )
    .expect("GET submit navigation");

    assert_eq!(
        target.as_str(),
        "https://www.baidu.com/s?wd=small+browser&from=render"
    );
}

#[test]
fn submit_and_text_controls_resolve_their_associated_form() {
    let document = parse_document(
        "<form id='search'><input id='query' name='wd'><button id='go'><span>Go</span></button></form>",
    );
    let dom = &document.dom;
    let mut pending = vec![dom.document()];
    let mut query = None;
    let mut text = None;
    while let Some(node) = pending.pop() {
        if dom.attribute(node, "id").ok().flatten() == Some("query") {
            query = Some(node);
        }
        if matches!(
            dom.node(node).map(render_core::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "Go"
        ) {
            text = Some(node);
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev().copied());
    }
    let mut forms = vec![dom.document()];
    let form = loop {
        let node = forms.pop().expect("form");
        if dom.attribute(node, "id").ok().flatten() == Some("search") {
            break node;
        }
        forms.extend(dom.children(node).unwrap_or_default().iter().rev().copied());
    };
    assert_eq!(
        associated_form_for_node(dom, query.expect("query")),
        Some(form)
    );
    assert_eq!(
        submit_form_for_node(dom, text.expect("button text")),
        Some(form)
    );
}

#[test]
fn get_submission_reads_the_live_text_input_value() {
    let mut document = parse_document(
        "<form action='/s'><input id=query type=search name=wd value=old><button id=go>Search</button></form>",
    );
    let mut pending = vec![document.dom.document()];
    let mut query = None;
    let mut submit = None;
    while let Some(node) = pending.pop() {
        if document.dom.attribute(node, "id").ok().flatten() == Some("query") {
            query = Some(node);
        }
        if document.dom.attribute(node, "id").ok().flatten() == Some("go") {
            submit = Some(node);
        }
        pending.extend(document.dom.children(node).unwrap_or_default().iter().rev());
    }
    document
        .dom
        .set_attribute(query.expect("query input"), "value", "实时 搜索")
        .expect("live value mutation");

    let target = get_content_navigation_target(
        &document.dom,
        submit.expect("submit button"),
        &Url::parse("https://www.baidu.com/").expect("valid base URL"),
    )
    .expect("GET submit navigation");

    assert_eq!(
        target.as_str(),
        "https://www.baidu.com/s?wd=%E5%AE%9E%E6%97%B6+%E6%90%9C%E7%B4%A2"
    );
}

#[test]
fn link_descendant_resolves_against_document_url() {
    let document = parse_document("<a href='/video/next'><span>Next</span></a>");
    let dom = &document.dom;
    let mut pending = vec![dom.document()];
    let hit_node = loop {
        let node = pending.pop().expect("link text exists");
        if matches!(dom.node(node).map(render_core::dom::Node::kind), Some(NodeKind::Text(text)) if text == "Next")
        {
            break node;
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev());
    };

    let target = get_content_navigation_target(
        dom,
        hit_node,
        &Url::parse("https://example.test/current/page").expect("valid base URL"),
    )
    .expect("link navigation");

    assert_eq!(target.as_str(), "https://example.test/video/next");
}

#[test]
fn page_states_own_independent_session_histories() {
    let mut first = PageState::new(home_source());
    let second = PageState::new(home_source());
    first
        .history
        .push(HistoryEntry::new(
            Url::parse("https://example.test/").expect("valid test URL"),
        ))
        .expect("history push");

    assert_eq!(first.history.len(), 2);
    assert_eq!(second.history.len(), 1);
    assert_eq!(
        first.history.current().url.as_str(),
        "https://example.test/"
    );
    assert_eq!(second.history.current().url.as_str(), "render://home");
}

#[test]
fn wheel_deltas_map_to_document_scroll_direction() {
    assert!(
        (wheel_document_delta_y(MouseScrollDelta::LineDelta(0.0, -2.0)) - 80.0).abs()
            < f32::EPSILON
    );
    assert!(
        (wheel_document_delta_y(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
            0.0, 12.5
        ))) + 12.5)
            .abs()
            < f32::EPSILON
    );
}

#[test]
fn committed_navigation_resets_only_that_tabs_scroll_state() {
    let mut first = PageState::new(home_source());
    let mut second = PageState::new(home_source());
    first.scroll.update_metrics(1_000.0, 300.0);
    second.scroll.update_metrics(1_000.0, 300.0);
    assert!(first.scroll.scroll_by(240.0));
    assert!(second.scroll.scroll_by(120.0));

    first.set_source(PageSource {
        html: "<!doctype html><title>Next</title>".into(),
        title: "Next".into(),
        target: render_browser::navigation::NavigationTarget::Home,
    });

    assert!(first.scroll.offset_y().abs() < f32::EPSILON);
    assert!((second.scroll.offset_y() - 120.0).abs() < f32::EPSILON);
}

#[test]
fn network_commit_takes_the_title_from_the_parsed_head_title() {
    let mut page = PageState::new(PageSource {
        html: "<!doctype html><html><head><meta charset=utf-8>\
                   <title>百度一下，你就知道</title></head><body></body></html>"
            .into(),
        title: "www.baidu.com".into(),
        target: NavigationTarget::Url(Url::parse("https://www.baidu.com/").expect("page URL")),
    });
    assert_eq!(page.navigation.committed().title, "www.baidu.com");

    assert!(page.sync_committed_title());

    assert_eq!(page.navigation.committed().title, "百度一下，你就知道");
}

#[test]
fn title_less_page_keeps_the_url_fallback_title() {
    let mut page = PageState::new(PageSource {
        html: "<!doctype html><html><body><p>plain</p></body></html>".into(),
        title: "www.baidu.com".into(),
        target: NavigationTarget::Url(Url::parse("https://www.baidu.com/").expect("page URL")),
    });

    assert!(!page.sync_committed_title());
    assert_eq!(page.navigation.committed().title, "www.baidu.com");
}

#[test]
fn script_assigned_document_title_propagates_to_the_committed_title() {
    let mut page = PageState::new(PageSource {
        html: "<!doctype html><html><head><title>Static</title></head>\
                   <body><script>document.title = 'Script Title';</script></body></html>"
            .into(),
        title: "www.baidu.com".into(),
        target: NavigationTarget::Url(Url::parse("https://www.baidu.com/").expect("page URL")),
    });
    page.page
        .queue_script("document.title = 'Script Title';")
        .expect("title script should queue");

    let (changed, _) = page.run_page_turns();

    assert!(changed);
    assert!(page.sync_committed_title());
    assert_eq!(page.navigation.committed().title, "Script Title");
}

#[test]
fn timer_deferred_document_title_propagates_on_a_later_turn() {
    let mut page = PageState::new(PageSource {
        html: "<!doctype html><html><body></body></html>".into(),
        title: "www.baidu.com".into(),
        target: NavigationTarget::Url(Url::parse("https://www.baidu.com/").expect("page URL")),
    });
    page.page
        .queue_script("setTimeout(function () { document.title = 'Async Title'; }, 10);")
        .expect("timer script should queue");

    // The timer has not fired yet, so the title stays at the URL fallback.
    page.page
        .pump_at_most_without_render(std::time::Duration::ZERO, 4)
        .expect("idle pump succeeds");
    assert!(!page.sync_committed_title());
    assert_eq!(page.navigation.committed().title, "www.baidu.com");

    page.page
        .pump_at_most_without_render(std::time::Duration::from_millis(20), 4)
        .expect("timer pump succeeds");
    assert!(page.sync_committed_title());
    assert_eq!(page.navigation.committed().title, "Async Title");
}

#[test]
fn prepared_scripts_mutate_the_persistent_page_document() {
    let mut page = PageState::new(PageSource {
            html: "<p id=message>before</p><script>var prefix = 'after';</script><script>document.getElementById('message').textContent = prefix;</script>".into(),
            title: "Scripts".into(),
            target: render_browser::navigation::NavigationTarget::Home,
        });
    let base_url = page.navigation.committed().target.history_url();
    let plan = plan_classic_scripts(
        page.page.document(),
        &base_url,
        ScriptDiscoveryLimits::default(),
    );
    let preparation = prepare_script_batch(
        page.page.document(),
        &plan,
        Vec::new(),
        &RuntimeLimits::default(),
    );

    assert!(page.execute_script_batch(preparation));
    let dom = page.page.document().dom();
    let mut pending = vec![dom.document()];
    let message = loop {
        let node = pending.pop().expect("message element exists");
        if matches!(
            dom.node(node).map(render_core::dom::Node::kind),
            Some(NodeKind::Element(_))
        ) && dom.attribute(node, "id").expect("element lookup succeeds") == Some("message")
        {
            break node;
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev());
    };
    let text = page
        .page
        .document()
        .dom()
        .children(message)
        .expect("children")[0];
    assert!(matches!(
        page.page
            .document()
            .dom()
            .node(text)
            .map(render_core::dom::Node::kind),
        Some(NodeKind::Text(value)) if value == "after"
    ));
}

#[test]
fn committed_page_target_is_visible_as_script_location() {
    let mut page = PageState::new(PageSource {
        html: "<!doctype html><p></p>".into(),
        title: "Location".into(),
        target: render_browser::navigation::NavigationTarget::Url(
            Url::parse("https://example.test/app/index.html?q=1#view").expect("page URL"),
        ),
    });
    page.page
        .queue_script("location.href;")
        .expect("script should queue");
    let turn = page
        .page
        .run_one_turn_reference()
        .expect("turn should run")
        .expect("script turn");
    assert_eq!(
        turn.executions[0]
            .result
            .as_ref()
            .expect("script should execute")
            .value,
        render_core::js::JsValue::String("https://example.test/app/index.html?q=1#view".to_owned())
    );
}

#[test]
fn executed_bootstrap_script_exposes_inserted_external_script_to_follow_up_scan() {
    let mut page = PageState::new(PageSource {
            html: "<main id=host></main><script>const chunk = document.createElement('script'); chunk.setAttribute('src', 'assets/chunk.js'); document.getElementById('host').appendChild(chunk);</script>".into(),
            title: "Dynamic scripts".into(),
            target: render_browser::navigation::NavigationTarget::Url(
                Url::parse("https://example.test/app/index.html").expect("page URL"),
            ),
        });
    let base_url = page.navigation.committed().target.history_url();
    let initial = plan_classic_scripts(
        page.page.document(),
        &base_url,
        ScriptDiscoveryLimits::default(),
    );
    let started = initial.owners().collect::<HashSet<_>>();
    let preparation = prepare_script_batch(
        page.page.document(),
        &initial,
        Vec::new(),
        &RuntimeLimits::default(),
    );

    assert!(page.execute_script_batch(preparation));
    let follow_up = plan_unstarted_classic_scripts(
        page.page.document(),
        &base_url,
        ScriptDiscoveryLimits::default(),
        &started,
        true,
    );

    assert_eq!(follow_up.resources.len(), 1);
    assert_eq!(
        follow_up.resources[0].request.url.as_str(),
        "https://example.test/app/assets/chunk.js"
    );
}

#[test]
fn pending_navigation_keeps_committed_page_until_commit() {
    let mut navigation = PageNavigation::<()>::new(home_source());
    let pending_url = Url::parse("https://example.test/pending").expect("valid URL");
    navigation.begin(pending_url.clone(), ());

    assert_eq!(navigation.committed().title, HOME_TITLE);
    assert_eq!(
        navigation.committed().target,
        render_browser::navigation::NavigationTarget::Home
    );
    assert_eq!(navigation.pending_url(), Some(&pending_url));

    let committed_url = Url::parse("https://example.test/committed").expect("valid URL");
    navigation.commit(PageSource {
        html: "<title>Committed</title>".into(),
        title: "Committed".into(),
        target: render_browser::navigation::NavigationTarget::Url(committed_url.clone()),
    });

    assert_eq!(navigation.pending_url(), None);
    assert_eq!(navigation.committed().title, "Committed");
    assert_eq!(
        navigation.committed().target,
        render_browser::navigation::NavigationTarget::Url(committed_url)
    );
}

/// Minimal baidu-like search page: a form whose action is a document-relative
/// path, a named text input, hidden fields, and a submit button.
const SEARCH_PAGE_HTML: &str = "<!doctype html><html><body>\
     <form id='form' action='/s'>\
       <input type='hidden' name='ie' value='utf-8'>\
       <span id='wrap'><input id='kw' name='wd' value='' maxlength='255'></span>\
       <input type='submit' id='su' value='submit'>\
     </form></body></html>";

fn find_id(dom: &Dom, id: &str) -> NodeId {
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        if dom.attribute(node, "id").ok().flatten() == Some(id) {
            return node;
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().copied());
    }
    panic!("element {id} should exist");
}

/// Builds a headless `BrowserApp` (no window, no render side effects) showing
/// the search page at a `file:` document URL so any submit navigation stays
/// offline.
fn headless_search_app() -> (BrowserApp, TabId, NodeId) {
    let url = Url::parse("file:///rENDER-test-fixtures/page.html").expect("test document URL");
    let source = PageSource {
        html: SEARCH_PAGE_HTML.to_owned(),
        title: "search".to_owned(),
        target: NavigationTarget::Url(url),
    };
    let tabs = TabModel::new(source.title.clone(), source.target.display_address());
    let active = tabs.active_id();
    let mut page = PageState::new(source);

    let fonts = Arc::new(SystemFontBackend::load().expect("system fonts load"));
    let network = NetworkWorker::start(HttpTransport::new(FetchConfig::default()))
        .expect("network worker starts");
    let render_worker = crate::render_worker::PageRenderWorker::start(
        RenderWorkerOptions::default(),
        |_job: RenderJob<PageRenderPayload>,
         _cancellation: &RenderCancellation|
         -> Result<PageRenderFrame, RenderFailure> { Err(RenderFailure::Cancelled) },
        || {},
    )
    .expect("render worker starts");

    let mut app = BrowserApp {
        tabs,
        pages: HashMap::new(),
        fonts,
        render_worker,
        network,
        http_cache: render_browser::cache::HttpCache::default(),
        disk_cache: None,
        pending_disk_clear: None,
        cache_clear_state: CacheClearUiState::Ready,
        editor: AddressEditor::new("file:///rENDER-test-fixtures/page.html"),
        content_editor: None,
        clipboard: render_browser::editor::NativeClipboard::default(),
        window: None,
        context: None,
        surface: None,
        layout: None,
        frame: Vec::new(),
        frame_size: WindowSize::new(800, 600),
        frame_damage: FrameDamage::default(),
        theme: render_browser::chrome::ChromeTheme::Light,
        cursor: Point { x: 0.0, y: 0.0 },
        hot: HitTarget::Chrome,
        cursor_icon: winit::window::CursorIcon::Default,
        drag: None,
        address_selecting: false,
        address_menu: None,
        modifiers: winit::keyboard::ModifiersState::default(),
        title_bar_clicks: render_browser::chrome::TitleBarClickTracker::default(),
        address_clicks: render_browser::chrome::AddressClickTracker::default(),
        left_pointer_down: false,
        started_at: Instant::now(),
    };
    app.layout = Some(ChromeLayout::new(800, 600, 1.0, app.tabs.tabs()));

    let kw = {
        let dom = page.page.document().dom();
        find_id(dom, "kw")
    };
    page.geometry.insert(
        kw.as_u64(),
        ElementRect {
            x: 200.0,
            y: 150.0,
            width: 400.0,
            height: 34.0,
        },
    );
    app.pages.insert(active, page);
    (app, active, kw)
}

fn pressed_character(character: &str) -> RawKeyInput {
    crate::app::RawKeyInput {
        state: ElementState::Pressed,
        logical_key: Key::Character(character.into()),
        text: Some(character.to_owned()),
    }
}

fn pressed_named(key: NamedKey) -> RawKeyInput {
    crate::app::RawKeyInput {
        state: ElementState::Pressed,
        logical_key: Key::Named(key),
        text: None,
    }
}

fn committed_value(app: &BrowserApp, tab: TabId, node: NodeId) -> String {
    app.content_text_input_value(tab, node).unwrap_or_default()
}

fn click_search_box(app: &mut BrowserApp) {
    let chrome_height = app.layout.as_ref().expect("chrome layout").chrome_height;
    app.cursor = Point {
        x: 400.0,
        y: chrome_height as f32 + 167.0,
    };
    app.handle_content_press();
}

#[test]
fn clicking_the_search_box_focuses_it_and_typing_renders_the_value() {
    let (mut app, tab, kw) = headless_search_app();
    assert!(app.content_editor.is_none());
    click_search_box(&mut app);

    let content = app.content_editor.as_ref().expect("box gains focus");
    assert_eq!(content.tab, tab);
    assert_eq!(content.node, kw);

    // A keystroke must both commit the value and schedule a page render; the
    // committed mutation happens before the page-turn revision baseline, so
    // the render has to be scheduled unconditionally.
    app.handle_keyboard(&pressed_character("a"));
    assert_eq!(committed_value(&app, tab, kw), "a");
    let page = app.pages.get(&tab).expect("page stays open");
    assert!(
        page.expected_render.is_some(),
        "typing must schedule a page render"
    );

    // Backspace removes the character and fires the same pipeline.
    app.handle_keyboard(&pressed_named(NamedKey::Backspace));
    assert_eq!(committed_value(&app, tab, kw), "");

    // Enter on the empty box submits the form through the GET pipeline.
    let history_before = app.pages.get(&tab).expect("page").history.len();
    app.handle_keyboard(&pressed_named(NamedKey::Enter));
    let page = app.pages.get(&tab).expect("page stays open");
    assert!(
        page.history.len() > history_before,
        "Enter must push a history entry for the form submission"
    );
    assert!(app.content_editor.is_none());
}

#[test]
fn ime_preedit_previews_and_commit_lands_in_the_input_value() {
    let (mut app, tab, kw) = headless_search_app();
    click_search_box(&mut app);

    app.handle_keyboard(&pressed_character("a"));

    // Live composition text is mirrored into the control so the user can see
    // it before committing.
    app.handle_content_preedit("拼");
    assert_eq!(committed_value(&app, tab, kw), "a拼");

    // Clearing the composition without committing restores the typed value.
    app.handle_content_preedit("");
    assert_eq!(committed_value(&app, tab, kw), "a");

    app.handle_content_preedit("拼音");
    assert_eq!(committed_value(&app, tab, kw), "a拼音");

    // The commit replaces the composition with the final text and fires the
    // input pipeline.
    app.handle_content_ime_commit("搜索单词");
    assert_eq!(committed_value(&app, tab, kw), "a搜索单词");
    let content = app.content_editor.as_ref().expect("editor stays focused");
    assert_eq!(content.editor.text(), "a搜索单词");
    assert!(content.editor.preedit().is_empty());
    let page = app.pages.get(&tab).expect("page stays open");
    assert!(
        page.expected_render.is_some(),
        "committing composition text must schedule a page render"
    );
}

#[test]
fn enter_confirming_an_ime_composition_does_not_submit_the_form() {
    let (mut app, tab, _kw) = headless_search_app();
    click_search_box(&mut app);
    app.handle_content_preedit("输入");
    app.handle_content_ime_commit("输入");
    let history_before = app.pages.get(&tab).expect("page").history.len();

    // The Enter keydown that confirms the composition is latched away.
    app.handle_keyboard(&pressed_named(NamedKey::Enter));
    assert!(
        app.content_editor.is_some(),
        "composition Enter must keep the editor focused"
    );
    assert_eq!(
        app.pages.get(&tab).expect("page").history.len(),
        history_before
    );

    // A second Enter is a real submission.
    app.handle_keyboard(&pressed_named(NamedKey::Enter));
    let page = app.pages.get(&tab).expect("page stays open");
    assert!(page.history.len() > history_before);
    assert!(app.content_editor.is_none());
}

#[test]
fn raw_keystrokes_are_dropped_while_a_composition_is_live() {
    let (mut app, _tab, _kw) = headless_search_app();
    click_search_box(&mut app);
    app.handle_content_preedit("p");
    // Platforms that deliver text events during composition must not
    // double-insert; the commit carries the final text.
    app.handle_keyboard(&pressed_character("p"));
    let content = app.content_editor.as_ref().expect("editor stays focused");
    assert_eq!(content.editor.text(), "");
    assert_eq!(content.editor.preedit(), "p");
}
