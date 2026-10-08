use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use std::time::Instant;

use render_core::document::{Document, ExternalStyleSheets};
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
    ContentHitRegion, ancestor_wrapper_control, associated_form_for_node, content_text_input_value,
    content_wrapper_control, fallback_submit_form, get_content_navigation_target,
    hit_test_content_regions, submit_form_for_node, sync_submit_control_value,
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
use crate::render_worker::merge_current_style_sheets;
use render_browser::chrome::ChromeLayout;
use render_browser::chrome::HitTarget;
use render_browser::chrome::Point;
use render_browser::editor::AddressCommand;
use render_browser::editor::AddressEditor;
use render_browser::font_backend::SystemFontBackend;
use render_browser::home::HOME_TITLE;
use render_browser::model::TabId;
use render_browser::model::TabModel;
use render_browser::navigation::NavigationTarget;
use render_browser::resources::plan_external_style_sheets;
use render_browser::scripts::{
    plan_classic_scripts, plan_unstarted_classic_scripts, prepare_script_batch,
};
use render_browser::settings::CacheClearUiState;
use render_browser::worker::CompletedRender;
use render_browser::worker::RenderCancellation;
use render_browser::worker::RenderFailure;
use render_browser::worker::RenderJob;
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
    let rendered = |_node: NodeId| false;
    let target = get_content_navigation_target(
        dom,
        hit_node,
        &Url::parse("https://www.baidu.com/").expect("valid base URL"),
        &rendered,
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

    let rendered = |_node: NodeId| false;
    let target = get_content_navigation_target(
        &document.dom,
        submit.expect("submit button"),
        &Url::parse("https://www.baidu.com/").expect("valid base URL"),
        &rendered,
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

    let rendered = |_node: NodeId| false;
    let target = get_content_navigation_target(
        dom,
        hit_node,
        &Url::parse("https://example.test/current/page").expect("valid base URL"),
        &rendered,
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
fn a_document_restores_its_origins_local_storage_and_reports_changes_back() {
    use crate::profile::ProfileStorage;

    let origin = "https://storage.example.test";
    let mut store = ProfileStorage::open(None);
    store.apply_changes(origin, &[], &[("theme".to_owned(), "dark".to_owned())]);

    let mut page = PageState::new(PageSource {
        html: "<!doctype html><p>stored</p>".into(),
        title: "stored".into(),
        target: NavigationTarget::Url(Url::parse(&format!("{origin}/")).expect("page URL")),
    });
    page.restore_local_storage(&store);
    assert_eq!(
        page.page.runtime().local_storage_entries(),
        vec![("theme".to_owned(), "dark".to_owned())]
    );

    // The document changes its own copy, then the browser syncs it. Only the
    // keys that differ from the restored copy reach the store.
    page.page.runtime_mut().seed_local_storage(&[
        ("theme".to_owned(), "light".to_owned()),
        ("lang".to_owned(), "zh".to_owned()),
    ]);
    page.sync_local_storage(&mut store);
    assert_eq!(
        store.area(origin),
        vec![
            ("lang".to_owned(), "zh".to_owned()),
            ("theme".to_owned(), "light".to_owned()),
        ]
    );
}

#[test]
fn a_document_without_a_persistent_origin_keeps_its_storage_to_itself() {
    use crate::profile::ProfileStorage;

    let mut store = ProfileStorage::open(None);
    let mut page = PageState::new(PageSource {
        html: "<!doctype html><p>local file</p>".into(),
        title: "local".into(),
        target: NavigationTarget::Url(Url::parse("file:///tmp/page.html").expect("page URL")),
    });
    assert_eq!(page.local_storage_origin(), None);
    page.page
        .runtime_mut()
        .seed_local_storage(&[("k".to_owned(), "v".to_owned())]);
    page.sync_local_storage(&mut store);
    assert!(store.area("file://").is_empty());
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
fn script_fetches_run_before_stylesheets_and_execution_waits_for_them() {
    let html = "<!doctype html><html><head><title>Static</title>\
                   <link rel=\"stylesheet\" href=\"sheet.css\">\
                   </head><body><script src=\"app.js\"></script></body></html>";
    let (mut app, tab) = headless_app_with(html);

    // The stylesheet batch is still in flight (`styles_resolved` is false),
    // yet script discovery and fetching must already proceed.
    app.start_classic_scripts(tab);
    let plan = {
        let page = app.pages.get(&tab).expect("page state exists");
        assert!(
            page.pending_scripts.is_some(),
            "script fetch must start while stylesheets are pending"
        );
        assert!(page.held_scripts.is_none());
        assert!(!page.styles_resolved);
        page.pending_scripts
            .as_ref()
            .expect("pending scripts checked above")
            .plan
            .clone()
    };

    // The script body arrives before the stylesheet does: the batch is held
    // instead of executed, so no script side effects are visible yet.
    let response = script_response(&plan, "document.title = 'Fetched Title';");
    app.finish_classic_scripts(tab, vec![Ok(response)]);
    {
        let page = app.pages.get_mut(&tab).expect("page state exists");
        assert!(
            page.held_scripts.is_some(),
            "script execution must wait for the stylesheets"
        );
        assert!(!page.sync_committed_title());
        assert_eq!(page.navigation.committed().title, "Static");
    }

    // Once the stylesheets resolve, the held batch executes in order.
    {
        let page = app.pages.get_mut(&tab).expect("page state exists");
        page.cancel_style_sheets();
        page.styles_resolved = true;
    }
    app.start_classic_scripts(tab);
    {
        let page = app.pages.get_mut(&tab).expect("page state exists");
        assert!(page.held_scripts.is_none());
        // `start_classic_scripts` already synced the title while flushing,
        // so a further sync reports no change.
        assert!(!page.sync_committed_title());
        assert_eq!(page.navigation.committed().title, "Fetched Title");
    }
}

#[test]
fn script_inserted_stylesheet_is_discovered_after_initial_styles_resolve() {
    let (mut app, tab) = headless_app_with("<link rel=stylesheet href=a.css>");
    let (fresh, revision) = {
        let page = app.pages.get_mut(&tab).expect("page state");
        let base = page.navigation.committed().target.history_url();
        let first = plan_external_style_sheets(
            page.page.document(),
            &base,
            render_core::document::DocumentLimits::default(),
        );
        page.started_style_sheets
            .insert(first.resources[0].key.clone());
        page.styles_resolved = true;

        let dom = page.page.document_mut().dom_mut();
        let head = dom
            .parent(first.resources[0].key.owner)
            .expect("link parent");
        let added = dom.create_element("link");
        dom.set_attribute(added, "rel", "stylesheet").expect("rel");
        dom.set_attribute(added, "href", "b.css").expect("href");
        dom.append_child(head, added).expect("append link");
        page.dom_revision = dom.revision().as_u64();
        (
            plan_external_style_sheets(
                page.page.document(),
                &base,
                render_core::document::DocumentLimits::default(),
            ),
            page.dom_revision,
        )
    };
    assert_eq!(fresh.resources.len(), 2);

    app.schedule_page_render(tab, WindowSize::new(800, 600), false);
    let identity = app.pages.get(&tab).unwrap().expected_render.unwrap();
    app.commit_render(CompletedRender {
        identity,
        result: Ok(PageRenderFrame {
            frame: Vec::new(),
            viewport: WindowSize::new(800, 600),
            display_list: None,
            paint_scene: None,
            raster_background: Color::rgb(255, 255, 255),
            content_height: 0.0,
            viewport_height: 600.0,
            applied_style_sheets: None,
            style_plan: Some(fresh),
            style_diagnostics: Vec::new(),
            computed_styles: None,
            geometry: None,
            document_revision: revision,
        }),
    });

    let page = app.pages.get(&tab).expect("page state");
    let pending = page
        .pending_style_sheets
        .as_ref()
        .expect("new stylesheet requested");
    assert_eq!(pending.plan.resources.len(), 1);
    assert!(
        pending.plan.resources[0]
            .request
            .url
            .as_str()
            .ends_with("/b.css")
    );
    assert_eq!(page.started_style_sheets.len(), 2);
}

#[test]
fn timer_inserted_script_runs_after_initial_script_scan_completed() {
    let (mut app, tab) = headless_app_with("<main id=host></main>");
    {
        let page = app.pages.get_mut(&tab).expect("page state");
        page.styles_resolved = true;
        page.scripts_resolved = true;
        page.initial_script_scan_completed = true;
        page.page
            .queue_script(
                r#"setTimeout(function () {
                    var s = document.createElement('script');
                    s.textContent = "document.title = 'Dynamic'";
                    document.getElementById('host').appendChild(s);
                }, 1);"#,
            )
            .expect("queue timer setup");
        page.run_page_turns();
        page.created_at -= Duration::from_millis(50);
        let (changed, _) = page.run_page_turns();
        assert!(changed, "timer inserts a script element");
        assert!(
            !page.scripts_resolved,
            "DOM change reopens script discovery"
        );
    }

    app.start_classic_scripts(tab);
    assert_eq!(
        app.pages.get(&tab).unwrap().navigation.committed().title,
        "Dynamic"
    );
}

#[test]
fn broken_image_does_not_refetch_on_every_render_but_new_source_does() {
    let (mut app, tab) = headless_app_with("<img id=photo src=https://example.test/a.png>");
    app.start_images(tab);
    {
        let page = app.pages.get_mut(&tab).expect("page state");
        let first = page.pending_images.as_ref().expect("first image request");
        assert!(
            first.plan.resources[0]
                .request
                .url
                .as_str()
                .ends_with("/a.png")
        );
        page.cancel_images();
    }

    app.start_images(tab);
    assert!(
        app.pages.get(&tab).unwrap().pending_images.is_none(),
        "the same failed source should not be requested each frame"
    );

    {
        let page = app.pages.get_mut(&tab).expect("page state");
        let dom = page.page.document_mut().dom_mut();
        let image = find_id(dom, "photo");
        dom.set_attribute(image, "src", "https://example.test/b.png")
            .expect("new source");
    }
    app.start_images(tab);
    let page = app.pages.get(&tab).expect("page state");
    let second = page.pending_images.as_ref().expect("new image request");
    assert!(
        second.plan.resources[0]
            .request
            .url
            .as_str()
            .ends_with("/b.png")
    );
}

#[test]
fn image_source_changed_during_fetch_starts_replacement_immediately() {
    let (mut app, tab) = headless_app_with("<img id=photo src=https://example.test/old.png>");
    app.start_images(tab);
    let old_url = app.pages[&tab]
        .pending_images
        .as_ref()
        .expect("old request")
        .plan
        .resources[0]
        .request
        .url
        .clone();
    {
        let page = app.pages.get_mut(&tab).expect("page state");
        let dom = page.page.document_mut().dom_mut();
        let image = find_id(dom, "photo");
        dom.set_attribute(image, "src", "https://example.test/new.png")
            .expect("new source");
    }
    app.finish_images(
        tab,
        vec![Ok(render_net::FetchResponse {
            requested_url: old_url.clone(),
            final_url: old_url.clone(),
            redirect_chain: vec![old_url],
            redirects: Vec::new(),
            status: render_net::HttpStatus::from_u16(200),
            headers: Vec::new(),
            content_type: None,
            body: Vec::new(),
        })],
    );
    let replacement = app.pages[&tab]
        .pending_images
        .as_ref()
        .expect("replacement request");
    assert!(
        replacement.plan.resources[0]
            .request
            .url
            .as_str()
            .ends_with("/new.png")
    );
}

const RENDER_TEST_VIEWPORT: WindowSize<u32> = WindowSize::new(800, 560);

/// What one submitted render job carried, so a test can assert the pipeline did
/// not lose a stylesheet batch between submits.
#[derive(Clone, Debug)]
struct SubmittedRender {
    generation: u64,
    carried_style_batch: bool,
    /// Whether the job observed that its work was superseded before it
    /// returned.
    cancelled: bool,
}

type RenderLog = Arc<Mutex<Vec<SubmittedRender>>>;
type RenderGate = Arc<(Mutex<bool>, Condvar)>;

/// A render worker whose first job blocks until the test releases it. Render
/// ordering therefore no longer depends on timing, which is what makes the
/// supersession and coalescing contracts testable at all.
fn gated_render_worker() -> (
    crate::render_worker::PageRenderWorker,
    Arc<AtomicUsize>,
    RenderLog,
    RenderGate,
) {
    let started = Arc::new(AtomicUsize::new(0));
    let log: RenderLog = Arc::new(Mutex::new(Vec::new()));
    let gate: RenderGate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker = crate::render_worker::PageRenderWorker::start(
        RenderWorkerOptions {
            queue_capacity: 8,
            worker_count: 1,
        },
        {
            let started = Arc::clone(&started);
            let log = Arc::clone(&log);
            let gate = Arc::clone(&gate);
            move |job: RenderJob<PageRenderPayload>, cancellation: &RenderCancellation| {
                let carried_style_batch = match &job.payload {
                    PageRenderPayload::Full(full) => full.style_batch.is_some(),
                    PageRenderPayload::RetainedRaster { .. } => false,
                };
                // Only the first job is gated: the resubmission under test must
                // be free to run so the test can observe the outcome.
                if job.identity.generation == 1 {
                    started.fetch_add(1, Ordering::SeqCst);
                    let (lock, condvar) = &*gate;
                    let mut released = lock.lock().expect("gate lock");
                    while !*released {
                        released = condvar.wait(released).expect("gate wait is not poisoned");
                    }
                } else {
                    started.fetch_add(1, Ordering::SeqCst);
                }
                log.lock().expect("render log lock").push(SubmittedRender {
                    generation: job.identity.generation,
                    carried_style_batch,
                    cancelled: cancellation.is_cancelled(),
                });
                let width = job.identity.viewport.width;
                let height = job.identity.viewport.height;
                let height_css = height as f32;
                Ok(PageRenderFrame {
                    frame: vec![0x00ff_ffff; (width * height) as usize],
                    viewport: WindowSize::new(width, height),
                    display_list: None,
                    paint_scene: None,
                    raster_background: Color::rgb(0xff, 0xff, 0xff),
                    content_height: height_css,
                    viewport_height: height_css,
                    applied_style_sheets: None,
                    style_plan: None,
                    style_diagnostics: Vec::new(),
                    computed_styles: None,
                    geometry: None,
                    document_revision: job.identity.dom_revision,
                })
            }
        },
        || {},
    )
    .expect("render worker starts");
    (worker, started, log, gate)
}

fn release_first_render(gate: &RenderGate) {
    let (lock, condvar) = &**gate;
    let mut released = lock.lock().expect("gate lock");
    *released = true;
    condvar.notify_all();
}

fn wait_for_renders(started: &AtomicUsize, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while started.load(Ordering::SeqCst) < count {
        assert!(
            Instant::now() < deadline,
            "expected {count} render(s), saw {}",
            started.load(Ordering::SeqCst)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn submitted_renders(log: &RenderLog) -> Vec<SubmittedRender> {
    log.lock().expect("render log lock").clone()
}

/// Polls committed frames until `count` render jobs have returned.
fn commit_until_logged(app: &mut BrowserApp, log: &RenderLog, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll_render_worker();
        if submitted_renders(log).len() >= count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "only {} render(s) returned, want {count}",
            submitted_renders(log).len()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Polls committed frames until `count` jobs have run to completion.
///
/// A released job finishes on the render worker, so a single poll can observe
/// nothing; the loop is bounded and fails loudly instead of hanging.
fn commit_until_renders(app: &mut BrowserApp, started: &AtomicUsize, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll_render_worker();
        if started.load(Ordering::SeqCst) >= count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "a discarded frame never released the coalesced repaint: {} render(s), want {count}",
            started.load(Ordering::SeqCst)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A 200 `text/css` response for one stylesheet slot.
fn stylesheet_response(
    resource: &render_browser::resources::StylesheetFetch,
    body: &str,
) -> render_net::FetchResponse {
    render_net::FetchResponse {
        requested_url: resource.key.requested_url.clone(),
        final_url: resource.key.requested_url.clone(),
        redirect_chain: vec![resource.key.requested_url.clone()],
        redirects: Vec::new(),
        status: render_net::HttpStatus::from_u16(200),
        headers: Vec::new(),
        content_type: Some(render_net::ContentType {
            media_type: "text/css".to_owned(),
            charset: Some("utf-8".to_owned()),
        }),
        body: body.as_bytes().to_vec(),
    }
}

/// Attaches a fetched stylesheet batch the way the network coordinator does, so
/// the next render carries it.
fn attach_stylesheet_batch(app: &mut BrowserApp, tab: TabId, css: &str) {
    let page = app.pages.get_mut(&tab).expect("page state");
    let base = page.navigation.committed().target.history_url();
    let plan = plan_external_style_sheets(
        page.page.document(),
        &base,
        render_core::document::DocumentLimits::default(),
    );
    let results = plan
        .resources
        .iter()
        .map(|resource| Ok(stylesheet_response(resource, css)))
        .collect::<Vec<render_net::FetchResult>>();
    page.style_batch = Some((plan, results));
    page.external_styles_generation = page.external_styles_generation.saturating_add(1);
}

fn second_document_source() -> PageSource {
    PageSource {
        html: "<main id=host>second</main>".to_owned(),
        title: "second".to_owned(),
        target: NavigationTarget::Url(Url::parse("https://example.test/second.html").expect("URL")),
    }
}

/// A navigation that commits while a render is running hands the tab to the
/// new document. The running frame is then discarded, and the repaint request
/// coalesced behind it must still be honoured: consuming that request only
/// after a successful commit left the tab frozen on the document it replaced.
#[test]
fn superseded_render_still_repaint_the_coalesced_request() {
    let (worker, started, log, gate) = gated_render_worker();
    let fonts = Arc::new(SystemFontBackend::load().expect("system fonts load"));
    let (mut app, tab) =
        headless_app_with_render_worker("<main id=host>first</main>", worker, Arc::clone(&fonts));

    app.schedule_page_render(tab, RENDER_TEST_VIEWPORT, false);
    wait_for_renders(&started, 1);

    app.pages
        .get_mut(&tab)
        .expect("page state")
        .set_source(second_document_source());
    app.schedule_page_render(tab, RENDER_TEST_VIEWPORT, false);
    {
        let page = app.pages.get(&tab).expect("page state");
        assert!(page.render_dirty);
        assert_eq!(page.render_dirty_viewport, Some(RENDER_TEST_VIEWPORT));
    }

    release_first_render(&gate);
    commit_until_renders(&mut app, &started, 2);

    assert_eq!(started.load(Ordering::SeqCst), 2);
    let page = app.pages.get(&tab).expect("page state");
    assert!(!page.render_dirty);
    assert_eq!(page.render_dirty_viewport, None);
    let renders = submitted_renders(&log);
    assert_eq!(renders.len(), 2);
    assert_eq!(renders[1].generation, 2);
}

/// The stylesheet batch a superseded render was carrying must survive the
/// discard. When it did not, a page whose stylesheet landed during a render
/// converged on the unstyled document it started with and never repainted.
#[test]
fn superseded_render_keeps_the_pending_stylesheet_batch() {
    let (worker, started, log, gate) = gated_render_worker();
    let fonts = Arc::new(SystemFontBackend::load().expect("system fonts load"));
    let (mut app, tab) = headless_app_with_render_worker(
        "<link rel=stylesheet href=a.css>",
        worker,
        Arc::clone(&fonts),
    );

    app.schedule_page_render(tab, RENDER_TEST_VIEWPORT, false);
    wait_for_renders(&started, 1);

    {
        let page = app.pages.get_mut(&tab).expect("page state");
        page.set_source(PageSource {
            html: "<link rel=stylesheet href=a.css>".to_owned(),
            title: "styled".to_owned(),
            target: NavigationTarget::Url(
                Url::parse("https://example.test/styled.html").expect("styled URL"),
            ),
        });
    }
    attach_stylesheet_batch(&mut app, tab, "h1 { color: red }");
    app.schedule_page_render(tab, RENDER_TEST_VIEWPORT, false);

    release_first_render(&gate);
    commit_until_renders(&mut app, &started, 2);

    assert_eq!(started.load(Ordering::SeqCst), 2);
    let renders = submitted_renders(&log);
    assert!(
        !renders[0].carried_style_batch,
        "the first job ran before the batch arrived"
    );
    assert!(
        renders[1].carried_style_batch,
        "the repaint after the discarded frame lost the stylesheet batch"
    );
    assert!(app.pages[&tab].style_batch.is_some());
}

/// A committed navigation must supersede the frames of the document it
/// replaces rather than leave them registered with the render worker, and the
/// repaint the new document needs must still happen even though the cancelled
/// render never reports a completion.
#[test]
fn navigation_while_rendering_supersedes_the_old_frame_and_still_repaints() {
    let (worker, started, log, gate) = gated_render_worker();
    let fonts = Arc::new(SystemFontBackend::load().expect("system fonts load"));
    let (mut app, tab) =
        headless_app_with_render_worker("<main id=host>first</main>", worker, Arc::clone(&fonts));

    app.schedule_page_render(tab, RENDER_TEST_VIEWPORT, false);
    wait_for_renders(&started, 1);

    app.install_source(tab, second_document_source(), true);
    // The new document's repaint is coalesced behind the render the navigation
    // just cancelled.
    app.schedule_page_render(tab, RENDER_TEST_VIEWPORT, false);
    {
        let page = app.pages.get(&tab).expect("page state");
        assert!(page.render_dirty);
        assert_eq!(page.render_dirty_viewport, Some(RENDER_TEST_VIEWPORT));
    }

    release_first_render(&gate);
    commit_until_logged(&mut app, &log, 1);
    assert!(
        app.pages[&tab].expected_render.is_none(),
        "the superseded frame must not be installable on the new document"
    );

    // A cancelled render reports nothing, so only the coordinator's recovery can
    // release the repaint it left behind. The event loop repeats this on every
    // tick until the worker frees the tab.
    let deadline = Instant::now() + Duration::from_secs(10);
    while started.load(Ordering::SeqCst) < 2 {
        app.poll_render_worker();
        app.recover_unresolved_render_requests();
        assert!(
            Instant::now() < deadline,
            "the repaint left behind by the cancelled render never ran"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(started.load(Ordering::SeqCst), 2);
    let renders = submitted_renders(&log);
    assert_eq!(renders.len(), 2);
    assert!(
        renders[0].cancelled,
        "the navigation did not cancel the old frame"
    );
    assert!(!renders[1].cancelled);
    let page = app.pages.get(&tab).expect("page state");
    assert!(!page.render_dirty);
    assert_eq!(page.render_dirty_viewport, None);
}

/// Every completed image batch marks the page dirty, including one that decoded
/// nothing: skipping those left a page whose images all fail permanently frozen
/// on its last commit.
#[test]
fn completed_image_batch_marks_the_page_dirty_even_when_nothing_decoded() {
    let (mut app, tab) = headless_app_with("<img id=photo src=https://example.test/a.png>");
    app.start_images(tab);
    let url = {
        let page = app.pages.get(&tab).expect("page state");
        page.pending_images
            .as_ref()
            .expect("image request started")
            .plan
            .resources[0]
            .request
            .url
            .clone()
    };

    app.finish_images(
        tab,
        vec![Ok(render_net::FetchResponse {
            requested_url: url.clone(),
            final_url: url.clone(),
            redirect_chain: vec![url],
            redirects: Vec::new(),
            status: render_net::HttpStatus::from_u16(200),
            headers: Vec::new(),
            content_type: Some(render_net::ContentType {
                media_type: "image/png".to_owned(),
                charset: None,
            }),
            // Not a decodable image: the batch completes with nothing loaded.
            body: b"not a png".to_vec(),
        })],
    );

    let page = app.pages.get(&tab).expect("page state");
    assert!(
        page.expected_render.is_some(),
        "a completed image batch must request a repaint even when it decoded nothing"
    );
    assert!(page.pending_images.is_none());
}

/// A stylesheet blocks script *execution*, not discovery, parsing, or
/// fetching: an inline body is prepared while the sheet is still in flight and
/// only its execution waits for the cascade.
#[test]
fn inline_script_body_is_prepared_before_stylesheets_and_runs_after() {
    let html = "<!doctype html><html><head><title>Static</title>\
                <link rel=\"stylesheet\" href=\"sheet.css\">\
                </head><body><script>document.title = 'Ran';</script></body></html>";
    let (mut app, tab) = headless_app_with(html);

    app.start_classic_scripts(tab);
    {
        let page = app.pages.get_mut(&tab).expect("page state");
        assert!(!page.styles_resolved, "the stylesheet is still pending");
        assert!(
            page.held_scripts.is_some(),
            "the inline body must be prepared while the stylesheet loads"
        );
        assert!(!page.sync_committed_title());
    }
    {
        let page = app.pages.get_mut(&tab).expect("page state");
        page.cancel_style_sheets();
        page.styles_resolved = true;
    }
    app.start_classic_scripts(tab);
    assert!(app.pages[&tab].held_scripts.is_none());
    assert_eq!(app.pages[&tab].navigation.committed().title, "Ran");
}

#[test]
fn dynamic_stylesheet_merge_keeps_previous_css_and_drops_retargeted_links() {
    let base = Url::parse("https://example.test/page.html").expect("base URL");
    let mut document =
        Document::parse("<link rel=stylesheet href=a.css><link rel=stylesheet href=old.css>");
    let original = plan_external_style_sheets(
        &document,
        &base,
        render_core::document::DocumentLimits::default(),
    );
    let mut existing = ExternalStyleSheets::default();
    existing.insert_css(original.resources[0].key.clone(), "h1 { color: red }");
    existing.insert_css(original.resources[1].key.clone(), "h1 { color: blue }");

    document
        .dom_mut()
        .set_attribute(original.resources[1].key.owner, "href", "b.css")
        .expect("retarget link");
    let current = plan_external_style_sheets(
        &document,
        &base,
        render_core::document::DocumentLimits::default(),
    );
    let mut incoming = ExternalStyleSheets::default();
    incoming.insert_css(current.resources[1].key.clone(), "h1 { color: green }");
    let merged = merge_current_style_sheets(&document, &base, &existing, &incoming);

    assert_eq!(merged.len(), 2);
    assert!(merged.get(&original.resources[0].key).is_some());
    assert!(merged.get(&original.resources[1].key).is_none());
    assert!(merged.get(&current.resources[1].key).is_some());
}

/// A 200 `text/javascript` response for the plan's single external script.
fn script_response(
    plan: &render_browser::scripts::ScriptFetchPlan,
    body: &str,
) -> render_net::FetchResponse {
    let url = plan.resources[0].request.url.clone();
    render_net::FetchResponse {
        requested_url: url.clone(),
        final_url: url.clone(),
        redirect_chain: vec![url],
        redirects: Vec::new(),
        status: render_net::HttpStatus::from_u16(200),
        headers: Vec::new(),
        content_type: Some(render_net::ContentType {
            media_type: "text/javascript".to_owned(),
            charset: Some("utf-8".to_owned()),
        }),
        body: body.as_bytes().to_vec(),
    }
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

/// Baidu-like page whose visible search box is a script-driven replacement:
/// a wrapper with a textarea and a submit button outside any form, plus the
/// classic (unrendered) form as the page's hidden submission channel.
const CHAT_PAGE_HTML: &str = "<!doctype html><html><body>\
     <div id='chat'>\
       <textarea id='chat-textarea' rows='1'></textarea>\
       <button id='chat-submit'>search</button>\
     </div>\
     <form id='form' action='/s'>\
       <input type='hidden' name='ie' value='utf-8'>\
       <input id='kw' name='wd' value=''>\
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
/// `html` at a `file:` document URL so any submit navigation stays offline.
pub(super) fn headless_app_with(html: &str) -> (BrowserApp, TabId) {
    let fonts = Arc::new(SystemFontBackend::load().expect("system fonts load"));
    let render_worker = crate::render_worker::PageRenderWorker::start(
        RenderWorkerOptions::default(),
        |_job: RenderJob<PageRenderPayload>,
         _cancellation: &RenderCancellation|
         -> Result<PageRenderFrame, RenderFailure> { Err(RenderFailure::Cancelled) },
        || {},
    )
    .expect("render worker starts");
    headless_app_with_render_worker(html, render_worker, fonts)
}

/// Builds a headless `BrowserApp` driving `render_worker`, so a test can
/// observe the real submit/commit ordering without a window.
fn headless_app_with_render_worker(
    html: &str,
    render_worker: crate::render_worker::PageRenderWorker,
    fonts: Arc<SystemFontBackend>,
) -> (BrowserApp, TabId) {
    let url = Url::parse("file:///rENDER-test-fixtures/page.html").expect("test document URL");
    let source = PageSource {
        html: html.to_owned(),
        title: "search".to_owned(),
        target: NavigationTarget::Url(url),
    };
    let tabs = TabModel::new(source.title.clone(), source.target.display_address());
    let active = tabs.active_id();
    let page = PageState::new(source);

    let network = NetworkWorker::start(HttpTransport::new(FetchConfig::default()))
        .expect("network worker starts");

    let mut app = BrowserApp {
        tabs,
        pages: HashMap::new(),
        fonts,
        render_worker,
        network,
        http_cache: render_browser::cache::HttpCache::default(),
        cookies: crate::profile::ProfileCookies::open(None),
        storage: crate::profile::ProfileStorage::open(None),
        pending_fetches: Vec::new(),
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
        tab_drag_paint: None,
        scrollbar_drag: None,
        scrollbar_hot: false,
        address_selecting: false,
        content_selecting: false,
        address_menu: None,
        modifiers: winit::keyboard::ModifiersState::default(),
        title_bar_clicks: render_browser::chrome::TitleBarClickTracker::default(),
        address_clicks: render_browser::chrome::AddressClickTracker::default(),
        left_pointer_down: false,
        started_at: Instant::now(),
    };
    app.layout = Some(ChromeLayout::new(800, 600, 1.0, app.tabs.tabs()));
    app.pages.insert(active, page);
    (app, active)
}

fn headless_search_app() -> (BrowserApp, TabId, NodeId) {
    let (mut app, active) = headless_app_with(SEARCH_PAGE_HTML);
    let kw = {
        let page = app.pages.get(&active).expect("page");
        let dom = page.page.document().dom();
        find_id(dom, "kw")
    };
    let page = app.pages.get_mut(&active).expect("page");
    page.geometry.insert(
        kw.as_u64(),
        ElementRect {
            x: 200.0,
            y: 150.0,
            width: 400.0,
            height: 34.0,
        },
    );
    (app, active, kw)
}

/// Chat-shaped page with the wrapper, textarea, and submit button rendered;
/// the classic form deliberately has no geometry (it is unrendered).
fn headless_chat_app() -> (BrowserApp, TabId, NodeId, NodeId) {
    headless_chat_app_with_geometry(false)
}

/// `form_rendered` controls whether the classic form gets a geometry entry.
fn headless_chat_app_with_geometry(form_rendered: bool) -> (BrowserApp, TabId, NodeId, NodeId) {
    let (mut app, active) = headless_app_with(CHAT_PAGE_HTML);
    let (chat, textarea, button, form) = {
        let page = app.pages.get(&active).expect("page");
        let dom = page.page.document().dom();
        (
            find_id(dom, "chat"),
            find_id(dom, "chat-textarea"),
            find_id(dom, "chat-submit"),
            find_id(dom, "form"),
        )
    };
    let page = app.pages.get_mut(&active).expect("page");
    let geometry = &mut page.geometry;
    let mut insert = |node: NodeId, x: f32, y: f32, width: f32, height: f32| {
        geometry.insert(
            node.as_u64(),
            ElementRect {
                x,
                y,
                width,
                height,
            },
        );
    };
    insert(chat, 0.0, 0.0, 800.0, 100.0);
    insert(textarea, 10.0, 10.0, 700.0, 40.0);
    insert(button, 740.0, 30.0, 50.0, 40.0);
    if form_rendered {
        insert(form, 0.0, 200.0, 800.0, 50.0);
    }
    (app, active, textarea, button)
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

fn click_content_at(app: &mut BrowserApp, x: f32, document_y: f32) {
    let chrome_height = app.layout.as_ref().expect("chrome layout").chrome_height;
    app.cursor = Point {
        x,
        y: chrome_height as f32 + document_y,
    };
    app.handle_content_press();
}

fn click_search_box(app: &mut BrowserApp) {
    click_content_at(app, 400.0, 167.0);
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
fn focused_page_input_exposes_a_caret_that_tracks_the_editor_cursor() {
    let (mut app, _tab, _kw) = headless_search_app();
    click_search_box(&mut app);

    let chrome_height = app.layout.as_ref().expect("chrome layout").chrome_height;
    let empty = app
        .content_caret_geometry(chrome_height)
        .expect("focused input has a caret");
    assert!((empty.clip.x - 200.0).abs() < f32::EPSILON);
    assert!((empty.clip.y - (chrome_height as f32 + 150.0)).abs() < f32::EPSILON);
    let empty_x = empty.rect.x;

    app.handle_keyboard(&pressed_character("a"));
    let typed = app
        .content_caret_geometry(chrome_height)
        .expect("caret remains visible after typing");
    assert!(
        typed.rect.x > empty_x,
        "typing must advance the visible caret"
    );

    app.handle_keyboard(&pressed_named(NamedKey::ArrowLeft));
    let moved = app
        .content_caret_geometry(chrome_height)
        .expect("caret remains visible after moving");
    assert!((moved.rect.x - empty_x).abs() < f32::EPSILON);

    app.close_content_editor();
    assert!(
        app.content_caret_geometry(chrome_height).is_none(),
        "blurring the control must remove its caret"
    );
}

#[test]
fn named_space_key_is_inserted_into_a_focused_page_input() {
    let (mut app, tab, kw) = headless_search_app();
    click_search_box(&mut app);
    app.handle_keyboard(&pressed_character("hello"));
    app.handle_keyboard(&pressed_named(NamedKey::Space));
    app.handle_keyboard(&pressed_character("world"));
    assert_eq!(committed_value(&app, tab, kw), "hello world");
}

#[test]
fn dragging_in_a_page_input_creates_a_selection_that_replaces_on_type() {
    let (mut app, tab, kw) = headless_search_app();
    content_interaction::set_content_text_value(
        app.pages
            .get_mut(&tab)
            .expect("page")
            .page
            .document_mut()
            .dom_mut(),
        kw,
        "hello world",
    )
    .expect("set input value");

    // The first click starts a pointer gesture.  Move while the button is
    // held, then release; this follows the same path as native Winit events.
    click_content_at(&mut app, 205.0, 167.0);
    app.left_pointer_down = true;
    let chrome_height = app.layout.as_ref().expect("chrome layout").chrome_height;
    app.handle_cursor_move(PhysicalPosition::new(
        235.0,
        f64::from(chrome_height) + 167.0,
    ));
    app.handle_pointer_release();
    let selected = app
        .content_editor
        .as_ref()
        .and_then(|content| content.editor.selected_text())
        .expect("dragging through a focused input must leave a non-empty selection")
        .to_owned();
    assert_eq!(selected, "hello");

    app.handle_keyboard(&pressed_character("X"));
    assert_eq!(committed_value(&app, tab, kw), "X world");
}

#[test]
fn clicking_inside_existing_page_input_text_places_the_caret_near_the_click() {
    let (mut app, tab, kw) = headless_search_app();
    content_interaction::set_content_text_value(
        app.pages
            .get_mut(&tab)
            .expect("page")
            .page
            .document_mut()
            .dom_mut(),
        kw,
        "abcd",
    )
    .expect("set input value");

    // The fallback text origin is x=205 in this headless fixture. A click a
    // few pixels into the first glyph should place the caret before the end.
    click_content_at(&mut app, 208.0, 167.0);
    let content = app.content_editor.as_ref().expect("input gains focus");
    assert!(
        content.editor.cursor() < content.editor.text().len(),
        "clicking within existing text must not always move the caret to the end"
    );
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

#[test]
fn enter_in_a_formless_control_submits_through_the_page_hidden_form() {
    let (mut app, tab, textarea, _button) = headless_chat_app();
    click_content_at(&mut app, 100.0, 30.0);
    let content = app.content_editor.as_ref().expect("box gains focus");
    assert_eq!(content.tab, tab);
    assert_eq!(content.node, textarea);

    app.handle_keyboard(&pressed_character("a"));
    app.handle_content_ime_commit("旅行");
    assert_eq!(committed_value(&app, tab, textarea), "a旅行");

    let history_before = app.pages.get(&tab).expect("page").history.len();
    // The Enter confirming a composition is latched away from submission.
    app.handle_keyboard(&pressed_named(NamedKey::Enter));
    assert!(
        app.content_editor.is_some(),
        "composition Enter stays inert"
    );
    assert_eq!(
        app.pages.get(&tab).expect("page").history.len(),
        history_before
    );

    // A second Enter submits through the page's hidden form.
    app.handle_keyboard(&pressed_named(NamedKey::Enter));
    let page = app.pages.get(&tab).expect("page stays open");
    assert!(
        page.history.len() > history_before,
        "Enter must submit through the page's hidden form"
    );
    assert_eq!(
        page.history.current().url.as_str(),
        "file:///s?ie=utf-8&wd=a%E6%97%85%E8%A1%8C",
        "the typed text must ride along as the hidden form's query field"
    );
    assert!(app.content_editor.is_none());
}

#[test]
fn formless_submit_button_click_carries_the_typed_text() {
    let (mut app, tab, _textarea, _button) = headless_chat_app();
    click_content_at(&mut app, 100.0, 30.0);
    let focused = app.content_editor.as_ref().expect("box gains focus").node;
    app.handle_content_ime_commit("搜索");
    assert_eq!(committed_value(&app, tab, focused), "搜索");

    // Click the submit button inside the same wrapper.
    click_content_at(&mut app, 765.0, 50.0);
    let page = app.pages.get(&tab).expect("page stays open");
    assert_eq!(
        page.history.current().url.as_str(),
        "file:///s?ie=utf-8&wd=%E6%90%9C%E7%B4%A2",
        "the wrapper's typed text must ride along when the button submits"
    );
}

#[test]
fn formless_submit_without_a_unique_hidden_form_stays_inert() {
    let ambiguous = "<!doctype html><html><body>\
         <div id='chat'><textarea id='chat-textarea' rows='1'></textarea>\
         <button id='chat-submit'>search</button></div>\
         <form id='one' action='/a'><input id='a' name='q' value=''></form>\
         <form id='two' action='/b'><input id='b' name='q' value=''></form>\
         </body></html>";
    let (mut app, tab, textarea, _button) = {
        let (mut app, active) = headless_app_with(ambiguous);
        let (chat, textarea, button) = {
            let page = app.pages.get(&active).expect("page");
            let dom = page.page.document().dom();
            (
                find_id(dom, "chat"),
                find_id(dom, "chat-textarea"),
                find_id(dom, "chat-submit"),
            )
        };
        let page = app.pages.get_mut(&active).expect("page");
        let mut insert = |node: NodeId, x: f32, y: f32, width: f32, height: f32| {
            page.geometry.insert(
                node.as_u64(),
                ElementRect {
                    x,
                    y,
                    width,
                    height,
                },
            );
        };
        insert(chat, 0.0, 0.0, 800.0, 100.0);
        insert(textarea, 10.0, 10.0, 700.0, 40.0);
        insert(button, 740.0, 30.0, 50.0, 40.0);
        (app, active, textarea, button)
    };
    click_content_at(&mut app, 100.0, 30.0);
    assert!(app.content_editor.is_some());
    app.handle_content_ime_commit("词");
    let history_before = app.pages.get(&tab).expect("page").history.len();
    app.handle_keyboard(&pressed_named(NamedKey::Enter));
    let page = app.pages.get(&tab).expect("page stays open");
    assert_eq!(
        page.history.len(),
        history_before,
        "two candidate forms make the fallback ambiguous, so Enter stays inert"
    );
    let _ = textarea;
}

#[test]
fn formless_submit_does_not_hijack_a_visible_form() {
    // The only form on the page is rendered, so it is not a hidden
    // submission channel and must not be submitted by a form-less control.
    let (mut app, tab, _textarea, _button) = headless_chat_app_with_geometry(true);
    click_content_at(&mut app, 100.0, 30.0);
    app.handle_content_ime_commit("词");
    let history_before = app.pages.get(&tab).expect("page").history.len();
    app.handle_keyboard(&pressed_named(NamedKey::Enter));
    let page = app.pages.get(&tab).expect("page stays open");
    assert_eq!(page.history.len(), history_before);
}

#[test]
fn formless_fallback_resolves_form_syncs_value_and_routes_the_button() {
    let mut document = parse_document(CHAT_PAGE_HTML);
    let dom = &mut document.dom;
    let chat = find_id(dom, "chat");
    let textarea = find_id(dom, "chat-textarea");
    let button = find_id(dom, "chat-submit");
    let form = find_id(dom, "form");
    let kw = find_id(dom, "kw");
    let mut geometry = BTreeMap::new();
    let mut insert = |node: NodeId, x: f32, y: f32, width: f32, height: f32| {
        geometry.insert(
            node.as_u64(),
            ElementRect {
                x,
                y,
                width,
                height,
            },
        );
    };
    insert(chat, 0.0, 0.0, 800.0, 100.0);
    insert(textarea, 10.0, 10.0, 700.0, 40.0);
    insert(button, 740.0, 30.0, 50.0, 40.0);
    let rendered = |node: NodeId| geometry.contains_key(&node.as_u64());

    assert_eq!(
        fallback_submit_form(dom, button, &rendered),
        Some(form),
        "the unique unrendered form is the fallback channel"
    );
    assert_eq!(
        fallback_submit_form(dom, button, &|node: NodeId| {
            let _ = node;
            true
        }),
        None,
        "a rendered form is not a hidden channel"
    );

    assert!(dom.append_text(textarea, "中文").is_ok());
    assert!(sync_submit_control_value(dom, textarea, form));
    assert_eq!(
        dom.attribute(kw, "value").ok().flatten(),
        Some("中文"),
        "the typed text must land in the hidden form's query field"
    );

    let base = Url::parse("file:///rENDER-test-fixtures/page.html").expect("base");
    let target = get_content_navigation_target(dom, button, &base, &rendered)
        .expect("button resolves through the fallback form");
    assert_eq!(target.as_str(), "file:///s?ie=utf-8&wd=%E4%B8%AD%E6%96%87");

    assert_eq!(
        ancestor_wrapper_control(dom, &geometry, button),
        Some(textarea),
        "the button's nearest wrapper routes to the dominant text control"
    );
}

/// One HTTP/1.1 request as a test origin received it.
struct ReceivedRequest {
    request_line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl ReceivedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Reads one HTTP/1.1 request from `stream`: its request line, every header,
/// and the body of its `Content-Length`.
fn read_http_request(stream: &mut std::net::TcpStream) -> ReceivedRequest {
    use std::io::Read;

    let mut buffer = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).expect("read request");
        assert!(read > 0, "the client closed before sending its headers");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8(buffer[..header_end].to_vec()).expect("ASCII headers");
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect();
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, value)| {
            value.parse::<usize>().expect("numeric length")
        });
    let mut body = buffer[header_end..].to_vec();
    while body.len() < length {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).expect("read body");
        assert!(read > 0, "the client closed before sending its body");
        body.extend_from_slice(&chunk[..read]);
    }
    ReceivedRequest {
        request_line,
        headers,
        body,
    }
}

impl ReceivedRequest {
    fn method(&self) -> &str {
        self.request_line.split(' ').next().unwrap_or_default()
    }
}

/// Serves connections until the requests stop: once one has arrived, the server
/// returns after `idle` with none. Before the first arrival it waits for up to
/// ten seconds, so the test may build its shell after starting the server. It
/// answers each request with `respond` and returns the requests in arrival order.
fn serve_until_idle(
    listener: &std::net::TcpListener,
    idle: std::time::Duration,
    respond: impl Fn(&ReceivedRequest) -> String,
) -> Vec<ReceivedRequest> {
    use std::io::Write;

    listener
        .set_nonblocking(true)
        .expect("non-blocking listener");
    let started = Instant::now();
    let mut served = Vec::new();
    let mut last_arrival: Option<Instant> = None;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).expect("blocking stream");
                let received = read_http_request(&mut stream);
                stream
                    .write_all(respond(&received).as_bytes())
                    .expect("send response");
                served.push(received);
                last_arrival = Some(Instant::now());
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let finished = match last_arrival {
                    Some(arrival) => arrival.elapsed() > idle,
                    None => started.elapsed() > Duration::from_secs(10),
                };
                if finished {
                    return served;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return served,
        }
    }
}

/// Runs the shell until no page transfer is in flight, or fails at the deadline.
fn drain_page_fetches(app: &mut BrowserApp) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !app.pending_fetches.is_empty() {
        app.poll_network();
        assert!(Instant::now() < deadline, "a page transfer never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_same_origin_fetch_stores_the_cookies_its_response_sets() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind page origin");
    let port = listener.local_addr().expect("local address").port();
    let page_origin = std::thread::spawn(move || {
        serve_until_idle(&listener, Duration::from_millis(800), |_| {
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Set-Cookie: session=xyz; Path=/; Max-Age=600\r\n\
             Content-Length: 2\r\nConnection: close\r\n\r\n{}"
                .to_owned()
        })
    });

    // The page itself is served from the same origin as the request it makes.
    let (mut app, tab) = headless_app_with("<!doctype html><p>page</p>");
    app.pages.insert(
        tab,
        PageState::new(PageSource {
            html: "<!doctype html><p>page</p>".into(),
            title: "page".into(),
            target: NavigationTarget::Url(
                Url::parse(&format!("http://127.0.0.1:{port}/")).expect("page URL"),
            ),
        }),
    );
    app.submit_page_fetch(
        tab,
        &render_core::js::PendingFetch {
            id: 4,
            url: format!("http://127.0.0.1:{port}/api/login"),
            method: "GET".to_owned(),
            headers: Vec::new(),
            body: None,
        },
    );
    drain_page_fetches(&mut app);
    let _ = page_origin.join().expect("page origin thread");

    let later =
        FetchRequest::get(Url::parse(&format!("http://127.0.0.1:{port}/account")).expect("URL"));
    assert_eq!(
        app.cookies.decorate_request(later).cookie.as_deref(),
        Some("session=xyz"),
        "the cookie a same-origin fetch response sets is kept for later requests"
    );
}

#[test]
fn a_cross_origin_put_is_sent_only_after_its_preflight_allows_it() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind other origin");
    let port = listener.local_addr().expect("local address").port();
    let other_origin = std::thread::spawn(move || {
        serve_until_idle(&listener, Duration::from_millis(800), |request| {
            if request.method() == "OPTIONS" {
                "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: null\r\n\
                 Access-Control-Allow-Methods: PUT\r\nAccess-Control-Allow-Headers: x-token\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned()
            } else {
                "HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: null\r\n\
                 Content-Length: 2\r\nConnection: close\r\n\r\nok"
                    .to_owned()
            }
        })
    });

    let (mut app, tab) = headless_app_with("<!doctype html><p>page</p>");
    app.submit_page_fetch(
        tab,
        &render_core::js::PendingFetch {
            id: 2,
            url: format!("http://127.0.0.1:{port}/items/1"),
            method: "PUT".to_owned(),
            headers: vec![("X-Token".to_owned(), "t".to_owned())],
            body: Some("{}".to_owned()),
        },
    );
    drain_page_fetches(&mut app);
    let served = other_origin.join().expect("other origin thread");

    let methods: Vec<&str> = served.iter().map(ReceivedRequest::method).collect();
    assert_eq!(methods, ["OPTIONS", "PUT"], "the PUT follows its preflight");
    assert_eq!(served[0].header("origin"), Some("null"));
    assert_eq!(
        served[0].header("access-control-request-method"),
        Some("PUT")
    );
    assert_eq!(
        served[0].header("access-control-request-headers"),
        Some("x-token")
    );
    assert_eq!(served[1].header("origin"), Some("null"));
    assert_eq!(served[1].header("x-token"), Some("t"));
    assert_eq!(served[1].body, b"{}".to_vec());
}

#[test]
fn a_refused_preflight_keeps_the_request_from_being_sent() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind other origin");
    let port = listener.local_addr().expect("local address").port();
    let other_origin = std::thread::spawn(move || {
        serve_until_idle(&listener, Duration::from_millis(800), |_| {
            // No Access-Control-Allow-Origin: the preflight is refused.
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
        })
    });

    let (mut app, tab) = headless_app_with("<!doctype html><p>page</p>");
    app.submit_page_fetch(
        tab,
        &render_core::js::PendingFetch {
            id: 3,
            url: format!("http://127.0.0.1:{port}/items/1"),
            method: "DELETE".to_owned(),
            headers: Vec::new(),
            body: None,
        },
    );
    drain_page_fetches(&mut app);
    let served = other_origin.join().expect("other origin thread");

    let methods: Vec<&str> = served.iter().map(ReceivedRequest::method).collect();
    assert_eq!(methods, ["OPTIONS"], "the DELETE is never sent");
}

#[test]
fn a_cross_origin_page_fetch_carries_no_cookies() {
    use std::io::Write;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind other origin");
    let port = listener.local_addr().expect("local address").port();
    let other_origin = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the page's fetch");
        let received = read_http_request(&mut stream);
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 6\r\n\
                        Connection: close\r\n\r\nsecret";
        stream
            .write_all(response.as_bytes())
            .expect("send response");
        received
    });

    // A cookie the browser already holds for that host, from the profile store.
    let directory =
        std::env::temp_dir().join(format!("render-cors-cookies-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("scratch directory");
    std::fs::write(
        directory.join("cookies.txt"),
        "# rENDER cookie store v1\nsession\tabc\t127.0.0.1\t/\t1\t0\t0\t-\t4102444800\n",
    )
    .expect("cookie store");
    let (mut app, tab) = headless_app_with("<!doctype html><p>page</p>");
    app.cookies = crate::profile::ProfileCookies::open(Some(directory.clone()));

    app.submit_page_fetch(
        tab,
        &render_core::js::PendingFetch {
            id: 1,
            url: format!("http://127.0.0.1:{port}/data"),
            method: "GET".to_owned(),
            headers: Vec::new(),
            body: None,
        },
    );
    let received = other_origin.join().expect("other origin thread");
    let _ = std::fs::remove_dir_all(&directory);

    assert_eq!(
        received.header("cookie"),
        None,
        "a cross-origin fetch must not send the browser's cookies"
    );
}

#[test]
fn a_post_form_sends_its_urlencoded_body_and_commits_the_response() {
    use std::io::Write;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local origin");
    let port = listener.local_addr().expect("local address").port();
    let origin = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the form submission");
        let received = read_http_request(&mut stream);
        let echoed = String::from_utf8_lossy(&received.body).into_owned();
        let page = format!("<!doctype html><title>signed in</title><p id=echo>{echoed}</p>");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("send response");
        received
    });

    let action = format!("http://127.0.0.1:{port}/login?next=/home");
    let (mut app, tab) = headless_app_with(&format!(
        "<!doctype html><form action='{action}' method=post>\
         <input name=user value='a b&c'><button id=submit>Sign in</button></form>"
    ));
    let submit = {
        let dom = app.pages.get(&tab).expect("page").page.document().dom();
        find_id(dom, "submit")
    };
    let document_url = app
        .pages
        .get(&tab)
        .expect("page")
        .navigation
        .committed()
        .target
        .history_url();
    let navigation = {
        let page = app.pages.get(&tab).expect("page");
        crate::content_interaction::content_navigation(
            page.page.document().dom(),
            submit,
            &document_url,
            &|_| true,
        )
    };
    let Some(crate::content_interaction::ContentNavigation::Post { url, body }) = navigation else {
        panic!("a POST form must yield a POST navigation");
    };
    app.submit_form_navigation(tab, url, body);

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.poll_network();
        let committed = app
            .pages
            .get(&tab)
            .expect("page")
            .navigation
            .committed()
            .html
            .clone();
        if committed.contains("id=echo") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the POST response never committed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let received = origin.join().expect("origin thread");
    assert_eq!(received.request_line, "POST /login?next=/home HTTP/1.1");
    assert_eq!(
        received.header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(received.body, b"user=a+b%26c".to_vec());
    let committed = app
        .pages
        .get(&tab)
        .expect("page")
        .navigation
        .committed()
        .html
        .clone();
    assert!(
        committed.contains("user=a+b%26c"),
        "the committed document is the server's response to the POST"
    );
}
