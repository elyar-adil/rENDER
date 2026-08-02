"""Integration tests for the persistent interactive page session."""

from engine import PageSession
from html.dom import Element
from rendering.display_list import PushTransform


def _find_by_id(document, element_id: str):
    stack = [document]
    while stack:
        node = stack.pop()
        if isinstance(node, Element) and node.attributes.get('id') == element_id:
            return node
        stack.extend(getattr(node, 'children', []))
    raise AssertionError(f'element #{element_id} not found')


def _center(node) -> tuple[float, float]:
    rect = node.box.border_rect
    return rect.x + rect.width / 2, rect.y + rect.height / 2


def test_click_dispatches_to_dom_and_relayouts_invalidated_page():
    session = PageSession(
        """
        <html><body>
          <button id="button" style="display:block;width:100px;height:40px">Grow</button>
          <script>
            var button = document.getElementById('button');
            button.addEventListener('click', function() {
              button.setAttribute('data-clicked', 'yes');
              button.style.width = '180px';
            });
          </script>
        </body></html>
        """,
        base_url='https://example.com/',
    ).load()
    button = _find_by_id(session.document, 'button')

    update = session.click(*_center(button))

    assert update.changed is True
    assert button.attributes['data-clicked'] == 'yes'
    assert button.attributes['style'].find('width: 180px') >= 0
    assert button.box.content_width == 180.0


def test_document_and_window_receive_bubbled_clicks():
    session = PageSession(
        """
        <html><body>
          <button id="button" style="display:block;width:100px;height:40px">Click</button>
          <script>
            var button = document.getElementById('button');
            document.addEventListener('click', function() {
              button.setAttribute('data-document-click', 'yes');
            });
            window.addEventListener('click', function() {
              button.setAttribute('data-window-click', 'yes');
            });
          </script>
        </body></html>
        """,
        base_url='https://example.com/',
    ).load()
    button = _find_by_id(session.document, 'button')

    session.click(*_center(button))

    assert button.attributes['data-document-click'] == 'yes'
    assert button.attributes['data-window-click'] == 'yes'


def test_prevent_default_blocks_link_navigation():
    session = PageSession(
        """
        <html><body>
          <a id="link" href="/next" style="display:block;width:100px;height:30px">Next</a>
          <script>
            document.getElementById('link').addEventListener('click', function(event) {
              event.preventDefault();
            });
          </script>
        </body></html>
        """,
        base_url='https://example.com/current',
    ).load()
    rect, _url, _node = session.link_targets[0]

    update = session.click(rect.x + 1, rect.y + 1)

    assert update.navigation_url is None


def test_page_zoom_uses_actual_viewport_and_scales_link_hit_testing():
    session = PageSession(
        """
        <html><body style="min-width:1000px">
          <a id="link" href="/next"
             style="display:block;width:100px;height:30px">Next</a>
          <script>
            function setScale() {
              document.body.style.zoom = window.innerWidth / 1000;
            }
            setScale();
            window.addEventListener('resize', setScale);
          </script>
        </body></html>
        """,
        base_url='https://example.com/current',
        viewport_width=500,
        viewport_height=400,
    ).load()

    body = next(
        node for node in session.document.children[0].children
        if getattr(node, 'tag', '') == 'body'
    )
    link = _find_by_id(session.document, 'link')
    transform = next(
        command for command in session.display_list
        if isinstance(command, PushTransform)
    )
    rect, url, _node = session.link_targets[0]

    assert body.style['zoom'] == '0.5'
    assert session.document._page_zoom == 0.5
    assert transform.scale_x == 0.5
    assert rect.width == link.line_boxes[0].items[0].width * 0.5
    assert session.click(rect.x + 1, rect.y + 1).navigation_url == url

    session.resize(800, 400)

    assert body.style['zoom'] == '0.8'
    assert session.document._page_zoom == 0.8


def test_native_get_form_accepts_text_and_submits_on_enter():
    session = PageSession(
        """
        <html><body>
          <form action="/search" method="get">
            <input id="query" name="q" type="search" placeholder="Search">
            <input name="source" type="hidden" value="home">
            <button type="submit">Go</button>
          </form>
        </body></html>
        """,
        base_url='https://example.com/start',
        viewport_width=600,
        viewport_height=300,
    ).load()
    query = _find_by_id(session.document, 'query')

    focus_update = session.click(*_center(query))
    assert focus_update.changed
    assert session.focused_element is query
    assert query._focused

    input_update = session.key_input('', '标准 web')
    assert input_update.changed
    assert query.attributes['value'] == '标准 web'

    submit_update = session.key_input('enter')
    assert submit_update.navigation_url == (
        'https://example.com/search?q=%E6%A0%87%E5%87%86+web&source=home'
    )


def test_textarea_accepts_newlines_and_serializes_its_live_value():
    session = PageSession(
        """
        <style>
          textarea { display:block; width:240px; height:60px; }
          button { display:block; width:80px; height:30px; }
        </style>
        <form action="/notes" method="get">
          <textarea id="notes" name="q">seed</textarea>
          <button id="submit" type="submit">Save</button>
        </form>
        """,
        base_url='https://example.com/start',
        viewport_width=500,
        viewport_height=240,
    ).load()
    notes = _find_by_id(session.document, 'notes')
    submit = _find_by_id(session.document, 'submit')

    session.click(*_center(notes))
    session.key_input('end')
    session.key_input('enter')
    session.key_input('', 'next')
    update = session.click(*_center(submit))

    assert update.navigation_url == 'https://example.com/notes?q=seed%0Anext'

    from rendering.display_list import DrawInput

    drawn = next(
        command for command in session.display_list
        if isinstance(command, DrawInput) and command.multiline
    )
    assert drawn.text == 'seed\nnext'


def test_password_control_paints_masked_value():
    session = PageSession(
        '<input id="password" type="password" value="secret">',
        viewport_width=400,
        viewport_height=120,
    ).load()

    from rendering.display_list import DrawInput

    drawn = next(command for command in session.display_list if isinstance(command, DrawInput))
    assert drawn.text == '••••••'


def test_delayed_timer_can_advance_and_trigger_relayout():
    session = PageSession(
        """
        <html><body>
          <div id="box" style="width:100px;height:20px"></div>
          <script>
            var box = document.getElementById('box');
            setTimeout(function() { box.style.width = '200px'; }, 50);
          </script>
        </body></html>
        """,
        base_url='https://example.com/',
    ).load()
    box = _find_by_id(session.document, 'box')

    before = session.advance_time(49)
    after = session.advance_time(1)

    assert before.changed is False
    assert after.changed is True
    assert box.box.content_width == 200.0
