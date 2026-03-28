"""JavaScript recursive-descent parser for rENDER browser engine.

Parses a subset of JavaScript into an AST suitable for interpretation.
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

    def _peek_next(self, type_=None, value=None) -> bool:
        pos = self.pos + 1
        if pos < len(self.tokens):
            t = self.tokens[pos]
        else:
            t = Token(EOF, None)
        if type_ and t.type != type_:
            return False
        if value is not None and t.value != value:
            return False
        return True

    def _peek_name(self) -> bool:
        return self._cur().type in (IDENT, KEYWORD)

    def _eat_name(self) -> Token:
        t = self._cur()
        if t.type in (IDENT, KEYWORD):
            self.pos += 1
            return t
        raise SyntaxError(f'Expected name, got {t} at line {t.line}')

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

    def _synchronize(self) -> None:
        while not self._peek(EOF):
            if self._peek(PUNCT, ';'):
                self.pos += 1
                return
            if self._peek(PUNCT, '}'):
                return
            self.pos += 1

    def _looks_like_for_in_of(self) -> bool:
        depth = 0
        pos = self.pos
        while pos < len(self.tokens):
            t = self.tokens[pos]
            if t.type == PUNCT and t.value in '([{':
                depth += 1
            elif t.type == PUNCT and t.value in ')]}':
                if depth == 0 and t.value == ')':
                    return False
                depth = max(0, depth - 1)
            elif depth == 0 and t.type == PUNCT and t.value == ';':
                return False
            elif depth == 0 and t.type == KEYWORD and t.value in ('in', 'of'):
                return True
            pos += 1
        return False

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
                self._eat_semi()
                return _node('Break')
            if kw == 'continue':
                self.pos += 1
                self._eat_semi()
                return _node('Continue')
            if kw == 'try':
                return self._try_stmt()
            if kw == 'throw':
                return self._throw_stmt()
            if kw == 'switch':
                return self._switch_stmt()

        if t.type == PUNCT and t.value == '{':
            return self._block()

        # Expression statement
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

    def _var_decl(self) -> ASTNode:
        kind = self._eat(KEYWORD).value
        decls = []
        while True:
            name = self._binding_target()
            init = None
            if self._peek(OP, '='):
                self.pos += 1
                init = self._assign_expr()
            decls.append((name, init))
            if self._peek(PUNCT, ','):
                self.pos += 1
            else:
                break
        self._eat_semi()
        return _node('VarDecl', kind=kind, decls=decls)

    def _function_decl(self) -> ASTNode:
        self._eat(KEYWORD, 'function')
        if self._peek(OP, '*'):
            self.pos += 1
        name = None
        if self._peek_name():
            name = self._eat_name().value
        params = self._param_list()
        body = self._block()
        return _node('FuncDecl', name=name, params=params, body=body)

    def _class_decl(self, as_expr: bool = False) -> ASTNode:
        self._eat(KEYWORD, 'class')
        name = None
        if self._peek_name():
            name = self._eat_name().value
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
            if self._peek_name() and self._cur().value == 'static':
                if self._peek_next(PUNCT, '[') or self._peek_next(STRING) or self._peek_next(NUMBER) \
                        or self._peek_next(IDENT) or self._peek_next(KEYWORD):
                    is_static = True
                    self.pos += 1
            computed = False
            if self._peek(PUNCT, '['):
                computed = True
                self.pos += 1
                key = self._assign_expr()
                self._eat(PUNCT, ']')
            elif self._peek(STRING):
                key = self._eat(STRING).value
            elif self._peek(NUMBER):
                key = str(self._eat(NUMBER).value)
            else:
                key = self._eat_name().value
            params = self._param_list()
            body = self._block()
            kind = 'constructor' if not is_static and key == 'constructor' and not computed else 'method'
            methods.append(_node('ClassMethod', key=key, params=params, body=body,
                                 kind=kind, static=is_static, computed=computed))
        self._eat(PUNCT, '}')
        return _node('ClassDecl', name=name, super_class=super_class,
                     methods=methods, as_expr=as_expr)

    def _param_list(self) -> list:
        self._eat(PUNCT, '(')
        params = []
        while not self._peek(PUNCT, ')') and not self._peek(EOF):
            params.append(self._parse_param())
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, ')')
        return params

    def _parse_param(self):
        rest = False
        if self._peek(OP, '...'):
            self.pos += 1
            rest = True
        target = self._binding_target()
        default = None
        if self._peek(OP, '='):
            self.pos += 1
            default = self._assign_expr()
        if isinstance(target, ASTNode) or default is not None or rest:
            return _node('Param', target=target, default=default, rest=rest)
        return target

    def _binding_target(self):
        t = self._cur()
        if t.type in (IDENT, KEYWORD):
            self.pos += 1
            return t.value
        if t.type == PUNCT and t.value == '[':
            return self._array_pattern()
        if t.type == PUNCT and t.value == '{':
            return self._object_pattern()
        raise SyntaxError(f'Expected binding target, got {t} at line {t.line}')

    def _array_pattern(self) -> ASTNode:
        self._eat(PUNCT, '[')
        elements = []
        while not self._peek(PUNCT, ']') and not self._peek(EOF):
            if self._peek(PUNCT, ','):
                elements.append(None)
                self.pos += 1
                continue
            if self._peek(OP, '...'):
                self.pos += 1
                elements.append(_node('RestPattern', target=self._binding_target()))
                if self._peek(PUNCT, ','):
                    self.pos += 1
                break
            target = self._binding_target()
            if self._peek(OP, '='):
                self.pos += 1
                target = _node('DefaultPattern', target=target, default=self._assign_expr())
            elements.append(target)
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, ']')
        return _node('ArrayPattern', elements=elements)

    def _object_pattern(self) -> ASTNode:
        self._eat(PUNCT, '{')
        props = []
        while not self._peek(PUNCT, '}') and not self._peek(EOF):
            if self._peek(OP, '...'):
                self.pos += 1
                props.append((_node('RestPattern', target=self._binding_target()), None))
                if self._peek(PUNCT, ','):
                    self.pos += 1
                continue
            t = self._cur()
            if t.type in (IDENT, KEYWORD, STRING, NUMBER):
                key = str(t.value)
                self.pos += 1
            else:
                raise SyntaxError(f'Expected object pattern key, got {t} at line {t.line}')
            if self._peek(OP, ':'):
                self.pos += 1
                target = self._binding_target()
            else:
                target = key
            if self._peek(OP, '='):
                self.pos += 1
                target = _node('DefaultPattern', target=target, default=self._assign_expr())
            props.append((key, target))
            if self._peek(PUNCT, ','):
                self.pos += 1
        self._eat(PUNCT, '}')
        return _node('ObjectPattern', props=props)

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

        # Check for for..in / for..of
        saved = self.pos
        if self._peek(KEYWORD) and self._cur().value in ('var', 'let', 'const'):
            kind = self._eat(KEYWORD).value
            try:
                target = self._binding_target()
            except SyntaxError:
                target = None
            if target is not None and (self._peek(KEYWORD, 'in') or self._peek(KEYWORD, 'of')):
                loop_type = self._eat(KEYWORD).value
                iterable = self._expression()
                self._eat(PUNCT, ')')
                body = self._statement()
                name = target if isinstance(target, str) else None
                return _node('ForIn', kind=kind, name=name, target=target,
                             loop_type=loop_type, iterable=iterable, body=body)
            self.pos = saved  # not for-in/of, reparse

        # Standard for loop
        init = None
        if not self._peek(PUNCT, ';'):
            if self._peek(KEYWORD) and self._cur().value in ('var', 'let', 'const'):
                init = self._var_decl_no_semi()
            elif self._looks_like_for_in_of():
                init = self._call_or_member()
                loop_type = self._eat(KEYWORD).value
                iterable = self._expression()
                self._eat(PUNCT, ')')
                body = self._statement()
                return _node('ForIn', kind=None, name=None, target=init,
                             loop_type=loop_type, iterable=iterable, body=body)
            else:
                init = self._assign_expr()
                while self._peek(PUNCT, ','):
                    self.pos += 1
                    init = _node('Comma', left=init, right=self._assign_expr())
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
            name = self._binding_target()
            init = None
            if self._peek(OP, '='):
                self.pos += 1
                init = self._assign_expr()
            decls.append((name, init))
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
        if self._peek(KEYWORD, 'async'):
            if self._peek_next(KEYWORD, 'function'):
                self.pos += 1
                return self._function_decl()
            if self.pos + 2 < len(self.tokens):
                n1 = self.tokens[self.pos + 1]
                n2 = self.tokens[self.pos + 2]
                if n1.type in (IDENT, KEYWORD) and n2.type == OP and n2.value == '=>':
                    self.pos += 1
                    name = self._eat_name().value
                    self._eat(OP, '=>')
                    if self._peek(PUNCT, '{'):
                        body = self._block()
                    else:
                        body = _node('Return', value=self._assign_expr())
                    return _node('FuncDecl', name=None, params=[name], body=body)
            if self._peek_next(PUNCT, '(') and self._is_arrow_param_group(self.pos + 1):
                self.pos += 1
                params = self._param_list()
                self._eat(OP, '=>')
                if self._peek(PUNCT, '{'):
                    body = self._block()
                else:
                    body = _node('Return', value=self._assign_expr())
                return _node('FuncDecl', name=None, params=params, body=body)
        if self._peek_name() and self._peek_next(OP, '=>'):
            name = self._eat_name().value
            self._eat(OP, '=>')
            if self._peek(PUNCT, '{'):
                body = self._block()
            else:
                body = _node('Return', value=self._assign_expr())
            return _node('FuncDecl', name=None, params=[name], body=body)
        left = self._ternary()
        t = self._cur()
        if t.type == OP and t.value in ('=', '+=', '-=', '*=', '/=', '%=',
                                         '&=', '|=', '^=', '<<=', '>>=', '>>>=', '**=',
                                         '&&=', '||=', '??='):
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
        if t.type == KEYWORD and t.value == 'async':
            self.pos += 1
            if self._peek(KEYWORD, 'function'):
                return self._postfix()
            operand = self._unary()
            return operand
        if t.type == KEYWORD and t.value == 'await':
            self.pos += 1
            operand = self._unary()
            return _node('UnaryOp', op='await', operand=operand)
        if t.type == KEYWORD and t.value == 'void':
            self.pos += 1
            operand = self._unary()
            return _node('UnaryOp', op='void', operand=operand)
        if t.type == KEYWORD and t.value == 'delete':
            self.pos += 1
            operand = self._unary()
            return _node('UnaryOp', op='delete', operand=operand)
        if t.type == OP and t.value in ('++', '--'):
            self.pos += 1
            operand = self._unary()
            return _node('UpdatePre', op=t.value, operand=operand)
        if t.type == KEYWORD and t.value == 'new':
            if self._peek_next(PUNCT, '.'):
                if self.pos + 2 < len(self.tokens) and self.tokens[self.pos + 2].type in (IDENT, KEYWORD) and self.tokens[self.pos + 2].value == 'target':
                    self.pos += 3
                    return _node('Literal', value=_UNDEF)
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
        callee = self._member_expr()
        args = []
        if self._peek(PUNCT, '('):
            args = self._arguments()
        expr = _node('New', callee=callee, args=args)
        return self._finish_call_or_member(expr)

    def _call_or_member(self) -> ASTNode:
        expr = self._member_expr()
        return self._finish_call_or_member(expr)

    def _member_expr(self) -> ASTNode:
        expr = self._primary()
        while True:
            if self._peek(PUNCT, '.'):
                self.pos += 1
                prop = self._eat_name().value
                expr = _node('Member', obj=expr, prop=prop, computed=False, optional=False)
            elif self._peek(PUNCT, '['):
                self.pos += 1
                prop = self._expression()
                self._eat(PUNCT, ']')
                expr = _node('Member', obj=expr, prop=prop, computed=True, optional=False)
            elif self._peek(OP, '?.'):
                self.pos += 1
                if self._peek(PUNCT, '('):
                    self.pos -= 1
                    break
                if self._peek(PUNCT, '['):
                    self.pos += 1
                    prop = self._expression()
                    self._eat(PUNCT, ']')
                    expr = _node('Member', obj=expr, prop=prop, computed=True, optional=True)
                else:
                    prop = self._eat_name().value
                    expr = _node('Member', obj=expr, prop=prop, computed=False, optional=True)
            else:
                break
        return expr

    def _finish_call_or_member(self, expr: ASTNode) -> ASTNode:
        while True:
            if self._peek(PUNCT, '('):
                args = self._arguments()
                expr = _node('Call', callee=expr, args=args, optional=False)
            elif self._peek(TEMPLATE):
                expr = _node('Call', callee=expr, args=[self._template_literal()], optional=False)
            elif self._peek(STRING):
                expr = _node('Call', callee=expr, args=[_node('Literal', value=self._eat(STRING).value)], optional=False)
            elif self._peek(PUNCT, '.'):
                self.pos += 1
                prop = self._eat_name().value
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
                    prop = self._eat_name().value
                    expr = _node('Member', obj=expr, prop=prop, computed=False, optional=True)
            else:
                break
        return expr

    def _template_literal(self) -> ASTNode:
        t = self._eat(TEMPLATE)
        nodes = []
        for kind, content in t.value:
            if kind == 'str':
                nodes.append(_node('Literal', value=content))
                continue
            from js.lexer import Lexer as _Lexer
            expr_tokens = _Lexer(content).tokenize()
            expr_parser = Parser(expr_tokens)
            nodes.append(expr_parser._expression())
        return _node('TemplateLiteral', nodes=nodes)

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
            return self._template_literal()

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
            if t.value == 'class':
                return self._class_decl(as_expr=True)
            if t.value == 'new' and self._peek_next(PUNCT, '.'):
                if self.pos + 2 < len(self.tokens) and self.tokens[self.pos + 2].type in (IDENT, KEYWORD) and self.tokens[self.pos + 2].value == 'target':
                    self.pos += 3
                    return _node('Literal', value=_UNDEF)
            if t.value == 'function':
                return self._function_decl()  # function expression
            if t.value == 'new':
                return self._new_expr()
            if t.value == 'typeof':
                self.pos += 1
                operand = self._unary()
                return _node('UnaryOp', op='typeof', operand=operand)

        if t.type == IDENT:
            self.pos += 1
            return _node('Ident', name=t.value)

        if t.type == KEYWORD:
            self.pos += 1
            return _node('Ident', name=t.value)

        if t.type == PUNCT:
            if t.value == '(':
                if self._is_arrow_param_group():
                    params = self._param_list()
                    self._eat(OP, '=>')
                    if self._peek(PUNCT, '{'):
                        body = self._block()
                    else:
                        body = _node('Return', value=self._assign_expr())
                    return _node('FuncDecl', name=None, params=params, body=body)
                self.pos += 1
                expr = self._expression()
                self._eat(PUNCT, ')')
                return expr

            if t.value == '[':
                return self._array_literal()

            if t.value == '{':
                return self._object_literal()

        # Fallback — skip token and return undefined
        self.pos += 1
        return _node('Literal', value=_UNDEF)

    def _is_arrow_param_group(self, start_pos=None) -> bool:
        pos = self.pos if start_pos is None else start_pos
        if pos >= len(self.tokens) or self.tokens[pos].type != PUNCT or self.tokens[pos].value != '(':
            return False
        depth = 0
        while pos < len(self.tokens):
            t = self.tokens[pos]
            if t.type == PUNCT and t.value == '(':
                depth += 1
            elif t.type == PUNCT and t.value == ')':
                depth -= 1
                if depth == 0:
                    pos += 1
                    return pos < len(self.tokens) and self.tokens[pos].type == OP and self.tokens[pos].value == '=>'
            pos += 1
        return False

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
            # Key
            t = self._cur()
            if self._peek(OP, '...'):
                self.pos += 1
                props.append((_node('SpreadProp', arg=self._assign_expr()), None))
                if self._peek(PUNCT, ','):
                    self.pos += 1
                continue
            if t.type in (IDENT, KEYWORD):
                key = t.value
                self.pos += 1
                if key == 'async' and self._cur().type in (IDENT, KEYWORD, STRING, NUMBER):
                    method = self._cur()
                    prop_key = str(method.value)
                    self.pos += 1
                    if self._peek(PUNCT, '('):
                        params = self._param_list()
                        body = self._block()
                        props.append((prop_key, _node('FuncDecl', name=prop_key, params=params, body=body)))
                        if self._peek(PUNCT, ','):
                            self.pos += 1
                        continue
                if key in ('get', 'set') and self._cur().type in (IDENT, KEYWORD, STRING, NUMBER):
                    accessor = self._cur()
                    prop_key = str(accessor.value)
                    self.pos += 1
                    params = self._param_list()
                    body = self._block()
                    props.append((prop_key, _node('Accessor', kind=key, name=prop_key, params=params, body=body)))
                    if self._peek(PUNCT, ','):
                        self.pos += 1
                    continue
                # Shorthand property: { foo } or method: { foo() {} }
                if self._peek(PUNCT, ',') or self._peek(PUNCT, '}'):
                    props.append((key, _node('Ident', name=key)))
                    if self._peek(PUNCT, ','):
                        self.pos += 1
                    continue
                if self._peek(PUNCT, '('):
                    # Method shorthand
                    params = self._param_list()
                    body = self._block()
                    props.append((key, _node('FuncDecl', name=key, params=params, body=body)))
                    if self._peek(PUNCT, ','):
                        self.pos += 1
                    continue
            elif t.type == STRING:
                key = t.value
                self.pos += 1
                if self._peek(PUNCT, '('):
                    params = self._param_list()
                    body = self._block()
                    props.append((key, _node('FuncDecl', name=key, params=params, body=body)))
                    if self._peek(PUNCT, ','):
                        self.pos += 1
                    continue
            elif t.type == NUMBER:
                key = str(t.value)
                self.pos += 1
                if self._peek(PUNCT, '('):
                    params = self._param_list()
                    body = self._block()
                    props.append((key, _node('FuncDecl', name=key, params=params, body=body)))
                    if self._peek(PUNCT, ','):
                        self.pos += 1
                    continue
            elif t.type == PUNCT and t.value == '[':
                # Computed key
                self.pos += 1
                key_expr = self._assign_expr()
                self._eat(PUNCT, ']')
                if self._peek(PUNCT, '('):
                    params = self._param_list()
                    body = self._block()
                    props.append((_node('Computed', expr=key_expr), _node('FuncDecl', name=None, params=params, body=body)))
                    if self._peek(PUNCT, ','):
                        self.pos += 1
                    continue
                self._eat(OP, ':')
                val = self._assign_expr()
                props.append((_node('Computed', expr=key_expr), val))
                if self._peek(PUNCT, ','):
                    self.pos += 1
                continue
            else:
                self.pos += 1  # skip
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
