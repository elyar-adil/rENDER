"""DOM node classes for the rENDER browser engine."""

class Node:
    def __init__(self):
        self.children = []
        self.parent = None
        self.node_type = ''  # 'document', 'element', 'text', 'comment'

    def __repr__(self):
        return f'<{self.__class__.__name__} type={self.node_type!r}>'


class Document(Node):
    def __init__(self):
        super().__init__()
        self.node_type = 'document'
        # Legacy compat: layout engine checks d.type == "ROOT"
        self.type = 'ROOT'

    def __repr__(self):
        return '<Document>'


class Element(Node):
    def __init__(self, tag: str, attributes: dict = None):
        super().__init__()
        self.node_type = 'element'
        self.type = 'ELEM'         # legacy compat
        self.tag = tag.lower()
        self.attributes = attributes if attributes is not None else {}
        self.attr = self.attributes   # legacy alias
        self.style = {}              # computed style dict (set by css/cascade.py)
        self.box = None              # set by layout engine

    def get(self, attr: str, default=None):
        return self.attributes.get(attr, default)

    def __repr__(self):
        attrs = ' '.join(f'{k}={v!r}' for k, v in list(self.attributes.items())[:3])
        return f'<Element <{self.tag} {attrs}>>'


class Text(Node):
    def __init__(self, data: str):
        super().__init__()
        self.node_type = 'text'
        self.type = 'TEXT'   # legacy compat
        self.data = data
        self.content = data  # legacy alias used by old style.py

    def __repr__(self):
        snippet = self.data[:30].replace('\n', '\\n')
        return f'<Text {snippet!r}>'


class Comment(Node):
    def __init__(self, data: str):
        super().__init__()
        self.node_type = 'comment'
        self.type = 'COMMENT'
        self.data = data

    def __repr__(self):
        return f'<Comment {self.data[:20]!r}>'
