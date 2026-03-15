"""CSS Parser — builds a Stylesheet from a CSS string.

Imports from css.tokenizer.
"""

from css.tokenizer import (
    Tokenizer, Token,
    IDENT, FUNCTION, AT_KEYWORD, HASH, STRING, URL, DELIM,
    NUMBER, PERCENTAGE, DIMENSION, WHITESPACE,
    COLON, SEMICOLON, COMMA,
    LBRACKET, RBRACKET, LPAREN, RPAREN, LBRACE, RBRACE,
    CDO, CDC, EOF,
)


# ---------------------------------------------------------------------------
# AST nodes
# ---------------------------------------------------------------------------

class Declaration:
    __slots__ = ('property', 'value', 'important')

    def __init__(self, prop: str, value: str, important: bool = False):
        self.property  = prop
        self.value     = value
        self.important = important

    def __repr__(self):
        imp = ' !important' if self.important else ''
        return f'Declaration({self.property}: {self.value}{imp})'


class QualifiedRule:
    __slots__ = ('prelude', 'declarations')

    def __init__(self, prelude: str, declarations: list):
        self.prelude      = prelude
        self.declarations = declarations

    def __repr__(self):
        return f'QualifiedRule({self.prelude!r}, {self.declarations})'


class AtRule:
    __slots__ = ('name', 'prelude', 'rules', 'declarations')

    def __init__(self, name: str, prelude: str,
                 rules: list = None, declarations: list = None):
        self.name         = name
        self.prelude      = prelude
        self.rules        = rules        if rules        is not None else []
        self.declarations = declarations if declarations is not None else []

    def __repr__(self):
        return f'AtRule({self.name}, {self.prelude!r})'


class Stylesheet:
    __slots__ = ('rules',)

    def __init__(self, rules: list):
        self.rules = rules

    def __repr__(self):
        return f'Stylesheet({self.rules})'


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def parse_stylesheet(css: str) -> Stylesheet:
    """Parse a complete CSS stylesheet."""
    parser = _Parser(css)
    return parser.parse_stylesheet()


def parse_declaration_block(css: str) -> list:
    """Parse declarations from the *content* of a { } block."""
    parser = _Parser(css)
    return parser.parse_declaration_list()


def parse_inline_style(css: str) -> dict:
    """Parse an inline style= string, return {property: value} dict."""
    decls = parse_declaration_block(css)
    result = {}
    for d in decls:
        result[d.property] = d.value
    return result


# ---------------------------------------------------------------------------
# Internal parser
# ---------------------------------------------------------------------------

class _Parser:
    def __init__(self, css: str):
        tokens = Tokenizer(css).tokenize()
        # Filter out CDO/CDC at the top level (they're ignored in stylesheet mode)
        self._tokens = tokens
        self._pos    = 0

    # ------------------------------------------------------------------
    # Token stream helpers
    # ------------------------------------------------------------------

    def _current(self) -> Token:
        if self._pos < len(self._tokens):
            return self._tokens[self._pos]
        return Token(EOF)

    def _peek(self, offset: int = 1) -> Token:
        idx = self._pos + offset
        if idx < len(self._tokens):
            return self._tokens[idx]
        return Token(EOF)

    def _advance(self) -> Token:
        tok = self._current()
        self._pos += 1
        return tok

    def _skip_whitespace(self):
        while self._current().type == WHITESPACE:
            self._advance()

    def _skip_whitespace_and_semicolons(self):
        while self._current().type in (WHITESPACE, SEMICOLON):
            self._advance()

    # ------------------------------------------------------------------
    # Stylesheet
    # ------------------------------------------------------------------

    def parse_stylesheet(self) -> Stylesheet:
        rules = []
        while True:
            self._skip_whitespace()
            tok = self._current()
            if tok.type == EOF:
                break
            if tok.type in (CDO, CDC):
                self._advance()
                continue
            if tok.type == AT_KEYWORD:
                rule = self._consume_at_rule()
                if rule is not None:
                    rules.append(rule)
            else:
                rule = self._consume_qualified_rule()
                if rule is not None:
                    rules.append(rule)
        return Stylesheet(rules)

    # ------------------------------------------------------------------
    # At-rule
    # ------------------------------------------------------------------

    def _consume_at_rule(self) -> AtRule | None:
        name_tok = self._advance()          # consume @keyword
        name = name_tok.value.lower()

        prelude_parts = []
        while True:
            self._skip_whitespace()
            tok = self._current()
            if tok.type == SEMICOLON:
                self._advance()
                return AtRule(name, ''.join(prelude_parts).strip())
            if tok.type == LBRACE:
                self._advance()             # consume '{'
                if name in ('media', 'supports', 'document', 'layer'):
                    rules = self._consume_rule_list_in_block()
                    return AtRule(name, ''.join(prelude_parts).strip(), rules=rules)
                elif name in ('keyframes', '-webkit-keyframes', '-moz-keyframes'):
                    rules = self._consume_rule_list_in_block()
                    return AtRule(name, ''.join(prelude_parts).strip(), rules=rules)
                else:
                    # @font-face, @page, etc. — declaration block
                    decls = self._consume_declaration_list_in_block()
                    return AtRule(name, ''.join(prelude_parts).strip(), declarations=decls)
            if tok.type == EOF:
                return None
            prelude_parts.append(self._token_to_text(tok))
            self._advance()

    def _consume_rule_list_in_block(self) -> list:
        rules = []
        while True:
            self._skip_whitespace()
            tok = self._current()
            if tok.type in (RBRACE, EOF):
                if tok.type == RBRACE:
                    self._advance()
                return rules
            if tok.type in (CDO, CDC):
                self._advance()
                continue
            if tok.type == AT_KEYWORD:
                rule = self._consume_at_rule()
                if rule is not None:
                    rules.append(rule)
            else:
                rule = self._consume_qualified_rule()
                if rule is not None:
                    rules.append(rule)

    # ------------------------------------------------------------------
    # Qualified rule (selector { declarations })
    # ------------------------------------------------------------------

    def _consume_qualified_rule(self) -> QualifiedRule | None:
        prelude_parts = []
        while True:
            tok = self._current()
            if tok.type == EOF:
                return None   # parse error
            if tok.type == LBRACE:
                self._advance()  # consume '{'
                decls = self._consume_declaration_list_in_block()
                prelude = self._normalise_whitespace(''.join(prelude_parts))
                return QualifiedRule(prelude, decls)
            prelude_parts.append(self._token_to_text(tok))
            self._advance()

    # ------------------------------------------------------------------
    # Declaration list (inside { })
    # ------------------------------------------------------------------

    def _consume_declaration_list_in_block(self) -> list:
        decls = []
        while True:
            self._skip_whitespace_and_semicolons()
            tok = self._current()
            if tok.type in (RBRACE, EOF):
                if tok.type == RBRACE:
                    self._advance()
                return decls
            if tok.type == AT_KEYWORD:
                # skip nested at-rule inside declaration block (unusual)
                self._consume_at_rule()
                continue
            decl = self._consume_declaration()
            if decl is not None:
                decls.append(decl)

    def parse_declaration_list(self) -> list:
        """Parse declarations from raw string (no surrounding braces)."""
        decls = []
        while True:
            self._skip_whitespace_and_semicolons()
            tok = self._current()
            if tok.type == EOF:
                return decls
            if tok.type == AT_KEYWORD:
                self._consume_at_rule()
                continue
            decl = self._consume_declaration()
            if decl is not None:
                decls.append(decl)

    def _consume_declaration(self) -> Declaration | None:
        self._skip_whitespace()
        tok = self._current()

        # Property name must be an IDENT (or custom property --foo)
        if tok.type not in (IDENT, DELIM):
            # skip until ';' or '}'
            self._skip_to_next_declaration()
            return None
        # Allow custom properties starting with '--'
        if tok.type == IDENT or (tok.type == DELIM and tok.value == '-'):
            prop_name = self._consume_property_name()
        else:
            self._skip_to_next_declaration()
            return None

        if not prop_name:
            self._skip_to_next_declaration()
            return None

        self._skip_whitespace()
        if self._current().type != COLON:
            self._skip_to_next_declaration()
            return None
        self._advance()  # consume ':'
        self._skip_whitespace()

        # Consume value tokens until ';' or '}' or EOF
        value_parts = []
        while True:
            tok = self._current()
            if tok.type in (SEMICOLON, RBRACE, EOF):
                break
            value_parts.append(self._token_to_text(tok))
            self._advance()

        raw_value = ''.join(value_parts).strip()

        # Check for !important
        important = False
        if raw_value.lower().endswith('!important'):
            important = True
            raw_value = raw_value[:-len('!important')].rstrip()
            # also strip trailing '!'
            if raw_value.endswith('!'):
                raw_value = raw_value[:-1].rstrip()
        else:
            # Handle "value ! important" (with spaces)
            import re as _re
            m = _re.search(r'(.*?)\s*!\s*important\s*$', raw_value, _re.IGNORECASE)
            if m:
                important = True
                raw_value = m.group(1).rstrip()

        raw_value = self._normalise_whitespace(raw_value)
        if not raw_value:
            return None
        return Declaration(prop_name, raw_value, important)

    def _consume_property_name(self) -> str:
        """Consume an IDENT or custom-property name."""
        tok = self._current()
        if tok.type == IDENT:
            self._advance()
            return tok.value.lower()
        # Could be a custom property '--foo' represented as DELIM('-') DELIM('-') IDENT
        # But typically the tokenizer gives IDENT for '--foo' already.
        return ''

    def _skip_to_next_declaration(self):
        """Skip tokens until we find a ';' or '}' (leave '}' unconsumed)."""
        while True:
            tok = self._current()
            if tok.type in (EOF, RBRACE):
                return
            if tok.type == SEMICOLON:
                self._advance()
                return
            self._advance()

    # ------------------------------------------------------------------
    # Token → text reconstruction
    # ------------------------------------------------------------------

    def _token_to_text(self, tok: Token) -> str:
        t = tok.type
        if t == WHITESPACE:
            return ' '
        if t == IDENT:
            return tok.value
        if t == STRING:
            # re-quote as double-quoted
            escaped = tok.value.replace('\\', '\\\\').replace('"', '\\"')
            return f'"{escaped}"'
        if t == HASH:
            return f'#{tok.value}'
        if t == AT_KEYWORD:
            return f'@{tok.value}'
        if t == FUNCTION:
            return f'{tok.value}('
        if t == URL:
            return f'url("{tok.value}")'
        if t == NUMBER:
            return str(tok.value)
        if t == PERCENTAGE:
            return f'{tok.value}%'
        if t == DIMENSION:
            return f'{tok.value}{tok.unit}'
        if t == DELIM:
            return tok.value
        if t == COLON:
            return ':'
        if t == SEMICOLON:
            return ';'
        if t == COMMA:
            return ','
        if t == LBRACKET:
            return '['
        if t == RBRACKET:
            return ']'
        if t == LPAREN:
            return '('
        if t == RPAREN:
            return ')'
        if t == LBRACE:
            return '{'
        if t == RBRACE:
            return '}'
        return ''

    @staticmethod
    def _normalise_whitespace(text: str) -> str:
        import re
        return re.sub(r'\s+', ' ', text).strip()
