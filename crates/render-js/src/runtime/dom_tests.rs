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
fn focus_moves_the_focused_area_and_fires_focus_events() {
    assert_eq!(
        eval(
            "var log = []; var one = $('one'), two = $('two'); \
             one.setAttribute('tabindex', '0'); two.setAttribute('tabindex', '-1'); \
             one.addEventListener('focus', function (e) { log.push('focus:' + e.isTrusted + ',' + e.bubbles + ',' + document.activeElement.id + ',' + (e.relatedTarget === null) + ',' + (e instanceof FocusEvent)); }); \
             one.addEventListener('blur', function (e) { log.push('blur:' + document.activeElement.nodeName + ',' + e.relatedTarget.id); }); \
             document.addEventListener('focusin', function (e) { log.push('focusin:' + e.target.id); }); \
             document.addEventListener('focusout', function (e) { log.push('focusout:' + e.target.id); }); \
             two.addEventListener('focus', function (e) { log.push('two-focus:' + e.relatedTarget.id); }); \
             var before = document.activeElement.nodeName; \
             one.focus(); one.focus(); two.focus(); \
             [before, document.activeElement.id, log.join(' ')].join('|')"
        ),
        "BODY|two|focus:true,false,one,true,true focusin:one blur:BODY,two focusout:one two-focus:one focusin:two"
    );
    assert_eq!(
        eval(
            "var li = $('three'); li.focus(); var first = document.activeElement.nodeName; \
             li.setAttribute('tabindex', '-1'); li.setAttribute('hidden', ''); li.focus(); \
             var hidden = document.activeElement.nodeName; \
             var one = $('one'); one.setAttribute('tabindex', '0'); one.focus(); one.blur(); \
             [first, hidden, document.activeElement.nodeName].join(',')"
        ),
        "BODY,BODY,BODY"
    );
    assert_eq!(
        eval(
            "var button = document.createElement('button'); document.body.appendChild(button); \
             button.focus(); var focused = document.activeElement.nodeName; \
             button.setAttribute('disabled', ''); var disabled = document.activeElement.nodeName; \
             button.removeAttribute('disabled'); button.focus(); var refocused = document.activeElement.nodeName; \
             button.parentNode.removeChild(button); \
             [focused, disabled, refocused, document.activeElement.nodeName].join(',')"
        ),
        "BUTTON,BODY,BUTTON,BODY"
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

/// Evaluate `source` in a document at `url` whose markup is `page`.
fn eval_at(page: &str, url: &str, source: &str) -> String {
    let mut dom = parse_document(page).dom;
    let document_url = url::Url::parse(url).expect("document url");
    let mut runtime = JsRuntime::with_url(&dom, &document_url);
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
fn boolean_reflection_adds_and_removes_the_attribute() {
    // HTML "Reflecting content attributes": the getter reports presence; the setter adds "" or removes.
    assert_eq!(
        eval(
            "var p = document.getElementById('para'); var out = [p.hidden]; \
             p.hidden = true; out.push(p.hidden, p.getAttribute('hidden') === ''); \
             p.hidden = false; out.push(p.hidden, p.hasAttribute('hidden')); out.join()"
        ),
        "false,true,true,false,false"
    );
}
#[test]
fn enumerated_reflection_reads_canonical_keywords_and_no_state() {
    // HTML "Enumerated attributes": keywords match ASCII case-insensitively and read in their
    // canonical case. `dir` has no missing or invalid default, so both read "".
    assert_eq!(
        eval(
            "var p = document.getElementById('para'); var out = [p.dir === '']; \
             p.setAttribute('dir', 'RTL'); out.push(p.dir); \
             p.dir = 'auto'; out.push(p.getAttribute('dir')); \
             p.dir = 'sideways'; out.push(p.getAttribute('dir'), p.dir === ''); out.join()"
        ),
        "true,rtl,auto,sideways,true"
    );
}
#[test]
fn enumerated_reflection_nullable_default_and_invalid_value() {
    // The script crossOrigin attribute is nullable: missing reads null, an
    // invalid value reads its invalid default, and writing null removes it.
    assert_eq!(
        eval(
            "var s = document.createElement('script'); var out = [s.crossOrigin === null]; \
             s.setAttribute('crossorigin', ''); out.push(s.crossOrigin); \
             s.crossOrigin = 'use-credentials'; out.push(s.getAttribute('crossorigin')); \
             s.crossOrigin = null; out.push(s.hasAttribute('crossorigin')); out.join()"
        ),
        "true,anonymous,use-credentials,false"
    );
}
#[test]
fn enumerated_reflection_with_missing_and_invalid_defaults() {
    // Media preload reads metadata when absent. Track kind reads subtitles when
    // absent and metadata when invalid (HTML "The track element").
    assert_eq!(
        eval(
            "var v = document.createElement('video'); var t = document.createElement('track'); \
             var out = [v.preload, t.kind]; v.setAttribute('preload', 'AUTO'); out.push(v.preload); \
             t.setAttribute('kind', 'bogus'); out.push(t.kind); out.join()"
        ),
        "metadata,subtitles,auto,metadata"
    );
}
#[test]
fn integer_reflection_parses_with_its_default() {
    // `long` (HTML "Reflecting content attributes"): the rules for parsing integers, defaulting when the
    // value is absent, not an integer, or outside the signed 32-bit range.
    assert_eq!(
        eval(
            "var li = document.getElementById('one'); var out = [li.value]; \
             li.value = '7'; out.push(li.value, li.getAttribute('value')); \
             li.setAttribute('value', ' +12x'); out.push(li.value); \
             li.setAttribute('value', '2147483648'); out.push(li.value); \
             li.value = 2147483648; out.push(li.getAttribute('value')); \
             var ol = document.createElement('ol'); out.push(ol.start); \
             ol.start = -5; out.push(ol.start); out.join()"
        ),
        "0,7,7,12,0,-2147483648,1,-5"
    );
}
#[test]
fn unsigned_integer_reflection_clamps_and_defaults() {
    // Table cells (HTML "The td element" and "The th element"): colSpan clamps to [1, 1000] and rowSpan to
    // [0, 65534]; both default to 1. Canvas defaults to 300 by 150.
    assert_eq!(
        eval(
            "var td = document.createElement('td'); var out = [td.colSpan, td.rowSpan]; \
             td.colSpan = 0; out.push(td.getAttribute('colspan'), td.colSpan); \
             td.colSpan = 5000; out.push(td.colSpan); \
             td.setAttribute('colspan', '-3'); out.push(td.colSpan); \
             td.rowSpan = 0; out.push(td.rowSpan); \
             var c = document.createElement('canvas'); out.push(c.width, c.height); out.join()"
        ),
        "1,1,0,1,1000,1,0,300,150"
    );
}
#[test]
fn url_reflection_resolves_against_the_base_element_and_tracks_it() {
    // HTML "Reflecting content attributes" (URL reflection) and "document base URL": the first
    // base element with an href sets the base, and a later change to it is seen.
    let page = "<!doctype html><html><head><base href='/base/'></head>\
                <body><a id=a href='page.html'>x</a><a id=b>y</a></body></html>";
    assert_eq!(
        eval_at(
            page,
            "https://example.test/dir/index.html",
            "var a = document.getElementById('a'); var b = document.getElementById('b'); \
             var out = [a.href, b.href]; b.href = 'other'; out.push(b.href, b.getAttribute('href')); \
             document.querySelector('base').setAttribute('href', '/moved/'); out.push(a.href); out.join()"
        ),
        "https://example.test/base/page.html,,https://example.test/base/other,other,https://example.test/moved/page.html"
    );
}
#[test]
fn reflection_is_limited_to_the_interfaces_that_declare_it() {
    // `download` belongs to HTMLAnchorElement and HTMLAreaElement only, while
    // the ARIA reflection (on Element) reads its missing default on any element.
    assert_eq!(
        eval(
            "var p = document.createElement('p'); var div = document.createElement('div'); \
             var out = [p.download === undefined, document.createElement('a').download === '']; \
             out.push(div.ariaLive); div.ariaLive = 'POLITE'; out.push(div.getAttribute('aria-live'), div.ariaLive); \
             out.join()"
        ),
        "true,true,off,POLITE,polite"
    );
}
#[test]
fn form_control_properties_keep_their_existing_behaviour() {
    // Form controls keep the legacy value and type behaviour until the form
    // reflection tables replace it.
    assert_eq!(
        eval(
            "var i = document.createElement('input'); i.value = 'abc'; \
             [i.value, i.type, i.getAttribute('value')].join()"
        ),
        "abc,text,abc"
    );
}
#[test]
fn translate_is_inherited_from_the_nearest_attribute() {
    // HTML "The translate attribute": an element is translate-enabled unless the nearest `translate`
    // attribute (on it or an ancestor) says `no`; the setter stores `yes` or `no`.
    assert_eq!(
        eval(
            "var d = document.createElement('div'); var c = document.createElement('span'); \
             d.appendChild(c); var out = [c.translate]; d.translate = false; \
             out.push(c.translate, d.getAttribute('translate')); c.translate = true; \
             out.push(c.getAttribute('translate'), c.translate); out.join()"
        ),
        "true,false,no,yes,true"
    );
}
#[test]
fn document_title_strips_and_collapses_and_creates_a_title() {
    // HTML "The title element": the getter strips and collapses ASCII whitespace; the setter
    // replaces the first title, or appends one to head when there is none.
    assert_eq!(
        eval(
            "var out = [document.title]; document.title = '  A   b\\n c  '; out.push(document.title); out.join('|')"
        ),
        "t|A b c"
    );
    assert_eq!(
        eval_at(
            "<!doctype html><html><head></head><body></body></html>",
            "https://example.test/",
            "var out = [document.title === '']; document.title = 'New'; \
             out.push(document.title, document.getElementsByTagName('title').length); out.join()"
        ),
        "true,New,1"
    );
}
#[test]
fn anchor_text_is_its_text_content() {
    // HTML "The a element": `text` reads and writes the element's textContent.
    assert_eq!(
        eval(
            "var a = document.createElement('a'); a.textContent = 'x y'; var out = [a.text]; \
             a.text = 'z'; out.push(a.textContent); out.join()"
        ),
        "x y,z"
    );
}
#[test]
fn legacy_document_colours_reflect_the_body_attributes() {
    // The obsolete document.fgColor (and bgColor, linkColor, vlinkColor,
    // alinkColor) read and write the body's text, bgcolor, link, vlink and alink
    // attributes, and a null write stores the empty string.
    assert_eq!(
        eval_at(
            "<!doctype html><html><body text=red></body></html>",
            "https://example.test/",
            "var out = [document.fgColor]; document.bgColor = 'green'; \
             out.push(document.body.getAttribute('bgcolor'), document.bgColor); \
             document.fgColor = null; out.push(document.body.hasAttribute('text'), document.fgColor === ''); out.join()"
        ),
        "red,green,green,true,true"
    );
}

#[test]
fn create_html_document_builds_the_standard_skeleton() {
    assert_eq!(
        eval(
            "var d = document.implementation.createHTMLDocument('t'); \
             [d.childNodes.length, d.doctype.name, d.documentElement.localName, \
              d.documentElement.childNodes.length, d.head.firstChild.localName, \
              d.head.firstChild.firstChild.data, d.body.localName, d.body.childNodes.length].join()"
        ),
        "2,html,html,2,title,t,body,0"
    );
    assert_eq!(
        eval(
            "var d = document.implementation.createHTMLDocument(); \
             [d.head.childNodes.length, d.documentElement.childNodes.length].join()"
        ),
        "0,2"
    );
}

#[test]
fn created_html_document_has_its_own_metadata() {
    assert_eq!(
        eval(
            "var d = document.implementation.createHTMLDocument(); \
             [d.URL, d.documentURI, d.contentType, d.compatMode, d.characterSet, d.readyState, \
              d.location, d.defaultView, d instanceof HTMLDocument, d instanceof Document, \
              d.createElement('DIV').localName].join()"
        ),
        "about:blank,about:blank,text/html,CSS1Compat,UTF-8,complete,,,true,true,div"
    );
}

#[test]
fn each_document_has_one_implementation() {
    assert_eq!(
        eval(
            "var d = document.implementation.createHTMLDocument(); \
             [document.implementation === document.implementation, \
              d.implementation === d.implementation, d.implementation === document.implementation, \
              d.implementation instanceof DOMImplementation].join()"
        ),
        "true,true,false,true"
    );
}

#[test]
fn create_document_makes_a_typed_xml_document_with_a_root() {
    assert_eq!(
        eval(
            "var d = document.implementation.createDocument('http://www.w3.org/2000/svg', 'svg', null); \
             [Object.getPrototypeOf(d) === XMLDocument.prototype, d instanceof XMLDocument, \
              d.contentType, d.documentElement.localName, d.documentElement.namespaceURI, \
              d.documentElement.ownerDocument === d, d.doctype, d.childNodes.length, d.readyState].join()"
        ),
        "true,true,image/svg+xml,svg,http://www.w3.org/2000/svg,true,,1,complete"
    );
    assert_eq!(
        eval(
            "var t = document.implementation.createDocumentType('html', '', ''); \
             var d = document.implementation.createDocument(null, '', t); \
             [d.contentType, d.documentElement, d.doctype.name, d.doctype.ownerDocument === d].join()"
        ),
        "application/xml,,html,true"
    );
}

#[test]
fn create_document_rejects_a_bad_name_and_a_non_doctype() {
    assert_eq!(
        eval(
            "var out = []; \
             try { document.implementation.createDocument(null, 'a:b'); } catch (e) { out.push(e.name); } \
             try { document.implementation.createDocument(); } catch (e) { out.push(e.name); } \
             try { document.implementation.createDocument(null, 'x', {}); } catch (e) { out.push(e.name); } \
             out.join()"
        ),
        "NamespaceError,TypeError,TypeError"
    );
}

#[test]
fn new_document_is_an_empty_xml_document_that_is_not_an_xml_document() {
    assert_eq!(
        eval(
            "var d = new Document(); \
             [Object.getPrototypeOf(d) === Document.prototype, d instanceof XMLDocument, \
              d.contentType, d.URL, d.location, d.doctype, d.documentElement, \
              d.createElement('DIV').localName, d.createElement('a').constructor === Element].join()"
        ),
        "true,false,application/xml,about:blank,,,,DIV,true"
    );
}

#[test]
fn html_and_xml_document_interfaces_have_no_constructor() {
    assert_eq!(
        eval(
            "var out = []; \
             [HTMLDocument, XMLDocument].forEach(function (C) { \
               try { new C(); out.push('constructed'); } catch (e) { out.push(e.name); } }); \
             out.join()"
        ),
        "TypeError,TypeError"
    );
}

#[test]
fn nodes_belong_to_the_document_that_created_or_adopted_them() {
    assert_eq!(
        eval(
            "var d = document.implementation.createHTMLDocument('x'); \
             var p = d.createElement('p'); var page = document.createElement('i'); \
             [p.ownerDocument === d, d.body.appendChild(page).ownerDocument === d, \
              page.ownerDocument === document, document.body.ownerDocument === document, \
              d.doctype.ownerDocument === d, d.ownerDocument, \
              document.implementation.createDocumentType('x', '', '').ownerDocument === document].join()"
        ),
        "true,true,false,true,true,,true"
    );
}

#[test]
fn adopt_node_moves_a_node_into_the_document() {
    assert_eq!(
        eval(
            "var d = document.implementation.createHTMLDocument(); var el = $('one'); \
             var moved = d.adoptNode(el); \
             var before = [moved === el, el.parentNode === null, el.ownerDocument === d, \
               document.getElementById('one') === null].join(); \
             d.body.appendChild(el); \
             var out = []; \
             try { d.adoptNode(document); } catch (e) { out.push(e.name); } \
             before + ';' + [el.parentNode === d.body, el.ownerDocument === d].join() + ';' + out.join()"
        ),
        "true,true,true,true;true,true;NotSupportedError"
    );
}

#[test]
fn import_node_copies_into_the_document() {
    assert_eq!(
        eval(
            "var d = document.implementation.createHTMLDocument(); \
             var copy = d.importNode($('two'), true); \
             [copy.ownerDocument === d, copy.textContent, $('two').ownerDocument === document, \
              copy.parentNode === null].join()"
        ),
        "true,2,true,true"
    );
}

#[test]
fn import_node_refuses_a_document() {
    assert_eq!(
        eval(
            "var d = document.implementation.createHTMLDocument(); var out = []; \
             try { document.importNode(d, true); } catch (e) { out.push(e.name); } \
             try { document.importNode(document, false); } catch (e) { out.push(e.name); } \
             out.join()"
        ),
        "NotSupportedError,NotSupportedError"
    );
}

#[test]
fn clone_node_copies_a_document_with_its_doctype_and_children() {
    assert_eq!(
        eval(
            "var d = document.implementation.createHTMLDocument('t'); var c = d.cloneNode(true); \
             var shallow = d.cloneNode(false); \
             [c === d, c instanceof HTMLDocument, c.contentType, c.childNodes.length, c.doctype.name, \
              c.title, c.ownerDocument === null, c.body.ownerDocument === c, shallow.childNodes.length].join()"
        ),
        "false,true,text/html,2,html,t,true,true,0"
    );
}
