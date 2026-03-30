"""Regression tests for the event-loop/render invalidation baseline."""

from __future__ import annotations

import unittest
from unittest.mock import patch

import engine
from html.parser import parse as parse_html
from js.dom_api import DOMBinding
from js.event_loop import get_event_loop, reset_event_loop
from js.interpreter import Interpreter
from js.lexer import Lexer
from js.parser import Parser
from js.xhr import create_xhr
from rendering.invalidation import InvalidationGraph


def _exec_dom(code: str, html: str = '<html><body><div id="box"></div></body></html>'):
    reset_event_loop()
    interp = Interpreter()
    document = parse_html(html)
    invalidation_graph = InvalidationGraph()
    binding = DOMBinding(document, interp, invalidation_graph=invalidation_graph)
    binding.setup()
    ast = Parser(Lexer(code).tokenize()).parse()
    interp.execute(ast)
    return interp, document, invalidation_graph


class TestEventLoopArchitecture(unittest.TestCase):
    def test_rendering_opportunity_runs_after_microtasks(self):
        loop = reset_event_loop()
        log: list[str] = []

        loop.set_render_callback(lambda: log.append('render'))
        loop.enqueue_microtask(lambda: log.append('microtask'))
        loop.request_render()
        loop.run_until_idle()

        self.assertEqual(log, ['microtask', 'render'])

    def test_mutation_observer_childlist_is_delivered_in_microtask(self):
        interp, _document, _graph = _exec_dom(
            """
            var log = [];
            var box = document.getElementById('box');
            var observer = new MutationObserver(function(records) {
                log.push('observer');
            });
            observer.observe(box, { childList: true });
            box.appendChild(document.createElement('span'));
            log.push('after-append');
            """
        )

        self.assertEqual(list(interp.global_env.get('log')), ['after-append'])

        get_event_loop().run_until_idle()
        self.assertEqual(list(interp.global_env.get('log')), ['after-append', 'observer'])

    def test_set_attribute_emits_attribute_mutation_record(self):
        interp, _document, _graph = _exec_dom(
            """
            var attrName = '';
            var box = document.getElementById('box');
            var observer = new MutationObserver(function(records) {
                attrName = records[0].attributeName;
            });
            observer.observe(box, { attributes: true });
            box.setAttribute('data-state', 'ready');
            """
        )

        self.assertEqual(interp.global_env.get('attrName'), '')
        get_event_loop().run_until_idle()
        self.assertEqual(interp.global_env.get('attrName'), 'data-state')

    def test_classlist_mutation_emits_attribute_record(self):
        interp, _document, _graph = _exec_dom(
            """
            var attrName = '';
            var box = document.getElementById('box');
            var observer = new MutationObserver(function(records) {
                attrName = records[0].attributeName;
            });
            observer.observe(box, { attributes: true });
            box.classList.add('active');
            """
        )

        self.assertEqual(interp.global_env.get('attrName'), '')
        get_event_loop().run_until_idle()
        self.assertEqual(interp.global_env.get('attrName'), 'class')

    def test_style_mutation_requests_render_with_style_dirty_snapshot(self):
        reset_event_loop()
        interp = Interpreter()
        document = parse_html('<html><body><div id="box"></div></body></html>')
        graph = InvalidationGraph()
        binding = DOMBinding(document, interp, invalidation_graph=graph)
        binding.setup()
        snapshots = []
        get_event_loop().set_render_callback(lambda: snapshots.append(graph.consume()))

        box = binding._get_by_id('box')
        box['style']['width'] = '120px'
        get_event_loop().run_until_idle()

        self.assertEqual(len(snapshots), 1)
        snapshot = snapshots[0]
        self.assertTrue(snapshot.style_dirty)
        self.assertTrue(snapshot.layout_dirty)
        self.assertTrue(snapshot.paint_dirty)
        self.assertEqual(snapshot.records[0].reason, 'inline-style:width')

    def test_fetch_resolves_from_network_task_queue(self):
        reset_event_loop()
        interp = Interpreter()
        engine._install_fetch(interp, 'https://example.com/base/')

        ast = Parser(Lexer(
            """
            var log = [];
            fetch('/data.json').then(function(response) {
                log.push('fetch');
            });
            log.push('sync');
            """
        ).tokenize()).parse()

        with patch('network.http.fetch', return_value=('{"ok":true}', 'https://example.com/data.json')):
            interp.execute(ast)
            self.assertEqual(list(interp.global_env.get('log')), ['sync'])
            get_event_loop().run_until_idle()

        self.assertEqual(list(interp.global_env.get('log')), ['sync', 'fetch'])

    def test_request_animation_frame_runs_after_microtasks(self):
        reset_event_loop()
        interp = Interpreter()
        ast = Parser(Lexer(
            """
            var log = [];
            Promise.resolve().then(function() { log.push('microtask'); });
            requestAnimationFrame(function() { log.push('raf'); });
            log.push('sync');
            """
        ).tokenize()).parse()

        interp.execute(ast)
        self.assertEqual(list(interp.global_env.get('log')), ['sync'])
        get_event_loop().run_until_idle()
        self.assertEqual(list(interp.global_env.get('log')), ['sync', 'microtask', 'raf'])

    def test_cancel_animation_frame_prevents_callback(self):
        reset_event_loop()
        interp = Interpreter()
        ast = Parser(Lexer(
            """
            var log = [];
            var token = requestAnimationFrame(function() { log.push('raf'); });
            cancelAnimationFrame(token);
            """
        ).tokenize()).parse()

        interp.execute(ast)
        get_event_loop().run_until_idle()
        self.assertEqual(list(interp.global_env.get('log')), [])

    def test_xhr_completion_runs_from_network_task_queue(self):
        reset_event_loop()
        interp = Interpreter()
        xhr_ctor = lambda: create_xhr(interp=interp, base_url='https://example.com/base/')
        interp.global_env.define('XMLHttpRequest', xhr_ctor)
        window = interp.global_env.get('window')
        if isinstance(window, dict):
            window['XMLHttpRequest'] = xhr_ctor

        ast = Parser(Lexer(
            """
            var log = [];
            var xhr = new XMLHttpRequest();
            xhr.open('GET', '/data.json');
            xhr.onreadystatechange = function() {
                if (xhr.readyState === 4) {
                    log.push('xhr');
                }
            };
            xhr.send();
            log.push('sync');
            """
        ).tokenize()).parse()

        with patch('network.http.fetch', return_value=('{"ok":true}', 'https://example.com/data.json')):
            interp.execute(ast)
            self.assertEqual(list(interp.global_env.get('log')), ['sync'])
            get_event_loop().run_until_idle()

        self.assertEqual(list(interp.global_env.get('log')), ['sync', 'xhr'])


if __name__ == '__main__':
    unittest.main()
