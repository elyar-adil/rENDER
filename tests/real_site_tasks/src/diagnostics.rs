//! The diagnostic stream a fixture produces, as a comparable set.
//!
//! Diagnostics are a **first-class output** here, not a side effect. The engine
//! reports a great deal that it cannot yet do - at-rules it parses and drops,
//! `@supports` conditions it cannot answer, declarations it throws out of a
//! block - and `render-browser` currently *counts* those without displaying
//! them, because mapping a message to a `StylesheetDiagnosticCode` does not
//! exist yet. This module is therefore the only place in the tree where a page's
//! diagnostic stream can be read at all.
//!
//! The expected set is stated per fixture, in [`crate::contract`]. The rule the
//! assertions encode is asymmetric on purpose:
//!
//! * A diagnostic this engine **owes its author** appearing is correct
//!   behaviour. A real page's stylesheet uses `@font-face`, and the honest
//!   expectation is that the engine says it dropped them.
//! * A diagnostic **going missing** is a defect, and the worst one available: a
//!   missing diagnostic is how a page silently loses 92 rules (the `@supports`
//!   case) or a whole declaration block (the star-hack case). Nothing in the
//!   render can tell, so nothing catches it.
//!
//! Hence a *set* comparison, not "no errors": a diagnostic appearing is pinned,
//! a diagnostic disappearing is a failure, and a diagnostic changing what it
//! says is a visible diff rather than a silent drift.

use std::collections::BTreeMap;
use std::fmt;

use render_core::dom::NodeId;

/// Which stage of the pipeline produced a diagnostic.
///
/// The stage matters because the stages do not have the same vocabulary. Four of
/// them carry a typed code; the stylesheet and computed-style stages carry only
/// a prose message, which is why [`Kind`] has to recover a comparable key from
/// the message text. That asymmetry is a finding in its own right and is
/// recorded in the round report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// HTML encoding sniffing, before parsing.
    HtmlDecode,
    /// HTML tokenizing and tree construction.
    HtmlParse,
    /// Stylesheet discovery: which slots exist, which are eligible.
    StyleDiscovery,
    /// Stylesheet syntax and capability reporting, per sheet.
    StyleSheet,
    /// Computed-value time: declarations the cascade threw out.
    ComputedStyle,
    /// Formatting-context construction.
    Formatting,
    /// Layout.
    Layout,
    /// Display-list construction.
    DisplayList,
    /// Rasterisation.
    Raster,
    /// Image discovery and the image store.
    Image,
    /// Script discovery.
    Script,
    /// Inline `svg` discovery and rasterisation.
    InlineSvg,
}

impl Stage {
    /// A stable lower-case name, for fixture tables and failure text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::HtmlDecode => "html-decode",
            Self::HtmlParse => "html-parse",
            Self::StyleDiscovery => "style-discovery",
            Self::StyleSheet => "stylesheet",
            Self::ComputedStyle => "computed-style",
            Self::Formatting => "formatting",
            Self::Layout => "layout",
            Self::DisplayList => "display-list",
            Self::Raster => "raster",
            Self::Image => "image",
            Self::Script => "script",
            Self::InlineSvg => "inline-svg",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A comparable identity for one diagnostic.
///
/// The stages that carry a typed code use the code, verbatim. The two that carry
/// only prose have their message reduced to a *kind plus the part that varies* -
/// the at-rule name, the `@supports` feature, the offending token - so the set
/// says "three `@font-face` blocks were dropped" rather than repeating one
/// sentence three times. Anything the reducer does not recognise becomes
/// [`Kind::Unclassified`] with the whole message, so a new message shape shows up
/// as a new set member instead of being folded into an existing one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// `@x is parsed but not evaluated yet`, carrying the spelling the author
    /// wrote.
    AtRuleNotEvaluated(String),
    /// `@x is not an at-rule this engine knows`.
    AtRuleUnknown(String),
    /// `@supports <feature> is not answered: <reason>`, carrying the feature.
    SupportsNotAnswered(String),
    /// An invalid `@supports` condition, so the block and its contents are
    /// dropped.
    SupportsConditionInvalid,
    /// A declaration thrown out of a declaration list, carrying the reason
    /// after the fixed prefix.
    InvalidDeclaration(String),
    /// A rule thrown out because its selector did not parse.
    InvalidSelector(String),
    /// An invalid cascade-layer name.
    InvalidLayerName,
    /// A token where §5.4.6 required a `<colon-token>`, carrying the token.
    UnexpectedToken(String),
    /// The input ended inside a declaration list.
    UnexpectedEndOfInput,
    /// A nested cascade layer, which is not implemented.
    NestedCascadeLayer,
    /// A diagnostic from a stage that carries a typed code, formatted as the
    /// code's own `Debug` spelling.
    Coded(&'static str, String),
    /// One glyph the reference raster could not mask.
    ///
    /// This is a property of the reference backend, not of the page.
    /// [`render_core::paint::NoGlyphMasks`] emits one of these for every glyph in
    /// every run and paints no letterforms at all, so the *number* says how much
    /// text a fixture has and nothing about the engine's capability. It gets its
    /// own kind with no detail for exactly that reason: the count belongs in a
    /// report, never in a pinned set, while "a raster diagnostic that is not a
    /// missing glyph" is a real defect and must fail the expected-set check.
    ReferenceGlyphMask,
    /// A message the reducer does not recognise, kept whole.
    Unclassified(String),
}

impl Kind {
    /// A short stable name, used in the per-fixture expected sets and in the
    /// per-kind counts.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::AtRuleNotEvaluated(name) => format!("at-rule-not-evaluated @{name}"),
            Self::AtRuleUnknown(name) => format!("at-rule-unknown @{name}"),
            Self::SupportsNotAnswered(feature) => format!("supports-not-answered {feature}"),
            Self::SupportsConditionInvalid => "supports-condition-invalid".to_owned(),
            Self::InvalidDeclaration(detail) => format!("invalid-declaration {detail}"),
            Self::InvalidSelector(detail) => format!("invalid-selector {detail}"),
            Self::InvalidLayerName => "invalid-layer-name".to_owned(),
            Self::UnexpectedToken(token) => format!("unexpected-token {token}"),
            Self::UnexpectedEndOfInput => "unexpected-end-of-input".to_owned(),
            Self::NestedCascadeLayer => "nested-cascade-layer".to_owned(),
            Self::Coded(code, detail) => {
                if detail.is_empty() {
                    (*code).to_owned()
                } else {
                    format!("{code} {detail}")
                }
            }
            Self::ReferenceGlyphMask => "reference-raster-missing-glyph".to_owned(),
            Self::Unclassified(message) => format!("unclassified {message}"),
        }
    }
}

/// One diagnostic, with enough context to point at a place in the page.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    pub stage: Stage,
    pub kind: Kind,
    /// The DOM node the diagnostic is about, when the stage names one.
    pub node: Option<NodeId>,
    /// The message exactly as the engine wrote it. The expected sets pin
    /// [`Self::kind`]; this is carried so a failure can quote the prose.
    pub message: String,
}

impl Entry {
    /// One diagnostic, with enough context to point at a place in the page.
    #[must_use]
    pub fn new(stage: Stage, kind: Kind, node: Option<NodeId>, message: String) -> Self {
        Self {
            stage,
            kind,
            node,
            message,
        }
    }

    /// The comparable key: stage, kind, and node. Two runs of the same fixture
    /// produce the same node ids, so this is stable across runs.
    #[must_use]
    pub fn key(&self) -> (Stage, Kind, Option<NodeId>) {
        (self.stage, self.kind.clone(), self.node)
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.stage, self.kind.summary())?;
        if let Some(node) = self.node {
            write!(formatter, " at {node:?}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

/// Every diagnostic one fixture load produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stream {
    entries: Vec<Entry>,
}

impl Stream {
    /// Build a stream from entries in the order the stages produced them.
    #[must_use]
    pub fn new(entries: Vec<Entry>) -> Self {
        Self { entries }
    }

    /// Every entry, in stage order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The comparable keys, deduplicated and ordered.
    #[must_use]
    pub fn keys(&self) -> Vec<(Stage, Kind, Option<NodeId>)> {
        let mut keys: Vec<(Stage, Kind, Option<NodeId>)> =
            self.entries.iter().map(Entry::key).collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// How many diagnostics of each `(stage, kind)` pair the load produced.
    ///
    /// Distinct from [`Self::keys`]: two `@font-face` blocks in one sheet are one
    /// key and a count of two, and the count is what catches a diagnostic being
    /// emitted once where the page has it twice.
    #[must_use]
    pub fn counts(&self) -> BTreeMap<(Stage, Kind), usize> {
        let mut counts: BTreeMap<(Stage, Kind), usize> = BTreeMap::new();
        for entry in &self.entries {
            *counts.entry((entry.stage, entry.kind.clone())).or_default() += 1;
        }
        counts
    }

    /// How many entries there are in total.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the load produced no diagnostics at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A one-line-per-kind report, for a human reading a failure or a run log.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.entries.is_empty() {
            return "  (no diagnostics)".to_owned();
        }
        let mut lines = Vec::new();
        for ((stage, kind), count) in self.counts() {
            lines.push(format!("  {stage:<14} {count:>4}  {}", kind.summary()));
        }
        lines.join("\n")
    }
}

/// Reduce a stylesheet diagnostic message to a comparable [`Kind`].
///
/// This is prose parsing, and it is prose parsing because
/// `StyleSheetDiagnostic` is `{ line, column, message }` with no code field. The
/// reduction is table-driven and total: an unrecognised message becomes
/// [`Kind::Unclassified`] rather than being dropped, so the set can never be
/// quietly short by one.
#[must_use]
pub fn classify_stylesheet_message(message: &str) -> Kind {
    if let Some(name) = between(message, "@", " is parsed but not evaluated yet") {
        return Kind::AtRuleNotEvaluated(name.to_owned());
    }
    if let Some(name) = between(message, "@", " is not an at-rule this engine knows") {
        return Kind::AtRuleUnknown(name.to_owned());
    }
    if let Some(feature) = between(message, "@supports ", " is not answered: ") {
        return Kind::SupportsNotAnswered(feature.to_owned());
    }
    if message
        .starts_with("@supports condition is not valid, so the rule and its contents are dropped")
    {
        return Kind::SupportsConditionInvalid;
    }
    if let Some(detail) = message.strip_prefix("invalid declaration: ") {
        return Kind::InvalidDeclaration(detail.to_owned());
    }
    if let Some(detail) = message.strip_prefix("invalid selector: ") {
        return Kind::InvalidSelector(detail.to_owned());
    }
    if message == "invalid cascade layer name" {
        return Kind::InvalidLayerName;
    }
    if let Some(token) = message.strip_prefix("unexpected token: ") {
        return Kind::UnexpectedToken(token.to_owned());
    }
    if message == "unexpected end of input" {
        return Kind::UnexpectedEndOfInput;
    }
    if message == "nested cascade layers are not implemented yet" {
        return Kind::NestedCascadeLayer;
    }
    if message == "this at-rule was discarded without a reason" {
        return Kind::Unclassified(message.to_owned());
    }
    Kind::Unclassified(message.to_owned())
}

/// Build a stylesheet entry from the message the engine wrote, with no owning
/// node.
///
/// A `None` node on the stylesheet stage is what the **user-agent** sheet
/// produces - it is collected with no owner - so this is the user-agent shape,
/// and [`crate::diagnostic_set`] treats it as such. Use
/// [`author_stylesheet_entry`] for a page's own sheets.
#[must_use]
pub fn stylesheet_entry(message: &str) -> Entry {
    let kind = classify_stylesheet_message(message);
    Entry::new(Stage::StyleSheet, kind, None, message.to_owned())
}

/// Build a stylesheet entry attributed to the element that declared it, which is
/// what an author sheet's diagnostics always carry.
#[must_use]
pub fn author_stylesheet_entry(node: NodeId, message: &str) -> Entry {
    let kind = classify_stylesheet_message(message);
    Entry::new(Stage::StyleSheet, kind, Some(node), message.to_owned())
}

/// Build a raster entry, folding the reference backend's per-glyph
/// `MissingGlyph` report into its own kind.
///
/// Called instead of [`coded_entry`] for the raster stage, because the one
/// diagnostic that stage produces *by design* says how much text the page has
/// rather than anything about the engine, and pinning it per fixture would
/// encode the fixture's word count as a contract.
#[must_use]
pub fn raster_entry(code: &str, message: &str) -> Entry {
    let kind = if message.starts_with("no mask for glyph ") {
        Kind::ReferenceGlyphMask
    } else {
        Kind::Coded("raster", format!("{code}: {message}"))
    };
    Entry::new(Stage::Raster, kind, None, message.to_owned())
}

/// Build a computed-value entry.
///
/// The stage carries no typed code - only a property name and a message - so
/// the caller supplies the comparable key, which for a dropped declaration is
/// `dropped <property>`. Grouping by property is what makes a *count* per
/// property meaningful: "three `color` declarations were dropped" is a
/// regression detector, and the same three messages three times over is not.
#[must_use]
pub fn computed_style_entry(code: &'static str, message: &str) -> Entry {
    let kind = Kind::Coded(code, String::new());
    Entry::new(Stage::ComputedStyle, kind, None, message.to_owned())
}

/// Build an entry for a stage that carries a typed code.
///
/// The kind is the **code alone**, with no message detail, and that is
/// deliberate. A code is the engine's own classification of what went wrong, and
/// it is the thing worth pinning: whether the engine said this kind of thing,
/// and how many times. Folding the message in would make every *node* its own
/// kind, so a page that legitimately reports the same condition 161 times would
/// need 161 expected set members, and the set would stop being readable as a
/// statement about the engine. The message is still carried on the entry, so a
/// failure can quote it.
#[must_use]
pub fn coded_entry(stage: Stage, code: &'static str, node: Option<NodeId>, message: &str) -> Entry {
    let kind = Kind::Coded(code, String::new());
    Entry::new(stage, kind, node, message.to_owned())
}

/// The substring between `start` and the first following `end`, when both are
/// present and the end follows the start.
fn between<'a>(message: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start = message.find(start)? + start.len();
    let rest = &message[start..];
    let end = rest.find(end)?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::{Entry, Kind, Stage, Stream, author_stylesheet_entry, classify_stylesheet_message};

    #[test]
    fn every_at_rule_wording_reduces_to_its_own_kind() {
        assert_eq!(
            classify_stylesheet_message("@font-face is parsed but not evaluated yet"),
            Kind::AtRuleNotEvaluated("font-face".to_owned())
        );
        assert_eq!(
            classify_stylesheet_message("@-webkit-keyframes is parsed but not evaluated yet"),
            Kind::AtRuleNotEvaluated("-webkit-keyframes".to_owned())
        );
        assert_eq!(
            classify_stylesheet_message("@nonesuch is not an at-rule this engine knows"),
            Kind::AtRuleUnknown("nonesuch".to_owned())
        );
    }

    #[test]
    fn the_supports_wording_reduces_to_the_feature_and_drops_the_reason() {
        let kind = classify_stylesheet_message(
            "@supports at-rule(@font-face) is not answered: its src must be fetched \
             by render-net and its face registered with a font backend",
        );
        assert_eq!(
            kind,
            Kind::SupportsNotAnswered("at-rule(@font-face)".to_owned())
        );
    }

    #[test]
    fn a_declaration_error_keeps_its_reason() {
        assert_eq!(
            classify_stylesheet_message(
                "invalid declaration: a declaration starts with a property name, and '*' \
                 is not one"
            ),
            Kind::InvalidDeclaration(
                "a declaration starts with a property name, and '*' is not one".to_owned()
            )
        );
    }

    /// The reducer is total on purpose: a message shape nobody anticipated has
    /// to show up in the set, or the expected set could be satisfied by a
    /// diagnostic going missing.
    #[test]
    fn an_unrecognised_message_is_kept_whole_rather_than_dropped() {
        assert_eq!(
            classify_stylesheet_message("something nobody has seen before"),
            Kind::Unclassified("something nobody has seen before".to_owned())
        );
    }

    #[test]
    fn counts_separate_repetition_from_distinctness() {
        let node = render_core::dom::Dom::new().document();
        let stream = Stream::new(vec![
            author_stylesheet_entry(node, "@font-face is parsed but not evaluated yet"),
            author_stylesheet_entry(node, "@font-face is parsed but not evaluated yet"),
            author_stylesheet_entry(node, "@keyframes is parsed but not evaluated yet"),
        ]);
        assert_eq!(stream.len(), 3, "three diagnostics were produced");
        assert_eq!(
            stream.keys().len(),
            2,
            "two distinct keys: font-face and keyframes"
        );
        let counts = stream.counts();
        assert_eq!(
            counts[&(
                Stage::StyleSheet,
                Kind::AtRuleNotEvaluated("font-face".to_owned())
            )],
            2
        );
        assert!(
            stream
                .describe()
                .contains("2  at-rule-not-evaluated @font-face")
        );
    }

    #[test]
    fn an_entry_renders_its_stage_kind_node_and_message() {
        let entry = Entry::new(
            Stage::StyleSheet,
            Kind::AtRuleNotEvaluated("font-face".to_owned()),
            Some(render_core::dom::Dom::new().document()),
            "@font-face is parsed but not evaluated yet".to_owned(),
        );
        let rendered = entry.to_string();
        assert!(rendered.starts_with("stylesheet at-rule-not-evaluated @font-face"));
        assert!(rendered.ends_with("@font-face is parsed but not evaluated yet"));
    }
}
