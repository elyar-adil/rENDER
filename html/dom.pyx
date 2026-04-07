"""DOM node classes for the rENDER browser engine."""

from typing import Any


class Node:
    children: list
    parent: Any
    node_type: str

    def __init__(self) -> None:
        self.children = []
        self.parent = None
        self.node_type = ''  # 'document', 'element', 'text', 'comment'

    def __repr__(self) -> str:
        return f'<{self.__class__.__name__} type={self.node_type!r}>'


class Document(Node):
    def __init__(self) -> None:
        super().__init__()
        self.node_type = 'document'

    def __repr__(self) -> str:
        return '<Document>'


class Element(Node):
    tag: str
    attributes: dict[str, str]
    style: dict[str, Any]
    css_vars: dict[str, str]
    box: Any
    line_boxes: list[Any]
    image: Any
    natural_width: int
    natural_height: int
    background_image: Any
    _pseudo_rules: dict[str, Any]
    _abs_children: list[Any]

    def __init__(self, tag: str, attributes: dict[str, str] | None = None) -> None:
        super().__init__()
        self.node_type = 'element'
        self.tag = tag.lower()
        self.attributes = attributes if attributes is not None else {}
        self.style: dict[str, Any] = {}        # computed style dict (set by css/cascade.py)
        self.css_vars: dict[str, str] = {}     # CSS custom property values (set by cascade)
        self.box = None              # BoxModel, set by layout engine
        self.line_boxes: list[Any] = []   # inline line boxes (set by inline layout)
        self.image = None            # platform image object for <img> (set by ImageLoader)
        self.natural_width: int = 0  # intrinsic image width in px
        self.natural_height: int = 0 # intrinsic image height in px
        self.background_image = None # platform image for CSS background-image
        # Internal: pseudo-element rules collected during cascade
        self._pseudo_rules: dict[str, Any] = {}
        # Internal: absolute-positioned children deferred by block layout
        self._abs_children: list[Any] = []

    def get(self, attr: str, default: Any = None) -> Any:
        return self.attributes.get(attr, default)

    def __repr__(self) -> str:
        attrs = ' '.join(f'{k}={v!r}' for k, v in list(self.attributes.items())[:3])
        return f'<Element <{self.tag} {attrs}>>'


class Text(Node):
    data: str

    def __init__(self, data: str) -> None:
        super().__init__()
        self.node_type = 'text'
        self.data = data

    def __repr__(self) -> str:
        snippet = self.data[:30].replace('\n', '\\n')
        return f'<Text {snippet!r}>'


class Comment(Node):
    data: str

    def __init__(self, data: str) -> None:
        super().__init__()
        self.node_type = 'comment'
        self.data = data

    def __repr__(self) -> str:
        return f'<Comment {self.data[:20]!r}>'
