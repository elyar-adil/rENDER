"""JavaScript AST parser (stub — to be implemented in Phase 5)."""

class ASTNode:
    def __init__(self, type_: str, **kwargs):
        self.type = type_
        self.__dict__.update(kwargs)

class Parser:
    """JavaScript parser stub."""
    def __init__(self, tokens: list):
        self.tokens = tokens

    def parse(self) -> ASTNode:
        return ASTNode('Program', body=[])
