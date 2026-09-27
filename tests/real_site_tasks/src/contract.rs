//! The acceptance contract, expressed once, as pure checks over a [`Session`].
//!
//! Every check here is one of the seven items `docs/real_site_acceptance.md`
//! lists, plus the extra shapes the portal fixture is required to preserve. Each
//! check is a statement about the *engine's* observable behaviour after a
//! normal offline load: the DOM, the computed styles, the fragment tree, the
//! display list, the resource discovery plans, or the raster.
//!
//! Nothing in this module asserts behaviour that a documented gap in
//! `docs/visual_fidelity_gaps.md` makes impossible. Where such a behaviour is
//! worth pinning down, it lives in `tests/real_site_capabilities.rs` behind an
//! `#[ignore]` that names the gap, not here.

use std::collections::BTreeSet;

use render_core::document::{AuthorStyleSource, DocumentDiagnosticCode};
use render_core::dom::{Dom, NodeId};
use render_core::html::HtmlDecodeDiagnosticCode;
use render_core::image::ImageSource;
use render_core::script::{ScriptScheduling, ScriptSource};

use crate::harness::Session;
use crate::inspect::{
    self, attribute, box_of, document_title, lines_in_reading_order, normalized_text,
    scroll_region, select, select_one, subtree_text, subtree_text_node_lines, tag,
};

/// The outcome of running a group of contract checks.
#[derive(Debug)]
pub struct Report {
    pub label: &'static str,
    /// How many individual requirements were checked.
    pub checks: usize,
    /// One message per unmet requirement.
    pub violations: Vec<String>,
}

impl Report {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            checks: 0,
            violations: Vec::new(),
        }
    }

    /// Record one requirement.
    fn require(&mut self, satisfied: bool, message: impl Into<String>) {
        self.checks += 1;
        if !satisfied {
            self.violations.push(message.into());
        }
    }

    /// Whether every requirement held.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }

    /// A failure message listing every unmet requirement, or a pass summary.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.violations.is_empty() {
            return format!("{}: {} contract checks passed", self.label, self.checks);
        }
        let mut message = format!(
            "{}: {} of {} contract checks failed",
            self.label,
            self.violations.len(),
            self.checks
        );
        for violation in &self.violations {
            message.push_str("\n  - ");
            message.push_str(violation);
        }
        message
    }
}

/// Run the seven contract items for one fixture.
#[must_use]
pub fn run(session: &Session) -> Report {
    let mut report = Report::new(session.fixture.label);
    decoded_title(&mut report, session);
    landmarks(&mut report, session);
    usable_links(&mut report, session);
    search_landmark(&mut report, session);
    resource_classification(&mut report, session);
    block_layout_past_first_screen(&mut report, session);
    ordered_scroll_region(&mut report, session);
    portal_shapes(&mut report, session);
    report
}

/// 1. A decoded document title.
fn decoded_title(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let decoded = &session.decoded;

    report.require(
        !decoded.encoding_name().eq_ignore_ascii_case("replacement"),
        format!("{label}: the fixture did not decode to a usable encoding"),
    );
    let replaced = decoded
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == HtmlDecodeDiagnosticCode::DecodingErrorReplaced)
        .count();
    report.require(
        replaced == 0,
        format!("{label}: {replaced} decode diagnostics replaced bytes with U+FFFD"),
    );

    let title = document_title(session.document.dom());
    report.require(
        title.as_deref() == Some(session.fixture.expected_title),
        format!(
            "{label}: document title is {title:?}, expected {:?}",
            session.fixture.expected_title
        ),
    );
    if let Some(title) = &title {
        report.require(
            !title.contains('\u{FFFD}'),
            format!("{label}: the document title contains a replacement character"),
        );
    }
}

/// 2. Header, navigation, main, article, section, and aside semantics.
fn landmarks(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let dom = session.document.dom();
    for selector in ["header", "nav", "main", "article", "section", "aside"] {
        let matched = select(dom, selector);
        report.require(
            !matched.is_empty(),
            format!("{label}: no <{selector}> landmark is present"),
        );
        for node in &matched {
            let laid_out = box_of(session.fragments(), *node).is_some_and(|laid_out| {
                laid_out.border.size.width > 0.0 && laid_out.border.size.height > 0.0
            });
            report.require(
                laid_out,
                format!("{label}: <{selector}> at {node:?} has no non-empty laid-out box"),
            );
        }
    }
}

/// 3. Usable links.
fn usable_links(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let dom = session.document.dom();
    let links = select(dom, "a[href]");
    report.require(
        links.len() >= session.fixture.min_links,
        format!(
            "{label}: {} navigable links, expected at least {}",
            links.len(),
            session.fixture.min_links
        ),
    );
    for node in &links {
        let href = attribute(dom, *node, "href").unwrap_or_default().trim();
        report.require(
            !href.is_empty(),
            format!("{label}: <a> at {node:?} has an empty href"),
        );
        match session.base_url.join(href) {
            Ok(resolved) => {
                report.require(
                    matches!(resolved.scheme(), "http" | "https"),
                    format!(
                        "{label}: link href {href:?} resolves to non-navigable scheme {}",
                        resolved.scheme()
                    ),
                );
            }
            Err(error) => report.require(
                false,
                format!("{label}: link href {href:?} does not resolve: {error}"),
            ),
        }
        report.require(
            accessible_name(dom, *node),
            format!("{label}: <a href={href:?}> has neither text nor an image alt text"),
        );
        // An inline link owns a text fragment rather than a box, and a link whose
        // whole content is an image owns neither, so the whole subtree counts.
        report.require(
            occupies_area(session, *node),
            format!("{label}: <a href={href:?}> laid out with no area"),
        );
    }
}

/// 4. A `role=search` form with a named input and a submit control.
fn search_landmark(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let dom = session.document.dom();
    let forms = select(dom, "form[role=search]");
    report.require(
        forms.len() == 1,
        format!(
            "{label}: {} forms carry role=search, expected exactly one",
            forms.len()
        ),
    );
    let named_inputs: Vec<NodeId> = select(dom, "form[role=search] input")
        .into_iter()
        .filter(|node| {
            let name = attribute(dom, *node, "name").unwrap_or_default().trim();
            let kind = attribute(dom, *node, "type")
                .unwrap_or("text")
                .trim()
                .to_ascii_lowercase();
            !name.is_empty() && kind != "hidden"
        })
        .collect();
    report.require(
        !named_inputs.is_empty(),
        format!("{label}: the search form has no named, visible input"),
    );
    for input in &named_inputs {
        report.require(
            occupies_area(session, *input),
            format!("{label}: the search input at {input:?} laid out with no area"),
        );
    }
    let submits = select(dom, "form[role=search] button")
        .into_iter()
        .chain(select(dom, r#"form[role=search] input[type="submit"]"#))
        .count();
    report.require(
        submits > 0,
        format!("{label}: the search form has no submit control"),
    );
}

/// 5. Stylesheet, image, and script resource classification.
fn resource_classification(report: &mut Report, session: &Session) {
    stylesheet_resources(report, session);
    image_resources(report, session);
    script_resources(report, session);
}

/// 5a. External stylesheets: every eligible slot resolved, was answered, and
/// reached layout.
fn stylesheet_resources(report: &mut Report, session: &Session) {
    let label = session.fixture.label;

    // Stylesheets: every eligible external slot resolved and was answered.
    let slots = session.external_style_slots();
    report.require(
        slots.len() == session.fixture.expected_stylesheets,
        format!(
            "{label}: {} eligible external stylesheets, expected {}",
            slots.len(),
            session.fixture.expected_stylesheets
        ),
    );
    for slot in &slots {
        let AuthorStyleSource::External {
            href,
            resolved_url: Some(resolved),
        } = &slot.source
        else {
            continue;
        };
        report.require(
            matches!(resolved.scheme(), "http" | "https") && !resolved.path().ends_with('/'),
            format!("{label}: stylesheet href {href:?} did not resolve to a file URL"),
        );
        report.require(
            resolved.path() != session.base_url.path(),
            format!("{label}: stylesheet href {href:?} did not resolve away from the page"),
        );
    }
    let unsupplied: Vec<String> = session
        .style_diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                diagnostic.code,
                DocumentDiagnosticCode::ExternalStyleSheetUnsupported
                    | DocumentDiagnosticCode::ExternalStyleSheetUnresolved
                    | DocumentDiagnosticCode::AuthorStyleSlotLimit
                    | DocumentDiagnosticCode::ExternalStyleSheetLimit
                    | DocumentDiagnosticCode::ExternalStyleSheetUrlBytesLimit
            )
        })
        .map(|diagnostic| diagnostic.message.clone())
        .collect();
    report.require(
        unsupplied.is_empty(),
        format!(
            "{label}: the engine could not use the supplied stylesheet responses: {}",
            unsupplied.join("; ")
        ),
    );
    // The external sheet's own rules must be in the cascade *and* in the layout,
    // not merely fetched: the main content column is exactly as wide and as far
    // right as the fixture's local stylesheet declares, and no UA rule declares
    // that. A sheet that was discovered but not applied fails here.
    let dom = session.document.dom();
    let main = select_one(dom, "main");
    report.require(
        main.is_some(),
        format!("{label}: the fixture has no <main>"),
    );
    if let Some(main) = main {
        match box_of(session.fragments(), main) {
            None => report.require(false, format!("{label}: <main> laid out no box at all")),
            Some(laid_out) => {
                let expected_width = session.fixture.content_width;
                report.require(
                    (laid_out.content.size.width - expected_width).abs() < 0.5,
                    format!(
                        "{label}: the content column is {} wide, but the supplied external \
                         stylesheet declares {expected_width}",
                        laid_out.content.size.width
                    ),
                );
                let expected_left = session.fixture.content_left;
                report.require(
                    (laid_out.content.origin.x - expected_left).abs() < 0.5,
                    format!(
                        "{label}: the content column starts at x={}, but the supplied external \
                         stylesheet declares {expected_left}",
                        laid_out.content.origin.x
                    ),
                );
            }
        }
    }
}

/// 5b. Images: discovery must find exactly the elements that declare a
/// fetchable source, and nothing else.
fn image_resources(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let dom = session.document.dom();
    let fetchable = images_with_a_fetchable_source(dom);
    report.require(
        fetchable.len() == session.fixture.expected_images,
        format!(
            "{label}: {} elements declare a fetchable image source, expected {}",
            fetchable.len(),
            session.fixture.expected_images
        ),
    );
    report.require(
        session.image_discovery.resources.len() == session.fixture.expected_images,
        format!(
            "{label}: image discovery planned {} requests, expected {}",
            session.image_discovery.resources.len(),
            session.fixture.expected_images
        ),
    );
    let failures = session.image_discovery_failures();
    report.require(
        failures.is_empty(),
        format!(
            "{label}: image discovery reported errors: {}",
            failures
                .iter()
                .map(|(code, message)| format!("{code:?}: {message}"))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    );
    for discovered in &session.image_discovery.resources {
        let url = &discovered.key.requested_url;
        report.require(
            matches!(url.scheme(), "http" | "https")
                && !url.host_str().unwrap_or_default().is_empty(),
            format!("{label}: discovered image URL {url} is not a fetchable absolute URL"),
        );
    }
}

/// 5c. Scripts: every discovered script is a deferred external script.
fn script_resources(report: &mut Report, session: &Session) {
    let label = session.fixture.label;

    // Scripts: every discovered script is a deferred external script.
    let scripts = &session.script_discovery.scripts;
    report.require(
        scripts.len() == session.fixture.expected_scripts,
        format!(
            "{label}: {} scripts discovered, expected {}",
            scripts.len(),
            session.fixture.expected_scripts
        ),
    );
    for script in scripts {
        match &script.source {
            ScriptSource::External { src, resolved_url } => {
                report.require(
                    matches!(resolved_url.scheme(), "http" | "https"),
                    format!("{label}: script src {src:?} did not resolve to an absolute URL"),
                );
            }
            ScriptSource::Inline { source } => report.require(
                false,
                format!(
                    "{label}: discovered an inline script of {} bytes; the fixtures declare none",
                    source.len()
                ),
            ),
        }
        report.require(
            script.scheduling == ScriptScheduling::Defer,
            format!(
                "{label}: script scheduling is {:?}, expected Defer",
                script.scheduling
            ),
        );
    }
    let script_failures: Vec<String> = session
        .script_discovery
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{:?}: {}", diagnostic.code, diagnostic.message))
        .collect();
    report.require(
        script_failures.is_empty(),
        format!(
            "{label}: script discovery reported {}",
            script_failures.join("; ")
        ),
    );
}

/// 6. Block layout that continues below a 600px viewport.
fn block_layout_past_first_screen(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let fragments = session.fragments();
    let first_screen = crate::harness::contract_viewport().height;
    let maximum_scroll = fragments.max_scroll_offset().y;
    report.require(
        maximum_scroll > 0.0,
        format!("{label}: the document does not scroll (max scroll y is {maximum_scroll})"),
    );
    report.require(
        fragments.scrollable_content_size.height > first_screen,
        format!(
            "{label}: scrollable content is {} tall, which fits in the {first_screen}px first screen",
            fragments.scrollable_content_size.height
        ),
    );
    let boxes = fragments
        .iter()
        .filter(|fragment| matches!(fragment.kind, render_core::layout::FragmentKind::Box(_)))
        .count();
    report.require(
        boxes > 100,
        format!("{label}: only {boxes} box fragments were laid out, which is not a page"),
    );
    let below_the_fold = fragments
        .iter()
        .any(|fragment| fragment.rect.origin.y + fragment.rect.size.height > first_screen);
    report.require(
        below_the_fold,
        format!("{label}: nothing is laid out below the {first_screen}px first screen"),
    );
}

/// 7. Ordered text blocks in the scroll region.
fn ordered_scroll_region(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let dom = session.document.dom();
    let fragments = session.fragments();
    let Some((region, blocks)) = scroll_region(dom, fragments, session.fixture.min_scroll_blocks)
    else {
        report.require(
            false,
            format!(
                "{label}: no element inside <main> holds {} ordered text blocks",
                session.fixture.min_scroll_blocks
            ),
        );
        return;
    };
    report.require(
        blocks.len() >= session.fixture.min_scroll_blocks,
        format!(
            "{label}: scroll region {region:?} holds {} text blocks",
            blocks.len()
        ),
    );
    let first_screen = crate::harness::contract_viewport().height;
    report.require(
        blocks
            .last()
            .is_some_and(|block| block.bottom > first_screen),
        format!("{label}: the scroll region ends above the first screen"),
    );
    for pair in blocks.windows(2) {
        let (left, right) = (&pair[0], &pair[1]);
        report.require(
            right.top > left.top,
            format!(
                "{label}: scroll blocks are out of order: {:?} at y={} is followed by {:?} at y={}",
                left.text, left.top, right.text, right.top
            ),
        );
    }
    for block in &blocks {
        report.require(
            !block.text.is_empty(),
            format!("{label}: scroll block at y={} carries no text", block.top),
        );
    }
    // Reading the whole page top-to-bottom must also follow document order.
    let reading = lines_in_reading_order(dom, fragments);
    let out_of_order = reading
        .windows(2)
        .find(|pair| pair[0].dom_rank > pair[1].dom_rank);
    report.require(
        out_of_order.is_none(),
        match out_of_order {
            None => String::new(),
            Some(pair) => format!(
                "{label}: page text is out of document order: {:?} (rank {}) is painted above {:?} (rank {})",
                pair[0].line.text, pair[0].dom_rank, pair[1].line.text, pair[1].dom_rank
            ),
        },
    );
}

/// The extra resource shapes the portal fixture is required to preserve.
///
/// Written generically, so it is evaluated for every fixture; the counts come
/// from each fixture's own table entry and are zero where the shape does not
/// occur.
fn portal_shapes(report: &mut Report, session: &Session) {
    image_alt_text(report, session);
    deferred_image_shapes(report, session);
    srcset_and_poster_shapes(report, session);
    channel_navigation(report, session);
}

/// Every image carries alt text.
fn image_alt_text(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let dom = session.document.dom();
    for node in select(dom, "img") {
        let alt = attribute(dom, node, "alt").unwrap_or_default();
        report.require(
            !alt.trim().is_empty(),
            format!("{label}: <img> at {node:?} has no alt text"),
        );
    }
}

/// Deferred `data-src` / `data-original` images keep usable candidates, are
/// never fetched, and are never reported as a discovery *error*. The engine
/// currently emits no diagnostic for them at all: `MissingSource` is the code
/// reserved for this case and is declared but never constructed, so the contract
/// permits it rather than requiring it.
fn deferred_image_shapes(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let dom = session.document.dom();
    let deferred = deferred_images(dom);
    report.require(
        deferred.len() == session.fixture.expected_deferred_images,
        format!(
            "{label}: {} deferred data-src images, expected {}",
            deferred.len(),
            session.fixture.expected_deferred_images
        ),
    );
    for node in &deferred {
        for attribute_name in ["data-src", "data-original"] {
            let value = attribute(dom, *node, attribute_name)
                .unwrap_or_default()
                .trim();
            report.require(
                !value.is_empty(),
                format!("{label}: deferred <img> at {node:?} has no {attribute_name}"),
            );
            report.require(
                value.is_empty() || session.base_url.join(value).is_ok(),
                format!("{label}: deferred <img> {attribute_name}={value:?} does not resolve"),
            );
        }
        report.require(
            !session
                .image_discovery
                .resources
                .iter()
                .any(|resource| resource.key.owner == *node),
            format!("{label}: a deferred <img> at {node:?} was fetched anyway"),
        );
    }
    let reported: BTreeSet<NodeId> = session.deferred_image_nodes().into_iter().collect();
    let expected: BTreeSet<NodeId> = deferred.iter().copied().collect();
    report.require(
        reported.is_subset(&expected),
        format!(
            "{label}: the engine reported {reported:?} as source-less, but only {expected:?} \
             are deferred"
        ),
    );
}

/// `srcset` images resolve to a real candidate from their own list, and a
/// `video[poster]` is discovered as a poster rather than as an element image.
fn srcset_and_poster_shapes(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let dom = session.document.dom();

    // `srcset` images resolve to a real candidate from their own list.
    let srcset_images = srcset_images(dom);
    report.require(
        srcset_images.len() == session.fixture.expected_srcset_images,
        format!(
            "{label}: {} srcset images, expected {}",
            srcset_images.len(),
            session.fixture.expected_srcset_images
        ),
    );
    for node in &srcset_images {
        let candidates = srcset_candidates(&session.base_url, dom, *node);
        report.require(
            candidates.len() >= 2,
            format!("{label}: <img srcset> at {node:?} has fewer than two candidates"),
        );
        let chosen = session
            .image_discovery
            .resources
            .iter()
            .find(|resource| resource.key.owner == *node)
            .map(|resource| resource.key.requested_url.clone());
        match chosen {
            None => report.require(
                false,
                format!("{label}: <img srcset> at {node:?} resolved to no candidate"),
            ),
            Some(url) => report.require(
                candidates.contains(&url),
                format!(
                    "{label}: <img srcset> at {node:?} resolved to {url}, which is not one of its candidates"
                ),
            ),
        }
    }

    // `video[poster]` images are discovered as posters, not as elements.
    let posters = select(dom, "video[poster]");
    report.require(
        posters.len() == session.fixture.expected_video_posters,
        format!(
            "{label}: {} video posters, expected {}",
            posters.len(),
            session.fixture.expected_video_posters
        ),
    );
    let poster_nodes: BTreeSet<NodeId> = session
        .discovered_by_source()
        .iter()
        .filter(|(source, _)| *source == ImageSource::VideoPoster)
        .map(|(_, discovered)| discovered.key.owner)
        .collect();
    report.require(
        poster_nodes == posters.iter().copied().collect(),
        format!("{label}: poster discovery found {poster_nodes:?}, expected {posters:?}"),
    );
}

/// Channel sections, and the channel links that reach them.
fn channel_navigation(report: &mut Report, session: &Session) {
    let label = session.fixture.label;
    let dom = session.document.dom();

    // Channel sections and the channel links that reach them.
    let channels: Vec<NodeId> = select(dom, "section")
        .into_iter()
        .filter(|node| descendant_link_count(dom, *node) >= 3)
        .collect();
    report.require(
        channels.len() >= session.fixture.min_channel_sections,
        format!(
            "{label}: {} channel sections carry at least three links, expected at least {}",
            channels.len(),
            session.fixture.min_channel_sections
        ),
    );
    let channel_links: Vec<NodeId> = select(dom, "nav a[href]")
        .into_iter()
        .chain(select(dom, "aside nav a[href]"))
        .collect();
    report.require(
        channel_links.len() >= 6 || session.fixture.expected_deferred_images == 0,
        format!(
            "{label}: only {} channel navigation links are present",
            channel_links.len()
        ),
    );
    for node in &channel_links {
        let href = attribute(dom, *node, "href").unwrap_or_default().trim();
        report.require(
            session.base_url.join(href).is_ok(),
            format!("{label}: channel link {href:?} does not resolve"),
        );
        report.require(
            occupies_area(session, *node),
            format!("{label}: channel link {href:?} laid out with no area"),
        );
    }
}

/// Elements that declare a fetchable image source right now.
#[must_use]
pub fn images_with_a_fetchable_source(dom: &Dom) -> Vec<NodeId> {
    select(dom, "img")
        .into_iter()
        .filter(|node| non_empty(dom, *node, "src") || non_empty(dom, *node, "srcset"))
        .chain(select(dom, "video[poster]"))
        .collect()
}

/// Images whose bytes have not been requested yet, because a lazy-loading
/// script has not run.
#[must_use]
pub fn deferred_images(dom: &Dom) -> Vec<NodeId> {
    select(dom, "img")
        .into_iter()
        .filter(|node| {
            (non_empty(dom, *node, "data-src") || non_empty(dom, *node, "data-original"))
                && !non_empty(dom, *node, "src")
                && !non_empty(dom, *node, "srcset")
        })
        .collect()
}

/// Images whose only source is a candidate list.
#[must_use]
pub fn srcset_images(dom: &Dom) -> Vec<NodeId> {
    select(dom, "img")
        .into_iter()
        .filter(|node| non_empty(dom, *node, "srcset") && !non_empty(dom, *node, "src"))
        .collect()
}

/// The absolute URLs a `srcset` attribute offers, in declaration order,
/// resolved against the document base URL.
#[must_use]
pub fn srcset_candidates(base: &url::Url, dom: &Dom, node: NodeId) -> Vec<url::Url> {
    attribute(dom, node, "srcset")
        .unwrap_or_default()
        .split(',')
        .filter_map(|candidate| {
            let reference = candidate.split_ascii_whitespace().next()?.trim();
            let reference = (!reference.is_empty()).then_some(reference)?;
            base.join(reference).ok()
        })
        .collect()
}

/// Whether an attribute holds a non-empty, non-blank value.
fn non_empty(dom: &Dom, node: NodeId, name: &str) -> bool {
    attribute(dom, node, name).is_some_and(|value| !value.trim().is_empty())
}

/// A link's accessible name: its own text, or the alt text of an image inside.
fn accessible_name(dom: &Dom, node: NodeId) -> bool {
    if !normalized_text(&subtree_text(dom, node)).is_empty() {
        return true;
    }
    descendants(dom, node).into_iter().any(|descendant| {
        tag(dom, descendant) == Some("img")
            && attribute(dom, descendant, "alt").is_some_and(|alt| !alt.trim().is_empty())
    })
}

fn descendants(dom: &Dom, node: NodeId) -> Vec<NodeId> {
    let mut found = Vec::new();
    let mut pending = dom.children(node).unwrap_or_default().to_vec();
    while let Some(current) = pending.pop() {
        if tag(dom, current).is_some() {
            found.push(current);
        }
        pending.extend(dom.children(current).unwrap_or_default());
    }
    found
}

fn descendant_link_count(dom: &Dom, node: NodeId) -> usize {
    descendants(dom, node)
        .into_iter()
        .filter(|descendant| {
            tag(dom, *descendant) == Some("a") && non_empty(dom, *descendant, "href")
        })
        .count()
}

/// Whether the node, or anything inside it, was laid out with non-zero area.
///
/// Three shapes have to be accepted, because the layout attributes each of them
/// differently: a block owns a box, an inline-block owns a box *and* has its
/// text attributed to a text node, and a plain inline owns neither - its text
/// lines belong to the text nodes under it. A link whose whole content is a
/// logo image is the fourth: the image owns the box.
fn occupies_area(session: &Session, node: NodeId) -> bool {
    let dom = session.document.dom();
    let fragments = session.fragments();
    let box_is_real = |candidate: NodeId| {
        box_of(fragments, candidate).is_some_and(|laid_out| {
            laid_out.border.size.width > 0.0 || laid_out.border.size.height > 0.0
        })
    };
    let line_is_real =
        |line: &inspect::TextLine| line.rect.size.width > 0.0 && line.rect.size.height > 0.0;
    box_is_real(node)
        || subtree_text_node_lines(dom, fragments, node)
            .iter()
            .any(line_is_real)
        || descendants(dom, node).into_iter().any(box_is_real)
}
