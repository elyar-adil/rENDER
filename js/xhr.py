import logging
_logger = logging.getLogger(__name__)
"""XMLHttpRequest bridge for rENDER JavaScript engine.

Provides a synchronous XHR implementation for JS scripts.
"""
from js.interpreter import JSObject, JSArray, _UNDEF, _to_str


class XMLHttpRequest(JSObject):
    """Minimal synchronous XMLHttpRequest implementation."""

    def __init__(self):
        super().__init__()
        self['readyState'] = 0
        self['status'] = 0
        self['statusText'] = ''
        self['responseText'] = ''
        self['responseXML'] = None
        self['response'] = ''
        self['responseType'] = ''
        self['onreadystatechange'] = None
        self['onload'] = None
        self['onerror'] = None
        self['timeout'] = 0
        self['withCredentials'] = False

        self._method = 'GET'
        self._url = ''
        self._headers = {}
        self._response_headers = {}

        self['open'] = lambda method, url, *a: self._open(method, url)
        self['send'] = lambda data=None: self._send(data)
        self['setRequestHeader'] = lambda k, v: self._headers.__setitem__(_to_str(k), _to_str(v))
        self['getResponseHeader'] = lambda k: self._response_headers.get(_to_str(k).lower(), None)
        self['getAllResponseHeaders'] = lambda: '\r\n'.join(f'{k}: {v}' for k, v in self._response_headers.items())
        self['abort'] = lambda: None
        self['addEventListener'] = lambda *a: None

    def _open(self, method, url):
        self._method = _to_str(method).upper()
        self._url = _to_str(url)
        self['readyState'] = 1

    def _send(self, data=None):
        try:
            from network.http import fetch as fetch_text
            text, final_url = fetch_text(self._url)
            self['readyState'] = 4
            self['status'] = 200
            self['statusText'] = 'OK'
            self['responseText'] = text
            self['response'] = text
        except Exception as e:
            self['readyState'] = 4
            self['status'] = 0
            self['statusText'] = str(e)
            self['responseText'] = ''
            self['response'] = ''

        # Fire callbacks
        cb = self.get('onreadystatechange')
        if cb and callable(cb):
            try:
                cb()
            except Exception as _exc:
                _logger.debug("Ignored: %s", _exc)
        cb = self.get('onload')
        if cb and callable(cb):
            try:
                cb()
            except Exception as _exc:
                _logger.debug("Ignored: %s", _exc)


def create_xhr():
    """Factory function for new XMLHttpRequest()."""
    return XMLHttpRequest()
