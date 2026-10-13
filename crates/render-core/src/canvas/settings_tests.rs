//! The `CanvasRenderingContext2DSettings` argument of `getContext`: converted as
//! `WebIDL` requires, and no value of a valid member is refused.

use super::tests::run;

#[test]
fn an_invalid_color_space_or_color_type_is_a_type_error() {
    assert_eq!(
        run("var c = document.getElementById('c'); var name = 'none'; \
             try { c.getContext('2d', {colorSpace: 'rec2020'}); } catch (e) { name = e.name; } name"),
        "TypeError"
    );
    assert_eq!(
        run("var c = document.getElementById('c'); var name = 'none'; \
             try { c.getContext('2d', {colorType: 'float32'}); } catch (e) { name = e.name; } name"),
        "TypeError"
    );
}

#[test]
fn an_options_argument_that_is_not_an_object_is_a_type_error() {
    assert_eq!(
        run("var c = document.getElementById('c'); var name = 'none'; \
             try { c.getContext('2d', 5); } catch (e) { name = e.name; } name"),
        "TypeError"
    );
}

#[test]
fn every_valid_settings_member_is_accepted_and_the_context_is_still_the_same_object() {
    assert_eq!(
        run("var c = document.getElementById('c'); \
             var a = c.getContext('2d', {alpha: false, desynchronized: true, \
                 colorSpace: 'display-p3', colorType: 'float16', willReadFrequently: true}); \
             var b = c.getContext('2d'); String(a === b && a !== null)"),
        "true"
    );
}
