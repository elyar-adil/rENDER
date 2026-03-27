"""Link extraction helpers for the layout engine."""


def _extract_links(document, base_url: str = '') -> list:
    """Walk the DOM and collect (Rect, url) for all clickable <a href> elements."""
    links = []
    _collect_links(document, base_url, links)
    return links


def _collect_links(node, base_url: str, links: list) -> None:
    from html.dom import Element
    from network.http import resolve_url
    if not isinstance(node, Element):
        for child in node.children:
            _collect_links(child, base_url, links)
        return

    # Collect from inline line_boxes
    if hasattr(node, 'line_boxes') and node.line_boxes:
        link_map = {}   # origin_node -> [items]
        for line in node.line_boxes:
            for item in line.items:
                ln = getattr(item, 'origin_node', None)
                if ln is not None:
                    link_map.setdefault(ln, []).append(item)
        for a_node, items in link_map.items():
            href = a_node.attributes.get('href', '').strip()
            if not href or href.startswith('#'):
                continue
            try:
                url = resolve_url(base_url, href) if base_url else href
            except Exception:
                url = href
            if items:
                x1 = min(it.x for it in items)
                y1 = min(it.y for it in items)
                x2 = max(it.x + it.width for it in items)
                y2 = max(it.y + it.height for it in items)
                from layout.box import Rect
                links.append((Rect(x1, y1, x2 - x1, y2 - y1), url))

    # Block-level <a> with its own box
    if node.tag == 'a' and hasattr(node, 'box') and node.box is not None:
        href = node.attributes.get('href', '').strip()
        if href and not href.startswith('#'):
            try:
                url = resolve_url(base_url, href) if base_url else href
            except Exception:
                url = href
            links.append((node.box.border_rect, url))

    for child in node.children:
        _collect_links(child, base_url, links)
