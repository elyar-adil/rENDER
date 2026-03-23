"""HTML5 tokenizer + tree builder for the rENDER browser engine.

This is a simplified but robust HTML5-style state-machine parser.
"""

from html.dom import Document, Element, Text, Comment
from html.entities import decode_entities

# ---------------------------------------------------------------------------
# Tokenizer states
# ---------------------------------------------------------------------------
DATA = 'DATA'
TAG_OPEN = 'TAG_OPEN'
END_TAG_OPEN = 'END_TAG_OPEN'
TAG_NAME = 'TAG_NAME'
BEFORE_ATTR_NAME = 'BEFORE_ATTR_NAME'
ATTR_NAME = 'ATTR_NAME'
BEFORE_ATTR_VALUE = 'BEFORE_ATTR_VALUE'
ATTR_VALUE_DOUBLE_QUOTED = 'ATTR_VALUE_DOUBLE_QUOTED'
ATTR_VALUE_SINGLE_QUOTED = 'ATTR_VALUE_SINGLE_QUOTED'
ATTR_VALUE_UNQUOTED = 'ATTR_VALUE_UNQUOTED'
AFTER_ATTR_VALUE = 'AFTER_ATTR_VALUE'
SELF_CLOSING_START_TAG = 'SELF_CLOSING_START_TAG'
BOGUS_COMMENT = 'BOGUS_COMMENT'
MARKUP_DECLARATION_OPEN = 'MARKUP_DECLARATION_OPEN'
COMMENT_START = 'COMMENT_START'
COMMENT = 'COMMENT'
COMMENT_END_DASH = 'COMMENT_END_DASH'
COMMENT_END = 'COMMENT_END'
RAWTEXT = 'RAWTEXT'


# ---------------------------------------------------------------------------
# Token classes
# ---------------------------------------------------------------------------

class DoctypeToken:
    __slots__ = ('name',)

    def __init__(self, name: str = ''):
        self.name = name

    def __repr__(self):
        return f'DoctypeToken({self.name!r})'


class StartTagToken:
    __slots__ = ('tag_name', 'attributes', 'self_closing')

    def __init__(self, tag_name: str, attributes: dict = None, self_closing: bool = False):
        self.tag_name = tag_name.lower()
        self.attributes = attributes if attributes is not None else {}
        self.self_closing = self_closing

    def __repr__(self):
        return f'StartTagToken({self.tag_name!r}, sc={self.self_closing})'


class EndTagToken:
    __slots__ = ('tag_name',)

    def __init__(self, tag_name: str):
        self.tag_name = tag_name.lower()

    def __repr__(self):
        return f'EndTagToken({self.tag_name!r})'


class CharacterToken:
    __slots__ = ('char',)

    def __init__(self, char: str):
        self.char = char

    def __repr__(self):
        return f'CharacterToken({self.char!r})'


class CommentToken:
    __slots__ = ('data',)

    def __init__(self, data: str):
        self.data = data

    def __repr__(self):
        return f'CommentToken({self.data[:20]!r})'


class EOFToken:
    def __repr__(self):
        return 'EOFToken'


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

VOID_ELEMENTS = frozenset({
    'area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input',
    'link', 'meta', 'param', 'source', 'track', 'wbr',
})

RAW_TEXT_ELEMENTS = frozenset({'script', 'style'})

# Tags that have optional closing tags (simplified set)
OPTIONAL_CLOSE = {
    'li': frozenset({'li'}),
    'dt': frozenset({'dd', 'dt', 'dl'}),
    'dd': frozenset({'dd', 'dt', 'dl'}),
    'p': frozenset({
        'address', 'article', 'aside', 'blockquote', 'details', 'dir',
        'div', 'dl', 'fieldset', 'figcaption', 'figure', 'footer', 'form',
        'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'header', 'hgroup', 'hr',
        'main', 'menu', 'nav', 'ol', 'p', 'pre', 'section', 'summary',
        'table', 'ul',
    }),
    'thead': frozenset({'tbody', 'tfoot'}),
    'tbody': frozenset({'tbody', 'tfoot'}),
    'tfoot': frozenset({'tbody'}),
    'tr': frozenset({'tr', 'thead', 'tbody', 'tfoot'}),
    'th': frozenset({'td', 'th', 'tr', 'thead', 'tbody', 'tfoot'}),
    'td': frozenset({'td', 'th', 'tr', 'thead', 'tbody', 'tfoot'}),
    'colgroup': frozenset({'colgroup'}),
    'option': frozenset({'option', 'optgroup', 'select'}),
    'optgroup': frozenset({'optgroup', 'select'}),
    'rp': frozenset({'rp', 'rt'}),
    'rt': frozenset({'rp', 'rt'}),
}


# ---------------------------------------------------------------------------
# Tokenizer
# ---------------------------------------------------------------------------

class Tokenizer:
    """Produce a stream of tokens from an HTML string."""

    def __init__(self, html: str):
        self._html = html
        self._pos = 0
        self._state = DATA
        self._tokens = []

    def tokenize(self):
        """Tokenize entire input and return list of tokens."""
        html = self._html
        n = len(html)
        pos = 0

        while pos < n:
            state = self._state

            # ------- DATA state -------
            if state == DATA:
                if html[pos] == '<':
                    self._state = TAG_OPEN
                    pos += 1
                elif html[pos] == '&':
                    # Collect the entity
                    end = pos + 1
                    while end < n and html[end] not in (';', '<', ' ', '\t', '\n', '\r') and end - pos < 32:
                        end += 1
                    if end < n and html[end] == ';':
                        end += 1
                    chunk = decode_entities(html[pos:end])
                    for ch in chunk:
                        self._tokens.append(CharacterToken(ch))
                    pos = end
                else:
                    # Collect run of plain text
                    start = pos
                    while pos < n and html[pos] not in ('<', '&'):
                        pos += 1
                    for ch in html[start:pos]:
                        self._tokens.append(CharacterToken(ch))

            # ------- TAG_OPEN state -------
            elif state == TAG_OPEN:
                if pos >= n:
                    self._tokens.append(CharacterToken('<'))
                    self._state = DATA
                    break
                ch = html[pos]
                if ch == '!':
                    self._state = MARKUP_DECLARATION_OPEN
                    pos += 1
                elif ch == '/':
                    self._state = END_TAG_OPEN
                    pos += 1
                elif ch.isalpha():
                    self._state = TAG_NAME
                    # don't advance; TAG_NAME will consume
                else:
                    # Emit '<' as character, go back to DATA
                    self._tokens.append(CharacterToken('<'))
                    self._state = DATA

            # ------- MARKUP_DECLARATION_OPEN -------
            elif state == MARKUP_DECLARATION_OPEN:
                rest = html[pos:]
                if rest.startswith('--'):
                    # Comment
                    pos += 2
                    self._state = COMMENT_START
                elif rest.lower().startswith('doctype'):
                    pos += 7
                    # Skip to >
                    end = html.find('>', pos)
                    if end == -1:
                        end = n
                    name = html[pos:end].strip()
                    self._tokens.append(DoctypeToken(name))
                    pos = end + 1 if end < n else n
                    self._state = DATA
                elif rest.startswith('[CDATA['):
                    # Treat as bogus comment
                    pos += 7
                    self._state = BOGUS_COMMENT
                else:
                    self._state = BOGUS_COMMENT

            # ------- BOGUS_COMMENT -------
            elif state == BOGUS_COMMENT:
                end = html.find('>', pos)
                if end == -1:
                    end = n
                data = html[pos:end]
                self._tokens.append(CommentToken(data))
                pos = end + 1 if end < n else n
                self._state = DATA

            # ------- COMMENT states -------
            elif state == COMMENT_START:
                end = html.find('-->', pos)
                if end == -1:
                    data = html[pos:]
                    pos = n
                else:
                    data = html[pos:end]
                    pos = end + 3
                self._tokens.append(CommentToken(data))
                self._state = DATA

            # ------- END_TAG_OPEN -------
            elif state == END_TAG_OPEN:
                if pos >= n:
                    self._tokens.append(CharacterToken('<'))
                    self._tokens.append(CharacterToken('/'))
                    self._state = DATA
                    break
                ch = html[pos]
                if ch.isalpha() or ch == '_':
                    self._state = TAG_NAME
                    # We need to track that we are in an end tag
                    # We'll reuse TAG_NAME but mark it
                    tag_start = pos
                    while pos < n and html[pos] not in ('>', ' ', '\t', '\n', '\r', '/'):
                        pos += 1
                    tag_name = html[tag_start:pos].strip().lower()
                    # Skip to >
                    end = html.find('>', pos)
                    if end == -1:
                        end = n
                    pos = end + 1 if end < n else n
                    self._tokens.append(EndTagToken(tag_name))
                    self._state = DATA
                elif ch == '>':
                    pos += 1
                    self._state = DATA
                else:
                    self._state = BOGUS_COMMENT

            # ------- TAG_NAME (start tag) -------
            elif state == TAG_NAME:
                tag_start = pos
                while pos < n and html[pos] not in ('>', ' ', '\t', '\n', '\r', '/'):
                    pos += 1
                tag_name = html[tag_start:pos].strip().lower()
                if not tag_name:
                    # Nothing; skip to >
                    end = html.find('>', pos)
                    if end == -1:
                        end = n
                    pos = end + 1 if end < n else n
                    self._state = DATA
                    continue

                # Skip whitespace
                while pos < n and html[pos] in (' ', '\t', '\n', '\r'):
                    pos += 1

                # Parse attributes
                attrs = {}
                self_closing = False
                while pos < n and html[pos] != '>':
                    if html[pos] == '/':
                        self_closing = True
                        pos += 1
                        if pos < n and html[pos] == '>':
                            break
                        continue

                    # Attribute name
                    attr_start = pos
                    while pos < n and html[pos] not in ('=', '>', '/', ' ', '\t', '\n', '\r'):
                        pos += 1
                    attr_name = html[attr_start:pos].strip().lower()

                    # Skip whitespace
                    while pos < n and html[pos] in (' ', '\t', '\n', '\r'):
                        pos += 1

                    if pos < n and html[pos] == '=':
                        pos += 1  # skip '='
                        # Skip whitespace
                        while pos < n and html[pos] in (' ', '\t', '\n', '\r'):
                            pos += 1

                        if pos < n and html[pos] == '"':
                            pos += 1
                            val_start = pos
                            while pos < n and html[pos] != '"':
                                pos += 1
                            val = html[val_start:pos]
                            if pos < n:
                                pos += 1  # skip closing "
                        elif pos < n and html[pos] == "'":
                            pos += 1
                            val_start = pos
                            while pos < n and html[pos] != "'":
                                pos += 1
                            val = html[val_start:pos]
                            if pos < n:
                                pos += 1  # skip closing '
                        else:
                            # Unquoted
                            val_start = pos
                            while pos < n and html[pos] not in ('>', ' ', '\t', '\n', '\r', '"', "'", '`', '=', '<'):
                                pos += 1
                            val = html[val_start:pos]

                        if attr_name:
                            attrs[attr_name] = decode_entities(val)
                    else:
                        # Boolean attribute
                        if attr_name:
                            attrs[attr_name] = ''

                    # Skip whitespace
                    while pos < n and html[pos] in (' ', '\t', '\n', '\r'):
                        pos += 1

                # Skip '>'
                if pos < n and html[pos] == '>':
                    pos += 1

                self._tokens.append(StartTagToken(tag_name, attrs, self_closing))
                self._state = DATA

                # Enter RAWTEXT mode immediately for script/style
                if tag_name in RAW_TEXT_ELEMENTS and not self_closing:
                    close_pat = f'</{tag_name}'
                    # Find closing tag (case-insensitive)
                    search_pos = pos
                    close_pos = -1
                    while search_pos < n:
                        idx = html.lower().find(close_pat, search_pos)
                        if idx == -1:
                            close_pos = n
                            break
                        # Verify it's followed by whitespace or '>'
                        after = idx + len(close_pat)
                        if after >= n or html[after] in ('>', ' ', '\t', '\n', '\r', '/'):
                            close_pos = idx
                            break
                        search_pos = idx + 1
                    if close_pos == -1:
                        close_pos = n
                    # Emit raw content as character tokens
                    raw_content = html[pos:close_pos]
                    for ch in raw_content:
                        self._tokens.append(CharacterToken(ch))
                    pos = close_pos
                    # Consume the end tag if present
                    if pos < n and html[pos:pos+2] == '</':
                        end_gt = html.find('>', pos)
                        if end_gt == -1:
                            end_gt = n - 1
                        self._tokens.append(EndTagToken(tag_name))
                        pos = end_gt + 1

            else:
                # Unknown state - advance
                pos += 1
                self._state = DATA

        self._tokens.append(EOFToken())
        return self._tokens


# ---------------------------------------------------------------------------
# Tree builder
# ---------------------------------------------------------------------------

class TreeBuilder:
    """Build a DOM tree from a list of tokens."""

    def __init__(self):
        self.document = Document()
        self.open_elements = [self.document]
        self._html_element = None
        self._head_element = None
        self._body_element = None
        self._in_rawtext = False
        self._rawtext_tag = None
        self._pending_text = []

    @property
    def current_node(self):
        return self.open_elements[-1]

    def _flush_text(self):
        if self._pending_text:
            text = ''.join(self._pending_text)
            self._pending_text = []
            if text.strip() or (text and not isinstance(self.current_node, Document)):
                node = Text(text)
                node.parent = self.current_node
                self.current_node.children.append(node)

    def _ensure_html_body(self):
        """Ensure <html> and <body> elements exist on the stack."""
        if self._html_element is None:
            html_el = Element('html')
            html_el.parent = self.document
            self.document.children.append(html_el)
            self.open_elements.append(html_el)
            self._html_element = html_el

        if self._body_element is None:
            body_el = Element('body')
            body_el.parent = self._html_element
            self._html_element.children.append(body_el)
            self.open_elements.append(body_el)
            self._body_element = body_el

    def _process_start_tag(self, token: StartTagToken):
        tag = token.tag_name

        # Handle optional close tags: auto-close current open element if needed
        if self.open_elements:
            current_tag = getattr(self.current_node, 'tag', None)
            if current_tag in OPTIONAL_CLOSE:
                closers = OPTIONAL_CLOSE[current_tag]
                if tag in closers:
                    self.open_elements.pop()

        # Ensure html/body exist for body content.
        # 'html', 'head', and 'body' are handled by dedicated elif branches below
        # and must NOT trigger _ensure_html_body() nor fall through to element creation.
        if tag not in ('html', 'head', 'body', 'meta', 'title', 'link', 'script', 'style', 'base', 'noscript'):
            self._ensure_html_body()
        elif tag == 'html':
            if self._html_element is None:
                el = Element(tag, token.attributes)
                el.parent = self.document
                self.document.children.append(el)
                self.open_elements.append(el)
                self._html_element = el
                return
            else:
                # Already have html element; update attributes
                for k, v in token.attributes.items():
                    if k not in self._html_element.attributes:
                        self._html_element.attributes[k] = v
                return
        elif tag == 'head':
            if self._head_element is None:
                if self._html_element is None:
                    self._process_start_tag(StartTagToken('html'))
                el = Element(tag, token.attributes)
                el.parent = self._html_element
                self._html_element.children.append(el)
                self.open_elements.append(el)
                self._head_element = el
            return
        elif tag == 'body':
            if self._body_element is None:
                if self._html_element is None:
                    self._process_start_tag(StartTagToken('html'))
                el = Element(tag, token.attributes)
                el.parent = self._html_element
                self._html_element.children.append(el)
                self.open_elements.append(el)
                self._body_element = el
            return

        el = Element(tag, token.attributes)
        el.parent = self.current_node
        self.current_node.children.append(el)

        if tag in VOID_ELEMENTS or token.self_closing:
            # Void elements don't go on the stack
            return

        self.open_elements.append(el)

        # Raw text mode for script/style
        if tag in RAW_TEXT_ELEMENTS:
            self._in_rawtext = True
            self._rawtext_tag = tag

    def _process_end_tag(self, token: EndTagToken):
        tag = token.tag_name

        if self._in_rawtext:
            if tag == self._rawtext_tag:
                self._in_rawtext = False
                self._rawtext_tag = None
                self._flush_text()
                # Pop the element
                for i in range(len(self.open_elements) - 1, 0, -1):
                    if getattr(self.open_elements[i], 'tag', None) == tag:
                        self.open_elements.pop(i)
                        break
            else:
                # Inside rawtext, end tags for other elements are just text
                self._pending_text.append(f'</{tag}>')
            return

        self._flush_text()

        # Find the matching open element
        for i in range(len(self.open_elements) - 1, 0, -1):
            node = self.open_elements[i]
            if getattr(node, 'tag', None) == tag:
                # Pop everything up to and including this element
                self.open_elements = self.open_elements[:i]
                return

        # No matching open element found - ignore the end tag

    def process_token(self, token):
        if isinstance(token, EOFToken):
            self._flush_text()
            return

        if isinstance(token, DoctypeToken):
            return

        if isinstance(token, CommentToken):
            self._flush_text()
            node = Comment(token.data)
            node.parent = self.current_node
            self.current_node.children.append(node)
            return

        if isinstance(token, CharacterToken):
            if self._in_rawtext:
                self._pending_text.append(token.char)
            else:
                self._pending_text.append(token.char)
            return

        if isinstance(token, StartTagToken):
            self._flush_text()
            self._process_start_tag(token)
            return

        if isinstance(token, EndTagToken):
            self._process_end_tag(token)
            return

    def build(self, tokens):
        for token in tokens:
            self.process_token(token)
        return self.document


# ---------------------------------------------------------------------------
# Raw-text aware tokenization for script/style
# ---------------------------------------------------------------------------

def _tokenize_with_rawtext(html: str):
    """Tokenize HTML, handling raw text elements (script/style) specially."""
    base_tokenizer = Tokenizer(html)
    tokens = base_tokenizer.tokenize()

    # Post-process: for script/style, merge character tokens between start/end tags
    result = []
    i = 0
    while i < len(tokens):
        tok = tokens[i]
        if isinstance(tok, StartTagToken) and tok.tag_name in RAW_TEXT_ELEMENTS:
            result.append(tok)
            i += 1
            # Collect all characters until matching end tag
            raw_chars = []
            close_tag = f'</{tok.tag_name}'
            # Find the closing tag in tokens
            while i < len(tokens):
                t = tokens[i]
                if isinstance(t, EndTagToken) and t.tag_name == tok.tag_name:
                    break
                if isinstance(t, CharacterToken):
                    raw_chars.append(t.char)
                elif isinstance(t, EOFToken):
                    break
                i += 1
            raw_text = ''.join(raw_chars)
            if raw_text:
                for ch in raw_text:
                    result.append(CharacterToken(ch))
            # The EndTagToken at position i will be appended in the next iteration
        else:
            result.append(tok)
            i += 1

    return result


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def parse(html: str) -> Document:
    """Parse an HTML string and return a Document node."""
    tokenizer = Tokenizer(html)
    tokens = tokenizer.tokenize()
    builder = TreeBuilder()
    return builder.build(tokens)
