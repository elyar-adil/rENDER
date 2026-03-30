"""XMLHttpRequest bridge for the rENDER JavaScript engine."""

from __future__ import annotations

import logging

from js.event_loop import get_event_loop
from js.interpreter import JSObject, _UNDEF, _to_bool, _to_str

_logger = logging.getLogger(__name__)


class XMLHttpRequest(JSObject):
    """Minimal XHR implementation with network task-queue completion."""

    def __init__(self, *, interp=None, base_url: str = ''):
        super().__init__()
        self._interp = interp
        self._base_url = base_url
        self._method = 'GET'
        self._url = ''
        self._async = True
        self._headers: dict[str, str] = {}
        self._response_headers: dict[str, str] = {}
        self._aborted = False

        self['UNSENT'] = 0
        self['OPENED'] = 1
        self['HEADERS_RECEIVED'] = 2
        self['LOADING'] = 3
        self['DONE'] = 4
        self['readyState'] = 0
        self['status'] = 0
        self['statusText'] = ''
        self['responseText'] = ''
        self['responseXML'] = None
        self['response'] = ''
        self['responseType'] = ''
        self['responseURL'] = ''
        self['onreadystatechange'] = None
        self['onload'] = None
        self['onerror'] = None
        self['timeout'] = 0
        self['withCredentials'] = False

        self['open'] = lambda method, url, async_=_UNDEF, *a: self._open(method, url, async_, *a)
        self['send'] = lambda data=None: self._send(data)
        self['setRequestHeader'] = lambda k, v: self._headers.__setitem__(_to_str(k), _to_str(v))
        self['getResponseHeader'] = lambda k: self._response_headers.get(_to_str(k).lower(), None)
        self['getAllResponseHeaders'] = lambda: '\r\n'.join(
            f'{k}: {v}' for k, v in self._response_headers.items()
        )
        self['abort'] = self._abort
        self['addEventListener'] = lambda *a: None

    def _open(self, method, url, async_=_UNDEF, *_args):
        self._method = _to_str(method).upper()
        self._url = _to_str(url)
        self._async = True if async_ is _UNDEF else _to_bool(async_)
        self._aborted = False
        self['readyState'] = 1

    def _send(self, data=None):
        if self._async:
            get_event_loop().enqueue_task('network', lambda: self._perform_request(data))
        else:
            self._perform_request(data)

    def _abort(self):
        self._aborted = True
        return None

    def _perform_request(self, data=None):
        if self._aborted:
            return
        try:
            from network.http import fetch as fetch_text, resolve_url

            resolved = resolve_url(self._base_url, self._url) if self._base_url else self._url
            text, final_url = fetch_text(resolved)
            self['readyState'] = 4
            self['status'] = 200
            self['statusText'] = 'OK'
            self['responseText'] = text
            self['response'] = text
            self['responseURL'] = final_url
            self._fire_handler('onreadystatechange')
            self._fire_handler('onload')
        except Exception as exc:
            self['readyState'] = 4
            self['status'] = 0
            self['statusText'] = str(exc)
            self['responseText'] = ''
            self['response'] = ''
            self._fire_handler('onreadystatechange')
            self._fire_handler('onerror')

    def _fire_handler(self, name: str) -> None:
        callback = self.get(name)
        if callback in (None, _UNDEF):
            return
        try:
            if self._interp is not None:
                self._interp._call_value(callback, [], self)
            elif callable(callback):
                callback()
        except Exception as exc:
            _logger.debug('Ignored XHR %s callback error: %s', name, exc)


def create_xhr(*, interp=None, base_url: str = ''):
    """Factory function used by the JS runtime."""
    return XMLHttpRequest(interp=interp, base_url=base_url)
