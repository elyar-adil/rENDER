import unittest

from paser import html


class HtmlParserTests(unittest.TestCase):
    def test_parse_nested_and_attributes(self):
        doc = """
        <html><body>
            <div id=container class='main box'>
                <img src="/a.png" alt='x' />
                <p data-v=1>Hello <b>world</b></p>
            </div>
        </body></html>
        """
        root = html.parse(doc)
        html_node = root.children[0]
        body = [c for c in html_node.children if getattr(c, "tag", "") == "body"][0]
        div = [c for c in body.children if getattr(c, "tag", "") == "div"][0]

        self.assertEqual(div.attr["id"], "container")
        self.assertEqual(div.attr["class"], "main box")
        img = [c for c in div.children if getattr(c, "tag", "") == "img"][0]
        self.assertEqual(img.attr["src"], "/a.png")

    def test_parse_complex_webpage_like_document(self):
        doc = """
        <!doctype html>
        <html lang="zh-CN">
        <head>
            <meta charset="utf-8">
            <meta name="viewport" content="width=device-width, initial-scale=1">
            <title>Complex Page</title>
            <script>
                window.__INITIAL_STATE__ = {"items": [1, 2, 3], "nested": {"ok": true}};
            </script>
            <style>
                .grid{display:grid;grid-template-columns:1fr 3fr;gap:12px;}
                #app .card > h2{font-size:18px;}
            </style>
        </head>
        <body>
            <header role="banner"><nav><a href="/">Home</a><a href="/docs">Docs</a></nav></header>
            <main id="app" data-env='prod'>
                <section class="grid">
                    <article class="card"><h2>Title A</h2><p>Paragraph A.</p></article>
                    <article class="card"><h2>Title B</h2><p>Paragraph B.</p></article>
                </section>
                <img src="hero.png">
                <input type="text" disabled>
                <br>
                <!-- analytics comment should be stripped -->
                <svg viewBox="0 0 10 10"><path d="M0 0L10 10"></path></svg>
            </main>
            <footer><small>copyright</small></footer>
        </body>
        </html>
        """
        root = html.parse(doc)
        self.assertEqual(root.type, "ROOT")
        self.assertTrue(any(getattr(c, "tag", "") == "html" for c in root.children))

    def test_unclosed_tags_do_not_crash(self):
        doc = "<div><span>hello<div>world"
        root = html.parse(doc)
        self.assertEqual(root.type, "ROOT")
        self.assertGreater(len(root.children), 0)


if __name__ == "__main__":
    unittest.main()
