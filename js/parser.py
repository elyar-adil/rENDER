"""JavaScript recursive-descent parser for rENDER browser engine.

Parses a subset of JavaScript into an AST suitable for interpretation.

ES6+ features supported:
  - let / const / var with destructuring patterns
  - Arrow functions (with and without parens)
  - Classes: declaration, expression, extends, super, static, get/set
  - Template literals with ${} interpolation
  - Destructuring: object and array patterns in var-decls, params, for-of
  - Default and rest parameters
  - Optional chaining: ?. and ?.[
  - Object spread: { ...obj }
  - for-of / for-in with destructuring
"""
from js.lexer import Token, KEYWORD, IDENT, NUMBER, STRING, PUNCT, OP, EOF, TEMPLATE


class ASTNode:
    """Generic AST node."""
    __slots__ = ('type', 'data')

    def __init__(self, type_: str, **kwargs):
        self.type = type_
        self.data = kwargs

    def __getattr__(self, name):
        if name in ('type', 'data'):
            raise AttributeError(name)
        try:
            return self.data[name]
        except KeyError:
            raise AttributeError(name)

    def __repr__(self):
        return f'AST({self.type})'


def _node(type_, **kw):
    return ASTNode(type_, **kw)


class Parser:
    """Recursive-descent parser for JavaScript subset."""

    def __init__(self, tokens: list[Token]):
        self.tokens = tokens
        self.pos = 0

    # ----- helpers -----

    def _cur(self) -> Token:
        if self.pos < len(self.tokens):
            return self.tokens[self.pos]
        return Token(EOF, None)

    def _peek(self, type_=None, value=None) -> bool:
        t = self._cur()
        if type_ and t.type != type_:
            return False
        if value is not None and t.value != value:
            return False
        return True

    def _peek2(self) -> Token:
        """Return the token after the current one."""
        if self.pos + 1 < len(self.tokens):
            return self.tokens[self.pos + 1]
        return Token(EOF, None)

    def _eat(self, type_=None, value=None) -> Token:
        t = self._cur()
        if type_ and t.type != type_:
            raise SyntaxError(f'Expected {type_}({value!r}), got {t} at line {t.line}')
        if value is not None and t.value != value:
            raise SyntaxError(f'Expected {value!r}, got {t.value!r} at line {t.line}')
        self.pos += 1
        return t

    def _eat_semi(self):
        """Consume optional semicolon (ASI)."""
        if self._peek(PUNCT, ';'):
            self.pos += 1

    def _is_ident_name(self) -> bool:
        """True if current token can be used as an identifier name (incl. contextual kw)."""
        t = self._cur()
        return t.type == IDENT or (t.type == KEYWORD and t.value in (
            'static', 'get', 'set', 'async', 'from', 'of', 'constructor',
        ))

    def _eat_ident_name(self) -> str:
        """Eat a token that may be an identifier or a contextual keyword."""
        t = self._cur()
        if t.type == IDENT or (t.type == KEYWORD and t.value in (
            'static', 'get', 'set', 'async', 'from', 'of', 'constructor',
        )):
            self.pos += 1
            return t.value
        raise SyntaxError(f'Expected identifier, got {t} at line {t.line}')

    # ----- entry -----

    def parse(self) -> ASTNode:
        body = []
        while not self._peek(EOF):
            stmt = self._statement()
            if stmt is not None:
                body.append(stmt)
        return _node('Program', body=body)

    # ----- statements -----

    def _statement(self):
        t = self._cur()

        if t.type == PUNCT and t.value == ';':
            self.pos += 1
            return None

        if t.type == KEYWORD:
            kw = t.value
            if kw in ('var', 'let', 'const'):
                return self._var_decl()
            if kw == 'function':
                return self._function_decl()
            if kw == 'class':
                return self._class_decl()
            if kw == 'return':
                return self._return_stmt()
            if kw == 'if':
                return self._if_stmt()
            if kw == 'for':
                return self._for_stmt()
            if kw == 'while':
                return self._while_stmt()
            if kw == 'do':
                return self._do_while_stmt()
            if kw == 'break':
                self.pos += 1
                label = None
                if self._peek(IDENT) and not self._peek(PUNCT, ';'):
                    label = self._cur().value; self.pos += 1
                self._eat_semi()
                return _node('Break', label=label)
            if kw == 'continue':
                self.pos += 1
                label = None
                if self._peek(IDENT) and not self._peek(PUNCT, ';'):
                    label = self._cur().value; self.pos += 1
                self._eat_semi()
                return _node('Continue', label=label)
            if kw == 'try':
                return self._try_stmt()
            if kw == 'throw':
                return self._throw_stmt()
            if kw == 'switch':
                return self._switch_stmt()

        # Labeled statement: ident ':'
        if t.type == IDENT and self._peek2().type == OP and self._peek2().value == ':':
            label = t.value
            self.pos += 2  # ident + ':'
            body = self._statement()
            return _node('Labeled', label=label, body=body)

        if t.type == PUNCT and t.value == '{':
            return self._block()

        # Expression statement (also handles async arrow, etc.)
        expr = self._expression()
        self._eat_semi()
        return _node('ExprStmt', expr=expr)

    def _block(self) -> ASTNode:
        self._eat(PUNCT, '{')
        body = []
        while not self._peek(PUNCT, '}') and not self._peek(EOF):
            stmt = self._statement()
            if stmt:
                body.append(stmt)
        self._eat(PUNCT, '}')
        return _node('Block', body=body)

    # ----- variable declarations -----

    def _var_decl(self) -> ASTNode:
        kind = self._eat(KEYWORD).value
        decls = []
        while True:
            pattern = self._binding_pattern()
            init = None
            if self._peek(OP, '='):
                self.pos += 1
                init = self._assign_expr()
            decls.append((pattern, init))
            if self._peek(PUNCT, ','):
                self.pos += 1
            else:
                break
        self._eat_semi()
        return _node('VarDecl', kind=kind, decls=decls)

    def _var_decl_no_semi(self) -> ASTNode:
        kind = self._eat(KEYWORD).value
        decls = []
        while True:
            pattern = self._binding_pattern()
            init = None
            if self._peek(OP, '='):
                self.pos += 1
                init = self._assign_expr()
            decls.append((pattern, init))
            if self._peek(PUNCT, ','):
                self.pos += 1
            else:
                break
        return _node('VarDecl', kind=kind, decls=decls)

    # ----- binding patterns (destructuring) -----

    def _binding_pattern(self) -> ASTNode:
        """Parse an identifier, array pattern, or object pattern."""
        if self._peek(PUNCT, '['):
            return self._array_pattern()
        if self._peek(PUNCT, '{'):
            return self._object_pattern()
        name = self._eat(IDENT).value
        return _node('BindingIdent', name=name)

    def _array_pattern(self) -> ASTNode:
        """Parse [a, b = 1, ...rest]."""
        self._eat(PUNCT, '[')
        elements = []  # list of (binding, default_node) or None for holes
        rest = None
        while not self._peek(PUNCT, ']') and not self._peek(EOF):
            if self._peek(PUNCT, ','):
                elements.append(None)  # elision / hole
                self.pos += 1
                continue
            if self._peek(OP, '...'):
                self.pos += 1
                rest = self._binding_pattern()
                if self._peek(PUNCT, ','):
                    self.pos += 1
                break
            binding = self._binding_pattern()
            default = None
            if self._peek(OP, '='):
                self.pos += 1
                default = self._assign_expr()
            elements.append((binding, default))
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, ']')
        return _node('ArrayPattern', elements=elements, rest=rest)

    def _object_pattern(self) -> ASTNode:
        """Parse {a, b: c, d = 1, e: f = 2, ...rest}."""
        self._eat(PUNCT, '{')
        props = []  # list of (key_str, binding_node, default_node)
        rest = None
        while not self._peek(PUNCT, '}') and not self._peek(EOF):
            if self._peek(OP, '...'):
                self.pos += 1
                rest = self._binding_pattern()
                if self._peek(PUNCT, ','):
                    self.pos += 1
                break
            # Key: identifier, string, number, or keyword-as-name
            t = self._cur()
            if t.type == PUNCT and t.value == '[':
                # Computed key in destructuring: { [expr]: binding }
                self.pos += 1
                key_expr = self._assign_expr()
                self._eat(PUNCT, ']')
                self._eat(OP, ':')
                binding = self._binding_pattern()
                default = None
                if self._peek(OP, '='):
                    self.pos += 1
                    default = self._assign_expr()
                props.append((key_expr, binding, default))
            elif t.type in (IDENT, STRING, NUMBER) or t.type == KEYWORD:
                key = t.value if t.type != NUMBER else str(t.value)
                self.pos += 1
                if self._peek(OP, ':'):
                    self.pos += 1
                    binding = self._binding_pattern()
                else:
                    # Shorthand: { a } → binds key 'a' to name 'a'
                    binding = _node('BindingIdent', name=str(key))
                default = None
                if self._peek(OP, '='):
                    self.pos += 1
                    default = self._assign_expr()
                props.append((key, binding, default))
            else:
                self.pos += 1  # skip unknown
                continue
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, '}')
        return _node('ObjectPattern', props=props, rest=rest)

    # ----- functions -----

    def _function_decl(self) -> ASTNode:
        self._eat(KEYWORD, 'function')
        is_generator = False
        if self._peek(OP, '*'):
            self.pos += 1
            is_generator = True
        name = None
        if self._peek(IDENT):
            name = self._eat(IDENT).value
        params, defaults, has_rest, patterns = self._param_list()
        body = self._block()
        return _node('FuncDecl', name=name, params=params, body=body,
                     param_defaults=defaults, param_rest=has_rest,
                     param_patterns=patterns, is_generator=is_generator)

    def _param_list(self) -> tuple:
        """Parse formal parameter list.

        Returns (names, defaults, has_rest, patterns) where:
          names    : list[str] — simple param names; '$pN' for patterns
          defaults : dict[int, ASTNode]
          has_rest : bool — last param is a rest param
          patterns : dict[int, ASTNode] — destructuring patterns by index
        """
        self._eat(PUNCT, '(')
        names: list[str] = []
        defaults: dict = {}
        patterns: dict = {}
        has_rest = False
        idx = 0
        while not self._peek(PUNCT, ')') and not self._peek(EOF):
            if self._peek(OP, '...'):
                self.pos += 1
                if self._peek(PUNCT, '[') or self._peek(PUNCT, '{'):
                    pat = self._binding_pattern()
                    names.append(f'$p{idx}')
                    patterns[idx] = pat
                else:
                    names.append(self._eat(IDENT).value)
                has_rest = True
                if self._peek(PUNCT, ','):
                    self.pos += 1
                break
            if self._peek(PUNCT, '[') or self._peek(PUNCT, '{'):
                pat = self._binding_pattern()
                names.append(f'$p{idx}')
                patterns[idx] = pat
            else:
                names.append(self._eat(IDENT).value)
            if self._peek(OP, '='):
                self.pos += 1
                defaults[idx] = self._assign_expr()
            if self._peek(PUNCT, ','):
                self.pos += 1
            idx += 1
        self._eat(PUNCT, ')')
        return names, defaults, has_rest, patterns

    # ----- class declarations -----

    def _class_decl(self, as_expr: bool = False) -> ASTNode:
        self._eat(KEYWORD, 'class')
        name = None
        if self._peek(IDENT):
            name = self._eat(IDENT).value
        super_class = None
        if self._peek(KEYWORD, 'extends'):
            self.pos += 1
            super_class = self._call_or_member()
        self._eat(PUNCT, '{')
        methods = []
        while not self._peek(PUNCT, '}') and not self._peek(EOF):
            if self._peek(PUNCT, ';'):
                self.pos += 1
                continue
            is_static = False
            kind = 'method'
            # 'static' contextual keyword
            if self._peek(IDENT) and self._cur().value == 'static':
                nt = self._peek2()
                if nt.type in (IDENT, STRING, NUMBER, KEYWORD) or \
                   (nt.type == PUNCT and nt.value in ('[', '{')):
                    is_static = True
                    self.pos += 1  # consume 'static'
            # 'get' / 'set' accessor
            if self._peek(IDENT) and self._cur().value in ('get', 'set'):
                nt = self._peek2()
                # Only treat as accessor if followed by a property name, not '('
                if nt.type in (IDENT, STRING, NUMBER, KEYWORD) or \
                   (nt.type == PUNCT and nt.value == '['):
                    kind = self._cur().value  # 'get' or 'set'
                    self.pos += 1
            # Method key
            if self._peek(PUNCT, '['):
                self.pos += 1
                key_expr = self._assign_expr()
                self._eat(PUNCT, ']')
                key = key_expr  # computed — ASTNode
            elif self._peek(IDENT) or (self._peek(KEYWORD) and self._cur().value in (
                'constructor', 'static', 'get', 'set', 'async',
            )):
                key = self._cur().value
                self.pos += 1
            elif self._peek(STRING):
                key = self._eat(STRING).value
            elif self._peek(NUMBER):
                key = str(self._eat(NUMBER).value)
            else:
                self.pos += 1
                continue
            if key == 'constructor' and not is_static:
                kind = 'constructor'
            params, defaults, has_rest, param_patterns = self._param_list()
            body = self._block()
            methods.append({
                'kind': kind,
                'static': is_static,
                'key': key,
                'params': params,
                'param_defaults': defaults,
                'param_rest': has_rest,
                'param_patterns': param_patterns,
                'body': body,
            })
        self._eat(PUNCT, '}')
        return _node('ClassDecl', name=name, super_class=super_class,
                     methods=methods, as_expr=as_expr)

    # ----- control flow -----

    def _return_stmt(self) -> ASTNode:
        self._eat(KEYWORD, 'return')
        if self._peek(PUNCT, ';') or self._peek(PUNCT, '}') or self._peek(EOF):
            self._eat_semi()
            return _node('Return', value=None)
        value = self._expression()
        self._eat_semi()
        return _node('Return', value=value)

    def _if_stmt(self) -> ASTNode:
        self._eat(KEYWORD, 'if')
        self._eat(PUNCT, '(')
        cond = self._expression()
        self._eat(PUNCT, ')')
        then = self._statement()
        else_ = None
        if self._peek(KEYWORD, 'else'):
            self.pos += 1
            else_ = self._statement()
        return _node('If', cond=cond, then=then, else_=else_)

    def _for_stmt(self) -> ASTNode:
        self._eat(KEYWORD, 'for')
        self._eat(PUNCT, '(')

        # Detect for-in / for-of (with optional destructuring)
        saved = self.pos
        if self._peek(KEYWORD) and self._cur().value in ('var', 'let', 'const'):
            kind = self._eat(KEYWORD).value
            # Destructuring pattern in for-of
            if self._peek(PUNCT, '[') or self._peek(PUNCT, '{'):
                pattern = self._binding_pattern()
                if self._peek(KEYWORD, 'of') or self._peek(KEYWORD, 'in'):
                    loop_type = self._eat(KEYWORD).value
                    iterable = self._expression()
                    self._eat(PUNCT, ')')
                    body = self._statement()
                    return _node('ForIn', kind=kind, pattern=pattern, name=None,
                                 loop_type=loop_type, iterable=iterable, body=body)
                self.pos = saved
            elif self._peek(IDENT):
                name = self._eat(IDENT).value
                if self._peek(KEYWORD, 'in') or self._peek(KEYWORD, 'of'):
                    loop_type = self._eat(KEYWORD).value
                    iterable = self._expression()
                    self._eat(PUNCT, ')')
                    body = self._statement()
                    return _node('ForIn', kind=kind, pattern=None, name=name,
                                 loop_type=loop_type, iterable=iterable, body=body)
                self.pos = saved
            else:
                self.pos = saved

        # Standard for loop
        init = None
        if not self._peek(PUNCT, ';'):
            if self._peek(KEYWORD) and self._cur().value in ('var', 'let', 'const'):
                init = self._var_decl_no_semi()
            else:
                init = self._expression()
        self._eat(PUNCT, ';')
        cond = None
        if not self._peek(PUNCT, ';'):
            cond = self._expression()
        self._eat(PUNCT, ';')
        update = None
        if not self._peek(PUNCT, ')'):
            update = self._expression()
        self._eat(PUNCT, ')')
        body = self._statement()
        return _node('For', init=init, cond=cond, update=update, body=body)

    def _while_stmt(self) -> ASTNode:
        self._eat(KEYWORD, 'while')
        self._eat(PUNCT, '(')
        cond = self._expression()
        self._eat(PUNCT, ')')
        body = self._statement()
        return _node('While', cond=cond, body=body)

    def _do_while_stmt(self) -> ASTNode:
        self._eat(KEYWORD, 'do')
        body = self._statement()
        self._eat(KEYWORD, 'while')
        self._eat(PUNCT, '(')
        cond = self._expression()
        self._eat(PUNCT, ')')
        self._eat_semi()
        return _node('DoWhile', cond=cond, body=body)

    def _try_stmt(self) -> ASTNode:
        self._eat(KEYWORD, 'try')
        block = self._block()
        catch_param = None
        catch_body = None
        finally_body = None
        if self._peek(KEYWORD, 'catch'):
            self.pos += 1
            if self._peek(PUNCT, '('):
                self._eat(PUNCT, '(')
                # Catch param may be a destructuring pattern
                if self._peek(PUNCT, '[') or self._peek(PUNCT, '{'):
                    catch_param = self._binding_pattern()
                else:
                    catch_param = _node('BindingIdent', name=self._eat(IDENT).value)
                self._eat(PUNCT, ')')
            catch_body = self._block()
        if self._peek(KEYWORD, 'finally'):
            self.pos += 1
            finally_body = self._block()
        return _node('Try', block=block, catch_param=catch_param,
                     catch_body=catch_body, finally_body=finally_body)

    def _throw_stmt(self) -> ASTNode:
        self._eat(KEYWORD, 'throw')
        value = self._expression()
        self._eat_semi()
        return _node('Throw', value=value)

    def _switch_stmt(self) -> ASTNode:
        self._eat(KEYWORD, 'switch')
        self._eat(PUNCT, '(')
        disc = self._expression()
        self._eat(PUNCT, ')')
        self._eat(PUNCT, '{')
        cases = []
        while not self._peek(PUNCT, '}') and not self._peek(EOF):
            if self._peek(KEYWORD, 'case'):
                self.pos += 1
                test = self._expression()
                self._eat(OP, ':')
                stmts = []
                while not self._peek(KEYWORD, 'case') and not self._peek(KEYWORD, 'default') \
                        and not self._peek(PUNCT, '}') and not self._peek(EOF):
                    s = self._statement()
                    if s:
                        stmts.append(s)
                cases.append(_node('Case', test=test, body=stmts))
            elif self._peek(KEYWORD, 'default'):
                self.pos += 1
                self._eat(OP, ':')
                stmts = []
                while not self._peek(KEYWORD, 'case') and not self._peek(PUNCT, '}') and not self._peek(EOF):
                    s = self._statement()
                    if s:
                        stmts.append(s)
                cases.append(_node('Default', body=stmts))
            else:
                self.pos += 1  # skip unexpected token
        self._eat(PUNCT, '}')
        return _node('Switch', disc=disc, cases=cases)

    # ----- expressions -----

    def _expression(self) -> ASTNode:
        """Comma expression (lowest precedence)."""
        expr = self._assign_expr()
        while self._peek(PUNCT, ','):
            self.pos += 1
            right = self._assign_expr()
            expr = _node('Comma', left=expr, right=right)
        return expr

    def _assign_expr(self) -> ASTNode:
        # Single-param arrow without parens: ident =>
        if self._peek(IDENT) and self._peek2().type == OP and self._peek2().value == '=>':
            param = self._eat(IDENT).value
            self.pos += 1  # skip '=>'
            if self._peek(PUNCT, '{'):
                body = self._block()
            else:
                body = _node('Return', value=self._assign_expr())
            return _node('FuncDecl', name=None, params=[param], body=body,
                         param_defaults={}, param_rest=False, param_patterns={},
                         is_generator=False)

        left = self._ternary()
        t = self._cur()
        if t.type == OP and t.value in ('=', '+=', '-=', '*=', '/=', '%=',
                                         '**=', '&&=', '||=', '??=',
                                         '<<=', '>>=', '>>>='):
            op = t.value
            self.pos += 1
            right = self._assign_expr()
            return _node('Assign', op=op, left=left, right=right)
        return left

    def _ternary(self) -> ASTNode:
        expr = self._or_expr()
        if self._peek(OP, '?'):
            self.pos += 1
            then = self._assign_expr()
            self._eat(OP, ':')
            else_ = self._assign_expr()
            return _node('Ternary', cond=expr, then=then, else_=else_)
        return expr

    def _or_expr(self) -> ASTNode:
        left = self._and_expr()
        while self._peek(OP, '||') or self._peek(OP, '??'):
            op = self._eat(OP).value
            right = self._and_expr()
            left = _node('BinOp', op=op, left=left, right=right)
        return left

    def _and_expr(self) -> ASTNode:
        left = self._bitor_expr()
        while self._peek(OP, '&&'):
            self.pos += 1
            right = self._bitor_expr()
            left = _node('BinOp', op='&&', left=left, right=right)
        return left

    def _bitor_expr(self) -> ASTNode:
        left = self._bitxor_expr()
        while self._peek(OP, '|'):
            self.pos += 1
            right = self._bitxor_expr()
            left = _node('BinOp', op='|', left=left, right=right)
        return left

    def _bitxor_expr(self) -> ASTNode:
        left = self._bitand_expr()
        while self._peek(OP, '^'):
            self.pos += 1
            right = self._bitand_expr()
            left = _node('BinOp', op='^', left=left, right=right)
        return left

    def _bitand_expr(self) -> ASTNode:
        left = self._equality()
        while self._peek(OP, '&'):
            self.pos += 1
            right = self._equality()
            left = _node('BinOp', op='&', left=left, right=right)
        return left

    def _equality(self) -> ASTNode:
        left = self._comparison()
        while self._cur().type == OP and self._cur().value in ('==', '!=', '===', '!=='):
            op = self._eat(OP).value
            right = self._comparison()
            left = _node('BinOp', op=op, left=left, right=right)
        return left

    def _comparison(self) -> ASTNode:
        left = self._shift()
        while True:
            t = self._cur()
            if t.type == OP and t.value in ('<', '>', '<=', '>='):
                op = self._eat(OP).value
                right = self._shift()
                left = _node('BinOp', op=op, left=left, right=right)
            elif t.type == KEYWORD and t.value == 'instanceof':
                self.pos += 1
                right = self._shift()
                left = _node('BinOp', op='instanceof', left=left, right=right)
            elif t.type == KEYWORD and t.value == 'in':
                self.pos += 1
                right = self._shift()
                left = _node('BinOp', op='in', left=left, right=right)
            else:
                break
        return left

    def _shift(self) -> ASTNode:
        left = self._additive()
        while self._cur().type == OP and self._cur().value in ('<<', '>>', '>>>'):
            op = self._eat(OP).value
            right = self._additive()
            left = _node('BinOp', op=op, left=left, right=right)
        return left

    def _additive(self) -> ASTNode:
        left = self._multiplicative()
        while self._cur().type == OP and self._cur().value in ('+', '-'):
            op = self._eat(OP).value
            right = self._multiplicative()
            left = _node('BinOp', op=op, left=left, right=right)
        return left

    def _multiplicative(self) -> ASTNode:
        left = self._unary()
        while self._cur().type == OP and self._cur().value in ('*', '/', '%', '**'):
            op = self._eat(OP).value
            right = self._unary()
            left = _node('BinOp', op=op, left=left, right=right)
        return left

    def _unary(self) -> ASTNode:
        t = self._cur()
        if t.type == OP and t.value in ('!', '-', '+', '~'):
            self.pos += 1
            operand = self._unary()
            return _node('UnaryOp', op=t.value, operand=operand)
        if t.type == KEYWORD and t.value == 'typeof':
            self.pos += 1
            operand = self._unary()
            return _node('UnaryOp', op='typeof', operand=operand)
        if t.type == KEYWORD and t.value == 'void':
            self.pos += 1
            operand = self._unary()
            return _node('UnaryOp', op='void', operand=operand)
        if t.type == KEYWORD and t.value == 'delete':
            self.pos += 1
            operand = self._unary()
            return _node('UnaryOp', op='delete', operand=operand)
        if t.type == KEYWORD and t.value == 'await':
            self.pos += 1
            operand = self._unary()
            return _node('Await', operand=operand)
        if t.type == OP and t.value in ('++', '--'):
            self.pos += 1
            operand = self._unary()
            return _node('UpdatePre', op=t.value, operand=operand)
        if t.type == KEYWORD and t.value == 'new':
            return self._new_expr()
        return self._postfix()

    def _postfix(self) -> ASTNode:
        expr = self._call_or_member()
        t = self._cur()
        if t.type == OP and t.value in ('++', '--'):
            self.pos += 1
            return _node('UpdatePost', op=t.value, operand=expr)
        return expr

    def _new_expr(self) -> ASTNode:
        self._eat(KEYWORD, 'new')
        callee = self._call_or_member()
        args = []
        if self._peek(PUNCT, '('):
            args = self._arguments()
        return _node('New', callee=callee, args=args)

    def _call_or_member(self) -> ASTNode:
        expr = self._primary()
        while True:
            if self._peek(PUNCT, '('):
                args = self._arguments()
                expr = _node('Call', callee=expr, args=args, optional=False)
            elif self._peek(PUNCT, '.'):
                self.pos += 1
                prop = self._eat_ident_name()
                expr = _node('Member', obj=expr, prop=prop, computed=False, optional=False)
            elif self._peek(PUNCT, '['):
                self.pos += 1
                prop = self._expression()
                self._eat(PUNCT, ']')
                expr = _node('Member', obj=expr, prop=prop, computed=True, optional=False)
            elif self._peek(OP, '?.'):
                self.pos += 1
                if self._peek(PUNCT, '('):
                    args = self._arguments()
                    expr = _node('Call', callee=expr, args=args, optional=True)
                elif self._peek(PUNCT, '['):
                    self.pos += 1
                    prop = self._expression()
                    self._eat(PUNCT, ']')
                    expr = _node('Member', obj=expr, prop=prop, computed=True, optional=True)
                else:
                    prop = self._eat_ident_name()
                    expr = _node('Member', obj=expr, prop=prop, computed=False, optional=True)
            else:
                break
        return expr

    def _arguments(self) -> list:
        self._eat(PUNCT, '(')
        args = []
        while not self._peek(PUNCT, ')') and not self._peek(EOF):
            if self._peek(OP, '...'):
                self.pos += 1
                args.append(_node('Spread', arg=self._assign_expr()))
            else:
                args.append(self._assign_expr())
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, ')')
        return args

    def _primary(self) -> ASTNode:
        t = self._cur()

        if t.type == NUMBER:
            self.pos += 1
            return _node('Literal', value=t.value)

        if t.type == STRING:
            self.pos += 1
            return _node('Literal', value=t.value)

        if t.type == TEMPLATE:
            self.pos += 1
            # value is list of ('str', text) | ('expr', code)
            nodes = []
            for kind, content in t.value:
                if kind == 'str':
                    nodes.append(_node('Literal', value=content))
                else:
                    from js.lexer import Lexer as _Lexer
                    sub_toks = _Lexer(content).tokenize()
                    sub_expr = Parser(sub_toks)._assign_expr()
                    nodes.append(sub_expr)
            return _node('TemplateLiteral', nodes=nodes)

        if t.type == KEYWORD:
            if t.value == 'true':
                self.pos += 1
                return _node('Literal', value=True)
            if t.value == 'false':
                self.pos += 1
                return _node('Literal', value=False)
            if t.value == 'null':
                self.pos += 1
                return _node('Literal', value=None)
            if t.value == 'undefined':
                self.pos += 1
                return _node('Literal', value=_UNDEF)
            if t.value == 'this':
                self.pos += 1
                return _node('This')
            if t.value == 'super':
                self.pos += 1
                return _node('Super')
            if t.value == 'function':
                return self._function_decl()
            if t.value == 'class':
                return self._class_decl(as_expr=True)
            if t.value == 'new':
                return self._new_expr()
            if t.value == 'typeof':
                self.pos += 1
                operand = self._unary()
                return _node('UnaryOp', op='typeof', operand=operand)

        if t.type == IDENT:
            self.pos += 1
            return _node('Ident', name=t.value)

        if t.type == PUNCT:
            if t.value == '(':
                self.pos += 1
                # Detect arrow function: () => or (params) =>
                if self._peek(PUNCT, ')'):
                    # () => ...
                    self.pos += 1
                    if self._peek(OP, '=>'):
                        self.pos += 1
                        if self._peek(PUNCT, '{'):
                            body = self._block()
                        else:
                            body = _node('Return', value=self._assign_expr())
                        return _node('FuncDecl', name=None, params=[], body=body,
                                     param_defaults={}, param_rest=False,
                                     param_patterns={}, is_generator=False)
                    # Not an arrow — it was an empty parens expression (unusual)
                    return _node('Literal', value=_UNDEF)

                expr = self._expression()
                self._eat(PUNCT, ')')
                # Check for arrow =>
                if self._peek(OP, '=>'):
                    self.pos += 1
                    params, defaults, has_rest, param_patterns = \
                        self._extract_arrow_params(expr)
                    if self._peek(PUNCT, '{'):
                        body = self._block()
                    else:
                        body = _node('Return', value=self._assign_expr())
                    return _node('FuncDecl', name=None, params=params, body=body,
                                 param_defaults=defaults, param_rest=has_rest,
                                 param_patterns=param_patterns, is_generator=False)
                return expr

            if t.value == '[':
                return self._array_literal()

            if t.value == '{':
                return self._object_literal()

        # Fallback — skip token and return undefined
        self.pos += 1
        return _node('Literal', value=_UNDEF)

    # ----- arrow-function param extraction -----

    def _extract_arrow_params(self, expr) -> tuple:
        """Turn a grouped expression into arrow-function parameter lists."""
        names: list[str] = []
        defaults: dict = {}
        has_rest = False
        patterns: dict = {}
        self._flatten_arrow_expr(expr, names, defaults, patterns)
        return names, defaults, has_rest, patterns

    def _flatten_arrow_expr(self, node, names, defaults, patterns):
        if node.type == 'Comma':
            self._flatten_arrow_expr(node.data['left'], names, defaults, patterns)
            self._flatten_arrow_expr(node.data['right'], names, defaults, patterns)
        elif node.type == 'Ident':
            names.append(node.data['name'])
        elif node.type == 'Assign':
            # default: (a = 1) in param list
            idx = len(names)
            if node.data['left'].type == 'Ident':
                names.append(node.data['left'].data['name'])
                defaults[idx] = node.data['right']
            else:
                names.append(f'$p{idx}')
                patterns[idx] = node.data['left']
                defaults[idx] = node.data['right']
        elif node.type in ('ArrayPattern', 'ObjectPattern', 'BindingIdent'):
            idx = len(names)
            if node.type == 'BindingIdent':
                names.append(node.data['name'])
            else:
                names.append(f'$p{idx}')
                patterns[idx] = node
        elif node.type == 'Spread':
            # (...rest)
            inner = node.data['arg']
            if inner.type == 'Ident':
                names.append(inner.data['name'])
        else:
            names.append(f'$p{len(names)}')

    # ----- literals -----

    def _array_literal(self) -> ASTNode:
        self._eat(PUNCT, '[')
        elements = []
        while not self._peek(PUNCT, ']') and not self._peek(EOF):
            if self._peek(PUNCT, ','):
                elements.append(_node('Literal', value=_UNDEF))
                self.pos += 1
                continue
            if self._peek(OP, '...'):
                self.pos += 1
                elements.append(_node('Spread', arg=self._assign_expr()))
            else:
                elements.append(self._assign_expr())
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, ']')
        return _node('Array', elements=elements)

    def _object_literal(self) -> ASTNode:
        self._eat(PUNCT, '{')
        props = []
        while not self._peek(PUNCT, '}') and not self._peek(EOF):
            # Spread: { ...obj }
            if self._peek(OP, '...'):
                self.pos += 1
                val = self._assign_expr()
                props.append((_node('SpreadProp'), val))
                if self._peek(PUNCT, ','):
                    self.pos += 1
                continue

            # Computed key: { [expr]: val }
            if self._peek(PUNCT, '['):
                self.pos += 1
                key_expr = self._assign_expr()
                self._eat(PUNCT, ']')
                self._eat(OP, ':')
                val = self._assign_expr()
                props.append((_node('Computed', expr=key_expr), val))
                if self._peek(PUNCT, ','):
                    self.pos += 1
                continue

            # Key from identifier, keyword, string, or number
            t = self._cur()
            if t.type == IDENT or (t.type == KEYWORD and t.value not in (
                'var', 'let', 'const', 'function', 'return', 'if', 'else',
                'for', 'while', 'do', 'break', 'continue', 'new', 'in', 'of',
                'null', 'undefined', 'true', 'false', 'try', 'catch', 'finally',
                'throw', 'switch', 'case', 'default', 'delete', 'void',
                'class', 'extends', 'super',
            )):
                key = t.value
                self.pos += 1
                # Shorthand property: { foo } or method: { foo() {} }
                if self._peek(PUNCT, ',') or self._peek(PUNCT, '}'):
                    props.append((key, _node('Ident', name=key)))
                    if self._peek(PUNCT, ','):
                        self.pos += 1
                    continue
                if self._peek(PUNCT, '('):
                    # Method shorthand: { foo(...) {} }
                    params, defaults, has_rest, param_patterns = self._param_list()
                    body = self._block()
                    fn = _node('FuncDecl', name=key, params=params, body=body,
                               param_defaults=defaults, param_rest=has_rest,
                               param_patterns=param_patterns, is_generator=False)
                    props.append((key, fn))
                    if self._peek(PUNCT, ','):
                        self.pos += 1
                    continue
            elif t.type == STRING:
                key = t.value
                self.pos += 1
            elif t.type == NUMBER:
                key = str(t.value)
                self.pos += 1
            else:
                self.pos += 1
                continue

            self._eat(OP, ':')
            val = self._assign_expr()
            props.append((key, val))
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, '}')
        return _node('Object', props=props)


class _Undefined:
    """Sentinel for JavaScript undefined."""
    _instance = None
    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance
    def __repr__(self):
        return 'undefined'
    def __bool__(self):
        return False
    def __str__(self):
        return 'undefined'

_UNDEF = _Undefined()
