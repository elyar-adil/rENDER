#![allow(clippy::similar_names)] // CSS em/ex/ch terminology is fixed by the specification.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use render_core::css::cascade::{CascadeInput, CascadeOrigin, cascade_element};
use render_core::css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
use render_core::css::selector::{MatchContext, parse_selector_list, select_all};
use render_core::css::stylesheet::parse_stylesheet;
use render_core::css::{LengthContext, resolve_length_expr as resolve_rust_length};
use render_core::dom::{Dom, Namespace, NodeId, NodeKind};
use render_core::html::{ParseOutput, parse_document};

#[allow(clippy::too_many_arguments)]
fn context(
    percentage_base: Option<f64>,
    em_base: f64,
    rem_base: f64,
    vw: f64,
    vh: f64,
    ex_base: Option<f64>,
    ch_base: Option<f64>,
) -> LengthContext {
    LengthContext {
        percentage_base,
        em_base,
        rem_base,
        viewport_width: vw,
        viewport_height: vh,
        ex_base,
        ch_base,
        ..LengthContext::default()
    }
}

/// Compatibility-shaped API for gradual adoption by the current Python engine.
/// Invalid or unresolved CSS values return None, matching its existing contract.
#[pyfunction]
#[pyo3(signature = (value, *, percentage_base=None, em_base=16.0, rem_base=16.0, vw=0.0, vh=0.0, ex_base=None, ch_base=None))]
#[allow(clippy::too_many_arguments)]
fn resolve_length_expr(
    value: &str,
    percentage_base: Option<f64>,
    em_base: f64,
    rem_base: f64,
    vw: f64,
    vh: f64,
    ex_base: Option<f64>,
    ch_base: Option<f64>,
) -> Option<f64> {
    resolve_rust_length(
        value,
        &context(percentage_base, em_base, rem_base, vw, vh, ex_base, ch_base),
    )
    .ok()
}

/// Strict API for new code and Agent diagnostics. Invalid CSS includes a useful
/// byte offset instead of being silently converted to an arbitrary value.
#[pyfunction]
#[pyo3(signature = (value, *, percentage_base=None, em_base=16.0, rem_base=16.0, vw=0.0, vh=0.0, ex_base=None, ch_base=None))]
#[allow(clippy::too_many_arguments)]
fn resolve_length_expr_strict(
    value: &str,
    percentage_base: Option<f64>,
    em_base: f64,
    rem_base: f64,
    vw: f64,
    vh: f64,
    ex_base: Option<f64>,
    ch_base: Option<f64>,
) -> PyResult<f64> {
    resolve_rust_length(
        value,
        &context(percentage_base, em_base, rem_base, vw, vh, ex_base, ch_base),
    )
    .map_err(|error| PyValueError::new_err(error.to_string()))
}

/// Parse HTML with the Rust tokenizer/tree builder and return an immutable,
/// structured snapshot. Stable node IDs and parse diagnostics are preserved for
/// future Agent observation and trace APIs.
#[pyfunction]
fn parse_html_snapshot(py: Python<'_>, html: &str) -> PyResult<Py<PyAny>> {
    let output = parse_document(html);
    Ok(snapshot_parse_output(py, &output)?.unbind().into_any())
}

/// Parse HTML and run a strict `querySelectorAll`-style selector against the
/// Rust DOM. The returned document snapshot and match IDs share the same stable
/// identity space.
#[pyfunction]
fn query_html_snapshot(py: Python<'_>, html: &str, selector: &str) -> PyResult<Py<PyAny>> {
    let output = parse_document(html);
    let selectors =
        parse_selector_list(selector).map_err(|error| PyValueError::new_err(error.to_string()))?;
    let context = MatchContext {
        quirks_mode: output.quirks_mode.as_str() == "quirks",
        ..MatchContext::default()
    };
    let matches = select_all(&output.dom, output.dom.document(), &selectors, &context);
    let result = snapshot_parse_output(py, &output)?;
    result.set_item(
        "match_ids",
        matches.iter().map(|node| node.as_u64()).collect::<Vec<_>>(),
    )?;
    result.set_item(
        "specificities",
        selectors
            .selectors()
            .iter()
            .map(|selector| {
                let specificity = selector.specificity();
                (specificity.ids, specificity.classes, specificity.types)
            })
            .collect::<Vec<_>>(),
    )?;
    Ok(result.unbind().into_any())
}

/// Parse HTML and an author stylesheet, then expose the cascaded (not yet
/// computed) property winners for each matching element.
#[pyfunction]
fn cascade_html_snapshot(
    py: Python<'_>,
    html: &str,
    css: &str,
    selector: &str,
) -> PyResult<Py<PyAny>> {
    let output = parse_document(html);
    let selectors =
        parse_selector_list(selector).map_err(|error| PyValueError::new_err(error.to_string()))?;
    let context = MatchContext {
        quirks_mode: output.quirks_mode.as_str() == "quirks",
        ..MatchContext::default()
    };
    let matches = select_all(&output.dom, output.dom.document(), &selectors, &context);
    let sheet = parse_stylesheet(css);
    let source = CascadeInput {
        sheet: &sheet,
        origin: CascadeOrigin::Author,
    };

    let result = snapshot_parse_output(py, &output)?;
    let styles = PyList::empty(py);
    for node in &matches {
        let style = cascade_element(&output.dom, *node, &[source], &context);
        let item = PyDict::new(py);
        item.set_item("node_id", node.as_u64())?;
        let properties = PyDict::new(py);
        for (property, value) in style.properties() {
            properties.set_item(property, &value.value)?;
        }
        item.set_item("properties", properties)?;
        styles.append(item)?;
    }
    result.set_item("styles", styles)?;

    let diagnostics = PyList::empty(py);
    for diagnostic in &sheet.diagnostics {
        let item = PyDict::new(py);
        item.set_item("line", diagnostic.line)?;
        item.set_item("column", diagnostic.column)?;
        item.set_item("message", &diagnostic.message)?;
        diagnostics.append(item)?;
    }
    result.set_item("stylesheet_diagnostics", diagnostics)?;
    Ok(result.unbind().into_any())
}

/// Parse HTML and an author stylesheet, then resolve token-level computed
/// values for every matching element in parent-before-child order.
#[pyfunction]
fn computed_html_snapshot(
    py: Python<'_>,
    html: &str,
    css: &str,
    selector: &str,
) -> PyResult<Py<PyAny>> {
    let output = parse_document(html);
    let selectors =
        parse_selector_list(selector).map_err(|error| PyValueError::new_err(error.to_string()))?;
    let context = MatchContext {
        quirks_mode: output.quirks_mode.as_str() == "quirks",
        ..MatchContext::default()
    };
    let matches = select_all(&output.dom, output.dom.document(), &selectors, &context);
    let sheet = parse_stylesheet(css);
    let source = CascadeInput {
        sheet: &sheet,
        origin: CascadeOrigin::Author,
    };
    let styles_by_node = compute_document_styles(
        &output.dom,
        &[source],
        &PropertyRegistry::standard_baseline(),
        &ComputationLimits::default(),
        &context,
    );

    let result = snapshot_parse_output(py, &output)?;
    let styles = PyList::empty(py);
    for node in &matches {
        let style = styles_by_node
            .get(node)
            .ok_or_else(|| PyValueError::new_err("computed style missing for matched element"))?;
        let item = PyDict::new(py);
        item.set_item("node_id", node.as_u64())?;
        let properties = PyDict::new(py);
        for (property, value) in style.properties() {
            properties.set_item(property, value.css_text())?;
        }
        for (property, value) in style.custom_properties() {
            properties.set_item(property, value.css_text())?;
        }
        item.set_item("properties", properties)?;
        let typed_properties = PyDict::new(py);
        for (property, value) in style.typed_properties() {
            let typed = PyDict::new(py);
            typed.set_item("kind", value.kind())?;
            typed.set_item("css", value.to_css())?;
            typed_properties.set_item(property, typed)?;
        }
        item.set_item("typed_properties", typed_properties)?;
        item.set_item(
            "invalid_custom_properties",
            style.invalid_custom_properties().iter().collect::<Vec<_>>(),
        )?;
        let computation_diagnostics = PyList::empty(py);
        for diagnostic in style.diagnostics() {
            let diagnostic_item = PyDict::new(py);
            diagnostic_item.set_item("property", diagnostic.property.as_deref())?;
            diagnostic_item.set_item("message", &diagnostic.message)?;
            computation_diagnostics.append(diagnostic_item)?;
        }
        item.set_item("diagnostics", computation_diagnostics)?;
        styles.append(item)?;
    }
    result.set_item("styles", styles)?;

    let diagnostics = PyList::empty(py);
    for diagnostic in &sheet.diagnostics {
        let item = PyDict::new(py);
        item.set_item("line", diagnostic.line)?;
        item.set_item("column", diagnostic.column)?;
        item.set_item("message", &diagnostic.message)?;
        diagnostics.append(item)?;
    }
    result.set_item("stylesheet_diagnostics", diagnostics)?;
    Ok(result.unbind().into_any())
}

fn snapshot_parse_output<'py>(
    py: Python<'py>,
    output: &ParseOutput,
) -> PyResult<Bound<'py, PyDict>> {
    let result = PyDict::new(py);
    result.set_item("quirks_mode", output.quirks_mode.as_str())?;

    let errors = PyList::empty(py);
    for error in &output.errors {
        let item = PyDict::new(py);
        item.set_item("offset", error.offset)?;
        item.set_item("code", error.code.as_str())?;
        errors.append(item)?;
    }
    result.set_item("errors", errors)?;
    result.set_item(
        "document",
        snapshot_node(py, &output.dom, output.dom.document())?,
    )?;
    Ok(result)
}

fn snapshot_node(py: Python<'_>, dom: &Dom, node_id: NodeId) -> PyResult<Py<PyAny>> {
    let node = dom
        .node(node_id)
        .ok_or_else(|| PyValueError::new_err("DOM snapshot referenced an unknown node"))?;
    let snapshot = PyDict::new(py);
    snapshot.set_item("id", node_id.as_u64())?;
    match node.kind() {
        NodeKind::Document => snapshot.set_item("type", "document")?,
        NodeKind::DocumentFragment => snapshot.set_item("type", "document-fragment")?,
        NodeKind::DocumentType(data) => {
            snapshot.set_item("type", "doctype")?;
            snapshot.set_item("name", &data.name)?;
            snapshot.set_item("public_id", &data.public_id)?;
            snapshot.set_item("system_id", &data.system_id)?;
        }
        NodeKind::Element(data) => {
            snapshot.set_item("type", "element")?;
            snapshot.set_item("namespace", namespace_name(&data.namespace))?;
            snapshot.set_item("local_name", &data.local_name)?;
            let attributes = PyDict::new(py);
            for attribute in &data.attributes {
                attributes.set_item(&attribute.local_name, &attribute.value)?;
            }
            snapshot.set_item("attributes", attributes)?;
        }
        NodeKind::Text(data) => {
            snapshot.set_item("type", "text")?;
            snapshot.set_item("data", data)?;
        }
        NodeKind::Comment(data) => {
            snapshot.set_item("type", "comment")?;
            snapshot.set_item("data", data)?;
        }
        NodeKind::ProcessingInstruction { target, data } => {
            snapshot.set_item("type", "processing-instruction")?;
            snapshot.set_item("target", target)?;
            snapshot.set_item("data", data)?;
        }
    }

    let children = PyList::empty(py);
    for child in node.children() {
        children.append(snapshot_node(py, dom, *child)?)?;
    }
    snapshot.set_item("children", children)?;
    Ok(snapshot.unbind().into_any())
}

fn namespace_name(namespace: &Namespace) -> &str {
    match namespace {
        Namespace::Html => "html",
        Namespace::Svg => "svg",
        Namespace::MathMl => "mathml",
        Namespace::Other(value) => value,
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(resolve_length_expr, module)?)?;
    module.add_function(wrap_pyfunction!(resolve_length_expr_strict, module)?)?;
    module.add_function(wrap_pyfunction!(parse_html_snapshot, module)?)?;
    module.add_function(wrap_pyfunction!(query_html_snapshot, module)?)?;
    module.add_function(wrap_pyfunction!(cascade_html_snapshot, module)?)?;
    module.add_function(wrap_pyfunction!(computed_html_snapshot, module)?)?;
    Ok(())
}
