import logging
_logger = logging.getLogger(__name__)
"""CSS Selector parsing and matching.

Implements:
  - Type / universal selectors
  - Class, ID selectors
  - Attribute selectors ([attr], [attr=val], [attr~=val], [attr|=val],
                          [attr^=val], [attr$=val], [attr*=val])
  - Pseudo-classes: :first-child, :last-child, :nth-child(n),
                    :nth-of-type(n), :not(selector), :hover, :focus,
                    :link, :visited, :checked, :disabled, :enabled,
                    :root, :empty, :only-child, :only-of-type
  - Combinators: descendant ( ), child (>), adjacent sibling (+),
                 general sibling (~)
  - Comma-separated selector groups
"""

import re
import functools
from html.dom import Element
from css.utils import split_paren_aware as _split_by_comma_outside_parens


# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------

class SimpleSelector:
    """A single simple selector component."""
    __slots__ = ('tag', 'id', 'classes', 'attributes', 'pseudo_class',
                 'pseudo_element', 'negation')

    def __init__(self):
        self.tag            = None    # str | None  (None = universal '*')
        self.id             = None    # str | None
        self.classes        = []      # list[str]
        self.attributes     = []      # list[(name, op, value)]
        self.pseudo_class   = None    # str | None
        self.pseudo_element = None    # str | None
        self.negation       = None    # SimpleSelector | None  (for :not())

    def __repr__(self):
        parts = []
        if self.tag:
            parts.append(self.tag)
        if self.id:
            parts.append(f'#{self.id}')
        for c in self.classes:
            parts.append(f'.{c}')
        for a in self.attributes:
            parts.append(f'[{a}]')
        if self.pseudo_class:
            parts.append(f':{self.pseudo_class}')
        return f'SimpleSelector({"".join(parts)})'


class Combinator:
    DESCENDANT = ' '
    CHILD      = '>'
    ADJACENT   = '+'
    SIBLING    = '~'


class CompoundSelector:
    """One or more SimpleSelectors with no combinator between them."""
    __slots__ = ('simple_selectors',)

    def __init__(self, simple_selectors: list = None):
        self.simple_selectors = simple_selectors or []

    def __repr__(self):
        return f'CompoundSelector({self.simple_selectors})'


class ComplexSelector:
    """A sequence of CompoundSelectors joined by combinators.

    parts alternates: CompoundSelector, combinator_str, CompoundSelector, ...
    """
    __slots__ = ('parts',)

    def __init__(self, parts: list = None):
        self.parts = parts or []

    def __repr__(self):
        return f'ComplexSelector({self.parts})'


class SelectorGroup:
    """Comma-separated list of ComplexSelectors."""
    __slots__ = ('selectors',)

    def __init__(self, selectors: list = None):
        self.selectors = selectors or []

    @property
    def specificity(self) -> tuple:
        """Return max specificity across all selectors."""
        if not self.selectors:
            return (0, 0, 0)
        specs = [_complex_specificity(s) for s in self.selectors]
        return max(specs)

    def __repr__(self):
        return f'SelectorGroup({self.selectors})'


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

@functools.lru_cache(maxsize=4096)
def parse_selector(text: str) -> SelectorGroup:
    """Parse a CSS selector string (may contain commas). LRU-cached."""
    parser = _SelectorParser(text.strip())
    return parser.parse()


def matches(element, selector_text: str) -> bool:
    """Return True if *element* matches the selector string."""
    if not isinstance(element, Element):
        return False
    try:
        group = parse_selector(selector_text)  # cached parse
    except Exception:
        return False
    for complex_sel in group.selectors:
        if _matches_complex(element, complex_sel):
            return True
    return False


def get_pseudo_element(selector_text: str) -> str | None:
    """Return the pseudo-element name ('before', 'after') if the selector
    targets one, otherwise None."""
    try:
        group = parse_selector(selector_text)
    except Exception:
        return None
    for complex_sel in group.selectors:
        parts = complex_sel.parts
        if parts:
            last_compound = parts[-1]
            if isinstance(last_compound, CompoundSelector):
                for ss in last_compound.simple_selectors:
                    if ss.pseudo_element:
                        return ss.pseudo_element
    return None


@functools.lru_cache(maxsize=4096)
def specificity(selector_text: str) -> tuple:
    """Return (a, b, c) specificity tuple for the most specific selector. Cached."""
    try:
        group = parse_selector(selector_text)
        return group.specificity
    except Exception:
        return (0, 0, 0)


# ---------------------------------------------------------------------------
# Specificity calculation
# ---------------------------------------------------------------------------

def _simple_specificity(s: SimpleSelector) -> tuple:
    a = 1 if s.id else 0
    b = len(s.classes) + len(s.attributes)
    if s.pseudo_class and not s.pseudo_class.lower().startswith('not'):
        b += 1
    # :not() contributes the specificity of its argument, not itself
    if s.negation is not None:
        na, nb, nc = _simple_specificity(s.negation)
        a += na
        b += nb
        # nc handled below
        c_extra = nc
    else:
        c_extra = 0
    c = (1 if (s.tag and s.tag != '*') else 0) + c_extra
    if s.pseudo_element:
        c += 1
    return (a, b, c)


def _compound_specificity(cs: CompoundSelector) -> tuple:
    a = b = c = 0
    for ss in cs.simple_selectors:
        sa, sb, sc = _simple_specificity(ss)
        a += sa; b += sb; c += sc
    return (a, b, c)


def _complex_specificity(cs: ComplexSelector) -> tuple:
    a = b = c = 0
    for part in cs.parts:
        if isinstance(part, CompoundSelector):
            sa, sb, sc = _compound_specificity(part)
            a += sa; b += sb; c += sc
    return (a, b, c)


# ---------------------------------------------------------------------------
# Matching
# ---------------------------------------------------------------------------

def _matches_complex(element: Element, cs: ComplexSelector) -> bool:
    """Return True if element matches the complex selector."""
    parts = cs.parts
    if not parts:
        return False
    # Work backwards: parts[-1] is the rightmost compound selector
    # parts[-2] (if any) is a combinator, parts[-3] is the next compound, etc.
    return _match_complex_backwards(element, parts, len(parts) - 1)


def _match_complex_backwards(element, parts, idx):
    """Recursively match complex selector from right to left."""
    if idx < 0:
        return True
    part = parts[idx]
    if not isinstance(part, CompoundSelector):
        return False  # malformed
    if not _matches_compound(element, part):
        return False
    if idx == 0:
        return True
    # There's a combinator to the left
    combinator = parts[idx - 1]
    if idx - 1 < 0:
        return True

    if combinator == Combinator.DESCENDANT:
        # Any ancestor must match
        anc = element.parent
        while anc is not None:
            if isinstance(anc, Element):
                if _match_complex_backwards(anc, parts, idx - 2):
                    return True
            anc = getattr(anc, 'parent', None)
        return False

    elif combinator == Combinator.CHILD:
        parent = element.parent
        if parent is None or not isinstance(parent, Element):
            return False
        return _match_complex_backwards(parent, parts, idx - 2)

    elif combinator == Combinator.ADJACENT:
        sibling = _prev_element_sibling(element)
        if sibling is None:
            return False
        return _match_complex_backwards(sibling, parts, idx - 2)

    elif combinator == Combinator.SIBLING:
        sibling = _prev_element_sibling(element)
        while sibling is not None:
            if _match_complex_backwards(sibling, parts, idx - 2):
                return True
            sibling = _prev_element_sibling(sibling)
        return False

    return False


def _matches_compound(element: Element, cs: CompoundSelector) -> bool:
    return all(_matches_simple(element, ss) for ss in cs.simple_selectors)


def _matches_simple(element: Element, ss: SimpleSelector) -> bool:
    # Tag
    if ss.tag and ss.tag != '*':
        if element.tag != ss.tag:
            return False

    # ID
    if ss.id is not None:
        if element.attributes.get('id', '') != ss.id:
            return False

    # Classes
    elem_classes = set(element.attributes.get('class', '').split())
    for cls in ss.classes:
        if cls not in elem_classes:
            return False

    # Attributes
    for (attr_name, op, attr_val) in ss.attributes:
        if not _match_attribute(element, attr_name, op, attr_val):
            return False

    # Pseudo-class
    if ss.pseudo_class:
        if not _match_pseudo_class(element, ss.pseudo_class, ss.negation):
            return False

    return True


def _match_attribute(element: Element, name: str, op: str, value: str) -> bool:
    actual = element.attributes.get(name)
    if op == 'exists':
        return actual is not None
    if actual is None:
        return False
    if op == '=':
        return actual == value
    if op == '~=':
        return value in actual.split()
    if op == '|=':
        return actual == value or actual.startswith(value + '-')
    if op == '^=':
        return actual.startswith(value)
    if op == '$=':
        return actual.endswith(value)
    if op == '*=':
        return value in actual
    return False


def _match_pseudo_class(element: Element, pseudo: str, negation_sel=None) -> bool:
    pseudo_lower = pseudo.lower()

    # :not(selector) — supports comma-separated list of selectors
    if pseudo_lower == 'not' or pseudo_lower.startswith('not('):
        # negation_sel holds only the first arg (legacy); re-parse the full arg list
        # by extracting from pseudo_lower if it embeds the args
        if pseudo_lower.startswith('not(') and pseudo_lower.endswith(')'):
            arg_str = pseudo_lower[4:-1]
        elif negation_sel is not None:
            # Use the negation_sel directly (single-arg path)
            return not _matches_simple(element, negation_sel)
        else:
            return True
        # Check all comma-separated arguments — if element matches ANY, return False
        try:
            for frag in _split_by_comma_outside_parens(arg_str):
                frag = frag.strip()
                if frag and matches(element, frag):
                    return False
            return True
        except Exception:
            if negation_sel is not None:
                return not _matches_simple(element, negation_sel)
            return True

    # :has(selector) — matches if element has a descendant matching selector
    if pseudo_lower.startswith('has(') and pseudo_lower.endswith(')'):
        arg = pseudo_lower[4:-1]
        from html.dom import Element as _Element
        def _has_descendant(el, sel_text):
            for child in el.children:
                if isinstance(child, _Element):
                    if matches(child, sel_text):
                        return True
                    if _has_descendant(child, sel_text):
                        return True
            return False
        try:
            for frag in _split_by_comma_outside_parens(arg):
                frag = frag.strip()
                if frag and _has_descendant(element, frag):
                    return True
        except Exception as _exc:
            _logger.debug("Ignored: %s", _exc)
        return False

    # Structural pseudo-classes
    if pseudo_lower == 'root':
        from html.dom import Document
        return isinstance(element.parent, Document)

    if pseudo_lower == 'first-child':
        return _is_nth_child(element, 1)

    if pseudo_lower == 'last-child':
        return _is_last_child(element)

    if pseudo_lower == 'only-child':
        return _is_nth_child(element, 1) and _is_last_child(element)

    if pseudo_lower == 'empty':
        return not any(isinstance(c, Element) or
                       (hasattr(c, 'data') and c.data.strip())
                       for c in element.children)

    if pseudo_lower.startswith('nth-child(') and pseudo_lower.endswith(')'):
        arg = pseudo_lower[len('nth-child('):-1].strip()
        return _nth_child_matches(element, arg)

    if pseudo_lower.startswith('nth-of-type(') and pseudo_lower.endswith(')'):
        arg = pseudo_lower[len('nth-of-type('):-1].strip()
        return _nth_of_type_matches(element, arg)

    if pseudo_lower == 'first-of-type':
        return _nth_of_type_matches(element, '1')

    if pseudo_lower == 'last-of-type':
        return _is_last_of_type(element)

    if pseudo_lower == 'only-of-type':
        return _nth_of_type_matches(element, '1') and _is_last_of_type(element)

    # :is(A, B, C) — matches if element matches any selector in the list
    if pseudo_lower.startswith('is(') and pseudo_lower.endswith(')'):
        arg = pseudo_lower[3:-1]
        try:
            for sel_fragment in _split_by_comma_outside_parens(arg):
                sel_fragment = sel_fragment.strip()
                if sel_fragment and matches(element, sel_fragment):
                    return True
        except Exception as _exc:
            _logger.debug("Ignored: %s", _exc)
        return False

    # :where(A, B, C) — same matching as :is() but zero specificity
    if pseudo_lower.startswith('where(') and pseudo_lower.endswith(')'):
        arg = pseudo_lower[6:-1]
        try:
            for sel_fragment in _split_by_comma_outside_parens(arg):
                sel_fragment = sel_fragment.strip()
                if sel_fragment and matches(element, sel_fragment):
                    return True
        except Exception as _exc:
            _logger.debug("Ignored: %s", _exc)
        return False

    # Dynamic pseudo-classes — match based on element state
    if pseudo_lower == 'link':
        return (element.tag == 'a' and bool(element.attributes.get('href')))
    if pseudo_lower == 'visited':
        return False  # no browsing history
    if pseudo_lower in ('hover', 'focus', 'active', 'focus-within',
                        'focus-visible', 'target'):
        return False  # static rendering, no interaction state
    if pseudo_lower == 'checked':
        return 'checked' in element.attributes
    if pseudo_lower == 'disabled':
        return 'disabled' in element.attributes
    if pseudo_lower == 'enabled':
        return (element.tag in ('input', 'select', 'textarea', 'button')
                and 'disabled' not in element.attributes)
    if pseudo_lower == 'placeholder':
        return False

    return False


# ---- structural helpers ---------------------------------------------------

def _element_siblings(element: Element) -> list:
    parent = element.parent
    if parent is None:
        return [element]
    return [c for c in parent.children if isinstance(c, Element)]


def _is_nth_child(element: Element, n: int) -> bool:
    siblings = _element_siblings(element)
    return siblings.index(element) + 1 == n if element in siblings else False


def _is_last_child(element: Element) -> bool:
    siblings = _element_siblings(element)
    return siblings[-1] is element if siblings else False


def _nth_child_matches(element: Element, arg: str) -> bool:
    siblings = _element_siblings(element)
    if element not in siblings:
        return False
    idx = siblings.index(element) + 1  # 1-based
    return _eval_nth(arg, idx)


def _nth_of_type_matches(element: Element, arg: str) -> bool:
    parent = element.parent
    if parent is None:
        return _eval_nth(arg, 1)
    same_type = [c for c in parent.children
                 if isinstance(c, Element) and c.tag == element.tag]
    if element not in same_type:
        return False
    idx = same_type.index(element) + 1
    return _eval_nth(arg, idx)


def _is_last_of_type(element: Element) -> bool:
    parent = element.parent
    if parent is None:
        return True
    same_type = [c for c in parent.children
                 if isinstance(c, Element) and c.tag == element.tag]
    return same_type[-1] is element if same_type else False


def _eval_nth(arg: str, index: int) -> bool:
    """Evaluate :nth-child / :nth-of-type argument against index (1-based)."""
    arg = arg.strip().lower()
    if arg == 'odd':
        return index % 2 == 1
    if arg == 'even':
        return index % 2 == 0
    # Try plain integer
    try:
        return index == int(arg)
    except ValueError:
        pass
    # An+B form
    m = re.fullmatch(r'([+-]?\d*n)\s*([+-]\s*\d+)?', arg)
    if m:
        a_str = m.group(1)
        b_str = (m.group(2) or '+0').replace(' ', '')
        if a_str in ('n', '+n'):
            a = 1
        elif a_str == '-n':
            a = -1
        else:
            a = int(a_str.replace('n', '') or '1')
        b = int(b_str)
        if a == 0:
            return index == b
        remainder = index - b
        if remainder % a != 0:
            return False
        k = remainder // a
        return k >= 0
    return False


def _prev_element_sibling(element: Element):
    parent = element.parent
    if parent is None:
        return None
    prev = None
    for child in parent.children:
        if child is element:
            return prev
        if isinstance(child, Element):
            prev = child
    return None


# ---------------------------------------------------------------------------
# Selector parser
# ---------------------------------------------------------------------------

class _SelectorParser:
    def __init__(self, text: str):
        self._text = text
        self._pos  = 0

    def _remaining(self) -> str:
        return self._text[self._pos:]

    def _peek(self) -> str:
        return self._text[self._pos] if self._pos < len(self._text) else ''

    def _advance(self) -> str:
        c = self._text[self._pos]
        self._pos += 1
        return c

    def _skip_whitespace(self):
        while self._pos < len(self._text) and self._text[self._pos] in ' \t\n\r\f':
            self._pos += 1

    def _at_end(self) -> bool:
        return self._pos >= len(self._text)

    def parse(self) -> SelectorGroup:
        selectors = []
        while not self._at_end():
            prev_pos = self._pos
            sel = self._parse_complex()
            if sel is not None:
                selectors.append(sel)
            if self._at_end():
                break
            if self._peek() == ',':
                self._advance()  # consume ','
            elif self._pos == prev_pos:
                # No progress — skip one char to avoid infinite loop
                self._advance()
        return SelectorGroup(selectors)

    def _parse_complex(self) -> ComplexSelector | None:
        parts = []
        self._skip_whitespace()

        compound = self._parse_compound()
        if compound is None:
            return None
        parts.append(compound)

        while True:
            # Save position before trying to read combinator
            saved = self._pos
            # Check for explicit combinator or whitespace
            had_ws = False
            while self._pos < len(self._text) and self._text[self._pos] in ' \t\n\r\f':
                had_ws = True
                self._pos += 1

            if self._at_end() or self._peek() in (',',):
                break

            c = self._peek()
            if c == '>':
                self._advance()
                self._skip_whitespace()
                combinator = Combinator.CHILD
            elif c == '+':
                self._advance()
                self._skip_whitespace()
                combinator = Combinator.ADJACENT
            elif c == '~':
                self._advance()
                self._skip_whitespace()
                combinator = Combinator.SIBLING
            elif had_ws:
                combinator = Combinator.DESCENDANT
            else:
                # Unknown char after non-whitespace: stop parsing this complex selector
                self._pos = saved
                break

            next_compound = self._parse_compound()
            if next_compound is None:
                # No compound after combinator — stop here, don't back up past whitespace
                # (backing up to saved would re-create the infinite loop)
                break
            parts.append(combinator)
            parts.append(next_compound)

        return ComplexSelector(parts)

    def _parse_compound(self) -> CompoundSelector | None:
        simple_selectors = []
        while not self._at_end():
            c = self._peek()
            if c in (' ', '\t', '\n', '\r', '\f', ',', '{', '>',  '+', '~'):
                break
            ss = self._parse_simple()
            if ss is None:
                break
            simple_selectors.append(ss)
        if not simple_selectors:
            return None
        return CompoundSelector(simple_selectors)

    def _parse_simple(self) -> SimpleSelector | None:
        c = self._peek()
        ss = SimpleSelector()

        if c == '*':
            self._advance()
            # universal selector
            return ss  # tag=None means universal

        if c == '.':
            self._advance()
            cls = self._consume_ident()
            if not cls:
                return None
            ss.classes = [cls]
            return ss

        if c == '#':
            self._advance()
            id_ = self._consume_name()
            if not id_:
                return None
            ss.id = id_
            return ss

        if c == '[':
            self._advance()
            attr = self._consume_attribute()
            if attr is None:
                return None
            ss.attributes = [attr]
            return ss

        if c == ':':
            self._advance()
            pseudo_element = False
            if self._peek() == ':':
                self._advance()
                pseudo_element = True
            name = self._consume_ident()
            # consume argument if present
            arg = None
            if not self._at_end() and self._peek() == '(':
                self._advance()
                arg = self._consume_until(')')
                if not self._at_end() and self._peek() == ')':
                    self._advance()
            if pseudo_element:
                ss.pseudo_element = name.lower()
            else:
                pname = name.lower()
                if pname == 'not' and arg is not None:
                    # Store full arg so multi-arg :not() works in matching
                    ss.pseudo_class = f'not({arg})'
                    try:
                        neg_group = parse_selector(arg.strip())
                        if neg_group.selectors:
                            neg_complex = neg_group.selectors[0]
                            if neg_complex.parts:
                                neg_compound = neg_complex.parts[0]
                                if isinstance(neg_compound, CompoundSelector) and neg_compound.simple_selectors:
                                    ss.negation = neg_compound.simple_selectors[0]
                    except Exception as _exc:
                        _logger.debug("Ignored: %s", _exc)
                elif arg is not None:
                    ss.pseudo_class = f'{pname}({arg})'
                else:
                    ss.pseudo_class = pname
            return ss

        # Tag name
        name = self._consume_ident()
        if name:
            ss.tag = name.lower()
            return ss

        return None

    def _consume_ident(self) -> str:
        """Consume CSS identifier characters."""
        start = self._pos
        # Allow leading '-'
        if self._pos < len(self._text) and self._text[self._pos] == '-':
            self._pos += 1
        while self._pos < len(self._text):
            c = self._text[self._pos]
            if c.isalnum() or c in ('-', '_') or ord(c) > 127:
                self._pos += 1
            elif c == '\\' and self._pos + 1 < len(self._text):
                self._pos += 2
            else:
                break
        return self._text[start:self._pos]

    def _consume_name(self) -> str:
        """Consume a name (ident + hyphens, used for #id)."""
        start = self._pos
        while self._pos < len(self._text):
            c = self._text[self._pos]
            if c.isalnum() or c in ('-', '_') or ord(c) > 127:
                self._pos += 1
            elif c == '\\' and self._pos + 1 < len(self._text):
                self._pos += 2
            else:
                break
        return self._text[start:self._pos]

    def _consume_until(self, stop: str) -> str:
        start = self._pos
        depth = 0
        while self._pos < len(self._text):
            c = self._text[self._pos]
            if c == '(' :
                depth += 1
            elif c == ')':
                if depth == 0:
                    break
                depth -= 1
            if c in stop and depth == 0:
                break
            self._pos += 1
        return self._text[start:self._pos]

    def _consume_attribute(self) -> tuple | None:
        """Consume [attr], [attr=val], etc. '[' has already been consumed."""
        self._skip_whitespace()
        attr_name = self._consume_ident()
        if not attr_name:
            return None
        self._skip_whitespace()
        if self._at_end() or self._peek() == ']':
            if not self._at_end():
                self._advance()  # consume ']'
            return (attr_name, 'exists', None)

        # Operator
        op = ''
        c = self._peek()
        if c in ('~', '|', '^', '$', '*'):
            self._advance()
            op += c
        if self._at_end():
            return None
        if self._peek() == '=':
            self._advance()
            op += '='
        elif not op:
            # skip to ']'
            self._consume_until(']')
            if not self._at_end():
                self._advance()
            return (attr_name, 'exists', None)

        self._skip_whitespace()
        # Value
        val = ''
        if not self._at_end():
            c = self._peek()
            if c in ('"', "'"):
                quote = c
                self._advance()
                val = self._consume_until(quote)
                if not self._at_end():
                    self._advance()
            else:
                val = self._consume_until('] ')

        self._skip_whitespace()
        # optional case flag i/s
        if not self._at_end() and self._peek() in ('i', 'I', 's', 'S'):
            self._advance()
        self._skip_whitespace()
        if not self._at_end() and self._peek() == ']':
            self._advance()

        return (attr_name, op, val)
