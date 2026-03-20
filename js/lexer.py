"""JavaScript lexer for rENDER browser engine."""


class Token:
    __slots__ = ('type', 'value', 'line')

    def __init__(self, type_: str, value=None, line: int = 0):
        self.type = type_
        self.value = value
        self.line = line

    def __repr__(self):
        return f'Token({self.type}, {self.value!r})'


# Token types
NUMBER = 'NUMBER'
STRING = 'STRING'
IDENT = 'IDENT'
KEYWORD = 'KEYWORD'
PUNCT = 'PUNCT'
OP = 'OP'
EOF = 'EOF'
TEMPLATE = 'TEMPLATE'

KEYWORDS = frozenset({
    'var', 'let', 'const', 'function', 'return', 'if', 'else', 'for',
    'while', 'do', 'break', 'continue', 'new', 'this', 'typeof',
    'instanceof', 'in', 'of', 'null', 'undefined', 'true', 'false',
    'try', 'catch', 'finally', 'throw', 'switch', 'case', 'default',
    'delete', 'void',
})

# Multi-character operators (longest-first for greedy matching)
_MULTI_OPS = (
    '>>>=', '===', '!==', '>>>', '<<=', '>>=',
    '**=', '&&=', '||=', '??=',
    '==', '!=', '<=', '>=', '&&', '||', '??',
    '++', '--', '+=', '-=', '*=', '/=', '%=',
    '**', '=>', '<<', '>>',
)

_SINGLE_OPS = set('+-*/%=<>!&|^~?:')
_PUNCTS = set('{}[]();,.')


class Lexer:
    """Tokenize JavaScript source code."""

    def __init__(self, source: str):
        self.source = source
        self.pos = 0
        self.line = 1
        self.tokens: list[Token] = []

    def tokenize(self) -> list[Token]:
        src = self.source
        length = len(src)
        tokens = self.tokens
        tokens.clear()

        while self.pos < length:
            ch = src[self.pos]

            # Whitespace
            if ch in ' \t\r':
                self.pos += 1
                continue
            if ch == '\n':
                self.line += 1
                self.pos += 1
                continue

            # Single-line comment
            if ch == '/' and self.pos + 1 < length and src[self.pos + 1] == '/':
                self.pos += 2
                while self.pos < length and src[self.pos] != '\n':
                    self.pos += 1
                continue

            # Multi-line comment
            if ch == '/' and self.pos + 1 < length and src[self.pos + 1] == '*':
                self.pos += 2
                while self.pos + 1 < length:
                    if src[self.pos] == '\n':
                        self.line += 1
                    if src[self.pos] == '*' and src[self.pos + 1] == '/':
                        self.pos += 2
                        break
                    self.pos += 1
                else:
                    self.pos = length
                continue

            # String literals
            if ch in ('"', "'"):
                tokens.append(self._read_string(ch))
                continue

            # Template literal (backtick)
            if ch == '`':
                tokens.append(self._read_template())
                continue

            # Numbers
            if ch.isdigit() or (ch == '.' and self.pos + 1 < length and src[self.pos + 1].isdigit()):
                tokens.append(self._read_number())
                continue

            # Identifiers and keywords
            if ch.isalpha() or ch == '_' or ch == '$':
                tokens.append(self._read_ident())
                continue

            # Punctuation
            if ch in _PUNCTS:
                # Check for '...' spread operator
                if ch == '.' and self.pos + 2 < length and src[self.pos + 1:self.pos + 3] == '..':
                    tokens.append(Token(OP, '...', self.line))
                    self.pos += 3
                    continue
                tokens.append(Token(PUNCT, ch, self.line))
                self.pos += 1
                continue

            # Multi-char operators (try longest match first)
            matched = False
            for op in _MULTI_OPS:
                if src[self.pos:self.pos + len(op)] == op:
                    tokens.append(Token(OP, op, self.line))
                    self.pos += len(op)
                    matched = True
                    break
            if matched:
                continue

            # Single-char operators
            if ch in _SINGLE_OPS:
                # Division vs regex: heuristic — if last token is a value, it's division
                if ch == '/':
                    if tokens and tokens[-1].type in (NUMBER, STRING, IDENT) or \
                       (tokens and tokens[-1].type == PUNCT and tokens[-1].value in (')', ']')):
                        tokens.append(Token(OP, '/', self.line))
                        self.pos += 1
                        continue
                    else:
                        # Skip regex literal
                        tokens.append(self._read_regex())
                        continue
                tokens.append(Token(OP, ch, self.line))
                self.pos += 1
                continue

            # Unknown character — skip
            self.pos += 1

        tokens.append(Token(EOF, None, self.line))
        return tokens

    def _read_string(self, quote: str) -> Token:
        src = self.source
        self.pos += 1  # skip opening quote
        parts = []
        while self.pos < len(src):
            ch = src[self.pos]
            if ch == '\\':
                self.pos += 1
                if self.pos < len(src):
                    esc = src[self.pos]
                    if esc == 'n':
                        parts.append('\n')
                    elif esc == 't':
                        parts.append('\t')
                    elif esc == 'r':
                        parts.append('\r')
                    elif esc == '\\':
                        parts.append('\\')
                    elif esc == quote:
                        parts.append(quote)
                    elif esc == 'u':
                        # Unicode escape \uXXXX
                        hex_str = src[self.pos + 1:self.pos + 5]
                        if len(hex_str) == 4:
                            try:
                                parts.append(chr(int(hex_str, 16)))
                                self.pos += 4
                            except ValueError:
                                parts.append(esc)
                        else:
                            parts.append(esc)
                    elif esc == '0':
                        parts.append('\0')
                    else:
                        parts.append(esc)
                    self.pos += 1
                continue
            if ch == quote:
                self.pos += 1
                break
            if ch == '\n':
                self.line += 1
            parts.append(ch)
            self.pos += 1
        return Token(STRING, ''.join(parts), self.line)

    def _read_template(self) -> Token:
        """Read backtick template literal as a plain string (no interpolation)."""
        src = self.source
        self.pos += 1  # skip `
        parts = []
        while self.pos < len(src):
            ch = src[self.pos]
            if ch == '\\':
                self.pos += 1
                if self.pos < len(src):
                    parts.append(src[self.pos])
                self.pos += 1
                continue
            if ch == '`':
                self.pos += 1
                break
            if ch == '\n':
                self.line += 1
            parts.append(ch)
            self.pos += 1
        return Token(STRING, ''.join(parts), self.line)

    def _read_number(self) -> Token:
        src = self.source
        start = self.pos
        # Hex
        if src[self.pos] == '0' and self.pos + 1 < len(src) and src[self.pos + 1] in 'xX':
            self.pos += 2
            while self.pos < len(src) and src[self.pos] in '0123456789abcdefABCDEF_':
                self.pos += 1
            return Token(NUMBER, int(src[start:self.pos].replace('_', ''), 16), self.line)
        # Decimal / float
        has_dot = False
        while self.pos < len(src):
            ch = src[self.pos]
            if ch.isdigit() or ch == '_':
                self.pos += 1
            elif ch == '.' and not has_dot:
                has_dot = True
                self.pos += 1
            elif ch in 'eE':
                self.pos += 1
                if self.pos < len(src) and src[self.pos] in '+-':
                    self.pos += 1
                while self.pos < len(src) and src[self.pos].isdigit():
                    self.pos += 1
                break
            else:
                break
        text = src[start:self.pos].replace('_', '')
        val = float(text) if '.' in text or 'e' in text.lower() else int(text)
        return Token(NUMBER, val, self.line)

    def _read_ident(self) -> Token:
        src = self.source
        start = self.pos
        while self.pos < len(src) and (src[self.pos].isalnum() or src[self.pos] in '_$'):
            self.pos += 1
        word = src[start:self.pos]
        if word in KEYWORDS:
            return Token(KEYWORD, word, self.line)
        return Token(IDENT, word, self.line)

    def _read_regex(self) -> Token:
        """Skip a regex literal /pattern/flags and return as a string token."""
        src = self.source
        self.pos += 1  # skip /
        parts = ['/']
        while self.pos < len(src):
            ch = src[self.pos]
            parts.append(ch)
            self.pos += 1
            if ch == '\\' and self.pos < len(src):
                parts.append(src[self.pos])
                self.pos += 1
            elif ch == '/':
                break
        # Flags
        while self.pos < len(src) and src[self.pos].isalpha():
            parts.append(src[self.pos])
            self.pos += 1
        return Token(STRING, ''.join(parts), self.line)
