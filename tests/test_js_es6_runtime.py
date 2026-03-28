from js.interpreter import Interpreter
from js.lexer import Lexer, TEMPLATE, NUMBER
from js.parser import Parser


def _exec(code: str) -> Interpreter:
    interp = Interpreter()
    ast = Parser(Lexer(code).tokenize()).parse()
    interp.execute(ast)
    return interp


def test_template_literal_token_keeps_expression_parts():
    tokens = Lexer("var msg = `hi ${name}!`;").tokenize()

    assert tokens[3].type == TEMPLATE
    assert tokens[3].value == [('str', 'hi '), ('expr', 'name'), ('str', '!')]


def test_binary_and_octal_literals_lex_as_numbers():
    tokens = Lexer("0b1010 0o17").tokenize()

    assert tokens[0].type == NUMBER
    assert tokens[0].value == 10
    assert tokens[1].type == NUMBER
    assert tokens[1].value == 15


def test_template_literal_interpolation_executes_expression():
    interp = _exec("""
        var name = 'world';
        var count = 2;
        var out = `hello ${name}:${count + 1}`;
    """)

    assert interp.global_env.get("out") == "hello world:3"


def test_class_declaration_constructor_and_instance_methods_execute():
    interp = _exec("""
        class Greeter {
          constructor(name) { this.name = name; }
          greet() { return 'hi ' + this.name; }
        }
        var out = new Greeter('Ada').greet();
    """)

    assert interp.global_env.get("out") == "hi Ada"


def test_class_extends_super_and_static_methods_execute():
    interp = _exec("""
        class Base {
          constructor(name) { this.name = name; }
          greet() { return 'base:' + this.name; }
          static kind() { return 'base'; }
        }
        class Child extends Base {
          constructor(name) { super(name); }
          greet() { return super.greet() + '!'; }
          static kind() { return Base.kind() + ':child'; }
        }
        var out = Child.kind() + '|' + new Child('Neo').greet();
    """)

    assert interp.global_env.get("out") == "base:child|base:Neo!"


def test_object_literal_still_allows_class_property_name():
    interp = _exec("""
        var props = { id: 'a' };
        var vnode = { props: { ...props, class: 'hero' } };
        var out = vnode.props.class + ':' + vnode.props.id;
    """)

    assert interp.global_env.get("out") == "hero:a"


def test_promise_resolve_and_queue_microtask_schedule_callbacks():
    interp = _exec("""
        var out = '';
        Promise.resolve(3).then(function(v) { out = out + 'p' + v; });
        queueMicrotask(function() { out = out + ':q'; });
    """)

    assert interp.global_env.get("out") == "p3:q"


def test_symbol_for_returns_stable_registry_symbol():
    interp = _exec("""
        var s1 = Symbol.for('react.element');
        var s2 = Symbol.for('react.element');
        var out = (typeof s1) + ':' + (s1 === s2) + ':' + Symbol.keyFor(s1);
    """)

    assert interp.global_env.get("out") == "symbol:true:react.element"


def test_map_and_set_support_basic_runtime_operations():
    interp = _exec("""
        var m = new Map();
        m.set('a', 1);
        var s = new Set();
        s.add('x');
        var out = m.get('a') + ':' + m.has('a') + ':' + s.has('x') + ':' + s.size;
    """)

    assert interp.global_env.get("out") == "1:true:true:1"
