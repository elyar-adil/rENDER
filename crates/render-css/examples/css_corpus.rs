//! Corpus measurement: how much of a real-world stylesheet actually reaches the
//! cascade.
//!
//! ```text
//! cargo run --release -p render-css --example css_corpus
//! ```
//!
//! The tool walks every `.css` file under `.diag/` and re-tokenizes each one
//! with a never-failing counter parser, so it knows exactly how many style
//! rules and declarations the sheet really contains. Comparing that against
//! `parse_stylesheet` gives the number of rules and declarations the engine
//! drops on the floor, and every drop is reported with its absolute byte
//! offset so it can be looked up in the source.

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use cssparser::{
    AtRuleParser, CowRcStr, ParseError, Parser, ParserInput, ParserState, QualifiedRuleParser,
    RuleBodyItemParser, RuleBodyParser, StyleSheetParser,
};
use render_css::cascade::{expand_shorthand, media_query_list_matches};
use render_css::properties::parse_typed_property;
use render_css::selector::{MatchContext, parse_selector_list};
use render_css::stylesheet::parse_stylesheet;

/// At-rules whose block is a rule list of style rules (css-cascade-5 §6,
/// css-syntax §5.4.2) and which this engine evaluates. A rule reached through
/// any other at-rule never becomes a style rule, so it is counted as inert.
const RULE_LIST_AT_RULES: &[&str] = &["media", "supports", "layer"];

/// At-rules whose block is a declaration list, not a rule list
/// (css-fonts §3, css-page §3, css-values §4). Counting qualified rules inside
/// them would be nonsense: every declaration looks like a selector.
const DECLARATION_AT_RULES: &[&str] = &[
    "font-face",
    "page",
    "property",
    "counter-style",
    "font-palette-values",
    "font-feature-values",
    "viewport",
    "view-transition",
    "position-try",
    "color-profile",
];

#[derive(Debug)]
struct Candidate {
    offset: usize,
    prelude: String,
    /// Enclosing at-rule names, outermost first.
    container: Vec<String>,
    /// Whether every enclosing at-rule is an evaluated rule-list at-rule.
    active: bool,
}

#[derive(Debug)]
struct DeclSite {
    offset: usize,
    name: String,
    value: String,
}

#[derive(Debug)]
struct NestedSite {
    offset: usize,
    prelude: String,
    /// Bytes of the nested rule's whole block, so the report can say how much
    /// source a nesting feature is responsible for.
    bytes: usize,
}

#[derive(Default)]
struct Tally {
    rules: Vec<Candidate>,
    declarations: Vec<DeclSite>,
    nested: Vec<NestedSite>,
    at_rules: BTreeMap<String, usize>,
}

struct AtPrelude {
    name: String,
}

struct TallyParser<'a> {
    tally: &'a mut Tally,
    container: Vec<String>,
    active: bool,
}

impl<'i> QualifiedRuleParser<'i> for TallyParser<'_> {
    type Prelude = String;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<String, ParseError<'i, ()>> {
        Ok(consume_raw(input))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: String,
        start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<(), ParseError<'i, ()>> {
        self.tally.rules.push(Candidate {
            offset: start.position().byte_index(),
            prelude,
            container: self.container.clone(),
            active: self.active,
        });
        count_declarations(input, self.tally)
    }
}

impl<'i> AtRuleParser<'i> for TallyParser<'_> {
    type Prelude = AtPrelude;
    type AtRule = ();
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<AtPrelude, ParseError<'i, ()>> {
        // The prelude must be consumed: cssparser's `parse_entirely` rejects a
        // closure that leaves input behind, which would drop the whole at-rule.
        let _ = consume_raw(input);
        Ok(AtPrelude {
            name: name.to_ascii_lowercase(),
        })
    }

    fn rule_without_block(&mut self, _prelude: AtPrelude, _start: &ParserState) -> Result<(), ()> {
        Ok(())
    }

    fn parse_block<'t>(
        &mut self,
        prelude: AtPrelude,
        _start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<(), ParseError<'i, ()>> {
        let name = prelude.name;
        let bucket = if name.ends_with("keyframes") {
            "keyframes".to_owned()
        } else {
            name.clone()
        };
        *self.tally.at_rules.entry(bucket).or_default() += 1;
        if name.ends_with("keyframes") || DECLARATION_AT_RULES.contains(&name.as_str()) {
            return consume_block(input);
        }
        let mut container = self.container.clone();
        container.push(name.clone());
        let active = self.active && RULE_LIST_AT_RULES.contains(&name.as_str());
        parse_nested(input, self.tally, &container, active);
        Ok(())
    }
}

impl<'i> RuleBodyItemParser<'i, (), ()> for DeclarationCounter<'_> {
    fn parse_declarations(&self) -> bool {
        true
    }

    /// CSS Syntax §5.5.5: a block's contents are declarations *and* rules, so
    /// both have to be counted, or a stylesheet that nests would look like it
    /// has no nested rules at all.
    fn parse_qualified(&self) -> bool {
        true
    }
}

/// Walk one declaration list, recording every declaration the tokenizer finds.
fn count_declarations<'i, 't>(
    input: &mut Parser<'i, 't>,
    tally: &mut Tally,
) -> Result<(), ParseError<'i, ()>> {
    let mut parser = DeclarationCounter { tally };
    for _ in RuleBodyParser::<'i, 't, '_, DeclarationCounter<'_>, (), ()>::new(input, &mut parser) {
    }
    Ok(())
}

/// `RuleBodyItemParser` needs one concrete parser type; wire the counter up.
struct DeclarationCounter<'a> {
    tally: &'a mut Tally,
}

impl<'i> cssparser::DeclarationParser<'i> for DeclarationCounter<'_> {
    type Declaration = ();
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _declaration_start: &ParserState,
    ) -> Result<(), ParseError<'i, ()>> {
        let start = input.position();
        let mut end = start;
        let mut components = 0_usize;
        let mut blocks = 0_usize;
        while input.next_including_whitespace_and_comments().is_ok() {
            end = input.position();
            components += 1;
        }
        // Only a `{}`-block counts here, exactly as in the engine: a function
        // or a `()`/`[]` group is an ordinary value component.
        let value = input.slice(start..end);
        if value.contains('{') || value.contains('}') {
            blocks += 1;
        }
        let _ = blocks;
        // CSS Syntax §5.5.5: no property takes a {}-block as part of a longer
        // value, so `div:hover { ... }` is a nested style rule. Failing here is
        // what makes cssparser reparse the construct as a qualified rule.
        let is_custom_property = name.as_ref().starts_with("--");
        if !is_custom_property
            && (value.contains('{') || value.contains('}'))
            && (components > 1 || !is_solely_a_block(value))
        {
            return Err(input.new_custom_error(()));
        }
        self.tally.declarations.push(DeclSite {
            offset: start.byte_index(),
            name: name.as_ref().to_owned(),
            value: value.trim().to_owned(),
        });
        Ok(())
    }
}

/// Whether a declaration value is nothing but one `{}`-block, which §5.5.5
/// still allows because only the property's own grammar can reject it.
fn is_solely_a_block(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with('{') && trimmed.ends_with('}') && {
        let mut depth = 0_i32;
        for (index, character) in trimmed.char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 && index + 1 != trimmed.len() {
                        return false;
                    }
                }
                _ => {}
            }
        }
        depth == 0
    }
}

impl<'i> QualifiedRuleParser<'i> for DeclarationCounter<'_> {
    type Prelude = String;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<String, ParseError<'i, ()>> {
        Ok(consume_raw(input))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: String,
        start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<(), ParseError<'i, ()>> {
        let offset = start.position().byte_index();
        count_declarations(input, self.tally)?;
        let end = input.position().byte_index();
        self.tally.nested.push(NestedSite {
            offset,
            prelude,
            bytes: end.saturating_sub(offset),
        });
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for DeclarationCounter<'_> {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
}

fn parse_nested<'i, 't>(
    input: &mut Parser<'i, 't>,
    tally: &mut Tally,
    container: &[String],
    active: bool,
) {
    let mut parser = TallyParser {
        tally,
        container: container.to_vec(),
        active,
    };
    for item in StyleSheetParser::new(input, &mut parser) {
        let _ = item;
    }
}

fn consume_raw(input: &mut Parser<'_, '_>) -> String {
    let start = input.position();
    while input.next_including_whitespace_and_comments().is_ok() {}
    input.slice_from(start).trim().to_owned()
}

/// Drain a block body. A parser handed to `parse_block` is already scoped to
/// the block, so it stops with `Err` at the closing brace; no
/// `parse_nested_block` wrapper is needed or allowed here.
fn consume_block<'i, 't>(input: &mut Parser<'i, 't>) -> Result<(), ParseError<'i, ()>> {
    while input.next_including_whitespace_and_comments().is_ok() {}
    Ok(())
}

fn tally(source: &str) -> Tally {
    let mut tally = Tally::default();
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    parse_nested(&mut parser, &mut tally, &[], true);
    tally
}

struct FileReport {
    path: String,
    bytes: usize,
    rules_seen: usize,
    rules_parsed: usize,
    rules_inert: usize,
    decls_seen: usize,
    decls_parsed: usize,
    nested: Vec<NestedSite>,
    media_gated: usize,
    media_active: usize,
    supports_rules: usize,
    at_rules: BTreeMap<String, usize>,
    dropped_rules: Vec<String>,
    dropped_decls: Vec<String>,
    dropped_decl_sites: Vec<String>,
    reasons: BTreeMap<String, usize>,
    diagnostics: BTreeMap<String, usize>,
    blocked_features: BTreeMap<String, usize>,
    invalid_values: BTreeMap<String, usize>,
    untyped_values: BTreeMap<String, usize>,
    shorthand_values: BTreeMap<String, usize>,
    invalid_samples: BTreeMap<String, Vec<String>>,
    /// How the `text-decoration` shorthand is doing in this file, which is the
    /// reported-bug measurement: before the shorthand expanded, a page's
    /// `text-decoration: none` was stored under its own name and never
    /// competed with a user-agent `text-decoration-line`.
    decoration: DecorationTally,
}

/// Per-file counts for the `text-decoration` shorthand. `seen` is what the
/// tokenizer found, `expanded` is how many of those the engine now turns into
/// `text-decoration-line` (or another longhand), and `line_none` is the subset
/// that was fighting the user-agent underline.
#[derive(Default)]
struct DecorationTally {
    seen: usize,
    expanded: usize,
    unexpanded: usize,
    line_none: usize,
    by_value: BTreeMap<String, usize>,
}

/// Every `(feature)` name in a media query list, so the report can attribute
/// gated-out rules to the specific feature the engine cannot evaluate.
fn media_features(query: &str) -> Vec<String> {
    let mut result = Vec::new();
    let bytes = query.as_bytes();
    let mut depth = 0_u32;
    let mut start = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => {
                if depth == 0 {
                    start = index + 1;
                }
                depth += 1;
            }
            b')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let inner = query[start..index].trim();
                    let name = inner
                        .split_once(':')
                        .map_or(inner, |(name, _)| name.trim())
                        .to_ascii_lowercase();
                    result.push(name);
                }
            }
            _ => {}
        }
        index += 1;
    }
    result
}

/// Why a raw declaration value was rejected by a typed grammar, once the
/// stages that run *before* typed parsing have had their chance.
fn classify(value: &str) -> &'static str {
    let lowered = value.to_ascii_lowercase();
    if lowered.contains("var(") {
        "var()"
    } else if matches!(
        lowered.trim(),
        "inherit" | "initial" | "unset" | "revert" | "revert-layer"
    ) {
        "css-wide"
    } else if ["-webkit-", "-moz-", "-ms-", "-o-"]
        .iter()
        .any(|prefix| lowered.contains(prefix))
    {
        "legacy-prefix"
    } else if lowered.contains("\\0")
        || lowered.contains("\\9")
        || lowered.ends_with('*')
        || lowered.starts_with('*')
    {
        "ie-hack"
    } else {
        "other"
    }
}

fn report(path: &Path, source: &str) -> FileReport {
    let mut tally = tally(source);
    let sheet = parse_stylesheet(source);
    let mut blocked_features: BTreeMap<String, usize> = BTreeMap::new();
    let mut invalid_values: BTreeMap<String, usize> = BTreeMap::new();
    let mut untyped_values: BTreeMap<String, usize> = BTreeMap::new();
    let mut shorthand_values: BTreeMap<String, usize> = BTreeMap::new();
    let mut invalid_samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let context = MatchContext {
        viewport_width: Some(1770.0),
        viewport_height: Some(1170.0),
        ..MatchContext::default()
    };

    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut dropped_rules = Vec::new();
    let mut rules_seen = 0_usize;
    let mut rules_inert = 0_usize;
    let mut supports_rules = 0_usize;
    for rule in &tally.rules {
        if !rule.active {
            rules_inert += 1;
            continue;
        }
        rules_seen += 1;
        if rule.container.iter().any(|name| name == "supports") {
            supports_rules += 1;
        }
        if let Err(error) = parse_selector_list(&rule.prelude) {
            let reason = normalize(&error.to_string());
            *reasons.entry(reason.clone()).or_default() += 1;
            if dropped_rules.len() < 6 {
                dropped_rules.push(format!(
                    "rule @{} {} => {reason}",
                    truncate(&rule.prelude, 110),
                    rule.offset
                ));
            }
        }
    }

    // Declaration-level drops. The engine keeps its declarations in source
    // order, so the kept list is a subsequence of what the tokenizer found and
    // a single forward walk reports every drop with its byte offset.
    let kept: Vec<&str> = sheet
        .rules
        .iter()
        .flat_map(|rule| rule.declarations.iter())
        .map(|declaration| declaration.name.as_str())
        .collect();
    let mut remaining = kept.iter().copied();
    let mut next_kept = remaining.next();
    let mut dropped: Vec<&DeclSite> = Vec::new();
    for site in &tally.declarations {
        if next_kept == Some(site.name.as_str()) {
            next_kept = remaining.next();
        } else {
            dropped.push(site);
        }
    }
    let decls_parsed = tally.declarations.len() - dropped.len();
    let mut shortfall: BTreeMap<&str, usize> = BTreeMap::new();
    for site in &dropped {
        *shortfall.entry(site.name.as_str()).or_default() += 1;
    }
    let dropped_decls: Vec<String> = shortfall
        .iter()
        .rev()
        .take(20)
        .map(|(name, count)| format!("{count:>5} x {name}"))
        .collect();
    let dropped_decl_sites: Vec<String> = dropped
        .iter()
        .take(6)
        .map(|site| {
            format!(
                "byte {} {}: {}",
                site.offset,
                site.name,
                truncate(&site.value, 60)
            )
        })
        .collect();

    let mut diagnostics: BTreeMap<String, usize> = BTreeMap::new();
    for diagnostic in &sheet.diagnostics {
        *diagnostics.entry(diagnostic.message.clone()).or_default() += 1;
    }

    // The `text-decoration` shorthand, measured by asking the engine's own
    // expansion rather than by pattern-matching the value here, so the number
    // reflects what the cascade will actually do.
    let mut decoration = DecorationTally::default();
    for site in &tally.declarations {
        if !site.name.eq_ignore_ascii_case("text-decoration") {
            continue;
        }
        decoration.seen += 1;
        let longhands = expand_shorthand(&site.name, &site.value);
        let expanded = longhands
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("text-decoration-line"));
        if expanded {
            decoration.expanded += 1;
            if longhands.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("text-decoration-line")
                    && value.trim().eq_ignore_ascii_case("none")
            }) {
                decoration.line_none += 1;
            }
        } else {
            decoration.unexpanded += 1;
        }
        let value = truncate(&site.value, 50);
        let entry = decoration.by_value.entry(value.clone()).or_default();
        *entry += 1;
    }

    let media_gated = sheet.rules.iter().filter(|r| !r.media.is_empty()).count();
    let media_active = sheet
        .rules
        .iter()
        .filter(|rule| {
            !rule.media.is_empty()
                && rule
                    .media
                    .iter()
                    .all(|query| media_query_list_matches(query, &context))
        })
        .count();
    for rule in &sheet.rules {
        for query in &rule.media {
            if media_query_list_matches(query, &context) {
                continue;
            }
            for feature in media_features(query) {
                *blocked_features.entry(feature).or_default() += 1;
            }
        }
        for declaration in &rule.declarations {
            if declaration.name.starts_with("--") {
                continue;
            }
            // Computed-value stage: a claimed grammar with an invalid value
            // drops the declaration outright; an unclaimed grammar still
            // cascades as a raw string.
            match parse_typed_property(&declaration.name, &declaration.value) {
                Some(Err(_)) => {
                    // `compute_style` substitutes `var()` and resolves CSS-wide
                    // keywords before the typed stage, so only the remaining
                    // categories are genuinely dropped declarations.
                    let kind = classify(&declaration.value);
                    *invalid_values
                        .entry(format!("{} [{kind}]", declaration.name))
                        .or_default() += 1;
                    if kind == "other" {
                        let samples = invalid_samples.entry(declaration.name.clone()).or_default();
                        let value = truncate(&declaration.value, 70);
                        if samples.len() < 6 && !samples.contains(&value) {
                            samples.push(value);
                        }
                    }
                }
                None => {
                    // A shorthand the engine expands has no typed grammar of its
                    // own - the longhands carry the types - and it does not
                    // cascade as a raw string either. Counting it as "untyped"
                    // would be a stale claim, so it gets its own bucket.
                    let expanded = expand_shorthand(&declaration.name, &declaration.value);
                    let is_shorthand = expanded.iter().any(|(name, _)| name != &declaration.name);
                    if is_shorthand {
                        *shorthand_values
                            .entry(declaration.name.clone())
                            .or_default() += 1;
                    } else {
                        *untyped_values.entry(declaration.name.clone()).or_default() += 1;
                    }
                }
                Some(Ok(_)) => {}
            }
        }
    }

    FileReport {
        path: path.display().to_string(),
        bytes: source.len(),
        rules_seen,
        rules_parsed: sheet.rules.len(),
        rules_inert,
        decls_seen: tally.declarations.len(),
        decls_parsed,
        nested: std::mem::take(&mut tally.nested),
        media_gated,
        media_active,
        supports_rules,
        at_rules: tally.at_rules,
        dropped_rules,
        dropped_decls,
        dropped_decl_sites,
        reasons,
        diagnostics,
        blocked_features,
        invalid_values,
        untyped_values,
        shorthand_values,
        invalid_samples,
        decoration,
    }
}

/// Strip the `at byte N` suffix so identical root causes aggregate.
fn normalize(message: &str) -> String {
    match message.rfind(" at byte ") {
        Some(index) => message[..index].to_owned(),
        None => message.to_owned(),
    }
}

fn truncate(text: &str, limit: usize) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= limit {
        return flattened;
    }
    let cut: String = flattened.chars().take(limit).collect();
    format!("{cut}...")
}

fn css_files(root: &Path, result: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            css_files(&path, result);
        } else if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains(".css"))
        {
            result.push(path);
        }
    }
    result.sort();
}

fn main() {
    let root = PathBuf::from(env::args().nth(1).unwrap_or_else(|| ".diag".to_owned()));
    let mut files = Vec::new();
    css_files(&root, &mut files);

    let mut out = String::new();
    let _ = writeln!(
        out,
        "corpus root: {}  ({} files)",
        root.display(),
        files.len()
    );
    let _ = writeln!(
        out,
        "{:<48} {:>8} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "file", "bytes", "rule?", "rule+", "rule-", "decl?", "decl+", "decl-", "inert"
    );
    let _ = writeln!(out, "{}", "-".repeat(120));

    let mut totals = [0_usize; 9];
    let mut at_rules: BTreeMap<String, usize> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut diagnostics: BTreeMap<String, usize> = BTreeMap::new();
    let mut lost_decls: BTreeMap<String, usize> = BTreeMap::new();
    let mut blocked_features: BTreeMap<String, usize> = BTreeMap::new();
    let mut invalid_values: BTreeMap<String, usize> = BTreeMap::new();
    let mut untyped_values: BTreeMap<String, usize> = BTreeMap::new();
    let mut shorthand_values: BTreeMap<String, usize> = BTreeMap::new();
    let mut invalid_samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut supports_rules = 0_usize;
    let mut media_gated = 0_usize;
    let mut media_active = 0_usize;
    let mut nested_total = 0_usize;
    let mut nested_bytes = 0_usize;
    let mut nested_samples: Vec<(usize, String)> = Vec::new();
    let mut decor_seen = 0_usize;
    let mut decor_expanded = 0_usize;
    let mut decor_unexpanded = 0_usize;
    let mut decor_line_none = 0_usize;
    let mut decor_values: BTreeMap<String, usize> = BTreeMap::new();

    for path in &files {
        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        let file = report(path, &source);
        for line in &file.dropped_decls {
            let Some((count, name)) = line.trim().split_once(" x ") else {
                continue;
            };
            *lost_decls.entry(name.to_owned()).or_default() += count.parse().unwrap_or(0);
        }
        let _ = writeln!(
            out,
            "{:<48} {:>8} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
            file.path,
            file.bytes,
            file.rules_seen,
            file.rules_parsed,
            file.rules_seen.saturating_sub(file.rules_parsed),
            file.decls_seen,
            file.decls_parsed,
            file.decls_seen.saturating_sub(file.decls_parsed),
            file.rules_inert
        );
        for (name, count) in &file.at_rules {
            *at_rules.entry(name.clone()).or_default() += count;
        }
        for (reason, count) in &file.reasons {
            *reasons.entry(reason.clone()).or_default() += count;
        }
        for (message, count) in &file.diagnostics {
            *diagnostics.entry(message.clone()).or_default() += count;
        }
        for (feature, count) in &file.blocked_features {
            *blocked_features.entry(feature.clone()).or_default() += count;
        }
        for (name, count) in &file.invalid_values {
            *invalid_values.entry(name.clone()).or_default() += count;
        }
        for (name, count) in &file.untyped_values {
            *untyped_values.entry(name.clone()).or_default() += count;
        }
        for (name, count) in &file.shorthand_values {
            *shorthand_values.entry(name.clone()).or_default() += count;
        }
        for (name, samples) in &file.invalid_samples {
            let entry = invalid_samples.entry(name.clone()).or_default();
            for sample in samples {
                if entry.len() < 6 && !entry.contains(sample) {
                    entry.push(sample.clone());
                }
            }
        }
        supports_rules += file.supports_rules;
        media_gated += file.media_gated;
        media_active += file.media_active;
        nested_total += file.nested.len();
        nested_bytes += file.nested.iter().map(|site| site.bytes).sum::<usize>();
        decor_seen += file.decoration.seen;
        decor_expanded += file.decoration.expanded;
        decor_unexpanded += file.decoration.unexpanded;
        decor_line_none += file.decoration.line_none;
        for (value, count) in &file.decoration.by_value {
            *decor_values.entry(value.clone()).or_default() += count;
        }
        for site in file.nested.iter().take(4) {
            nested_samples.push((site.offset, truncate(&site.prelude, 80)));
        }
        totals[0] += file.bytes;
        totals[1] += file.rules_seen;
        totals[2] += file.rules_parsed;
        totals[3] += file.decls_seen;
        totals[4] += file.decls_parsed;
        totals[5] += file.rules_inert;
        totals[6] += file.rules_seen.saturating_sub(file.rules_parsed);
        totals[7] += file.decls_seen.saturating_sub(file.decls_parsed);
        totals[8] += file.dropped_rules.len();
    }

    let _ = writeln!(out, "{}", "-".repeat(120));
    let _ = writeln!(
        out,
        "{:<48} {:>8} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "TOTAL",
        totals[0],
        totals[1],
        totals[2],
        totals[6],
        totals[3],
        totals[4],
        totals[7],
        totals[5]
    );
    let _ = writeln!(
        out,
        "\nrule? = style rules the sheet contains, rule+ = kept, rule- = dropped \
         ({} of {} files report any)",
        totals[8], totals[6]
    );
    let _ = writeln!(
        out,
        "declarations dropped: {} of {} ({:.2}%)",
        totals[7],
        totals[3],
        100.0 * totals[7] as f64 / totals[3].max(1) as f64
    );
    let _ = writeln!(
        out,
        "nested rules (CSS Nesting §3.1): {nested_total} covering {nested_bytes} bytes",
    );
    if nested_total > 0 {
        for site in nested_samples.iter().take(8) {
            let _ = writeln!(out, "    byte {} {}", site.0, site.1);
        }
    }
    let _ = writeln!(
        out,
        "@media: {media_gated} rules gated, {media_active} of them apply at 1770x1170"
    );
    let _ = writeln!(
        out,
        "@supports: {supports_rules} rules applied unconditionally (query never evaluated)"
    );

    let _ = writeln!(out, "\nat-rule occurrences (whole corpus):");
    for (name, count) in at_rules.iter().rev() {
        let _ = writeln!(out, "  {name:<24} {count:>6}");
    }

    let _ = writeln!(
        out,
        "\nmedia features that gate out rules (rule x query occurrences):"
    );
    for (feature, count) in blocked_features.iter().rev() {
        let _ = writeln!(out, "  {count:>6}  ({feature})");
    }

    let _ = writeln!(out, "\ndropped declarations by property (whole corpus):");
    for (name, count) in lost_decls.iter().rev() {
        let _ = writeln!(out, "  {count:>6}  {name}");
    }

    let invalid_total: usize = invalid_values.values().sum();
    let untyped_total: usize = untyped_values.values().sum();
    let mut by_category: BTreeMap<&str, usize> = BTreeMap::new();
    for key in invalid_values.keys() {
        let category = key
            .rsplit_once('[')
            .and_then(|(_, rest)| rest.strip_suffix(']'))
            .unwrap_or("other");
        *by_category.entry(category).or_default() += invalid_values[key];
    }
    let _ = writeln!(
        out,
        "\ndeclarations a typed grammar rejects: {invalid_total} \
         (var() and CSS-wide keywords are resolved before this stage)"
    );
    for (category, count) in by_category.iter().rev() {
        let _ = writeln!(out, "  {count:>6}  {category}");
    }
    let _ = writeln!(out, "  real drops (`other`):");
    let mut others: Vec<(&String, &usize)> = invalid_values
        .iter()
        .filter(|(name, _)| name.ends_with("[other]"))
        .collect();
    others.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (name, count) in others {
        let _ = writeln!(out, "  {count:>6}  {name}");
        if let Some(samples) = name
            .strip_suffix(" [other]")
            .and_then(|name| invalid_samples.get(name))
        {
            for sample in samples {
                let _ = writeln!(out, "            e.g. {sample}");
            }
        }
    }
    let _ = writeln!(
        out,
        "\ntext-decoration shorthand (the reported underline bug):"
    );
    let _ = writeln!(
        out,
        "  {} declarations use the shorthand; {} now expand to longhands, {} do not",
        decor_seen, decor_expanded, decor_unexpanded
    );
    let _ = writeln!(
        out,
        "  {} of them resolve to text-decoration-line: none, the value that was \
         dead against a user-agent underline",
        decor_line_none
    );
    for (value, count) in decor_values.iter().rev().take(20) {
        let _ = writeln!(out, "  {count:>6}  text-decoration: {value}");
    }

    let _ = writeln!(
        out,
        "\ndeclarations with no typed grammar (value still cascades as a string): {untyped_total} \
         over {} distinct properties",
        untyped_values.len()
    );
    for (name, count) in untyped_values.iter().rev().take(25) {
        let _ = writeln!(out, "  {count:>6}  {name}");
    }
    let shorthand_total: usize = shorthand_values.values().sum();
    let _ = writeln!(
        out,
        "\nshorthands with no grammar of their own (expanded into typed longhands): \
         {shorthand_total} over {} distinct properties",
        shorthand_values.len()
    );
    for (name, count) in shorthand_values.iter().rev().take(15) {
        let _ = writeln!(out, "  {count:>6}  {name}");
    }

    let _ = writeln!(out, "\nselector drop reasons (whole corpus):");
    for (reason, count) in reasons.iter().rev() {
        let _ = writeln!(out, "  {count:>6}  {reason}");
    }

    let _ = writeln!(out, "\nstylesheet diagnostics (whole corpus):");
    for (message, count) in diagnostics.iter().rev() {
        let _ = writeln!(out, "  {count:>6}  {message}");
    }

    for path in &files {
        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        let file = report(path, &source);
        if file.dropped_rules.is_empty() && file.dropped_decl_sites.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\nfirst drops in {}:", file.path);
        for line in file.dropped_rules.iter().chain(&file.dropped_decl_sites) {
            let _ = writeln!(out, "  {line}");
        }
    }

    print!("{out}");
}
