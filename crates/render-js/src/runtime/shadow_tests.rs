//! The shadow DOM a script sees: `attachShadow`, `shadowRoot`, `ShadowRoot`,
//! `getRootNode`, `isConnected`, retargeted event targets, `composedPath`, and
//! slot assignment.

use crate::JsRuntime;
use render_html::parse_document;

/// Run `source` on a page with a `<div id=host>` holding a `<span id=light>`,
/// and an `<input id=plain>` beside it. The script reports through `log`.
fn run(source: &str) -> String {
    let mut dom = parse_document(
        "<!doctype html><html><body><div id=host><span id=light>L</span></div>\
         <input id=plain></body></html>",
    )
    .dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var log = []; var host = document.getElementById('host'); \
             var light = document.getElementById('light'); \
             var plain = document.getElementById('plain');",
        )
        .expect("setup");
    runtime
        .execute(&mut dom, source)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"));
    assert!(runtime.prelude_error().is_none());
    runtime
        .execute(&mut dom, "log.join(',')")
        .expect("log")
        .value
        .to_js_string()
        .to_lossy()
}

#[test]
fn attach_shadow_returns_a_fragment_that_reports_its_options() {
    assert_eq!(
        run(
            "var root = host.attachShadow({ mode: 'open', delegatesFocus: true, \
             slotAssignment: 'manual' }); \
             log.push(root.nodeType, root.mode, root.host === host, root.delegatesFocus, \
             root.slotAssignment, host.shadowRoot === root, root instanceof DocumentFragment);"
        ),
        "11,open,true,true,manual,true,true"
    );
}

#[test]
fn a_closed_root_is_attached_but_never_handed_out() {
    assert_eq!(
        run("var root = host.attachShadow({ mode: 'closed' }); \
             log.push(host.shadowRoot === null, root.mode, root.slotAssignment, \
             root.delegatesFocus);"),
        "true,closed,named,false"
    );
}

#[test]
fn attach_shadow_names_its_exceptions_and_checks_the_mode() {
    assert_eq!(
        run("host.attachShadow({ mode: 'open' }); \
             try { host.attachShadow({ mode: 'open' }); } catch (e) { log.push(e.name); } \
             try { plain.attachShadow({ mode: 'open' }); } catch (e) { log.push(e.name); } \
             try { light.attachShadow({ mode: 'bogus' }); } catch (e) { log.push(e.name); } \
             try { host.attachShadow(); } catch (e) { log.push(e.name); }"),
        "InvalidStateError,NotSupportedError,TypeError,TypeError"
    );
}

#[test]
fn only_elements_have_attach_shadow_and_shadow_root() {
    assert_eq!(
        run(
            "log.push(typeof host.attachShadow, typeof light.firstChild.attachShadow, \
             typeof light.firstChild.shadowRoot, typeof light.firstChild.assignedSlot);"
        ),
        "function,undefined,undefined,object"
    );
}

#[test]
fn get_root_node_stops_at_a_shadow_root_unless_composed() {
    assert_eq!(
        run("var root = host.attachShadow({ mode: 'open' }); \
             var inner = document.createElement('i'); root.appendChild(inner); \
             var detached = document.createElement('div'); \
             var droot = detached.attachShadow({ mode: 'open' }); \
             var deep = document.createElement('b'); droot.appendChild(deep); \
             log.push(inner.getRootNode() === root, \
             inner.getRootNode({ composed: true }) === document, inner.isConnected, \
             root.isConnected, deep.getRootNode({ composed: true }) === detached, \
             deep.isConnected, light.getRootNode() === document, root.getRootNode() === root);"),
        "true,true,true,true,true,false,true,true"
    );
}

#[test]
fn a_composed_event_is_retargeted_to_the_host_outside_its_shadow_tree() {
    assert_eq!(
        run("var root = host.attachShadow({ mode: 'open' }); \
             var inner = document.createElement('button'); root.appendChild(inner); \
             document.addEventListener('x', function (e) { log.push('doc:' + (e.target === host)); }); \
             host.addEventListener('x', function (e) { \
               log.push('host:' + (e.target === host) + ':' + e.eventPhase); }); \
             root.addEventListener('x', function (e) { log.push('root:' + (e.target === inner)); }); \
             inner.addEventListener('x', function (e) { log.push('inner:' + (e.target === inner)); }); \
             inner.dispatchEvent(new Event('x', { bubbles: true, composed: true }));"),
        "inner:true,root:true,host:true:2,doc:true"
    );
}

#[test]
fn a_non_composed_event_stops_at_its_shadow_root() {
    assert_eq!(
        run("var root = host.attachShadow({ mode: 'open' }); \
             var inner = document.createElement('button'); root.appendChild(inner); \
             document.addEventListener('x', function () { log.push('doc'); }); \
             host.addEventListener('x', function () { log.push('host'); }); \
             root.addEventListener('x', function () { log.push('root'); }); \
             inner.addEventListener('x', function () { log.push('inner'); }); \
             inner.dispatchEvent(new Event('x', { bubbles: true }));"),
        "inner,root"
    );
}

#[test]
fn composed_path_hides_a_closed_root_from_listeners_outside_it() {
    assert_eq!(
        run("var root = host.attachShadow({ mode: 'closed' }); \
             var inner = document.createElement('button'); root.appendChild(inner); \
             document.addEventListener('x', function (e) { \
               log.push('doc:' + e.composedPath().length + ':' + (e.composedPath()[0] === host)); }); \
             inner.addEventListener('x', function (e) { \
               log.push('inner:' + e.composedPath().length + ':' + (e.composedPath()[1] === root)); }); \
             inner.dispatchEvent(new Event('x', { bubbles: true, composed: true }));"),
        "inner:7:true,doc:5:true"
    );
}

#[test]
fn related_target_reads_as_the_host_outside_the_shadow_tree() {
    assert_eq!(
        run("var root = host.attachShadow({ mode: 'open' }); \
             var inner = document.createElement('input'); root.appendChild(inner); \
             document.addEventListener('y', function (e) { log.push('doc:' + (e.relatedTarget === host)); }); \
             plain.addEventListener('y', function (e) { log.push('plain:' + (e.relatedTarget === host)); }); \
             var ev = new Event('y', { bubbles: true }); ev.relatedTarget = inner; \
             plain.dispatchEvent(ev);"),
        "plain:true,doc:true"
    );
}

#[test]
fn a_shadow_root_is_a_queryable_container_with_its_own_markup() {
    assert_eq!(
        run("var root = host.attachShadow({ mode: 'open' }); \
             root.innerHTML = '<p class=a>one</p><p>two</p>'; \
             log.push(root.querySelectorAll('p').length, root.querySelector('.a').textContent, \
             root.innerHTML, document.querySelectorAll('p').length);"),
        "2,one,<p class=\"a\">one</p><p>two</p>,0"
    );
}

#[test]
fn focusing_a_delegating_host_focuses_its_first_focusable_part() {
    assert_eq!(
        run(
            "var root = host.attachShadow({ mode: 'open', delegatesFocus: true }); \
             var span = document.createElement('span'); \
             var field = document.createElement('input'); \
             root.appendChild(span); root.appendChild(field); \
             host.focus(); \
             log.push(document.activeElement === host, root.activeElement === field, \
             field.getRootNode() === root);"
        ),
        "true,true,true"
    );
}

#[test]
fn slots_assign_light_children_by_name_and_report_them() {
    assert_eq!(
        run("var root = host.attachShadow({ mode: 'open' }); \
             root.innerHTML = '<slot name=t></slot><slot></slot>'; \
             var t = document.createElement('h1'); t.setAttribute('slot', 't'); \
             host.appendChild(t); \
             var slots = root.querySelectorAll('slot'); \
             log.push(slots[0].assignedNodes().length, slots[0].assignedNodes()[0] === t, \
             t.assignedSlot === slots[0], light.assignedSlot === slots[1], \
             slots[1].assignedElements().length, slots[1].assignedNodes().length);"),
        "1,true,true,true,1,1"
    );
}
