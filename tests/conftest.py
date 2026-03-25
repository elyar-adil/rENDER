"""Shared test fixtures and helpers for integration tests."""
import os
import sys

# Ensure project root is on path
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# Force offscreen Qt platform for headless testing
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

import pytest
from PyQt6.QtWidgets import QApplication

# Singleton QApplication required by Qt
_app = None


@pytest.fixture(autouse=True, scope='session')
def qt_app():
    """Ensure a QApplication exists for font metrics."""
    global _app
    if _app is None:
        _app = QApplication.instance() or QApplication([])
    return _app
