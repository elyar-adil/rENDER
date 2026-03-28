from pathlib import Path

from js.interpreter import Interpreter
from js.lexer import Lexer
from js.parser import Parser


REACT_BUNDLE = Path(__file__).parent / "fixtures" / "react" / "react-18.2.0.production.min.js"
REACT_DOM_BUNDLE = Path(__file__).parent / "fixtures" / "react" / "react-dom-18.2.0.production.min.js"


def _execute(interp: Interpreter, code: str) -> None:
    ast = Parser(Lexer(code).tokenize()).parse()
    interp.execute(ast)


def _load_real_react() -> Interpreter:
    interp = Interpreter()
    _execute(interp, REACT_BUNDLE.read_text(encoding="utf-8"))
    return interp


def _load_real_react_dom() -> Interpreter:
    interp = _load_real_react()
    _execute(interp, REACT_DOM_BUNDLE.read_text(encoding="utf-8"))
    return interp


def test_real_react_umd_bundle_exposes_window_react_and_global_react():
    interp = _load_real_react()

    window = interp.global_env.get("window")
    react = window.get("React") if isinstance(window, dict) else None

    assert isinstance(react, dict)
    assert "createElement" in react
    assert react.get("version") == "18.2.0"
    assert interp.global_env.get("React") is react


def test_real_react_create_element_preserves_props_and_single_child():
    interp = _load_real_react()
    _execute(
        interp,
        """
        var vnode = React.createElement('div', { className: 'hero', id: 'app' }, 'hello');
        var out = vnode.type + ':' + vnode.props.className + ':' + vnode.props.id + ':' + vnode.props.children;
        """,
    )

    assert interp.global_env.get("out") == "div:hero:app:hello"


def test_real_react_element_uses_symbol_type_tag():
    interp = _load_real_react()
    _execute(
        interp,
        """
        var vnode = React.createElement('div', null, 'hello');
        var out = (typeof vnode.$$typeof) + ':' + Symbol.keyFor(vnode.$$typeof);
        """,
    )

    assert interp.global_env.get("out") == "symbol:react.element"


def test_real_react_dom_umd_bundle_exposes_global_reactdom():
    interp = _load_real_react_dom()

    window = interp.global_env.get("window")
    react_dom = window.get("ReactDOM") if isinstance(window, dict) else None

    assert isinstance(react_dom, dict)
    assert "render" in react_dom
    assert interp.global_env.get("ReactDOM") is react_dom


def test_real_react_function_component_with_nested_children_runs():
    interp = _load_real_react()
    _execute(
        interp,
        """
        function App() {
          var c1 = React.createElement('h1', null, 'Hello React');
          var c2 = React.createElement('p', null, 'count=' + 3);
          return React.createElement('section', { className: 'app-shell' }, c1, c2);
        }
        var vnode = App();
        var out = vnode.type + ':' + vnode.props.className + ':' +
          vnode.props.children.length + ':' +
          vnode.props.children[0].type + ':' +
          vnode.props.children[1].props.children;
        """,
    )

    assert interp.global_env.get("out") == "section:app-shell:2:h1:count=3"
