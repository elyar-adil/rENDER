"""JavaScript lexer (stub — to be implemented in Phase 5)."""

class Token:
    def __init__(self, type_: str, value=None, line: int = 0):
        self.type = type_
        self.value = value
        self.line = line
    def __repr__(self):
        return f'Token({self.type}, {self.value!r})'

class Lexer:
    """JavaScript lexer stub."""
    def __init__(self, source: str):
        self.source = source
        self.tokens = []

    def tokenize(self) -> list:
        return []
