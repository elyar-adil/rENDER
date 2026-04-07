"""CSS Tokenizer per CSS Syntax Module Level 3."""
from __future__ import annotations

# Token type constants
IDENT       = 'IDENT'
FUNCTION    = 'FUNCTION'
AT_KEYWORD  = 'AT_KEYWORD'
HASH        = 'HASH'
STRING      = 'STRING'
BAD_STRING  = 'BAD_STRING'
URL         = 'URL'
BAD_URL     = 'BAD_URL'
DELIM       = 'DELIM'
NUMBER      = 'NUMBER'
PERCENTAGE  = 'PERCENTAGE'
DIMENSION   = 'DIMENSION'
WHITESPACE  = 'WHITESPACE'
CDO         = 'CDO'
CDC         = 'CDC'
COLON       = 'COLON'
SEMICOLON   = 'SEMICOLON'
COMMA       = 'COMMA'
LBRACKET    = 'LBRACKET'
RBRACKET    = 'RBRACKET'
LPAREN      = 'LPAREN'
RPAREN      = 'RPAREN'
LBRACE      = 'LBRACE'
RBRACE      = 'RBRACE'
EOF         = 'EOF'


class Token:
    __slots__ = ('type', 'value', 'unit', 'flag')

    type: str
    value: str | float | int | None
    unit: str | None
    flag: str | None

    def __init__(self, type_: str, value: str | float | int | None = None, unit: str | None = None, flag: str | None = None) -> None:
        self.type  = type_
        self.value = value   # string/number value
        self.unit  = unit    # for DIMENSION tokens
        self.flag  = flag    # 'id' for HASH; 'integer'/'number' for NUMBER/DIMENSION

    def __repr__(self) -> str:
        parts: list[str] = [self.type]
        if self.value is not None:
            parts.append(repr(self.value))
        if self.unit is not None:
            parts.append(f'unit={self.unit!r}')
        if self.flag is not None:
            parts.append(f'flag={self.flag!r}')
        return f'Token({", ".join(parts)})'


# ---------------------------------------------------------------------------
# Helper predicates (operating on single characters / code points)
# ---------------------------------------------------------------------------

def _is_ident_start(c: str) -> bool:
    return c.isalpha() or c == '_' or ord(c) > 127

def _is_ident_char(c: str) -> bool:
    return c.isalnum() or c in ('-', '_') or ord(c) > 127

def _is_digit(c: str) -> bool:
    return '0' <= c <= '9'

def _is_hex(c: str) -> bool:
    return c in '0123456789abcdefABCDEF'

def _is_whitespace(c: str) -> bool:
    return c in ' \t\n\r\f'


# ---------------------------------------------------------------------------
# Tokenizer
# ---------------------------------------------------------------------------

class Tokenizer:
    _src: str
    _pos: int

    def __init__(self, css: str) -> None:
        # Normalise line endings per spec
        css = css.replace('\r\n', '\n').replace('\r', '\n').replace('\f', '\n')
        self._src = css
        self._pos = 0

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def tokenize(self) -> list[Token]:
        """Return all tokens (including EOF)."""
        tokens: list[Token] = []
        while True:
            tok = self.next_token()
            tokens.append(tok)
            if tok.type == EOF:
                break
        return tokens

    def next_token(self) -> Token:
        """Return the next token from the stream."""
        self._skip_comments()

        if self._pos >= len(self._src):
            return Token(EOF)

        c = self._current()

        # Whitespace
        if _is_whitespace(c):
            return self._consume_whitespace()

        # String
        if c in ('"', "'"):
            return self._consume_string(c)

        # Hash
        if c == '#':
            self._advance()
            if self._pos < len(self._src) and (
                    _is_ident_char(self._current()) or self._current() == '\\'):
                flag = 'id' if self._would_start_ident() else 'unrestricted'
                name = self._consume_ident_value()
                return Token(HASH, name, flag=flag)
            return Token(DELIM, '#')

        # Plus / full stop / minus — may start a number
        if c in ('+', '.'):
            if self._would_start_number(self._pos):
                return self._consume_numeric()
            self._advance()
            return Token(DELIM, c)

        if c == '-':
            # CDC: -->
            if self._src[self._pos:self._pos+3] == '-->':
                self._pos += 3
                return Token(CDC)
            if self._would_start_number(self._pos):
                return self._consume_numeric()
            if self._would_start_ident_at(self._pos):
                return self._consume_ident_like()
            self._advance()
            return Token(DELIM, c)

        # At-keyword
        if c == '@':
            self._advance()
            if self._pos < len(self._src) and self._would_start_ident_at(self._pos):
                name = self._consume_ident_value()
                return Token(AT_KEYWORD, name)
            return Token(DELIM, '@')

        # Backslash (escaped ident start)
        if c == '\\':
            if self._pos + 1 < len(self._src) and self._src[self._pos+1] != '\n':
                return self._consume_ident_like()
            self._advance()
            return Token(DELIM, c)

        # CDO: <!--
        if self._src[self._pos:self._pos+4] == '<!--':
            self._pos += 4
            return Token(CDO)

        # Digits — numeric token
        if _is_digit(c):
            return self._consume_numeric()

        # Ident start
        if _is_ident_start(c):
            return self._consume_ident_like()

        # Single-char tokens
        single: dict[str, str] = {
            ':': COLON,
            ';': SEMICOLON,
            ',': COMMA,
            '[': LBRACKET,
            ']': RBRACKET,
            '(': LPAREN,
            ')': RPAREN,
            '{': LBRACE,
            '}': RBRACE,
        }
        if c in single:
            self._advance()
            return Token(single[c], c)

        # Anything else → DELIM
        self._advance()
        return Token(DELIM, c)

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _current(self) -> str:
        return self._src[self._pos]

    def _advance(self) -> None:
        self._pos += 1

    def _peek(self, offset: int = 1) -> str:
        idx = self._pos + offset
        return self._src[idx] if idx < len(self._src) else ''

    def _skip_comments(self) -> None:
        while self._pos < len(self._src) - 1 and self._src[self._pos:self._pos+2] == '/*':
            end = self._src.find('*/', self._pos + 2)
            if end == -1:
                self._pos = len(self._src)
            else:
                self._pos = end + 2

    # ---- whitespace -------------------------------------------------------

    def _consume_whitespace(self) -> Token:
        while self._pos < len(self._src) and _is_whitespace(self._src[self._pos]):
            self._pos += 1
        return Token(WHITESPACE, ' ')

    # ---- strings ----------------------------------------------------------

    def _consume_string(self, quote: str) -> Token:
        self._advance()  # consume opening quote
        buf: list[str] = []
        while self._pos < len(self._src):
            c = self._src[self._pos]
            if c == quote:
                self._advance()
                return Token(STRING, ''.join(buf))
            if c == '\n':
                # bad string — return BAD_STRING, leave newline in stream
                return Token(BAD_STRING, ''.join(buf))
            if c == '\\':
                self._advance()
                if self._pos >= len(self._src):
                    break
                nc = self._src[self._pos]
                if nc == '\n':
                    self._advance()  # escaped newline — ignored
                else:
                    buf.append(self._consume_escape())
                continue
            buf.append(c)
            self._advance()
        return Token(STRING, ''.join(buf))  # EOF in string — treat as closed

    def _consume_escape(self) -> str:
        """Consume after backslash has been consumed."""
        if self._pos >= len(self._src):
            return '\ufffd'
        c = self._src[self._pos]
        if _is_hex(c):
            hex_digits: list[str] = []
            for _ in range(6):
                if self._pos < len(self._src) and _is_hex(self._src[self._pos]):
                    hex_digits.append(self._src[self._pos])
                    self._advance()
                else:
                    break
            # optional whitespace after hex escape
            if self._pos < len(self._src) and _is_whitespace(self._src[self._pos]):
                self._advance()
            cp = int(''.join(hex_digits), 16)
            if cp == 0 or cp > 0x10FFFF or (0xD800 <= cp <= 0xDFFF):
                return '\ufffd'
            return chr(cp)
        else:
            ch = c
            self._advance()
            return ch

    # ---- numbers ----------------------------------------------------------

    def _would_start_number(self, pos: int) -> bool:
        """True if pos starts a CSS number."""
        src = self._src
        n = len(src)
        c = src[pos] if pos < n else ''
        if c in ('+', '-'):
            pos += 1
            c = src[pos] if pos < n else ''
        if _is_digit(c):
            return True
        if c == '.':
            c2 = src[pos+1] if pos+1 < n else ''
            return _is_digit(c2)
        return False

    def _consume_numeric(self) -> Token:
        num_str, flag = self._consume_number_repr()
        num_val: float | int = float(num_str)
        if flag == 'integer':
            num_val = int(num_str)

        # Check for '%'
        if self._pos < len(self._src) and self._src[self._pos] == '%':
            self._advance()
            return Token(PERCENTAGE, num_val, flag=flag)

        # Check for dimension unit (ident following immediately)
        if self._pos < len(self._src) and self._would_start_ident_at(self._pos):
            unit = self._consume_ident_value()
            return Token(DIMENSION, num_val, unit=unit.lower(), flag=flag)

        return Token(NUMBER, num_val, flag=flag)

    def _consume_number_repr(self) -> tuple[str, str]:
        """Consume a CSS number, return (string_repr, 'integer'|'number')."""
        src = self._src
        n = len(src)
        start = self._pos
        flag = 'integer'

        if self._pos < n and src[self._pos] in ('+', '-'):
            self._pos += 1
        while self._pos < n and _is_digit(src[self._pos]):
            self._pos += 1
        if self._pos < n and src[self._pos] == '.':
            if self._pos + 1 < n and _is_digit(src[self._pos+1]):
                flag = 'number'
                self._pos += 1  # consume '.'
                while self._pos < n and _is_digit(src[self._pos]):
                    self._pos += 1
        if self._pos < n and src[self._pos] in ('e', 'E'):
            nxt = src[self._pos+1] if self._pos+1 < n else ''
            if _is_digit(nxt) or (nxt in ('+', '-') and self._pos+2 < n and _is_digit(src[self._pos+2])):
                flag = 'number'
                self._pos += 1  # consume 'e'/'E'
                if self._pos < n and src[self._pos] in ('+', '-'):
                    self._pos += 1
                while self._pos < n and _is_digit(src[self._pos]):
                    self._pos += 1
        return src[start:self._pos], flag

    # ---- identifiers / functions / urls -----------------------------------

    def _would_start_ident_at(self, pos: int) -> bool:
        src = self._src
        n = len(src)
        c = src[pos] if pos < n else ''
        if c == '-':
            c2 = src[pos+1] if pos+1 < n else ''
            if c2 == '-' or _is_ident_start(c2):
                return True
            if c2 == '\\' and pos+2 < n and src[pos+2] != '\n':
                return True
            return False
        if _is_ident_start(c):
            return True
        if c == '\\' and pos+1 < n and src[pos+1] != '\n':
            return True
        return False

    def _would_start_ident(self) -> bool:
        return self._would_start_ident_at(self._pos)

    def _consume_ident_value(self) -> str:
        buf: list[str] = []
        src = self._src
        n = len(src)
        # consume leading '-' or '--'
        if self._pos < n and src[self._pos] == '-':
            buf.append('-')
            self._pos += 1
            if self._pos < n and src[self._pos] == '-':
                buf.append('-')
                self._pos += 1
        while self._pos < n:
            c = src[self._pos]
            if _is_ident_char(c):
                buf.append(c)
                self._pos += 1
            elif c == '\\' and self._pos+1 < n and src[self._pos+1] != '\n':
                self._pos += 1
                buf.append(self._consume_escape())
            else:
                break
        return ''.join(buf)

    def _consume_ident_like(self) -> Token:
        name = self._consume_ident_value()
        if self._pos < len(self._src) and self._src[self._pos] == '(':
            self._advance()
            if name.lower() == 'url':
                return self._consume_url()
            return Token(FUNCTION, name)
        return Token(IDENT, name)

    def _consume_url(self) -> Token:
        """Consume after 'url(' has been consumed."""
        # Skip whitespace
        while self._pos < len(self._src) and _is_whitespace(self._src[self._pos]):
            self._pos += 1

        if self._pos >= len(self._src):
            return Token(URL, '')

        c = self._src[self._pos]
        if c in ('"', "'"):
            # url("...") — delegate to string then expect ')'
            str_tok = self._consume_string(c)
            # skip whitespace
            while self._pos < len(self._src) and _is_whitespace(self._src[self._pos]):
                self._pos += 1
            if self._pos < len(self._src) and self._src[self._pos] == ')':
                self._advance()
            if str_tok.type == BAD_STRING:
                self._consume_bad_url_remnants()
                return Token(BAD_URL, str_tok.value)
            return Token(URL, str_tok.value)

        # Unquoted URL
        buf: list[str] = []
        while self._pos < len(self._src):
            c = self._src[self._pos]
            if c == ')':
                self._advance()
                return Token(URL, ''.join(buf))
            if _is_whitespace(c):
                # skip trailing whitespace, then expect ')'
                while self._pos < len(self._src) and _is_whitespace(self._src[self._pos]):
                    self._pos += 1
                if self._pos < len(self._src) and self._src[self._pos] == ')':
                    self._advance()
                else:
                    self._consume_bad_url_remnants()
                    return Token(BAD_URL, ''.join(buf))
                return Token(URL, ''.join(buf))
            if c in ('"', "'", '(') or (ord(c) <= 8 or c == '\x0b' or (0x0e <= ord(c) <= 0x1f) or ord(c) == 0x7f):
                self._consume_bad_url_remnants()
                return Token(BAD_URL, ''.join(buf))
            if c == '\\':
                if self._pos+1 < len(self._src) and self._src[self._pos+1] != '\n':
                    self._pos += 1
                    buf.append(self._consume_escape())
                    continue
                else:
                    self._consume_bad_url_remnants()
                    return Token(BAD_URL, ''.join(buf))
            buf.append(c)
            self._pos += 1
        return Token(URL, ''.join(buf))

    def _consume_bad_url_remnants(self) -> None:
        while self._pos < len(self._src):
            c = self._src[self._pos]
            if c == ')':
                self._pos += 1
                return
            if c == '\\' and self._pos+1 < len(self._src):
                self._pos += 2
            else:
                self._pos += 1
