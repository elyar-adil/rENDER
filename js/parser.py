"""JavaScript recursive-descent parser for rENDER browser engine.

Parses a subset of JavaScript (ES2020) into an AST suitable for interpretation.
"""
from js.lexer import Token, KEYWORD, IDENT, NUMBER, STRING, PUNCT, OP, EOF, TEMPLATE
from js.ast import ASTNode, _node
from js.types import _UNDEF


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

    def _peek2(self, offset=1, type_=None, value=None) -> bool:
        idx = self.pos + offset
        if idx >= len(self.tokens):
            return False
        t = self.tokens[idx]
        if type_ and t.type != type_:
            return False
        if value is not None and t.value != value:
            return False
        return True

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
                    label = self._eat(IDENT).value
                self._eat_semi()
                return _node('Break', label=label)
            if kw == 'continue':
                self.pos += 1
                label = None
                if self._peek(IDENT) and not self._peek(PUNCT, ';'):
                    label = self._eat(IDENT).value
                self._eat_semi()
                return _node('Continue', label=label)
            if kw == 'try':
                return self._try_stmt()
            if kw == 'throw':
                return self._throw_stmt()
            if kw == 'switch':
                return self._switch_stmt()
            if kw in ('import', 'export'):
                return self._skip_module_decl()
            if kw in ('async',):
                # async function
                if self._peek2(1, KEYWORD, 'function'):
                    return self._async_function_decl()

        if t.type == PUNCT and t.value == '{':
            return self._block()

        # Labeled statement: ident ':'
        if t.type == IDENT and self._peek2(1, OP, ':'):
            label = self._eat(IDENT).value
            self._eat(OP, ':')
            body = self._statement()
            return _node('Labeled', label=label, body=body)

        # Expression statement
        expr = self._expression()
        self._eat_semi()
        return _node('ExprStmt', expr=expr)

    def _skip_module_decl(self):
        """Skip import/export declarations (stubs)."""
        while not self._peek(PUNCT, ';') and not self._peek(EOF) and not self._peek(PUNCT, '{'):
            if self._peek(KEYWORD, 'from'):
                self.pos += 1
                if self._peek(STRING):
                    self.pos += 1
                break
            self.pos += 1
        self._eat_semi()
        return None

    def _block(self) -> ASTNode:
        self._eat(PUNCT, '{')
        body = []
        while not self._peek(PUNCT, '}') and not self._peek(EOF):
            stmt = self._statement()
            if stmt:
                body.append(stmt)
        self._eat(PUNCT, '}')
        return _node('Block', body=body)

    # ----- var decl with destructuring -----

    def _var_decl(self) -> ASTNode:
        kind = self._eat(KEYWORD).value
        decls = []
        while True:
            decl = self._var_declarator()
            decls.append(decl)
            if self._peek(PUNCT, ','):
                self.pos += 1
            else:
                break
        self._eat_semi()
        return _node('VarDecl', kind=kind, decls=decls)

    def _var_declarator(self):
        """Returns (name_or_pattern, init_node) where name_or_pattern is
        either a str (simple) or an ASTNode (ObjectPattern / ArrayPattern)."""
        t = self._cur()
        if t.type == PUNCT and t.value == '{':
            pattern = self._object_pattern()
        elif t.type == PUNCT and t.value == '[':
            pattern = self._array_pattern()
        else:
            name = self._eat(IDENT).value
            init = None
            if self._peek(OP, '='):
                self.pos += 1
                init = self._assign_expr()
            return (name, init)
        init = None
        if self._peek(OP, '='):
            self.pos += 1
            init = self._assign_expr()
        return (pattern, init)

    def _object_pattern(self) -> ASTNode:
        """Parse { a, b: c, ...rest } destructuring pattern."""
        self._eat(PUNCT, '{')
        props = []
        rest = None
        while not self._peek(PUNCT, '}') and not self._peek(EOF):
            if self._peek(OP, '...'):
                self.pos += 1
                rest = self._eat(IDENT).value
                break
            key = self._eat(IDENT).value
            if self._peek(OP, ':'):
                self.pos += 1
                # Value can be a nested pattern or ident with default
                value = self._binding_element()
            else:
                # Shorthand { key } or { key = default }
                default = None
                if self._peek(OP, '='):
                    self.pos += 1
                    default = self._assign_expr()
                value = _node('BindingDefault', name=key, default=default)
            props.append((key, value))
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, '}')
        return _node('ObjectPattern', props=props, rest=rest)

    def _array_pattern(self) -> ASTNode:
        """Parse [a, b, ...rest] destructuring pattern."""
        self._eat(PUNCT, '[')
        elements = []
        rest = None
        while not self._peek(PUNCT, ']') and not self._peek(EOF):
            if self._peek(PUNCT, ','):
                elements.append(None)  # hole
                self.pos += 1
                continue
            if self._peek(OP, '...'):
                self.pos += 1
                rest = self._eat(IDENT).value
                break
            elements.append(self._binding_element())
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, ']')
        return _node('ArrayPattern', elements=elements, rest=rest)

    def _binding_element(self) -> ASTNode:
        """A single binding in a pattern: ident, pattern, or ident=default."""
        t = self._cur()
        if t.type == PUNCT and t.value == '{':
            pat = self._object_pattern()
            default = None
            if self._peek(OP, '='):
                self.pos += 1
                default = self._assign_expr()
            if default is not None:
                return _node('BindingDefault', name=pat, default=default)
            return pat
        if t.type == PUNCT and t.value == '[':
            pat = self._array_pattern()
            default = None
            if self._peek(OP, '='):
                self.pos += 1
                default = self._assign_expr()
            if default is not None:
                return _node('BindingDefault', name=pat, default=default)
            return pat
        name = self._eat(IDENT).value
        default = None
        if self._peek(OP, '='):
            self.pos += 1
            default = self._assign_expr()
        return _node('BindingDefault', name=name, default=default)

    # ----- function decl -----

    def _function_decl(self, is_async=False) -> ASTNode:
        self._eat(KEYWORD, 'function')
        is_generator = False
        if self._peek(OP, '*'):
            self.pos += 1
            is_generator = True
        name = None
        if self._peek(IDENT):
            name = self._eat(IDENT).value
        params, defaults, rest, patterns = self._param_list_full()
        body = self._block()
        return _node('FuncDecl', name=name, params=params, body=body,
                     param_defaults=defaults, param_rest=rest,
                     param_patterns=patterns, is_async=is_async,
                     is_generator=is_generator)

    def _async_function_decl(self) -> ASTNode:
        self._eat(KEYWORD, 'async')
        return self._function_decl(is_async=True)

    def _param_list_full(self):
        """Parse parameter list. Returns (params, defaults, rest, patterns).

        params    : list of str (simple names, or '__pattern__N' for destructured)
        defaults  : dict mapping param index -> default ASTNode
        rest      : str | None  (rest param name)
        patterns  : dict mapping param index -> ASTNode (for destructured params)
        """
        self._eat(PUNCT, '(')
        params = []
        defaults = {}
        rest = None
        patterns = {}
        idx = 0
        while not self._peek(PUNCT, ')') and not self._peek(EOF):
            if self._peek(OP, '...'):
                self.pos += 1
                rest = self._eat(IDENT).value
                break
            # Destructuring param
            if self._peek(PUNCT, '{'):
                pat = self._object_pattern()
                placeholder = f'__pattern__{idx}'
                params.append(placeholder)
                patterns[idx] = pat
                if self._peek(OP, '='):
                    self.pos += 1
                    defaults[idx] = self._assign_expr()
            elif self._peek(PUNCT, '['):
                pat = self._array_pattern()
                placeholder = f'__pattern__{idx}'
                params.append(placeholder)
                patterns[idx] = pat
                if self._peek(OP, '='):
                    self.pos += 1
                    defaults[idx] = self._assign_expr()
            else:
                name = self._eat(IDENT).value
                params.append(name)
                if self._peek(OP, '='):
                    self.pos += 1
                    defaults[idx] = self._assign_expr()
            idx += 1
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, ')')
        return params, defaults, rest, patterns

    def _param_list(self) -> list[str]:
        """Simple param list — used by legacy callers."""
        params, _, _, _ = self._param_list_full()
        return params

    # ----- class decl -----

    def _class_decl(self, as_expr=False) -> ASTNode:
        self._eat(KEYWORD, 'class')
        name = None
        if self._peek(IDENT):
            name = self._eat(IDENT).value
        superclass = None
        if self._peek(KEYWORD, 'extends'):
            self.pos += 1
            superclass = self._left_hand_side()
        self._eat(PUNCT, '{')
        methods = []
        while not self._peek(PUNCT, '}') and not self._peek(EOF):
            if self._peek(PUNCT, ';'):
                self.pos += 1
                continue
            is_static = False
            if self._peek(KEYWORD, 'static'):
                self.pos += 1
                is_static = True
            kind = 'method'
            if self._peek(KEYWORD, 'get') and not self._peek2(1, PUNCT, '('):
                self.pos += 1
                kind = 'get'
            elif self._peek(KEYWORD, 'set') and not self._peek2(1, PUNCT, '('):
                self.pos += 1
                kind = 'set'
            is_async = False
            if self._peek(KEYWORD, 'async'):
                self.pos += 1
                is_async = True
            is_generator = False
            if self._peek(OP, '*'):
                self.pos += 1
                is_generator = True
            # Method name
            t = self._cur()
            if t.type == IDENT or t.type == KEYWORD:
                mname = t.value
                self.pos += 1
            elif t.type == STRING:
                mname = t.value
                self.pos += 1
            elif t.type == NUMBER:
                mname = str(t.value)
                self.pos += 1
            elif t.type == PUNCT and t.value == '[':
                self.pos += 1
                mname_expr = self._assign_expr()
                self._eat(PUNCT, ']')
                mname = _node('Computed', expr=mname_expr)
            else:
                self.pos += 1
                continue
            params, param_defaults, param_rest, param_patterns = self._param_list_full()
            body = self._block()
            methods.append(_node('ClassMethod', name=mname, params=params, body=body,
                                 is_static=is_static, kind=kind, is_async=is_async,
                                 param_defaults=param_defaults, param_rest=param_rest,
                                 param_patterns=param_patterns))
        self._eat(PUNCT, '}')
        return _node('ClassDecl', name=name, superclass=superclass,
                     methods=methods, as_expr=as_expr)

    # ----- other stmts -----

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
        is_await = False
        if self._peek(KEYWORD, 'await'):
            self.pos += 1
            is_await = True
        self._eat(PUNCT, '(')

        # Check for for..in / for..of  (with possible destructuring)
        saved = self.pos
        if self._peek(KEYWORD) and self._cur().value in ('var', 'let', 'const'):
            kind = self._eat(KEYWORD).value
            # pattern or ident
            if self._peek(PUNCT, '{') or self._peek(PUNCT, '['):
                if self._peek(PUNCT, '{'):
                    pattern = self._object_pattern()
                else:
                    pattern = self._array_pattern()
                if self._peek(KEYWORD, 'in') or self._peek(KEYWORD, 'of'):
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
                    return _node('ForIn', kind=kind, name=name, pattern=None,
                                 loop_type=loop_type, iterable=iterable, body=body)
                self.pos = saved  # not for-in/of, reparse
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

    def _var_decl_no_semi(self) -> ASTNode:
        kind = self._eat(KEYWORD).value
        decls = []
        while True:
            decl = self._var_declarator()
            decls.append(decl)
            if self._peek(PUNCT, ','):
                self.pos += 1
            else:
                break
        return _node('VarDecl', kind=kind, decls=decls)

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
                catch_param = self._eat(IDENT).value
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
                self.pos += 1
        self._eat(PUNCT, '}')
        return _node('Switch', disc=disc, cases=cases)

    # ----- expressions -----

    def _expression(self) -> ASTNode:
        expr = self._assign_expr()
        while self._peek(PUNCT, ','):
            self.pos += 1
            right = self._assign_expr()
            expr = _node('Comma', left=expr, right=right)
        return expr

    def _assign_expr(self) -> ASTNode:
        left = self._ternary()
        t = self._cur()
        if t.type == OP and t.value in ('=', '+=', '-=', '*=', '/=', '%=',
                                         '**=', '&&=', '||=', '??=',
                                         '<<=', '>>=', '>>>=', '&=', '|=', '^='):
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
            return _node('Await', value=operand)
        if t.type == KEYWORD and t.value == 'yield':
            self.pos += 1
            operand = None
            if not self._peek(PUNCT, ';') and not self._peek(PUNCT, '}') and not self._peek(EOF):
                operand = self._assign_expr()
            return _node('Yield', value=operand)
        if t.type == OP and t.value in ('++', '--'):
            self.pos += 1
            operand = self._unary()
            return _node('UpdatePre', op=t.value, operand=operand)
        if t.type == KEYWORD and t.value == 'new':
            return self._new_expr()
        return self._postfix()

    def _postfix(self) -> ASTNode:
        expr = self._left_hand_side()
        t = self._cur()
        if t.type == OP and t.value in ('++', '--'):
            self.pos += 1
            return _node('UpdatePost', op=t.value, operand=expr)
        return expr

    def _new_expr(self) -> ASTNode:
        self._eat(KEYWORD, 'new')
        callee = self._left_hand_side()
        args = []
        if self._peek(PUNCT, '('):
            args = self._arguments()
        return _node('New', callee=callee, args=args)

    def _left_hand_side(self) -> ASTNode:
        """Call/member expressions including optional chaining."""
        expr = self._primary()
        while True:
            if self._peek(PUNCT, '('):
                args = self._arguments()
                expr = _node('Call', callee=expr, args=args, optional=False)
            elif self._peek(PUNCT, '.'):
                self.pos += 1
                prop = self._eat_ident_or_keyword()
                expr = _node('Member', obj=expr, prop=prop, computed=False, optional=False)
            elif self._peek(PUNCT, '['):
                self.pos += 1
                prop = self._expression()
                self._eat(PUNCT, ']')
                expr = _node('Member', obj=expr, prop=prop, computed=True, optional=False)
            elif self._peek(OP, '?.'):
                self.pos += 1
                # Optional chaining: ?.prop / ?.[expr] / ?.(args)
                if self._peek(PUNCT, '['):
                    self.pos += 1
                    prop = self._expression()
                    self._eat(PUNCT, ']')
                    expr = _node('Member', obj=expr, prop=prop, computed=True, optional=True)
                elif self._peek(PUNCT, '('):
                    args = self._arguments()
                    expr = _node('Call', callee=expr, args=args, optional=True)
                else:
                    prop = self._eat_ident_or_keyword()
                    expr = _node('Member', obj=expr, prop=prop, computed=False, optional=True)
            else:
                break
        return expr

    def _eat_ident_or_keyword(self) -> str:
        """Eat an identifier or keyword used as property name."""
        t = self._cur()
        if t.type in (IDENT, KEYWORD):
            self.pos += 1
            return t.value
        raise SyntaxError(f'Expected identifier, got {t}')

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
            return self._make_template_literal(t.value)

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
            if t.value == 'async':
                # async arrow: async (params) => body  or  async x => body
                saved = self.pos
                self.pos += 1
                if self._peek(KEYWORD, 'function'):
                    return self._function_decl(is_async=True)
                # Try async arrow
                if self._peek(IDENT):
                    name = self._eat(IDENT).value
                    if self._peek(OP, '=>'):
                        self.pos += 1
                        if self._peek(PUNCT, '{'):
                            body = self._block()
                        else:
                            body = _node('Return', value=self._assign_expr())
                        return _node('FuncDecl', name=None, params=[name], body=body,
                                     param_defaults={}, param_rest=None, param_patterns={},
                                     is_async=True, is_generator=False)
                self.pos = saved
                self.pos += 1
                return _node('Ident', name='async')

        if t.type == IDENT:
            self.pos += 1
            name = t.value
            # Arrow: ident =>
            if self._peek(OP, '=>'):
                self.pos += 1
                if self._peek(PUNCT, '{'):
                    body = self._block()
                else:
                    body = _node('Return', value=self._assign_expr())
                return _node('FuncDecl', name=None, params=[name], body=body,
                             param_defaults={}, param_rest=None, param_patterns={},
                             is_async=False, is_generator=False)
            return _node('Ident', name=name)

        if t.type == PUNCT:
            if t.value == '(':
                self.pos += 1
                # Empty arrow: () => ...
                if self._peek(PUNCT, ')'):
                    self.pos += 1
                    if self._peek(OP, '=>'):
                        self.pos += 1
                        if self._peek(PUNCT, '{'):
                            body = self._block()
                        else:
                            body = _node('Return', value=self._assign_expr())
                        return _node('FuncDecl', name=None, params=[], body=body,
                                     param_defaults={}, param_rest=None, param_patterns={},
                                     is_async=False, is_generator=False)
                    return _node('Literal', value=_UNDEF)

                expr = self._expression()
                self._eat(PUNCT, ')')
                if self._peek(OP, '=>'):
                    self.pos += 1
                    params, defaults, rest, patterns = self._extract_arrow_params_full(expr)
                    if self._peek(PUNCT, '{'):
                        body = self._block()
                    else:
                        body = _node('Return', value=self._assign_expr())
                    return _node('FuncDecl', name=None, params=params, body=body,
                                 param_defaults=defaults, param_rest=rest,
                                 param_patterns=patterns, is_async=False, is_generator=False)
                return expr

            if t.value == '[':
                return self._array_literal()

            if t.value == '{':
                return self._object_literal()

        if t.type == OP and t.value == '...':
            # Spread in primary position (shouldn't appear, but handle gracefully)
            self.pos += 1
            return _node('Spread', arg=self._assign_expr())

        # Fallback — skip token and return undefined
        self.pos += 1
        return _node('Literal', value=_UNDEF)

    def _make_template_literal(self, parts) -> ASTNode:
        """Convert TEMPLATE token value (list of str/token-list) into AST."""
        # parts: [str, list[Token], str, list[Token], ..., str]
        nodes = []
        for part in parts:
            if isinstance(part, str):
                nodes.append(_node('Literal', value=part))
            else:
                # Re-parse the expression tokens
                sub_parser = Parser(part + [Token(EOF, None)])
                expr = sub_parser._expression()
                nodes.append(expr)
        return _node('TemplateLiteral', parts=nodes)

    def _extract_arrow_params_full(self, expr):
        """Extract params from grouped expression for arrow function."""
        params = []
        defaults = {}
        patterns = {}
        self._flatten_arrow_params(expr, params, defaults, patterns)
        return params, defaults, None, patterns

    def _flatten_arrow_params(self, node, params, defaults, patterns):
        if node.type == 'Comma':
            self._flatten_arrow_params(node.data['left'], params, defaults, patterns)
            self._flatten_arrow_params(node.data['right'], params, defaults, patterns)
        elif node.type == 'Ident':
            params.append(node.data['name'])
        elif node.type == 'Assign':
            # default: x = expr
            left = node.data['left']
            if left.type == 'Ident':
                idx = len(params)
                params.append(left.data['name'])
                defaults[idx] = node.data['right']
        elif node.type in ('ObjectPattern', 'ArrayPattern'):
            idx = len(params)
            placeholder = f'__pattern__{idx}'
            params.append(placeholder)
            patterns[idx] = node
        elif node.type == 'Spread':
            # rest param: ...name
            arg = node.data['arg']
            if arg.type == 'Ident':
                # We can't directly set rest here since we return None for rest
                # Mark as rest by using special placeholder
                params.append(f'...{arg.data["name"]}')

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
            if self._peek(OP, '...'):
                self.pos += 1
                props.append((_node('SpreadProp'), self._assign_expr()))
                if self._peek(PUNCT, ','):
                    self.pos += 1
                continue
            t = self._cur()
            if t.type == KEYWORD and t.value in ('get', 'set'):
                # Getter/setter or property named 'get'/'set'
                saved = self.pos
                kind = t.value
                self.pos += 1
                if self._peek(IDENT) or self._peek(STRING) or self._peek(NUMBER):
                    pt = self._cur()
                    key = pt.value
                    self.pos += 1
                    if self._peek(PUNCT, '('):
                        params, defaults, rest, patterns = self._param_list_full()
                        body = self._block()
                        fn = _node('FuncDecl', name=key, params=params, body=body,
                                   param_defaults=defaults, param_rest=rest,
                                   param_patterns=patterns, is_async=False, is_generator=False)
                        props.append((f'__{kind}__:{key}', fn))
                        if self._peek(PUNCT, ','):
                            self.pos += 1
                        continue
                self.pos = saved  # property named 'get'/'set'
                t = self._cur()

            if t.type == KEYWORD and t.value == 'async':
                self.pos += 1
                # async method
                is_generator = False
                if self._peek(OP, '*'):
                    self.pos += 1
                    is_generator = True
                if self._peek(IDENT) or self._peek(KEYWORD):
                    key = self._cur().value
                    self.pos += 1
                    if self._peek(PUNCT, '('):
                        params, defaults, rest, patterns = self._param_list_full()
                        body = self._block()
                        fn = _node('FuncDecl', name=key, params=params, body=body,
                                   param_defaults=defaults, param_rest=rest,
                                   param_patterns=patterns, is_async=True, is_generator=is_generator)
                        props.append((key, fn))
                        if self._peek(PUNCT, ','):
                            self.pos += 1
                        continue
                continue

            if t.type == IDENT or t.type == KEYWORD:
                key = t.value
                self.pos += 1
                if self._peek(PUNCT, ',') or self._peek(PUNCT, '}'):
                    props.append((key, _node('Ident', name=key)))
                    if self._peek(PUNCT, ','):
                        self.pos += 1
                    continue
                if self._peek(PUNCT, '('):
                    params, defaults, rest, patterns = self._param_list_full()
                    body = self._block()
                    fn = _node('FuncDecl', name=key, params=params, body=body,
                               param_defaults=defaults, param_rest=rest,
                               param_patterns=patterns, is_async=False, is_generator=False)
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
            elif t.type == PUNCT and t.value == '[':
                self.pos += 1
                key_expr = self._assign_expr()
                self._eat(PUNCT, ']')
                self._eat(OP, ':')
                val = self._assign_expr()
                props.append((_node('Computed', expr=key_expr), val))
                if self._peek(PUNCT, ','):
                    self.pos += 1
                continue
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


# _UNDEF and _Undefined are imported from js.types (re-exported for legacy callers)
