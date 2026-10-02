//! DOM methods and Web APIs that are self-hosted in `prelude.js`.

use crate::JsRuntime;
use render_html::parse_document;

const PAGE: &str = "<!doctype html><html><head><title>t</title></head><body>\
    <ul id=list><li id=one class=item>1</li><li id=two class=item>2</li><li id=three>3</li></ul>\
    <p id=para>hello <b id=bold>world</b></p></body></html>";

fn eval(source: &str) -> String {
    let mut dom = parse_document(PAGE).dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var $ = function (id) { return document.getElementById(id); }; \
             var ids = function (nodes) { var out = []; for (var i = 0; i < nodes.length; i++) out.push(nodes[i].id || nodes[i].nodeName); return out.join(','); };",
        )
        .expect("setup");
    let value = runtime
        .execute(&mut dom, source)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
        .value
        .to_js_string();
    assert!(runtime.prelude_error().is_none());
    value
}

#[test]
fn closest_walks_up_and_includes_the_element_itself() {
    assert_eq!(
        eval(
            "[$('bold').closest('p').id, $('bold').closest('b').id, String($('bold').closest('ul')), $('two').closest('.item').id].join()"
        ),
        "para,bold,null,two"
    );
}

#[test]
fn parent_node_mixins_accept_nodes_and_strings() {
    assert_eq!(
        eval(
            "var list = $('list'); var li = document.createElement('li'); li.id = 'new'; \
              list.append(li, 'tail'); list.prepend('head'); \
              [list.firstChild.data, list.lastChild.data, list.children.length, list.childElementCount, \
               list.firstElementChild.id, list.lastElementChild.id].join()"
        ),
        "head,tail,4,4,one,new"
    );
    assert_eq!(
        eval(
            "var list = $('list'); list.replaceChildren(document.createElement('hr'), 'x'); \
              [list.childNodes.length, list.firstChild.nodeName].join()"
        ),
        "2,HR"
    );
}

#[test]
fn child_node_mixins_insert_around_and_replace() {
    assert_eq!(
        eval(
            "var two = $('two'); var a = document.createElement('i'); a.id = 'a'; var b = document.createElement('i'); b.id = 'b'; \
              two.before(a); two.after(b, 'txt'); ids($('list').children)"
        ),
        "one,a,two,b,three"
    );
    assert_eq!(
        eval(
            "var two = $('two'); var r = document.createElement('em'); r.id = 'r'; two.replaceWith(r, 'z'); \
              [ids($('list').children), $('two') === null, two.parentNode === null].join()"
        ),
        "one,r,three,true,true"
    );
    assert_eq!(
        eval(
            "var t = $('para').firstChild; t.after('!'); [$('para').childNodes.length, t.nextSibling.data].join()"
        ),
        "3,!"
    );
    assert_eq!(
        eval(
            "var detached = document.createElement('div'); detached.before('x'); detached.after('y'); 'no parent, no error'"
        ),
        "no parent, no error"
    );
}

#[test]
fn sibling_navigation_skips_non_elements() {
    assert_eq!(
        eval(
            "var para = $('para'); [para.firstChild.nextElementSibling.id, $('bold').previousElementSibling, \
              $('one').nextElementSibling.id, $('three').previousElementSibling.id].join()"
        ),
        "bold,,two,two"
    );
}

#[test]
fn insert_adjacent_family() {
    assert_eq!(
        eval(
            "var two = $('two'); two.insertAdjacentHTML('beforebegin', '<li id=x>x</li>'); \
              two.insertAdjacentHTML('afterend', '<li id=y>y</li>'); \
              two.insertAdjacentHTML('afterbegin', '<b id=in1></b>'); two.insertAdjacentHTML('beforeend', '<b id=in2></b>'); \
              [ids($('list').children), ids(two.children)].join('|')"
        ),
        "one,x,two,y,three|in1,in2"
    );
    assert_eq!(
        eval(
            "var e = document.createElement('i'); e.id = 'e'; var r = $('two').insertAdjacentElement('beforebegin', e); \
              $('two').insertAdjacentText('beforeend', 'T'); [r === e, ids($('list').children), $('two').lastChild.data].join()"
        ),
        "true,one,e,two,three,T"
    );
    assert_eq!(
        eval("try { $('two').insertAdjacentHTML('sideways', 'x'); } catch (e) { e.name }"),
        "SyntaxError"
    );
}

#[test]
fn attributes_helpers() {
    assert_eq!(
        eval(
            "var p = $('para'); p.setAttribute('data-a', '1'); \
              [p.getAttributeNames().join('|'), p.hasAttributes(), p.toggleAttribute('hidden'), p.hasAttribute('hidden'), \
               p.toggleAttribute('hidden'), p.hasAttribute('hidden'), p.toggleAttribute('open', true), p.toggleAttribute('open', true), \
               p.toggleAttribute('open', false)].join()"
        ),
        "id|data-a,true,true,true,false,false,true,true,false"
    );
}

#[test]
fn node_relationships() {
    assert_eq!(
        eval(
            "var d = document.createElement('div'); \
              [$('one').isConnected, d.isConnected, $('bold').getRootNode() === document, d.getRootNode() === d, \
               $('list').hasChildNodes(), d.hasChildNodes(), $('one').isSameNode($('one')), $('one').isSameNode($('two'))].join()"
        ),
        "true,false,true,true,true,false,true,false"
    );
    assert_eq!(
        eval(
            "var one = $('one'), two = $('two'), list = $('list'); \
              [one.compareDocumentPosition(two), two.compareDocumentPosition(one), list.compareDocumentPosition(one), \
               one.compareDocumentPosition(list), one.compareDocumentPosition(one), \
               one.compareDocumentPosition(document.createElement('x')) & 1].join()"
        ),
        "4,2,20,10,0,1"
    );
    assert_eq!(
        eval(
            "var a = $('list').cloneNode(true); var b = $('list').cloneNode(true); var before = a.isEqualNode(b); \
              b.lastChild.textContent = 'different'; [before, a.isEqualNode(b), a.isEqualNode(null)].join()"
        ),
        "true,false,false"
    );
    assert_eq!(
        eval(
            "var p = document.createElement('p'); p.appendChild(document.createTextNode('a')); p.appendChild(document.createTextNode('')); \
              p.appendChild(document.createTextNode('b')); p.normalize(); [p.childNodes.length, p.firstChild.data].join()"
        ),
        "1,ab"
    );
    assert_eq!(
        eval(
            "var list = $('list'); var n = document.createElement('li'); n.id = 'n'; var old = list.replaceChild(n, $('two')); \
              [old.id, ids(list.children)].join()"
        ),
        "two,one,n,three"
    );
}

#[test]
fn document_properties() {
    assert_eq!(
        eval(
            "[document.URL === location.href, document.characterSet, document.contentType, document.hidden, \
              document.visibilityState, document.referrer === '', document.doctype.nodeType, \
              document.getElementsByName('nope').length].join()"
        ),
        "true,UTF-8,text/html,false,visible,true,10,0"
    );
    assert_eq!(
        eval(
            "document.body.insertAdjacentHTML('beforeend', '<form id=f><input name=q></form><img id=im><a href=\"/x\">l</a>'); \
              [document.forms.length, document.images.length, document.links.length, document.getElementsByName('q').length].join()"
        ),
        "1,1,1,1"
    );
}

#[test]
fn tree_walker_and_node_iterator() {
    assert_eq!(
        eval(
            "var w = document.createTreeWalker($('list'), NodeFilter.SHOW_ELEMENT); var out = []; var n; \
              while ((n = w.nextNode())) out.push(n.id); out.join()"
        ),
        "one,two,three"
    );
    assert_eq!(
        eval(
            "var w = document.createTreeWalker($('para'), NodeFilter.SHOW_TEXT); var out = []; var n; \
              while ((n = w.nextNode())) out.push(n.data); out.join('|')"
        ),
        "hello |world"
    );
    assert_eq!(
        eval(
            "var w = document.createTreeWalker($('list'), NodeFilter.SHOW_ELEMENT, \
                { acceptNode: function (n) { return n.id === 'two' ? NodeFilter.FILTER_SKIP : NodeFilter.FILTER_ACCEPT; } }); \
              var out = []; var n; while ((n = w.nextNode())) out.push(n.id); out.join()"
        ),
        "one,three"
    );
    assert_eq!(
        eval(
            "var it = document.createNodeIterator($('list'), NodeFilter.SHOW_ELEMENT); var out = []; var n; \
              while ((n = it.nextNode())) out.push(n.id); out.join()"
        ),
        "list,one,two,three"
    );
}

#[test]
fn event_target_works_for_plain_objects_and_subclasses() {
    assert_eq!(
        eval(
            "var t = new EventTarget(); var log = []; \
              t.addEventListener('ping', function (e) { log.push(e.type, e.target === t, e.currentTarget === t); }); \
              t.addEventListener('ping', { handleEvent: function (e) { log.push('obj'); } }); \
              var once = function () { log.push('once'); }; t.addEventListener('ping', once, { once: true }); \
              var r = t.dispatchEvent(new Event('ping', { cancelable: true })); t.dispatchEvent(new Event('ping')); log.push(r); log.join()"
        ),
        "ping,true,true,obj,once,ping,true,true,obj,true"
    );
    assert_eq!(
        eval(
            "class Bus extends EventTarget { emit(n) { this.dispatchEvent(new CustomEvent('msg', { detail: n })); } } \
              var bus = new Bus(), got = []; bus.addEventListener('msg', function (e) { got.push(e.detail); }); \
              bus.emit(1); bus.emit({ a: 2 }); [bus instanceof EventTarget, got[0], got[1].a].join()"
        ),
        "true,1,2"
    );
    assert_eq!(
        eval(
            "var t = new EventTarget(); var c = new AbortController(); var n = 0; \
              t.addEventListener('x', function () { n++; }, { signal: c.signal }); \
              t.dispatchEvent(new Event('x')); c.abort(); t.dispatchEvent(new Event('x')); n"
        ),
        "1"
    );
}

#[test]
fn event_classes_carry_their_fields() {
    assert_eq!(
        eval(
            "var m = new MouseEvent('click', { clientX: 5, clientY: 6, button: 2, ctrlKey: true, bubbles: true }); \
              [m.type, m.clientX, m.clientY, m.button, m.ctrlKey, m.shiftKey, m.bubbles, m instanceof MouseEvent, \
               m instanceof UIEvent, m instanceof Event, m.detail].join()"
        ),
        "click,5,6,2,true,false,true,true,true,true,0"
    );
    assert_eq!(
        eval(
            "var k = new KeyboardEvent('keydown', { key: 'a', code: 'KeyA', metaKey: true }); \
              [k.key, k.code, k.repeat, k.getModifierState('Meta'), k.getModifierState('Shift'), k instanceof KeyboardEvent].join()"
        ),
        "a,KeyA,false,true,false,true"
    );
    assert_eq!(
        eval(
            "var c = new CustomEvent('x', { detail: { n: 1 } }); [c.detail.n, new CustomEvent('y').detail, c instanceof CustomEvent].join()"
        ),
        "1,,true"
    );
    assert_eq!(
        eval(
            "class Click extends MouseEvent { constructor() { super('click', { clientX: 9 }); this.extra = 1; } } \
              var c = new Click(); [c instanceof Click, c instanceof MouseEvent, c.clientX, c.extra].join()"
        ),
        "true,true,9,1"
    );
    assert_eq!(
        eval(
            "var seen = ''; $('one').addEventListener('click', function (e) { seen = e.clientX + ':' + (e instanceof MouseEvent); }); \
              $('one').dispatchEvent(new MouseEvent('click', { clientX: 42, bubbles: true })); seen"
        ),
        "42:true"
    );
}

#[test]
fn messaging_primitives_deliver_asynchronously() {
    let mut dom = parse_document(PAGE).dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var log = []; \
             var mc = new MessageChannel(); mc.port2.onmessage = function (e) { log.push('port2:' + e.data.n); }; \
             mc.port1.postMessage({ n: 1 }); \
             window.addEventListener('message', function (e) { log.push('window:' + e.data + ':' + (e.source === window)); }); \
             window.postMessage('hi', '*'); \
             var a = new BroadcastChannel('c'), b = new BroadcastChannel('c'), other = new BroadcastChannel('d'); \
             b.onmessage = function (e) { log.push('b:' + e.data); }; a.onmessage = function () { log.push('a-should-not-hear'); }; \
             other.onmessage = function () { log.push('d-should-not-hear'); }; \
             a.postMessage('bc'); log.push('sync-done');",
        )
        .expect("setup");
    for _ in 0..20 {
        let microtasks = runtime.take_pending_microtasks();
        let timers = runtime.take_pending_timer_requests();
        if microtasks.is_empty() && timers.is_empty() {
            break;
        }
        for microtask in microtasks {
            let _ = runtime.invoke_microtask(&mut dom, microtask);
        }
        for timer in timers {
            if let crate::TimerRequest::Schedule { id, .. } = timer {
                let _ = runtime.fire_timer(&mut dom, id);
            }
        }
    }
    let log = runtime
        .execute(&mut dom, "log.join(',')")
        .expect("log")
        .value
        .to_js_string();
    assert_eq!(log, "sync-done,port2:1,window:hi:true,b:bc");
}

#[test]
fn window_surface_is_present() {
    assert_eq!(
        eval(
            "[window.devicePixelRatio, typeof window.alert, window.confirm('?'), window.prompt('?'), window.open('x'), \
              window.name === '', window.frames === window, window.isSecureContext, window.getSelection().toString() === '', \
              window.getSelection().rangeCount].join()"
        ),
        "1,function,false,,,true,true,false,true,0"
    );
}
