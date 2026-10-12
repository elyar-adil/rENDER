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
        .to_js_string()
        .to_lossy();
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
        .to_js_string()
        .to_lossy();
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

#[test]
fn element_type_returns_the_spec_default_and_canonical_keyword() {
    // Button (HTML 4.10.6.3): keyword match is ASCII case-insensitive, and a
    // missing or invalid value means `submit`. Input (4.10.5.1.1) defaults to
    // `text`. Textarea, output and fieldset have fixed values.
    assert_eq!(
        eval(
            "var b = document.createElement('button'); var i = document.createElement('input'); \
             var out = [b.type]; b.setAttribute('type', 'RESET'); out.push(b.type); \
             b.setAttribute('type', 'bogus'); out.push(b.type); \
             out.push(i.type); i.setAttribute('type', 'CheckBox'); out.push(i.type); \
             out.push(document.createElement('textarea').type, document.createElement('output').type, \
                      document.createElement('fieldset').type); out.join()"
        ),
        "submit,reset,submit,text,checkbox,textarea,output,fieldset"
    );
}

#[test]
fn collections_are_live_and_keep_identity_across_reads() {
    // DOM 4.2.10: `childNodes` and `children` are one live object per node, and
    // `getElementsByTagName` is a live HTMLCollection that sees later inserts.
    assert_eq!(
        eval(
            "var list = $('list'); var tags = document.getElementsByTagName('li'); \
             var before = tags.length; list.appendChild(document.createElement('li')); \
             [list.childNodes === list.childNodes, list.children === list.children, \
              before, tags.length, tags[tags.length - 1].nodeName, \
              Object.prototype.toString.call(tags), tags.item(99) === null].join()"
        ),
        "true,true,3,4,LI,[object HTMLCollection],true"
    );
}

#[test]
fn node_list_and_html_collection_iterate_and_index_like_arrays() {
    assert_eq!(
        eval(
            "var list = $('list'); var seen = []; list.childNodes.forEach(function (n) { seen.push(n.nodeName); }); \
             var kids = [...list.children].map(function (e) { return e.id; }); \
             [seen.length, kids.join('|'), list.children.namedItem('two').id, \
              String(list.children.namedItem('')), list.children.item(0).id, \
              list.childNodes.item(-1) === null, Array.isArray(list.childNodes)].join()"
        ),
        "3,one|two|three,two,null,one,true,false"
    );
}

#[test]
fn query_selector_all_returns_a_static_node_list() {
    assert_eq!(
        eval(
            "var all = document.querySelectorAll('li'); var list = $('list'); \
             list.appendChild(document.createElement('li')); \
             [all.length, all.item(0).id, Object.prototype.toString.call(all)].join()"
        ),
        "3,one,[object NodeList]"
    );
}

#[test]
fn get_element_by_id_ignores_the_empty_string_and_fragments_scope_their_search() {
    assert_eq!(
        eval(
            "var empty = document.createElement('div'); empty.id = ''; document.body.appendChild(empty); \
             var frag = document.createDocumentFragment(); var inner = document.createElement('span'); \
             inner.id = 'inner'; frag.appendChild(inner); \
             [String(document.getElementById('')), frag.getElementById('inner').id, \
              String(frag.getElementById('missing'))].join()"
        ),
        "null,inner,null"
    );
}

#[test]
fn dom_token_list_is_an_ordered_set_with_literal_value_and_update_steps() {
    assert_eq!(
        eval(
            "var span = document.createElement('span'); span.setAttribute('class', '   a  a b '); \
             var list = span.classList; var out = [list.length, String(list), list.value]; \
             list.add('a'); out.push(JSON.stringify(span.getAttribute('class'))); \
             out.push(list.toggle('c', undefined), list.toggle('c', true), list.toggle('a', false)); \
             out.push(list.replace('b', 'z'), list.replace('missing', 'q')); \
             out.push(span.getAttribute('class'), [...list].join('+'), list[0], list[9] === undefined); \
             out.join('|')"
        ),
        "2|   a  a b |   a  a b |\"a b\"|true|true|false|true|false|z c|z+c|z|true"
    );
}

#[test]
fn dom_token_list_iteration_is_live_and_stringifies_to_value() {
    assert_eq!(
        eval(
            "var span = document.createElement('span'); span.className = 'x y'; \
             var list = span.classList; var keys = [...list.keys()]; var entries = [...list.entries()].join(';'); \
             var seen = []; list.forEach(function (t, i) { seen.push(i + t); }); \
             [keys.join(), entries, seen.join(), list.toString(), \
              Object.prototype.toString.call(list)].join('#')"
        ),
        "0,1#0,x;1,y#0x,1y#x y#[object DOMTokenList]"
    );
}

#[test]
fn character_data_offsets_are_utf16_and_out_of_range_throws_index_size() {
    assert_eq!(
        eval(
            "var text = document.createTextNode('a\u{1F600}b'); \
             var out = [text.length, text.substringData(1, 2).length]; \
             text.appendData('!'); text.insertData(0, '>'); text.deleteData(1, 1); \
             text.replaceData(0, 1, 'X'); out.push(text.data, text.nodeValue); \
             try { text.substringData(99, 1); } catch (e) { out.push(e.name, e instanceof DOMException); } \
             out.join('|')"
        ),
        "4|2|X\u{1F600}b!|X\u{1F600}b!|IndexSizeError|true"
    );
}

#[test]
fn text_nodes_reject_children_with_a_hierarchy_request_dom_exception() {
    assert_eq!(
        eval(
            "var text = document.createTextNode('t'); var out = []; \
             try { text.appendChild(document.createElement('i')); } catch (e) { out.push(e.name, e instanceof DOMException); } \
             try { document.body.removeChild(document.createElement('i')); } catch (e) { out.push(e.name); } \
             out.join()"
        ),
        "HierarchyRequestError,true,NotFoundError"
    );
}

#[test]
fn create_element_ns_keeps_prefix_namespace_and_case_and_validates_names() {
    assert_eq!(
        eval(
            "var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg:linearGradient'); \
             var plain = document.createElementNS(null, 'Foo'); \
             var out = [svg.prefix, svg.localName, svg.tagName, svg.namespaceURI, String(plain.namespaceURI), plain.tagName]; \
             try { document.createElementNS('', ''); } catch (e) { out.push(e.name); } \
             try { document.createElementNS('http://example.com/', 'foo:'); } catch (e) { out.push(e.name); } \
             try { document.createElementNS(null, 'p:x'); } catch (e) { out.push(e.name); } \
             out.join()"
        ),
        "svg,linearGradient,svg:linearGradient,http://www.w3.org/2000/svg,null,Foo,InvalidCharacterError,InvalidCharacterError,NamespaceError"
    );
}

#[test]
fn namespaced_attributes_are_found_by_qualified_name_regardless_of_namespace() {
    assert_eq!(
        eval(
            "var el = document.createElement('div'); el.setAttributeNS('urn:example', 'foo', 'v'); \
             var out = [el.getAttributeNS('urn:example', 'foo'), el.hasAttribute('foo'), el.getAttribute('foo')]; \
             el.removeAttributeNS('urn:example', 'foo'); out.push(el.hasAttribute('foo'), String(el.getAttributeNS('', 'foo'))); \
             out.join()"
        ),
        "v,true,v,false,null"
    );
}

#[test]
fn document_implementation_creates_doctypes_and_rejects_bad_names() {
    assert_eq!(
        eval(
            "var doctype = document.implementation.createDocumentType('html', 'pub', 'sys'); \
             var out = [doctype.name, doctype.publicId, doctype.systemId, doctype.nodeType, \
                        document.implementation.hasFeature(), document.implementation === document.implementation]; \
             try { document.implementation.createDocumentType('bad name', '', ''); } catch (e) { out.push(e.name); } \
             out.join()"
        ),
        "html,pub,sys,10,true,true,InvalidCharacterError"
    );
}

#[test]
fn create_cdata_section_throws_in_html_and_processing_instructions_carry_their_target() {
    assert_eq!(
        eval(
            "var out = []; try { document.createCDATASection('x'); } catch (e) { out.push(e.name); } \
             var pi = document.createProcessingInstruction('xml-stylesheet', 'href=a'); \
             out.push(pi.target, pi.data, pi.nodeValue, pi.nodeType); \
             out.join()"
        ),
        "NotSupportedError,xml-stylesheet,href=a,href=a,7"
    );
}
