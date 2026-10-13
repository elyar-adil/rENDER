//! Argument conversion at the script boundary: the context-kind enumeration,
//! and the `[EnforceRange]` size setters of `OffscreenCanvas`.

use super::tests::run;

#[test]
fn an_unknown_context_kind_is_a_type_error() {
    for kind in ["", "2D", "2d#", "null", "undefined"] {
        let script = format!(
            "var ctx = document.getElementById('c'); var name = 'none'; \
             try {{ ctx.getContext('{kind}'); }} catch (e) {{ name = e.name; }} name"
        );
        assert_eq!(run(&script), "TypeError", "kind {kind:?}");
    }
    assert_eq!(
        run(
            "var name = 'none'; try { document.getElementById('c').getContext(); } \
             catch (e) { name = e.name; } name"
        ),
        "TypeError"
    );
}

#[test]
fn a_known_kind_this_surface_does_not_provide_is_null() {
    assert_eq!(
        run("String(document.getElementById('c').getContext('webgl2'))"),
        "null"
    );
}

#[test]
fn offscreen_size_setters_enforce_the_unsigned_long_range() {
    for value in ["'100em'", "-1", "4294967296", "NaN"] {
        let script = format!(
            "var o = new OffscreenCanvas(4, 4); var name = 'none'; \
             try {{ o.width = {value}; }} catch (e) {{ name = e.name; }} name"
        );
        assert_eq!(run(&script), "TypeError", "value {value}");
    }
    assert_eq!(
        run(
            "var o = new OffscreenCanvas(4, 4); o.width = '12'; o.height = 7.9; \
             [o.width, o.height].join(',')"
        ),
        "12,7"
    );
}

#[test]
fn an_element_size_setter_takes_the_attribute_rules_without_throwing() {
    assert_eq!(
        run("var c = document.getElementById('c'); c.width = '100em'; String(c.width)"),
        "0"
    );
}
