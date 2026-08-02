"""Regression tests for the Qt browser controller's load lifecycle."""

from unittest.mock import MagicMock

from backend.qt.app import (
    Browser,
    _LoadSpec,
    _TabState,
    normalize_address_input,
)
from backend.qt.home import HOME_HTML, HOME_URL
from backend.qt.painter import BrowserWidget, apply_browser_theme


def _bare_browser() -> Browser:
    browser = Browser.__new__(Browser)
    state = _TabState(tab_id=1)
    browser._tabs = {1: state}
    browser._closing_tabs = {}
    browser._tab_order = [1]
    browser._active_tab_id = 1
    browser._load_to_tab = {}
    browser._next_load_id = 1
    browser._test_state = state
    browser._win = MagicMock()
    return browser


def test_address_input_distinguishes_websites_files_and_searches():
    assert normalize_address_input('google.com') == 'https://google.com'
    assert normalize_address_input('example.com:8443/docs?q=1') == (
        'https://example.com:8443/docs?q=1'
    )
    assert normalize_address_input('localhost:8000') == 'https://localhost:8000'
    assert normalize_address_input('//www.qq.com/news') == 'https://www.qq.com/news'
    assert normalize_address_input('HTTPS://EXAMPLE.COM') == 'HTTPS://EXAMPLE.COM'
    assert normalize_address_input('README.md') == 'README.md'
    assert normalize_address_input('网页标准 测试') == (
        'https://www.baidu.com/s?wd=%E7%BD%91%E9%A1%B5%E6%A0%87%E5%87%86+%E6%B5%8B%E8%AF%95'
    )


def test_browser_navigation_normalizes_address_bar_input():
    browser = _bare_browser()
    browser._start_load = MagicMock()

    browser.navigate('google.com')

    browser._start_load.assert_called_once_with(
        browser._test_state,
        'https://google.com',
        record_history=True,
    )


def test_new_navigation_cancels_and_queues_behind_active_worker():
    browser = _bare_browser()
    state = browser._test_state
    state.thread = MagicMock()
    state.thread.isRunning.return_value = True
    state.loader = MagicMock()
    spec = _LoadSpec(load_id=2, tab_id=1, target='https://example.com/new')

    browser._queue_load(state, spec)

    assert state.pending_load == spec
    state.loader.cancel.assert_called_once_with()
    state.thread.quit.assert_not_called()
    state.thread.wait.assert_not_called()


def test_different_tabs_can_launch_workers_in_parallel():
    browser = _bare_browser()
    first = browser._test_state
    first.thread = MagicMock()
    first.thread.isRunning.return_value = True
    second = _TabState(tab_id=2)
    browser._tabs[2] = second
    browser._tab_order.append(2)
    browser._launch_load = MagicMock()
    spec = _LoadSpec(load_id=2, tab_id=2, target='https://example.com/two')

    browser._queue_load(second, spec)

    browser._launch_load.assert_called_once_with(second, spec)
    assert first.thread.isRunning()


def test_pending_navigation_starts_only_after_active_thread_finishes():
    browser = _bare_browser()
    state = browser._test_state
    state.active_load_id = 1
    state.thread = MagicMock()
    state.loader = MagicMock()
    state.load_specs[1] = _LoadSpec(load_id=1, tab_id=1, target='https://example.com/old')
    browser._load_to_tab[1] = 1
    pending = _LoadSpec(load_id=2, tab_id=1, target='https://example.com/new')
    state.pending_load = pending
    browser._launch_load = MagicMock()

    browser._on_thread_finished(1)

    assert state.active_load_id is None
    assert state.thread is None
    assert state.loader is None
    assert state.pending_load is None
    browser._launch_load.assert_called_once_with(state, pending)


def test_stale_load_result_is_ignored():
    browser = _bare_browser()
    state = browser._test_state
    state.latest_load_id = 2
    browser._load_to_tab[1] = 1

    browser._on_done(
        1,
        object(),
        600,
        'Old',
        'https://example.com/old',
        [],
        '<p>old</p>',
        object(),
    )

    browser._win.set_display_list.assert_not_called()


def test_stale_deferred_resource_update_is_ignored():
    browser = _bare_browser()
    state = browser._test_state
    state.latest_load_id = 2
    browser._load_to_tab[1] = 1

    browser._on_updated(1, object(), 700, [], 'Old page')

    browser._win.set_display_list.assert_not_called()
    browser._win.canvas.set_links.assert_not_called()


def test_current_deferred_resource_update_repaints_page():
    browser = _bare_browser()
    state = browser._test_state
    state.latest_load_id = 2
    browser._load_to_tab[2] = 1
    display_list = object()
    links = [object()]

    browser._on_updated(2, display_list, 700, links, 'Updated page')

    browser._win.set_display_list.assert_called_once_with(
        display_list,
        page_height=700,
        title='Updated page',
    )
    browser._win.canvas.set_links.assert_called_once_with(links)


def test_hydration_completion_only_clears_current_load_status():
    browser = _bare_browser()
    state = browser._test_state
    state.latest_load_id = 2
    browser._load_to_tab.update({1: 1, 2: 1})

    browser._on_hydrated(1)
    browser._win.set_status.assert_not_called()

    browser._on_hydrated(2)
    browser._win.set_status.assert_called_once_with('')


def test_browser_theme_styles_native_context_menus(qt_app):
    from PyQt6.QtGui import QPalette

    apply_browser_theme(qt_app)

    assert 'QMenu' in qt_app.styleSheet()
    assert qt_app.palette().color(QPalette.ColorRole.Window).name() == '#202124'


def test_browser_shell_uses_integrated_compact_chrome(qt_app):
    from PyQt6.QtCore import Qt

    apply_browser_theme(qt_app)
    widget = BrowserWidget()

    assert widget._tab_strip.height() == 42
    assert widget.tab_bar.count() == 1
    assert widget.tab_bar.isMovable()
    assert widget.tab_bar.tabsClosable()
    assert not widget.tab_bar.drawBase()
    assert widget._toolbar.height() == 50
    assert widget._addr_pill.height() == 36
    assert widget.back_btn.text() == ''
    assert widget.forward_btn.text() == ''
    assert widget.reload_btn.text() == ''
    assert widget.home_btn.text() == ''
    assert widget._status_bar.parent() is widget.scroll_area.viewport()
    if widget._custom_frame:
        assert widget.windowFlags() & Qt.WindowType.FramelessWindowHint
        assert widget._window_min_btn is not None
        assert widget._window_max_btn is not None
        assert widget._window_close_btn is not None

    widget.close()


def test_tab_strip_supports_new_close_switch_and_move(qt_app):
    apply_browser_theme(qt_app)
    widget = BrowserWidget()
    changed = MagicMock()
    closed = MagicMock()
    moved = MagicMock()
    widget.tab_changed_callback = changed
    widget.tab_close_callback = closed
    widget.tab_moved_callback = moved

    widget.add_tab('Second')
    widget.add_tab('Third')
    assert widget.tab_bar.count() == 3
    assert widget.current_tab_index() == 2
    assert changed.called

    widget.tab_bar.moveTab(2, 0)
    moved.assert_called_with(2, 0)

    widget._on_tab_close_requested(0)
    closed.assert_called_once_with(0)
    widget.close()


def test_tab_titles_escape_qt_mnemonic_underlines(qt_app):
    apply_browser_theme(qt_app)
    widget = BrowserWidget()

    widget.set_tab_title(0, 'News & Finance')

    assert widget.tab_bar.tabText(0) == 'News && Finance'
    assert widget.tab_bar.tabToolTip(0) == 'News & Finance'
    widget.close()


def test_loading_and_error_states_use_distinct_chrome(qt_app):
    apply_browser_theme(qt_app)
    widget = BrowserWidget()

    widget.set_status('Loading...')
    assert not widget._load_progress.isHidden()
    assert widget._status_bar.isHidden()
    assert widget._spin_timer.isActive()
    assert widget._tab_icon._loading
    assert widget.reload_btn._icon_name == 'stop'

    widget.set_status('Error: HTTP Error 404: NOT FOUND')
    assert widget._load_progress.isHidden()
    assert not widget._status_bar.isHidden()
    assert not widget._spin_timer.isActive()
    assert not widget._tab_icon._loading
    assert widget.reload_btn._icon_name == 'reload'

    widget.close()


def test_stop_button_cancels_instead_of_reloading(qt_app):
    apply_browser_theme(qt_app)
    widget = BrowserWidget()
    stop = MagicMock()
    navigate = MagicMock()
    widget.stop_callback = stop
    widget.navigate_callback = navigate
    widget.address_bar.setText('https://example.com')

    widget.set_status('Loading...')
    widget._on_reload()

    stop.assert_called_once_with()
    navigate.assert_not_called()
    widget.close()


def test_canvas_forwards_text_input_keys_to_page_controller(qt_app):
    from PyQt6.QtTest import QTest

    apply_browser_theme(qt_app)
    widget = BrowserWidget()
    callback = MagicMock()
    widget.page_key_callback = callback
    widget.show()
    widget.canvas.setFocus()

    QTest.keyClicks(widget.canvas, 'abc')

    assert ''.join(call.args[1] for call in callback.call_args_list) == 'abc'
    widget.close()


def test_home_button_uses_browser_home_callback(qt_app):
    apply_browser_theme(qt_app)
    widget = BrowserWidget()
    go_home = MagicMock()
    widget.home_callback = go_home

    widget._on_home()

    go_home.assert_called_once_with()
    assert widget.home_btn._icon_name == 'home'
    widget.close()


def test_browser_home_navigation_uses_internal_url():
    browser = _bare_browser()
    browser._start_load = MagicMock()

    browser.go_home()

    browser._start_load.assert_called_once_with(
        browser._test_state,
        HOME_URL,
        record_history=True,
    )


def test_home_page_is_standard_html_with_dom_links():
    from engine import PageSession, _extract_title

    session = PageSession(
        HOME_HTML,
        base_url=HOME_URL,
        viewport_width=1100,
        viewport_height=720,
        defer_noncritical=True,
    ).load()

    assert _extract_title(session.document) == 'rENDER 主页'
    targets = {url for _rect, url in session.links}
    assert 'https://www.qq.com/' in targets
    assert 'https://www.hao123.com/' in targets
    assert 'https://www.tom.com/' in targets
