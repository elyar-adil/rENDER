from __future__ import annotations

from unittest.mock import patch

import engine
import pytest
from js.interpreter import Interpreter
from js.lexer import Lexer
from js.parser import Parser
from tests.layout_test_utils import iter_elements


def _exec(code: str) -> Interpreter:
    interp = Interpreter()
    ast = Parser(Lexer(code).tokenize()).parse()
    interp.execute(ast)
    return interp


def _pipeline_offline(html: str, *, viewport_w: int = 320, viewport_h: int = 240):
    with (
        patch("engine._fetch_subresources", return_value=([], [])),
        patch("engine._fetch_background_images", return_value=[]),
    ):
        return engine._pipeline(
            html,
            base_url="",
            viewport_width=viewport_w,
            viewport_height=viewport_h,
        )


def _find_by_attr(document, key: str, value: str):
    for node in iter_elements(document):
        if getattr(node, "attributes", {}).get(key) == value:
            return node
    return None


def _find_all_by_class(document, class_name: str):
    matches = []
    for node in iter_elements(document):
        classes = getattr(node, "attributes", {}).get("class", "").split()
        if class_name in classes:
            matches.append(node)
    return matches


def test_jquery_style_queue_and_callback_snippet_runs():
    code = """
        var queue = [];
        queue.push(function(next){ next(1); });
        var value = 0;
        queue[0](function(v){ value = v; });
    """
    interp = _exec(code)

    assert interp.global_env.get("value") == 1


def test_react_style_create_element_factory_snippet_runs():
    code = """
        var React = {
          createElement: function(type, props) {
            return { type: type, props: props, argc: arguments.length };
          }
        };
        var node = React.createElement('div', {className: 'app'}, 'hi', 3);
        var out = node.type + ':' + node.props.className + ':' + node.argc;
    """
    interp = _exec(code)

    assert interp.global_env.get("out") == "div:app:4"


def test_vue_style_render_helper_and_spread_props_snippet_runs():
    code = """
        var _ctx = { msg: 'hello' };
        function _toDisplayString(v){ return String(v); }
        var props = { id: 'a' };
        var vnode = { props: { ...props, class: 'hero' } };
        var out = _toDisplayString(_ctx.msg) + ':' + vnode.props.id + ':' + vnode.props.class;
    """
    interp = _exec(code)

    assert interp.global_env.get("out") == "hello:a:hero"


def test_react_style_commonjs_bundle_wrapper_runs():
    code = """
        var __modules = {
          1: function(module, exports) { exports.answer = 42; }
        };
        var __cache = {};
        function __require(id) {
          if (__cache[id]) return __cache[id].exports;
          var module = __cache[id] = {exports:{}};
          __modules[id](module, module.exports);
          return module.exports;
        }
        var out = __require(1).answer;
    """
    interp = _exec(code)

    assert interp.global_env.get("out") == 42


def test_jquery_style_selector_script_can_mutate_dom_nodes():
    html = """
        <html>
          <body>
            <ul>
              <li class="item">a</li>
              <li class="item">b</li>
            </ul>
            <script>
              var $ = function(sel){ return document.querySelectorAll(sel); };
              var items = $('.item');
              items[0].classList.add('ready');
              items[1].setAttribute('data-role', 'second');
            </script>
          </body>
        </html>
    """
    _display_list, _page_height, document = _pipeline_offline(html)

    items = _find_all_by_class(document, "item")
    assert len(items) == 2
    assert "ready" in items[0].attributes.get("class", "").split()
    assert items[1].attributes.get("data-role") == "second"


def test_react_style_factory_script_can_render_markup_into_dom():
    html = """
        <html>
          <body>
            <div id="app"></div>
            <script>
              var React = {
                createElement: function(type, props, child) {
                  return '<' + type + ' class="' + props.className + '">' + child + '</' + type + '>';
                }
              };
              document.getElementById('app').innerHTML =
                React.createElement('div', {className: 'hero'}, 'hello');
            </script>
          </body>
        </html>
    """
    _display_list, _page_height, document = _pipeline_offline(html)

    app = _find_by_attr(document, "id", "app")
    hero = _find_by_attr(document, "class", "hero")
    assert app is not None
    assert hero is not None
    assert hero.parent is app


def test_vue_style_list_render_script_can_expand_children():
    html = """
        <html>
          <body>
            <ul id="list"></ul>
            <script>
              var items = ['a', 'b', 'c'];
              var html = items
                .map(function(v, i){ return '<li class="item">' + i + ':' + v + '</li>'; })
                .join('');
              document.querySelector('#list').innerHTML = html;
            </script>
          </body>
        </html>
    """
    _display_list, _page_height, document = _pipeline_offline(html)

    items = _find_all_by_class(document, "item")
    assert len(items) == 3
    assert items[0].box.y < items[1].box.y < items[2].box.y


def test_jquery_style_function_objects_can_hold_plugin_properties():
    code = """
        function jQuery(){}
        jQuery.fn = {};
        jQuery.expando = 'render';
        var out = jQuery.expando;
    """
    interp = _exec(code)

    assert interp.global_env.get("out") == "render"


def test_vue_react_style_object_assign_merge_helper_exists():
    code = """
        var props = Object.assign({ id: 'a' }, { class: 'hero' });
        var out = props.class;
    """
    interp = _exec(code)

    assert interp.global_env.get("out") == "hero"


def test_react_style_transpiled_helper_can_mount_component_markup():
    html = """
        <html>
          <body>
            <div id="app"></div>
            <script>
              function App(props) {
                return '<main id="' + props.id + '" class="' + props.className + '">' +
                  App.displayName +
                  '</main>';
              }
              App.displayName = 'ReactApp';
              var props = Object.assign({ id: 'root' }, { className: 'shell' });
              document.getElementById('app').innerHTML = App.call(null, props);
            </script>
          </body>
        </html>
    """
    _display_list, _page_height, document = _pipeline_offline(html)

    app = _find_by_attr(document, "id", "app")
    root = _find_by_attr(document, "id", "root")
    assert app is not None
    assert root is not None
    assert root.parent is app
    assert root.attributes.get("class") == "shell"


def test_vue_style_compiled_list_helper_can_spread_items_and_bind_context():
    html = """
        <html>
          <body>
            <ul id="list"></ul>
            <script>
              function renderList(source, renderItem) {
                return source.map(function(item, index) {
                  return renderItem.call({ prefix: 'item' }, item, index);
                }).join('');
              }
              var state = { items: ['a', 'b'] };
              var extra = ['c'];
              var merged = [...state.items, ...extra];
              var attrs = { ...{ class: 'entry' }, 'data-count': merged.length };
              document.getElementById('list').innerHTML = renderList(merged, function(item, index) {
                return '<li class="' + attrs.class + '">' + this.prefix + '-' + index + ':' + item + '</li>';
              });
            </script>
          </body>
        </html>
    """
    _display_list, _page_height, document = _pipeline_offline(html)

    items = _find_all_by_class(document, "entry")
    assert [item.children[0].data for item in items] == [
        "item-0:a",
        "item-1:b",
        "item-2:c",
    ]
