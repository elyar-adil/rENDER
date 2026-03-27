"""AST node type for the JavaScript parser."""
from js.types import _UNDEF  # re-exported so callers can do: from js.ast import _UNDEF


class ASTNode:
    """Generic AST node with attribute-style access into data dict."""
    __slots__ = ('type', 'data')

    def __init__(self, type_: str, **kwargs):
        self.type = type_
        self.data = kwargs

    def __getattr__(self, name):
        if name in ('type', 'data'):
            raise AttributeError(name)
        try:
            return self.data[name]
        except KeyError:
            raise AttributeError(name)

    def __repr__(self):
        return f'AST({self.type})'


def _node(type_: str, **kw) -> ASTNode:
    return ASTNode(type_, **kw)
