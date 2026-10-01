//! Offline probe: per-element accounting of elements that resolve to a
//! non-`none` display and produce no box.
//!
//! ```text
//! cargo run --release -p render-layout --example vanish_probe -- \
//!     --ua <user-agent.css> <doc.html> <sheet1.css> [sheet2.css ...] \
//!     [--width N] [--override <css>] [--lookup <selector>]
//! ```
//!
//! A frame report that counts elements with a non-`none` display and no box is
//! answering a question whose premise needs checking, and this tool exists to
//! check it per element rather than in aggregate. For every such element it
//! reports the value the cascade produced, every author `display` declaration
//! that matched it, whether a `display: none` ancestor removed the subtree, and
//! the chain of formatting nodes above it. `--override` re-runs the same
//! document with one extra stylesheet appended, which is how a "the box is
//! missing" observation is turned into a causal claim: if the box appears when
//! one declaration changes, that declaration is the cause.
//!
//! # `--ua` is required
//!
//! A measurement tool that silently measures a different configuration than the
//! one it is standing in for produces wrong numbers that look right, so this one
//! refuses to run without the sheet. `render-layout` does not depend on
//! `render-core`, and `render-core`'s user-agent stylesheet constructor is
//! private, so the sheet has to be read from a file - and omitted, it parses as
//! an *empty* sheet rather than an error, which silently drops every
//! `display: block`, `display: none` and default-margin rule the engine relies
//! on. An earlier run of this probe measured an empty sheet and recorded 4/122
//! where the same document with the real sheet gives 20/151; the number in the
//! register was wrong because of exactly that.
//!
//! Nothing here is site-specific. The document, the sheets and the selectors
//! all arrive on the command line.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;

use render_css::cascade::{CascadeInput, CascadeOrigin, media_query_list_matches};
use render_css::computed::{
    ComputationLimits, ComputedStyle, PropertyRegistry, compute_document_styles,
};
use render_css::selector::{MatchContext, matches_selector_list, parse_selector_list, select_all};
use render_css::stylesheet::{StyleSheet, parse_stylesheet};
use render_dom::{Dom, NodeId, NodeKind};
use render_html::parse_document;
use render_layout::{
    FormattingLimits, FormattingNodeId, FormattingNodeKind, LayoutOptions, PhysicalSize,
    SimpleTextMeasurer, build_formatting_tree, layout_formatting_tree,
};

/// Display values that legitimately produce no box fragment of their own.
fn excluded_from_vanished(display: &str) -> bool {
    matches!(
        display,
        "inline" | "contents" | "table-row-group" | "table-row"
    )
}

fn display_of(style: Option<&ComputedStyle>) -> String {
    style
        .and_then(|style| style.get("display"))
        .map_or_else(|| "block".to_owned(), |value| value.css_text().to_owned())
}

fn describe(dom: &Dom, node: NodeId) -> String {
    let Some(data) = dom.node(node).map(render_dom::Node::kind) else {
        return "<missing>".to_owned();
    };
    let NodeKind::Element(element) = data else {
        return format!("{data:?}");
    };
    let mut out = element.local_name.clone();
    if let Ok(Some(id)) = dom.attribute(node, "id") {
        out.push('#');
        out.push_str(id);
    }
    if let Ok(Some(class)) = dom.attribute(node, "class") {
        out.push('.');
        out.push_str(class);
    }
    out
}

fn ancestor_chain(dom: &Dom, node: NodeId) -> Vec<NodeId> {
    let mut chain = Vec::new();
    let mut current = dom.parent(node);
    while let Some(parent) = current {
        chain.push(parent);
        current = dom.parent(parent);
    }
    chain
}

fn kind_name(kind: &FormattingNodeKind) -> String {
    match kind {
        FormattingNodeKind::BlockContainer { context } => format!("block/{context:?}"),
        FormattingNodeKind::AtomicInline { context } => format!("atomic/{context:?}"),
        FormattingNodeKind::AnonymousBlock => "anonymous".to_owned(),
        FormattingNodeKind::Inline => "inline".to_owned(),
        FormattingNodeKind::Text(_) => "text".to_owned(),
        FormattingNodeKind::Root => "root".to_owned(),
    }
}

struct Report {
    vanished: Vec<NodeId>,
    box_sources: HashSet<NodeId>,
    /// Formatting nodes by the DOM node that sourced them.
    formatting_sources: HashMap<NodeId, Vec<FormattingNodeId>>,
    /// The formatting node's own parent, so a missing box can be traced upward.
    format_parent: HashMap<FormattingNodeId, FormattingNodeId>,
    kinds: HashMap<FormattingNodeId, FormattingNodeKind>,
    /// Formatting nodes that reached the fragment tree, keyed by the solver's
    /// own node identity rather than by source, so the root box and the
    /// anonymous boxes - neither of which has a source - still read correctly
    /// in a chain.
    fragment_nodes: HashSet<FormattingNodeId>,
    fragment_count: usize,
    under_hidden: usize,
    styles: BTreeMap<NodeId, ComputedStyle>,
    dom: Dom,
    sheets: Vec<(String, StyleSheet)>,
    context: MatchContext,
}

/// The author sheets, named by file so a report line can be read against the
/// source it came from.
fn read_sheets(paths: &[String], override_css: &str) -> Vec<(String, StyleSheet)> {
    let mut sheets: Vec<(String, StyleSheet)> = paths
        .iter()
        .map(|path| {
            let name = path
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(path.as_str())
                .to_owned();
            (
                name,
                parse_stylesheet(&fs::read_to_string(path).unwrap_or_default()),
            )
        })
        .collect();
    if !override_css.trim().is_empty() {
        sheets.push(("override.css".to_owned(), parse_stylesheet(override_css)));
    }
    sheets
}

/// The user-agent sheet, read from a file because this crate does not depend on
/// `render-core` and its constructor there is private.
///
/// The source is handed in already read and already checked by
/// [`UserAgentSheet::load`], so the `unwrap_or_default` that used to be here -
/// which turned a missing file into an empty sheet and made a measurement of "no
/// user-agent origin" look like a measurement of the engine - cannot come back.
fn build(
    html: &str,
    sheet_paths: &[String],
    width: f32,
    ua_source: &str,
    override_css: &str,
) -> Report {
    let parsed = parse_document(html);
    let dom = parsed.dom;
    let sheets = read_sheets(sheet_paths, override_css);
    let context = MatchContext {
        viewport_width: Some(width),
        viewport_height: Some(600.0),
        ..MatchContext::default()
    };
    let ua = parse_stylesheet(ua_source);
    let mut inputs: Vec<CascadeInput<'_>> = vec![CascadeInput {
        sheet: &ua,
        origin: CascadeOrigin::UserAgent,
    }];
    inputs.extend(sheets.iter().map(|(_name, sheet)| CascadeInput {
        sheet,
        origin: CascadeOrigin::Author,
    }));
    let styles = compute_document_styles(
        &dom,
        &inputs,
        &PropertyRegistry::standard_baseline(),
        &ComputationLimits::default(),
        &context,
    );
    let formatting = build_formatting_tree(&dom, &styles, &FormattingLimits::default());
    let layout = layout_formatting_tree(
        &dom,
        &formatting,
        &styles,
        LayoutOptions {
            viewport: PhysicalSize {
                width,
                height: 600.0,
            },
            ..LayoutOptions::default()
        },
        &SimpleTextMeasurer,
    );
    let mut box_sources = HashSet::new();
    let mut fragment_nodes = HashSet::new();
    for fragment in layout.fragments.iter() {
        fragment_nodes.insert(fragment.formatting_node);
        if let Some(source) = fragment.source {
            box_sources.insert(source);
        }
    }
    let mut formatting_sources: HashMap<NodeId, Vec<FormattingNodeId>> = HashMap::new();
    let mut format_parent: HashMap<FormattingNodeId, FormattingNodeId> = HashMap::new();
    let mut kinds: HashMap<FormattingNodeId, FormattingNodeKind> = HashMap::new();
    for node in formatting.iter() {
        kinds.insert(node.id, node.kind.clone());
        if let Some(source) = node.source {
            formatting_sources.entry(source).or_default().push(node.id);
        }
        for child in &node.children {
            format_parent.insert(*child, node.id);
        }
    }
    let hidden_ancestor = |node: NodeId| -> Option<NodeId> {
        ancestor_chain(&dom, node)
            .into_iter()
            .find(|ancestor| display_of(styles.get(ancestor)) == "none")
    };
    let under_hidden = styles
        .keys()
        .filter(|node| hidden_ancestor(**node).is_some())
        .count();
    let vanished = styles
        .iter()
        .filter(|(node, style)| {
            // The browser frame report splits on the element's own display
            // first: `none` is counted separately, and only the remainder is
            // eligible to be called vanished.
            let display = display_of(Some(style));
            display != "none" && !excluded_from_vanished(&display) && !box_sources.contains(node)
        })
        .map(|(node, _)| *node)
        .collect();
    Report {
        vanished,
        box_sources,
        formatting_sources,
        format_parent,
        kinds,
        fragment_count: layout.fragments.iter().count(),
        fragment_nodes,
        under_hidden,
        styles,
        dom,
        sheets,
        context,
    }
}

fn author_display_declaration(report: &Report, node: NodeId) -> String {
    let mut lines = Vec::new();
    for (name, sheet) in &report.sheets {
        for rule in &sheet.rules {
            if !rule
                .media
                .iter()
                .all(|query| media_query_list_matches(query, &report.context))
            {
                continue;
            }
            if !matches_selector_list(&report.dom, node, &rule.selectors, &report.context) {
                continue;
            }
            for declaration in &rule.declarations {
                if declaration.name.eq_ignore_ascii_case("display") {
                    lines.push(format!(
                        "{name} rule#{} => display: {}",
                        rule.source_order,
                        declaration.value.trim()
                    ));
                }
            }
        }
    }
    if lines.is_empty() {
        "no matching author display declaration".to_owned()
    } else {
        lines.join(" | ")
    }
}

/// Walk the formatting tree upward from a vanished element's own formatting
/// node, reporting each ancestor's kind and whether it reached the fragment
/// tree. The first ancestor in the chain that *is* in the fragment tree is the
/// boundary: everything above it was laid out, so the break is at or below it.
fn format_chain(report: &Report, node: NodeId) -> String {
    let Some(ids) = report.formatting_sources.get(&node) else {
        return "no formatting node".to_owned();
    };
    let mut out = Vec::new();
    for id in ids {
        let mut current = Some(*id);
        let mut depth = 0;
        while let Some(id) = current {
            let kind = report
                .kinds
                .get(&id)
                .map_or_else(|| "?".to_owned(), kind_name);
            let marker = if report.fragment_nodes.contains(&id) {
                "[box]"
            } else {
                "[-]"
            };
            out.push(format!("{depth}:{kind}{marker}"));
            current = report.format_parent.get(&id).copied();
            depth += 1;
            if depth > 40 {
                break;
            }
        }
    }
    out.join(" <- ")
}

fn hidden_ancestor_of(report: &Report, node: NodeId) -> Option<NodeId> {
    ancestor_chain(&report.dom, node)
        .into_iter()
        .find(|ancestor| display_of(report.styles.get(ancestor)) == "none")
}

impl Report {
    /// Whether the element reached the box tree at all, which is the
    /// observation every count in this tool exists to qualify.
    fn has_box(&self, node: NodeId) -> bool {
        self.box_sources.contains(&node)
    }
}

fn classify(report: &Report, node: NodeId) -> String {
    let style = report.styles.get(&node);
    let display = display_of(style);
    if let Some(ancestor) = hidden_ancestor_of(report, node) {
        return format!("under-display-none({})", describe(&report.dom, ancestor));
    }
    match report.formatting_sources.get(&node) {
        None => "no-formatting-node".to_owned(),
        Some(_) => format!(
            "has-formatting-node display={display} :: {}",
            format_chain(report, node)
        ),
    }
}

fn lookup(report: &Report, selector_text: &str) {
    let Ok(selector) = parse_selector_list(selector_text) else {
        println!("  {selector_text}: unparseable selector");
        return;
    };
    let matched = select_all(
        &report.dom,
        report.dom.document(),
        &selector,
        &report.context,
    );
    println!("  {selector_text}: {} match(es)", matched.len());
    for node in matched {
        let style = report.styles.get(&node);
        let display = display_of(style);
        let hidden = hidden_ancestor_of(report, node);
        println!(
            "    {} display={display} has_box={} hidden_ancestor={} :: {}",
            describe(&report.dom, node),
            report.has_box(node),
            hidden.map_or_else(|| "-".to_owned(), |id| describe(&report.dom, id)),
            format_chain(report, node)
        );
        println!("      author: {}", author_display_declaration(report, node));
    }
}

/// What the run is actually measuring, printed in the header so a report can be
/// read against the configuration that produced it.
///
/// A user-agent sheet supplied on the command line is trusted verbatim, so it can
/// also be stale or hand-edited. Its path, its rule count and the size of the
/// file are what make that visible: two reports of the same document are
/// comparable only if they stood for the same sheet.
struct UserAgentSheet {
    path: String,
    source: String,
    bytes: usize,
    rules: usize,
}

impl UserAgentSheet {
    /// Read and parse the sheet, or refuse. An unreadable or empty file is the
    /// same failure as no file at all, because both leave the cascade with no
    /// user-agent rules and both would be measured as though they were the
    /// configuration under test.
    fn load(path: &str) -> Self {
        let source = fs::read_to_string(path).unwrap_or_else(|error| {
            eprintln!("vanish_probe: --ua {path} could not be read: {error}");
            eprintln!(
                "vanish_probe: refusing to run without the user-agent stylesheet. Without it \
                 the cascade has no user-agent origin at all, so every default `display` and \
                 every presentational hint resolves differently and the counts below describe a \
                 configuration the engine does not run."
            );
            std::process::exit(2);
        });
        let sheet = parse_stylesheet(&source);
        if sheet.rules.is_empty() {
            eprintln!("vanish_probe: --ua {path} parsed to zero rules");
            eprintln!(
                "vanish_probe: refusing to run with an empty user-agent stylesheet, for the same \
                 reason an absent one is refused: the measurement would be of a cascade with no \
                 user-agent origin."
            );
            std::process::exit(2);
        }
        Self {
            path: path.to_owned(),
            bytes: source.len(),
            rules: sheet.rules.len(),
            source,
        }
    }

    /// The header line, so a report names the sheet it was measured against and a
    /// stale or hand-edited one is visible in the output rather than inferred.
    fn describe(&self) -> String {
        format!(
            "ua={} ({} bytes, {} rules)",
            self.path, self.bytes, self.rules
        )
    }
}

/// The command line, parsed into the values the report needs.
///
/// Split out of `main` so that the argument grammar and the refusal to run
/// without a user-agent sheet are two readable pieces rather than one long
/// function, and so the usage message sits next to the flags it describes.
struct Options {
    width: f32,
    paths: Vec<String>,
    lookups: Vec<String>,
    ua_path: String,
    override_css: String,
}

fn parse_options(args: &[String]) -> Options {
    let mut options = Options {
        width: 1280.0,
        paths: Vec::new(),
        lookups: Vec::new(),
        ua_path: String::new(),
        override_css: String::new(),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || iter.next().cloned();
        match arg.as_str() {
            "--width" => {
                options.width = value()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1280.0);
            }
            "--lookup" => {
                if let Some(value) = value() {
                    options.lookups.push(value);
                }
            }
            "--ua" => {
                if let Some(value) = value() {
                    options.ua_path = value;
                } else {
                    eprintln!("vanish_probe: --ua needs the path to a user-agent stylesheet");
                    std::process::exit(2);
                }
            }
            "--override" => {
                if let Some(value) = value() {
                    options.override_css = value;
                }
            }
            _ => options.paths.push(arg.clone()),
        }
    }
    options
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!(
            "usage: vanish_probe --ua <user-agent.css> <doc.html> <sheet1.css> [sheet2.css ...] \
             [--width N] [--lookup SEL]"
        );
        std::process::exit(2);
    }
    let Options {
        width,
        paths,
        lookups,
        ua_path,
        override_css,
    } = parse_options(&args);
    if ua_path.is_empty() {
        eprintln!("vanish_probe: --ua <user-agent.css> is required");
        eprintln!(
            "vanish_probe: render-layout does not depend on render-core, and render-core's \
             user-agent stylesheet constructor is private, so the sheet is read from a file. \
             Omitting it leaves the cascade with no user-agent origin: the registry's initial \
             `display` is `inline`, so every div, p and table in the document collapses into \
             anonymous inlines, presentational hints never apply, and the counts below describe a \
             configuration the engine never runs. One recorded measurement in the register is \
             wrong for exactly this reason."
        );
        std::process::exit(2);
    }
    let user_agent = UserAgentSheet::load(&ua_path);
    let (html_path, sheet_paths) = paths.split_first().expect("checked above");
    let html = fs::read_to_string(html_path).expect("document readable");
    let detail = std::env::var_os("RENDER_PROBE_DETAIL").is_some();

    for label in ["unstyled", "styled"] {
        let sheets: Vec<String> = if label == "styled" {
            sheet_paths.to_vec()
        } else {
            Vec::new()
        };
        let report = build(&html, &sheets, width, &user_agent.source, &override_css);
        println!("=== {label} (width {width}, {}) ===", user_agent.describe());
        println!(
            "author_sheets={} override={}",
            report.sheets.len(),
            if override_css.trim().is_empty() {
                "none"
            } else {
                "yes"
            }
        );
        println!(
            "elements={} fragments={} elements_with_a_box={} under_display_none={} vanished={}",
            report.styles.len(),
            report.fragment_count,
            report.box_sources.len(),
            report.under_hidden,
            report.vanished.len()
        );
        let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
        for node in &report.vanished {
            *by_category.entry(classify(&report, *node)).or_default() += 1;
        }
        println!("--- categories ---");
        for (category, count) in &by_category {
            println!("{count:5}  {category}");
        }
        if detail {
            println!("--- per element ---");
            for node in &report.vanished {
                let style = report.styles.get(node);
                println!(
                    "  {} display={} specified={} | {} | {}",
                    describe(&report.dom, *node),
                    display_of(style),
                    style.is_some_and(|style| style.specified("display")),
                    author_display_declaration(&report, *node),
                    classify(&report, *node)
                );
            }
        }
        let mut names: BTreeSet<String> = BTreeSet::new();
        for node in &report.vanished {
            names.insert(describe(&report.dom, *node));
        }
        println!("--- vanished names ({} distinct) ---", names.len());
        for name in &names {
            println!("  {name}");
        }
        for selector in &lookups {
            lookup(&report, selector);
        }
    }
}
