"""DOM/BOM API bindings for JavaScript (stub — to be implemented in Phase 5)."""

class DOMBinding:
    """Binds JavaScript to the DOM tree."""
    def __init__(self, document, interpreter):
        self.document = document
        self.interpreter = interpreter

    def setup(self) -> None:
        """Set up document, window, console objects in JS scope."""
        pass
