import os
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import engine
from css.cascade import _pick_font_face_src, bind
from html.parser import parse as parse_html
from js.interpreter import Interpreter
from js.lexer import Lexer
from js.parser import Parser


class TestJSCompatibility(unittest.TestCase):
    def _exec(self, code: str) -> Interpreter:
        interp = Interpreter()
        ast = Parser(Lexer(code).tokenize()).parse()
        interp.execute(ast)
        return interp

    def test_parser_accepts_keyword_parameter_and_bare_for_in(self):
        code = "(function(window,undefined){var last='';for(v in {a:1,b:2}){last=v}window.loopResult=last;})(window)"
        interp = self._exec(code)
        window = interp.global_env.get('window')
        self.assertEqual(window['loopResult'], 'b')

    def test_parser_executes_arrow_optional_chain_and_spread(self):
        code = """
        var pick = (obj) => obj?.value ?? 5;
        var arr = [1, ...[2, 3]];
        var merged = {a: 1, ...{b: 2}};
        function sum(a, b, c) { return a + b + c; }
        var out = pick({value: 7}) + sum(...arr) + merged.b;
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('out'), 15)

    def test_parser_supports_empty_arrow_numeric_methods_and_scientific_notation(self):
        code = """
        var make = () => 1e3;
        var moduleMap = { 617(a, b) { return a + b; } };
        var out = make() + moduleMap[617](2, 3);
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('out'), 1005)

    def test_parser_and_runtime_support_destructured_params(self):
        code = """
        var fn = ([first], {value, ...rest}) => first + value + rest.extra;
        var out = fn([3], {value: 4, extra: 5});
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('out'), 12)

    def test_parser_supports_tagged_templates_and_bitwise_assignment(self):
        code = """
        var tag = function(x) { return x; };
        var out = 1;
        out |= 2;
        var tpl = tag`<div>${out}</div>`;
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('out'), 3)
        self.assertIn('<div>', interp.global_env.get('tpl'))

    def test_parser_supports_regex_classes_accessor_methods_and_await(self):
        code = """
        var obj = {
          get policy() { return 7; },
          set policy(v) { this.value = v; }
        };
        var re = /[/?]/g;
        var out = (await obj.policy) + ('a/b'.replace(re, '_').indexOf('_') >= 0 ? 1 : 0);
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('out'), 8)

    def test_parser_supports_async_arrow_and_new_target_checks(self):
        code = """
        var fn = async value => value + 1;
        var out = fn(2);
        var guard = (function(){ return new.target === undefined; })();
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('out'), 3)
        self.assertTrue(interp.global_env.get('guard'))

    def test_parser_supports_async_function_iife_and_async_object_method(self):
        code = """
        var obj = { async load(v) { return v + 2; } };
        var out = (async function(){ return obj.load(3); })();
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('out'), 5)

    def test_parser_supports_destructured_for_of(self):
        code = """
        var total = 0;
        for (const [a, b] of [[1, 2], [3, 4]]) total = total + a + b;
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('total'), 10)

    def test_parser_raises_on_syntax_error_instead_of_skipping_ahead(self):
        code = "if (true { window.after = 1; }"
        with self.assertRaises(SyntaxError):
            Parser(Lexer(code).tokenize()).parse()

    def test_request_animation_frame_defers_nested_frame(self):
        code = """
        var count = 0;
        function tick() {
          count = count + 1;
          requestAnimationFrame(tick);
        }
        requestAnimationFrame(tick);
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('count'), 1)

    def test_promise_then_runs_after_late_resolve(self):
        code = """
        var resolveLater = null;
        var result = 0;
        var chained = 0;
        var p = new Promise(function(resolve) { resolveLater = resolve; });
        p.then(function(value) { result = value; return value + 1; })
         .then(function(value) { chained = value; });
        resolveLater(41);
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('result'), 41)
        self.assertEqual(interp.global_env.get('chained'), 42)

    def test_runtime_supports_corejs_object_and_iterator_primitives(self):
        code = """
        var protoDepthSafe = Object.getPrototypeOf(Object.getPrototypeOf([1].keys())) !== undefined;
        var names = Object.getOwnPropertyNames(window);
        var hasDocument = names.indexOf('document') >= 0;
        var defined = {};
        Object.defineProperty(defined, 'hidden', { value: 7, enumerable: false });
        var desc = Object.getOwnPropertyDescriptor(defined, 'hidden');
        var out = [
          String(undefined),
          String(),
          protoDepthSafe,
          hasDocument,
          desc.value,
          desc.enumerable
        ].join(':');
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('out'), 'undefined::true:true:7:false')

    def test_set_and_map_prototype_methods_support_call(self):
        code = """
        var set = new Set();
        Set.prototype.add.call(set, 'x');
        var setValue = Set.prototype.values.call(set).next().value;
        var map = new Map();
        Map.prototype.set.call(map, 'k', 9);
        var mapEntry = Map.prototype.entries.call(map).next().value;
        var out = [
          Set.prototype.has.call(set, 'x'),
          setValue,
          mapEntry[0],
          mapEntry[1]
        ].join(':');
        """
        interp = self._exec(code)
        self.assertEqual(interp.global_env.get('out'), 'true:x:k:9')


class TestDOMCompatibility(unittest.TestCase):
    def test_remove_child_clears_parent_pointer(self):
        doc = parse_html('<html><body><div id="a"><span id="b"></span></div></body></html>')
        interp = Interpreter()
        from js.dom_api import DOMBinding
        binding = DOMBinding(doc, interp)
        binding.setup()

        parent = binding._get_by_id('a')
        child = binding._get_by_id('b')
        removed = parent['removeChild'](child)

        self.assertIs(removed, child)
        self.assertIsNone(child['parentNode'])
        self.assertEqual(len(parent['children']), 0)


class TestLayoutCompatibility(unittest.TestCase):
    def test_nested_inline_blocks_layout_without_recursion(self):
        html = """
        <html>
          <head>
            <style>
              .outer { display: inline-block; padding: 4px; border: 1px solid #000; }
              .inner { display: inline-block; padding: 2px; }
            </style>
          </head>
          <body>
            <div class="outer"><span class="inner">Hello</span></div>
          </body>
        </html>
        """
        display_list, height, doc = engine._pipeline(html, base_url='', viewport_width=320, viewport_height=200)
        self.assertGreater(height, 0)
        self.assertGreater(len(list(display_list)), 0)

        html_el = doc.children[0]
        body_el = next(child for child in html_el.children if getattr(child, 'tag', '') == 'body')
        outer = next(child for child in body_el.children if getattr(child, 'tag', '') == 'div')
        inner = next(child for child in outer.children if getattr(child, 'tag', '') == 'span')
        self.assertIsNotNone(outer.box)
        self.assertIsNotNone(inner.box)

    def test_absolute_percent_offset_and_auto_width_shrink_to_fit(self):
        html = """
        <html>
          <head>
            <style>
              html, body { margin: 0; padding: 0; }
              #stage { position: relative; width: 1000px; height: 200px; }
              #search { position: absolute; left: 50%; margin-left: -200px; min-width: 300px; }
              #search .logo { display: inline-block; width: 80px; }
              #search .toggle { display: inline-block; width: 38px; padding-left: 16px; padding-right: 10px; }
              #search form { display: inline-block; }
              #search input { width: 240px; margin-left: 20px; padding-left: 12px; padding-right: 18px; }
            </style>
          </head>
          <body>
            <div id="stage">
              <div id="search">
                <div class="logo">Logo</div>
                <a class="toggle">Web</a>
                <form><input type="text" value="query"></form>
              </div>
            </div>
          </body>
        </html>
        """
        _, _, doc = engine._pipeline(html, base_url='', viewport_width=1000, viewport_height=300)

        def _find_by_id(node, target):
            if getattr(node, 'attributes', {}).get('id') == target:
                return node
            for child in getattr(node, 'children', []):
                found = _find_by_id(child, target)
                if found is not None:
                    return found
            return None

        search = _find_by_id(doc, 'search')
        self.assertIsNotNone(search)
        self.assertIsNotNone(search.box)
        self.assertAlmostEqual(search.box.x, 300.0, delta=1.0)
        self.assertGreaterEqual(search.box.content_width, 300.0)
        self.assertLess(search.box.content_width, 500.0)

        form = next(child for child in search.children if getattr(child, 'tag', '') == 'form')
        self.assertIsNotNone(form.box)
        self.assertGreater(form.box.x, search.box.x)


class TestFontFaceSafety(unittest.TestCase):
    def test_pick_font_face_prefers_truetype_source(self):
        src = (
            "url('//cdn.example.com/font.eot') format('embedded-opentype'), "
            "url('//cdn.example.com/font.woff2') format('woff2'), "
            "url('//cdn.example.com/font.ttf') format('truetype')"
        )
        self.assertEqual(_pick_font_face_src(src), '//cdn.example.com/font.ttf')

    def test_bind_skips_font_face_registration_on_windows(self):
        doc = parse_html("""
        <html>
          <head>
            <style>
              @font-face {
                font-family: "Test";
                src: url("https://example.com/test.ttf") format("truetype");
              }
              body { font-family: "Test", sans-serif; }
            </style>
          </head>
          <body>Hello</body>
        </html>
        """)
        with patch('css.cascade.sys.platform', 'win32'):
            bind(doc, engine.UA_CSS, viewport_width=320, viewport_height=200)


if __name__ == '__main__':
    unittest.main()
