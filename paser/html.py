import re


class DOMNode:
    """DOM Node Class"""

    def __init__(self):
        self.children = []
        self.type = ""


_VOID_TAGS = {
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link",
    "meta", "param", "source", "track", "wbr",
}


def __skip_whitespace(s, p):
    while p < len(s) and s[p].isspace():
        p += 1
    return p


def __parse_attr(attr_str):
    # supports: key="v", key='v', key=v, boolean-key
    attr = {}
    for key, v1, v2, v3 in re.findall(r'([:\w-]+)(?:\s*=\s*(?:"([^"]*)"|\'([^\']*)\'|([^\s"\'>/]+)))?', attr_str):
        value = v1 or v2 or v3 or ""
        attr[key] = value
    return attr


def __parse_children(node, html_lines, pos, stop_tag=None):
    while pos < len(html_lines):
        pos = __skip_whitespace(html_lines, pos)
        if pos >= len(html_lines):
            return pos

        if html_lines[pos] == '<':
            close_pos = html_lines.find('>', pos)
            if close_pos == -1:
                return len(html_lines)

            token = html_lines[pos + 1:close_pos].strip()
            pos = close_pos + 1

            if not token:
                continue

            if token.startswith('!'):
                # doctype/comment-like declarations already stripped in parse()
                continue

            if token.startswith('/'):
                tag_name = token[1:].strip().split()[0] if token[1:].strip() else ""
                if stop_tag is None or tag_name.lower() == stop_tag.lower():
                    return pos
                continue

            self_closing = token.endswith('/')
            if self_closing:
                token = token[:-1].strip()

            parts = token.split(None, 1)
            tag = parts[0]
            attr_text = parts[1] if len(parts) > 1 else ""

            child = DOMNode()
            child.type = "ELEM"
            child.tag = tag
            child.attr = __parse_attr(attr_text)
            child.parent = node
            node.children.append(child)

            if self_closing or tag.lower() in _VOID_TAGS:
                continue

            pos = __parse_children(child, html_lines, pos, stop_tag=tag)
        else:
            start = pos
            while pos < len(html_lines) and html_lines[pos] != '<':
                pos += 1

            text = html_lines[start:pos]
            if text:
                child = DOMNode()
                child.type = "TEXT"
                child.content = text
                child.parent = node
                node.children.append(child)

    return pos


def parse(html):
    # remove comments first
    html = re.sub(r"<!--.*?-->", "", html, flags=re.DOTALL)
    root = DOMNode()
    root.type = "ROOT"
    __parse_children(root, html, 0)
    root.parent = None
    return root
