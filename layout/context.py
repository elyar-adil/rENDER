"""LayoutContext — shared state for a layout pass, and LayoutEngine ABC."""
from __future__ import annotations
from abc import ABC, abstractmethod
from layout.box import BoxModel


class LayoutContext:
    """Carries shared layout state (viewport size, float manager) through the
    layout tree.  Pass one instance down through the tree; call fork() when
    entering a new Block Formatting Context (e.g. a float or overflow:hidden
    element) so inner floats do not escape their BFC.
    """

    def __init__(self, viewport_width: int = 980, viewport_height: int = 600):
        self.viewport_width = viewport_width
        self.viewport_height = viewport_height
        from layout.float_manager import FloatManager
        self.float_mgr = FloatManager()
        self.absolute_nodes: list = []
        self.initial_containing_block = None
        self.absolute_containing_block = None
        self.absolute_containing_node = None

    def fork(self) -> LayoutContext:
        """Return a new context with a fresh FloatManager (new BFC)."""
        child = LayoutContext(self.viewport_width, self.viewport_height)
        child.absolute_nodes = self.absolute_nodes
        child.initial_containing_block = self.initial_containing_block
        child.absolute_containing_block = self.absolute_containing_block
        child.absolute_containing_node = self.absolute_containing_node
        return child

    def layout(self, node, container: BoxModel) -> BoxModel | None:
        """Dispatch to the appropriate layout engine and return the node's BoxModel.
        Sets node.box before returning.
        Returns None if node has display:none.
        """
        from layout._dispatch import layout_node
        return layout_node(node, container, self)


class LayoutEngine(ABC):
    """Abstract base for all CSS layout engines.

    Each subclass handles one value of the CSS `display` property (block,
    inline, flex, grid, table).  The public interface is a single method,
    `layout`, so engines are interchangeable (Liskov substitution) and new
    display types can be added without modifying callers (Open/Closed).
    """

    @abstractmethod
    def layout(self, node, container: BoxModel, ctx: LayoutContext) -> BoxModel:
        """Lay out *node* within *container* using shared *ctx*.

        Must:
        - Return a fully-positioned BoxModel for *node*.
        - Set box.x, box.y, box.content_width, box.content_height, box.margin,
          box.padding, box.border.
        - NOT call node.box = ... — the caller does that.
        - NOT modify ctx.float_mgr for floated descendants; call ctx.fork() first.
        """
