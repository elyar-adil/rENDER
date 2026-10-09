//! A compact backtracking regular-expression engine covering the subset of
//! ECMAScript `RegExp` syntax that real-world pages rely on.
//!
//! Supported: literals, `.`, character classes with ranges/negation/shorthand,
//! `\d \D \s \S \w \W \b \B`, escapes (`\f \n \r \t \v \0 \xHH \uHHHH \u{H+}`
//! plus escaped punctuators), capturing and `(?:)` groups, alternation,
//! greedy/lazy quantifiers (`* + ? {n} {n,} {n,m}`), anchors (`^ $` with `m`),
//! lookahead (`(?= )` `(?! )`), lookbehind (`(?<= )` `(?<! )`), named groups
//! (`(?<name> )`, `\k<name>`), backreferences (`\1`–`\9`), `\cX` control
//! escapes, a documented subset of unicode property escapes (`\p{…}`), and the
//! `i m s g y` flags (`u` is accepted for escape strictness parity but does not
//! change ASCII semantics).
//!
//! Lookbehind is matched by trying each start position at or before the
//! current one and requiring the body to end exactly there, rather than by
//! matching right to left. The two agree on whether a lookbehind succeeds;
//! they can differ in what a capture group inside one captures.
//!
//! Explicitly rejected with a syntax error rather than misinterpreted: a
//! property name outside [`property`]'s table, and set operations inside
//! classes.

use std::fmt;

/// Why a pattern could not be compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexSyntaxError {
    message: String,
    /// `true` for a construct that ECMAScript accepts but this engine does not
    /// implement. Parse-time validation lets these through, so the failure is
    /// reported when the literal is evaluated rather than rejecting the script.
    unsupported: bool,
}

impl RegexSyntaxError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            unsupported: false,
        }
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            unsupported: true,
        }
    }
}

impl fmt::Display for RegexSyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "regex flags are inherently independent switches"
)]
pub struct Flags {
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub sticky: bool,
}

impl Flags {
    /// Parse the standard flag letters; duplicates are rejected by the lexer.
    ///
    /// # Errors
    ///
    /// Returns an error for any letter outside the supported set.
    pub fn parse(flags: &str) -> Result<Self, RegexSyntaxError> {
        let mut parsed = Self::default();
        let mut seen = String::new();
        for character in flags.chars() {
            if seen.contains(character) {
                return Err(RegexSyntaxError::new(format!(
                    "duplicate regex flag {character:?}"
                )));
            }
            seen.push(character);
            match character {
                'g' => parsed.global = true,
                'i' => parsed.ignore_case = true,
                'm' => parsed.multiline = true,
                's' => parsed.dot_all = true,
                'y' => parsed.sticky = true,
                'd' | 'u' | 'v' => {}
                other => {
                    return Err(RegexSyntaxError::new(format!(
                        "unsupported regex flag {other:?}"
                    )));
                }
            }
        }
        if seen.contains('u') && seen.contains('v') {
            return Err(RegexSyntaxError::new(
                "flags u and v are mutually exclusive",
            ));
        }
        Ok(parsed)
    }

    #[must_use]
    pub fn describe(self) -> String {
        let mut text = String::new();
        if self.global {
            text.push('g');
        }
        if self.ignore_case {
            text.push('i');
        }
        if self.multiline {
            text.push('m');
        }
        if self.dot_all {
            text.push('s');
        }
        if self.sticky {
            text.push('y');
        }
        text
    }
}

#[derive(Clone, Debug)]
enum ClassItem {
    Char(char),
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
    /// `\p{…}` (`true`) or `\P{…}` (`false`).
    Property(property::Property, bool),
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Literal(char),
    AnyChar,
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
    Sequence(Vec<Node>),
    Alternative(Vec<Node>),
    Group {
        index: Option<usize>,
        body: Box<Node>,
    },
    Lookahead {
        negated: bool,
        body: Box<Node>,
    },
    Lookbehind {
        negated: bool,
        body: Box<Node>,
    },
    Backreference(usize),
    Quantifier {
        min: u32,
        max: Option<u32>,
        greedy: bool,
        body: Box<Node>,
    },
    AnchorStart,
    AnchorEnd,
    WordBoundary(bool),
}

/// A compiled pattern ready to be matched against inputs.
#[derive(Clone, Debug)]
pub struct Compiled {
    root: Node,
    group_count: usize,
    /// `(name, 1-based capture index)` for each `(?<name>…)`, in source order.
    group_names: Vec<(String, usize)>,
    flags: Flags,
    source: String,
}

const MAX_MATCH_STEPS: u32 = 1_000_000;

/// Recursion bound of the backtracking matcher. A match that would nest deeper
/// fails instead of overflowing the native stack, which would abort the whole
/// process.
const MAX_DEPTH_INLINE: u32 = 6_000;
const MAX_DEPTH_LARGE_STACK: u32 = 60_000;
/// Inputs longer than this are matched on a thread with a large stack.
const LONG_INPUT: usize = 600;
const LARGE_STACK_BYTES: usize = 256 << 20;

/// One successful match: overall span plus per-group spans (character indices).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchRanges {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<(usize, usize)>>,
    /// Named groups of the pattern that matched: `(name, 1-based index)`.
    pub names: std::sync::Arc<[(String, usize)]>,
}

impl Compiled {
    #[must_use]
    #[allow(
        dead_code,
        reason = "engine introspection used by the regex conformance tests"
    )]
    pub fn group_count(&self) -> usize {
        self.group_count
    }

    #[must_use]
    pub const fn flags(&self) -> Flags {
        self.flags
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Find the leftmost match starting at or after `from`.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "retry loop, sticky handling and capture extraction belong together"
    )]
    pub fn find(&self, input: &[char], from: usize) -> Option<MatchRanges> {
        if input.len() <= LONG_INPUT {
            return self.find_with_depth(input, from, MAX_DEPTH_INLINE);
        }
        // Backtracking recurses once per repetition, so a long input needs far
        // more stack than a thread normally has. Memory is only committed as
        // the recursion actually goes deep.
        std::thread::scope(|scope| {
            let spawned = std::thread::Builder::new()
                .stack_size(LARGE_STACK_BYTES)
                .spawn_scoped(scope, || {
                    self.find_with_depth(input, from, MAX_DEPTH_LARGE_STACK)
                });
            match spawned {
                Ok(handle) => handle
                    .join()
                    .unwrap_or_else(|_| self.find_with_depth(input, from, MAX_DEPTH_INLINE)),
                Err(_) => self.find_with_depth(input, from, MAX_DEPTH_INLINE),
            }
        })
    }

    fn find_with_depth(&self, input: &[char], from: usize, max_depth: u32) -> Option<MatchRanges> {
        let mut start = from.min(input.len());
        loop {
            let mut matcher = Matcher {
                input,
                flags: self.flags,
                captures: vec![None; self.group_count + 1],
                steps: 0,
                depth: 0,
                max_depth,
            };
            let end = core::cell::Cell::new(None);
            let accepted = matcher.node(&self.root, start, &mut |_matcher, position| {
                end.set(Some(position));
                Some(())
            });
            if accepted.is_some()
                && let Some(end) = end.get()
            {
                return Some(MatchRanges {
                    start,
                    end,
                    // Slot 0 is unused scratch for the 1-based capture slots.
                    groups: matcher.captures.into_iter().skip(1).collect(),
                    names: self.group_names.clone().into(),
                });
            }
            if self.flags.sticky {
                return None;
            }
            if start >= input.len() {
                return None;
            }
            start += 1;
        }
    }
}

struct Matcher<'a> {
    input: &'a [char],
    flags: Flags,
    captures: Vec<Option<(usize, usize)>>,
    steps: u32,
    depth: u32,
    max_depth: u32,
}

type Continuation<'k> = dyn FnMut(&mut Matcher<'_>, usize) -> Option<()> + 'k;

impl Matcher<'_> {
    fn tick(&mut self) -> Option<()> {
        self.steps += 1;
        if self.steps > MAX_MATCH_STEPS {
            None
        } else {
            Some(())
        }
    }

    /// Match `node` at `position`, invoking `next` to continue the match.
    #[allow(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        reason = "one arm per AST node keeps the backtracking engine readable"
    )]
    fn node(&mut self, node: &Node, position: usize, next: &mut Continuation<'_>) -> Option<()> {
        if self.depth >= self.max_depth {
            // Out of recursion budget: give up on this path rather than
            // overflow the native stack.
            self.steps = MAX_MATCH_STEPS + 1;
            return None;
        }
        self.depth += 1;
        let result = self.node_inner(node, position, next);
        self.depth -= 1;
        result
    }

    #[allow(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        reason = "one arm per AST node keeps the backtracking engine readable"
    )]
    fn node_inner(
        &mut self,
        node: &Node,
        position: usize,
        next: &mut Continuation<'_>,
    ) -> Option<()> {
        self.tick()?;
        match node {
            Node::Empty => next(self, position),
            Node::Literal(expected) => {
                let actual = *self.input.get(position)?;
                if self.chars_match(*expected, actual) {
                    next(self, position + 1)
                } else {
                    None
                }
            }
            Node::AnyChar => {
                let actual = *self.input.get(position)?;
                if !self.flags.dot_all && actual == '\n' {
                    return None;
                }
                next(self, position + 1)
            }
            Node::Class { negated, items } => {
                let actual = *self.input.get(position)?;
                let contained = class_contains(items, actual, self.flags.ignore_case);
                if contained == *negated {
                    None
                } else {
                    next(self, position + 1)
                }
            }
            Node::AnchorStart => {
                let at_start = position == 0
                    || (self.flags.multiline && self.input.get(position - 1) == Some(&'\n'));
                if at_start { next(self, position) } else { None }
            }
            Node::AnchorEnd => {
                let at_end = position == self.input.len()
                    || (self.flags.multiline && self.input[position] == '\n');
                if at_end { next(self, position) } else { None }
            }
            Node::WordBoundary(expected) => {
                let before = position
                    .checked_sub(1)
                    .and_then(|index| self.input.get(index))
                    .is_some_and(|character| is_word_character(*character));
                let after = self
                    .input
                    .get(position)
                    .is_some_and(|character| is_word_character(*character));
                if (before != after) == *expected {
                    next(self, position)
                } else {
                    None
                }
            }
            Node::Sequence(items) => self.sequence(items, position, next),
            Node::Alternative(branches) => {
                for branch in branches {
                    if let Some(result) = self.node(branch, position, next) {
                        return Some(result);
                    }
                }
                None
            }
            Node::Group { index, body } => match index {
                None => self.node(body, position, next),
                Some(index) => {
                    let index = *index;
                    let saved = self.captures[index];
                    let matched = self.node(body, position, &mut |matcher, end_position| {
                        let previous = matcher.captures[index];
                        matcher.captures[index] = Some((position, end_position));
                        if let Some(result) = next(matcher, end_position) {
                            Some(result)
                        } else {
                            matcher.captures[index] = previous;
                            None
                        }
                    });
                    if matched.is_none() {
                        self.captures[index] = saved;
                    }
                    matched
                }
            },
            Node::Lookahead { negated, body } => {
                let mut probe = Matcher {
                    input: self.input,
                    flags: self.flags,
                    // The lookahead sees the same captures; restore on failure.
                    captures: std::mem::take(&mut self.captures),
                    steps: self.steps,
                    depth: self.depth,
                    max_depth: self.max_depth,
                };
                let succeeded = probe
                    .node(body, position, &mut |_matcher, _position| Some(()))
                    .is_some();
                self.steps = probe.steps;
                self.captures = probe.captures;
                if succeeded == *negated {
                    None
                } else {
                    next(self, position)
                }
            }
            Node::Lookbehind { negated, body } => {
                // Try every start at or before `position` for a match of the
                // body that ends exactly at `position`.
                let mut probe = Matcher {
                    input: self.input,
                    flags: self.flags,
                    captures: std::mem::take(&mut self.captures),
                    steps: self.steps,
                    depth: self.depth,
                    max_depth: self.max_depth,
                };
                let mut succeeded = false;
                for start in (0..=position).rev() {
                    let reached = probe.node(body, start, &mut |_matcher, end| {
                        if end == position { Some(()) } else { None }
                    });
                    if reached.is_some() {
                        succeeded = true;
                        break;
                    }
                    if probe.steps > MAX_MATCH_STEPS {
                        break;
                    }
                }
                self.steps = probe.steps;
                self.captures = probe.captures;
                if succeeded == *negated {
                    None
                } else {
                    next(self, position)
                }
            }
            Node::Backreference(index) => {
                let Some(Some((start, end))) = self.captures.get(*index).copied() else {
                    return next(self, position);
                };
                let length = end - start;
                if position + length > self.input.len() {
                    return None;
                }
                for offset in 0..length {
                    let expected = self.input[start + offset];
                    let actual = self.input[position + offset];
                    if !self.chars_match(expected, actual) {
                        return None;
                    }
                }
                next(self, position + length)
            }
            Node::Quantifier {
                min,
                max,
                greedy,
                body,
            } => {
                if matches!(
                    body.as_ref(),
                    Node::Literal(_) | Node::AnyChar | Node::Class { .. }
                ) {
                    return self.simple_repeat(*min, *max, *greedy, body, position, next);
                }
                self.quantifier(*min, *max, *greedy, body, position, 0, next)
            }
        }
    }

    /// Whether the single-character node `node` matches at `position`.
    fn single_matches(&self, node: &Node, position: usize) -> bool {
        let Some(actual) = self.input.get(position).copied() else {
            return false;
        };
        match node {
            Node::Literal(expected) => self.chars_match(*expected, actual),
            Node::AnyChar => self.flags.dot_all || actual != '\n',
            Node::Class { negated, items } => {
                class_contains(items, actual, self.flags.ignore_case) != *negated
            }
            _ => false,
        }
    }

    /// A repetition of a single-character node: count the run once, then try
    /// the continuation from the longest (greedy) or shortest (lazy) end. The
    /// order of attempts is the same as the general backtracking quantifier,
    /// but the recursion depth does not grow with the length of the run.
    fn simple_repeat(
        &mut self,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        body: &Node,
        position: usize,
        next: &mut Continuation<'_>,
    ) -> Option<()> {
        let mut count = 0usize;
        while max.is_none_or(|limit| count < limit as usize)
            && self.single_matches(body, position + count)
        {
            count += 1;
            self.tick()?;
        }
        let minimum = min as usize;
        if count < minimum {
            return None;
        }
        if greedy {
            for length in (minimum..=count).rev() {
                self.tick()?;
                if let Some(result) = next(self, position + length) {
                    return Some(result);
                }
            }
        } else {
            for length in minimum..=count {
                self.tick()?;
                if let Some(result) = next(self, position + length) {
                    return Some(result);
                }
            }
        }
        None
    }

    fn sequence(
        &mut self,
        items: &[Node],
        position: usize,
        next: &mut Continuation<'_>,
    ) -> Option<()> {
        let Some((first, rest)) = items.split_first() else {
            return next(self, position);
        };
        self.node(first, position, &mut |matcher, mid_position| {
            matcher.sequence(rest, mid_position, next)
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the continuation chain needs the full quantifier state"
    )]
    fn quantifier(
        &mut self,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        body: &Node,
        position: usize,
        count: u32,
        next: &mut Continuation<'_>,
    ) -> Option<()> {
        self.tick()?;
        let can_continue = max.is_none_or(|limit| count < limit);
        let attempt_more = |matcher: &mut Matcher<'_>, next: &mut Continuation<'_>| {
            if !can_continue {
                return None;
            }
            matcher.node(body, position, &mut |inner, advanced| {
                if advanced == position && min <= count + 1 {
                    // An empty-body repetition would loop forever.
                    return next(inner, advanced);
                }
                inner.quantifier(min, max, greedy, body, advanced, count + 1, next)
            })
        };
        if count < min {
            return attempt_more(self, next);
        }
        if greedy {
            if let Some(result) = attempt_more(self, next) {
                return Some(result);
            }
            next(self, position)
        } else {
            if let Some(result) = next(self, position) {
                return Some(result);
            }
            attempt_more(self, next)
        }
    }

    fn chars_match(&self, expected: char, actual: char) -> bool {
        if expected == actual {
            return true;
        }
        if self.flags.ignore_case {
            return chars_equal_ignoring_case(expected, actual);
        }
        false
    }
}

fn class_contains(items: &[ClassItem], character: char, ignore_case: bool) -> bool {
    items.iter().any(|item| match item {
        ClassItem::Char(expected) => {
            *expected == character
                || (ignore_case && chars_equal_ignoring_case(*expected, character))
        }
        ClassItem::Range(start, end) => {
            in_range(*start, *end, character)
                || (ignore_case
                    && character
                        .to_lowercase()
                        .chain(character.to_uppercase())
                        .any(|folded| folded != character && in_range(*start, *end, folded)))
        }
        ClassItem::Digit(positive) => character.is_ascii_digit() == *positive,
        ClassItem::Word(positive) => is_word_character(character) == *positive,
        ClassItem::Space(positive) => character.is_whitespace() == *positive,
        ClassItem::Property(property, positive) => property.contains(character) == *positive,
    })
}

fn in_range(start: char, end: char, character: char) -> bool {
    start <= character && character <= end
}

fn chars_equal_ignoring_case(left: char, right: char) -> bool {
    if left == right {
        return true;
    }
    let left_folded = left.to_lowercase();
    let mut right_folded = right.to_lowercase();
    left_folded.eq(right_folded.by_ref()) && right_folded.next().is_none()
}

fn is_word_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

struct PatternParser<'a> {
    characters: &'a [char],
    cursor: usize,
    group_count: usize,
    /// Names found by [`scan_group_names`], so `\k<name>` can refer forward.
    names: Vec<(String, usize)>,
}

/// Find the named capture groups of `characters` and number them the way the
/// parser will, without building anything. Escapes and character classes are
/// skipped so a `(` inside them is not mistaken for a group.
fn scan_group_names(characters: &[char]) -> Vec<(String, usize)> {
    let mut names = Vec::new();
    let mut count = 0usize;
    let mut cursor = 0usize;
    let mut in_class = false;
    while cursor < characters.len() {
        match characters[cursor] {
            '\\' => cursor += 1,
            '[' => in_class = true,
            ']' => in_class = false,
            '(' if !in_class => {
                if characters.get(cursor + 1) != Some(&'?') {
                    count += 1;
                } else if characters.get(cursor + 2) == Some(&'<')
                    && !matches!(characters.get(cursor + 3), Some('=' | '!'))
                {
                    count += 1;
                    let start = cursor + 3;
                    let end = characters[start..]
                        .iter()
                        .position(|character| *character == '>')
                        .map(|offset| start + offset);
                    if let Some(end) = end {
                        names.push((characters[start..end].iter().collect(), count));
                    }
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    names
}

/// Compile a pattern with the given flags.
///
/// # Errors
///
/// Returns [`RegexSyntaxError`] for malformed patterns and for constructs the
/// engine deliberately does not support.
pub fn compile(pattern: &str, flags: &str) -> Result<Compiled, RegexSyntaxError> {
    let parsed_flags = Flags::parse(flags)?;
    let characters: Vec<char> = pattern.chars().collect();
    let names = scan_group_names(&characters);
    let mut parser = PatternParser {
        characters: &characters,
        cursor: 0,
        group_count: 0,
        names: names.clone(),
    };
    let root = parser.alternative(true)?;
    if parser.cursor != characters.len() {
        return Err(RegexSyntaxError::new(
            "unexpected ')' in pattern".to_owned(),
        ));
    }
    Ok(Compiled {
        root,
        group_count: parser.group_count,
        group_names: names,
        flags: parsed_flags,
        source: pattern.to_owned(),
    })
}

/// Parse-time validation of a regular expression literal: the early errors of
/// ECMA-262 §22.2.1 for the constructs this engine implements. A construct it
/// does not implement is accepted here and fails when the literal is evaluated.
///
/// # Errors
///
/// Returns the syntax error for a pattern or flag list ECMAScript rejects.
pub fn validate(pattern: &str, flags: &str) -> Result<(), RegexSyntaxError> {
    match compile(pattern, flags) {
        Ok(_) => Ok(()),
        Err(error) if error.unsupported => Ok(()),
        Err(error) => Err(error),
    }
}

impl PatternParser<'_> {
    fn peek(&self) -> Option<char> {
        self.characters.get(self.cursor).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.cursor += 1;
        Some(character)
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn alternative(&mut self, top_level: bool) -> Result<Node, RegexSyntaxError> {
        let mut branches = vec![self.sequence(top_level)?];
        while self.peek() == Some('|') {
            self.cursor += 1;
            branches.push(self.sequence(top_level)?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().unwrap_or(Node::Empty)
        } else {
            Node::Alternative(branches)
        })
    }

    fn sequence(&mut self, top_level: bool) -> Result<Node, RegexSyntaxError> {
        let mut items = Vec::new();
        while let Some(character) = self.peek() {
            if character == '|' || character == ')' {
                break;
            }
            let atom = self.atom(top_level)?;
            let atom = self.maybe_quantifier(atom)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.into_iter().next().unwrap_or(Node::Empty),
            _ => Node::Sequence(items),
        })
    }

    fn maybe_quantifier(&mut self, atom: Node) -> Result<Node, RegexSyntaxError> {
        let (min, max) = match self.peek() {
            Some('*') => {
                self.cursor += 1;
                (0, None)
            }
            Some('+') => {
                self.cursor += 1;
                (1, None)
            }
            Some('?') => {
                self.cursor += 1;
                (0, Some(1))
            }
            Some('{') => match self.try_bounds()? {
                Some(bounds) => bounds,
                None => return Ok(atom),
            },
            _ => return Ok(atom),
        };
        let greedy = !self.eat('?');
        if matches!(
            atom,
            Node::AnchorStart | Node::AnchorEnd | Node::WordBoundary(_) | Node::Lookbehind { .. }
        ) {
            return Err(RegexSyntaxError::new(
                "quantifier applied to an assertion".to_owned(),
            ));
        }
        Ok(Node::Quantifier {
            min,
            max,
            greedy,
            body: Box::new(atom),
        })
    }

    /// Parse `{n}`, `{n,}`, `{n,m}`; a `{` that is not valid bounds is a
    /// literal brace (Annex B tolerance used by real pages).
    fn try_bounds(&mut self) -> Result<Option<(u32, Option<u32>)>, RegexSyntaxError> {
        let saved = self.cursor;
        self.cursor += 1;
        let Some(min) = self.digits() else {
            self.cursor = saved;
            return Ok(None);
        };
        let max = if self.eat(',') {
            self.digits()
        } else {
            Some(min)
        };
        if !self.eat('}') {
            self.cursor = saved;
            return Ok(None);
        }
        if let Some(maximum) = max
            && maximum < min
        {
            return Err(RegexSyntaxError::new(
                "quantifier upper bound below lower bound".to_owned(),
            ));
        }
        Ok(Some((min, max)))
    }

    fn digits(&mut self) -> Option<u32> {
        let start = self.cursor;
        while self.peek().is_some_and(|value| value.is_ascii_digit()) {
            self.cursor += 1;
        }
        if self.cursor == start {
            return None;
        }
        let text: String = self.characters[start..self.cursor].iter().collect();
        text.parse().ok()
    }

    fn atom(&mut self, top_level: bool) -> Result<Node, RegexSyntaxError> {
        // Annex B: a braced quantifier with nothing before it is an early error,
        // while a `{` that cannot be a quantifier is an ordinary character.
        if self.peek() == Some('{') && self.try_bounds()?.is_some() {
            return Err(RegexSyntaxError::new("quantifier has nothing to repeat"));
        }
        let Some(character) = self.bump() else {
            return Err(RegexSyntaxError::new(
                "unexpected end of pattern".to_owned(),
            ));
        };
        match character {
            '^' => Ok(Node::AnchorStart),
            '$' => Ok(Node::AnchorEnd),
            '.' => Ok(Node::AnyChar),
            '[' => self.class(),
            '(' => self.group(top_level),
            '\\' => self.escape(),
            '*' | '+' | '?' => Err(RegexSyntaxError::new(
                "quantifier has nothing to repeat".to_owned(),
            )),
            other => Ok(Node::Literal(other)),
        }
    }

    fn group(&mut self, top_level: bool) -> Result<Node, RegexSyntaxError> {
        let mut index = None;
        if self.eat('?') {
            match self.bump() {
                Some(':') => {}
                Some('=') => {
                    let body = self.alternative(false)?;
                    if !self.eat(')') {
                        return Err(RegexSyntaxError::new("unterminated lookahead".to_owned()));
                    }
                    return Ok(Node::Lookahead {
                        negated: false,
                        body: Box::new(body),
                    });
                }
                Some('!') => {
                    let body = self.alternative(false)?;
                    if !self.eat(')') {
                        return Err(RegexSyntaxError::new("unterminated lookahead".to_owned()));
                    }
                    return Ok(Node::Lookahead {
                        negated: true,
                        body: Box::new(body),
                    });
                }
                Some('<') => {
                    if matches!(self.peek(), Some('=' | '!')) {
                        let negated = self.bump() == Some('!');
                        let body = self.alternative(false)?;
                        if !self.eat(')') {
                            return Err(RegexSyntaxError::new(
                                "unterminated lookbehind".to_owned(),
                            ));
                        }
                        return Ok(Node::Lookbehind {
                            negated,
                            body: Box::new(body),
                        });
                    }
                    // `(?<name>…)`: a capture group that also has a name.
                    let mut name = String::new();
                    loop {
                        match self.bump() {
                            Some('>') => break,
                            Some(character)
                                if character.is_alphanumeric()
                                    || character == '_'
                                    || character == '$' =>
                            {
                                name.push(character);
                            }
                            _ => {
                                return Err(RegexSyntaxError::new(
                                    "invalid capture group name".to_owned(),
                                ));
                            }
                        }
                    }
                    if name.is_empty() || name.starts_with(|first: char| first.is_ascii_digit()) {
                        return Err(RegexSyntaxError::new(
                            "invalid capture group name".to_owned(),
                        ));
                    }
                    let duplicates = self.names.iter().filter(|(existing, _)| *existing == name);
                    if duplicates.count() > 1 {
                        return Err(RegexSyntaxError::new(
                            "duplicate capture group name".to_owned(),
                        ));
                    }
                    self.group_count += 1;
                    index = Some(self.group_count);
                }
                Some('i' | 'm' | 's' | '-') => {
                    self.cursor -= 1;
                    return self.modifier_group();
                }
                _ => {
                    return Err(RegexSyntaxError::new("invalid group modifier".to_owned()));
                }
            }
        } else {
            self.group_count += 1;
            index = Some(self.group_count);
        }
        let body = self.alternative(top_level)?;
        if !self.eat(')') {
            return Err(RegexSyntaxError::new("unterminated group".to_owned()));
        }
        Ok(Node::Group {
            index,
            body: Box::new(body),
        })
    }

    /// `(?ims-ims:…)`: the modifiers proposal's group. Malformed modifier syntax
    /// (an unknown or repeated letter, a letter both added and removed, or an
    /// empty `(?-:`) is an early error. Well-formed modifiers are valid
    /// ECMAScript this engine does not implement, so they are deferred.
    fn modifier_group(&mut self) -> Result<Node, RegexSyntaxError> {
        let mut add = String::new();
        while let Some(letter @ ('i' | 'm' | 's')) = self.peek() {
            self.cursor += 1;
            add.push(letter);
        }
        let mut remove = String::new();
        let has_remove = self.eat('-');
        if has_remove {
            while let Some(letter @ ('i' | 'm' | 's')) = self.peek() {
                self.cursor += 1;
                remove.push(letter);
            }
        }
        if !self.eat(':') {
            return Err(RegexSyntaxError::new("invalid group modifier".to_owned()));
        }
        let unique = |text: &str| {
            text.chars()
                .enumerate()
                .all(|(index, letter)| !text[..index].contains(letter))
        };
        let disjoint = add.chars().all(|letter| !remove.contains(letter));
        if !unique(&add)
            || !unique(&remove)
            || !disjoint
            || (has_remove && add.is_empty() && remove.is_empty())
        {
            return Err(RegexSyntaxError::new("invalid group modifier".to_owned()));
        }
        Err(RegexSyntaxError::unsupported(
            "regular expression modifiers are not implemented".to_owned(),
        ))
    }

    fn class(&mut self) -> Result<Node, RegexSyntaxError> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut closed = false;
        while let Some(character) = self.bump() {
            if character == ']' {
                closed = true;
                break;
            }
            let low = if character == '\\' {
                match self.class_escape()? {
                    ClassEscape::Char(value) => value,
                    ClassEscape::Shorthand(item) => {
                        items.push(item);
                        continue;
                    }
                }
            } else {
                character
            };
            if self.peek() == Some('-')
                && self
                    .characters
                    .get(self.cursor + 1)
                    .is_some_and(|next| *next != ']')
            {
                self.cursor += 1;
                let high_character = self.bump().unwrap_or(']');
                let high = if high_character == '\\' {
                    match self.class_escape()? {
                        ClassEscape::Char(value) => value,
                        ClassEscape::Shorthand(_) => {
                            return Err(RegexSyntaxError::new(
                                "shorthand cannot bound a class range".to_owned(),
                            ));
                        }
                    }
                } else {
                    high_character
                };
                if high < low {
                    return Err(RegexSyntaxError::new("class range out of order".to_owned()));
                }
                items.push(ClassItem::Range(low, high));
            } else {
                items.push(ClassItem::Char(low));
            }
        }
        if !closed {
            return Err(RegexSyntaxError::new(
                "unterminated character class".to_owned(),
            ));
        }
        Ok(Node::Class { negated, items })
    }

    fn escape(&mut self) -> Result<Node, RegexSyntaxError> {
        let Some(character) = self.bump() else {
            return Err(RegexSyntaxError::new(
                "pattern ends with a lone backslash".to_owned(),
            ));
        };
        match character {
            'b' => Ok(Node::WordBoundary(true)),
            'B' => Ok(Node::WordBoundary(false)),
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                let item = shorthand_item(character);
                Ok(Node::Class {
                    negated: false,
                    items: vec![item],
                })
            }
            'p' | 'P' => {
                let property = self.property_escape()?;
                Ok(Node::Class {
                    negated: false,
                    items: vec![ClassItem::Property(property, character == 'p')],
                })
            }
            '1'..='9' => Ok(Node::Backreference(
                character.to_digit(10).unwrap_or_default() as usize,
            )),
            'k' if !self.names.is_empty() => {
                if !self.eat('<') {
                    return Err(RegexSyntaxError::new("invalid named reference".to_owned()));
                }
                let mut name = String::new();
                loop {
                    match self.bump() {
                        Some('>') => break,
                        Some(character) => name.push(character),
                        None => {
                            return Err(RegexSyntaxError::new(
                                "invalid named reference".to_owned(),
                            ));
                        }
                    }
                }
                let index = self
                    .names
                    .iter()
                    .find(|(candidate, _)| *candidate == name)
                    .map(|(_, index)| *index)
                    .ok_or_else(|| RegexSyntaxError::new("undefined named reference".to_owned()))?;
                Ok(Node::Backreference(index))
            }
            other => Ok(Node::Literal(self.escape_char(other)?)),
        }
    }

    fn class_escape(&mut self) -> Result<ClassEscape, RegexSyntaxError> {
        let Some(character) = self.bump() else {
            return Err(RegexSyntaxError::new(
                "class ends with a lone backslash".to_owned(),
            ));
        };
        match character {
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                Ok(ClassEscape::Shorthand(shorthand_item(character)))
            }
            'b' => Ok(ClassEscape::Char('\u{0008}')),
            'p' | 'P' => {
                let property = self.property_escape()?;
                Ok(ClassEscape::Shorthand(ClassItem::Property(
                    property,
                    character == 'p',
                )))
            }
            other => Ok(ClassEscape::Char(self.escape_char(other)?)),
        }
    }

    /// The `{Name}` or `{Name=Value}` after `\p`/`\P`.
    fn property_escape(&mut self) -> Result<property::Property, RegexSyntaxError> {
        if !self.eat('{') {
            return Err(RegexSyntaxError::new("invalid property escape".to_owned()));
        }
        let mut name = String::new();
        loop {
            match self.bump() {
                Some('}') => break,
                Some(character) => name.push(character),
                None => return Err(RegexSyntaxError::new("invalid property escape".to_owned())),
            }
        }
        property::Property::parse(&name).ok_or_else(|| {
            RegexSyntaxError::unsupported(format!("unsupported unicode property {name:?}"))
        })
    }

    fn escape_char(&mut self, character: char) -> Result<char, RegexSyntaxError> {
        match character {
            'f' => Ok('\u{000c}'),
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            'v' => Ok('\u{000b}'),
            '0' if !self
                .peek()
                .is_some_and(|value: char| value.is_ascii_digit()) =>
            {
                Ok('\0')
            }
            // §22.2.1 ControlEscape: `\cA` is U+0001 … `\cZ` is U+001A.
            'c' => match self.peek() {
                Some(letter) if letter.is_ascii_alphabetic() => {
                    self.cursor += 1;
                    Ok(char::from(letter as u8 % 32))
                }
                _ => Ok('\\'),
            },
            'x' => self.hex_escape(2),
            'u' => {
                if self.eat('{') {
                    let start = self.cursor;
                    while self.peek().is_some_and(|value| value.is_ascii_hexdigit()) {
                        self.cursor += 1;
                    }
                    let digits: String = self.characters[start..self.cursor].iter().collect();
                    if !(1..=6).contains(&digits.len()) || !self.eat('}') {
                        return Err(RegexSyntaxError::new(
                            "invalid \\u{...} escape in pattern".to_owned(),
                        ));
                    }
                    u32::from_str_radix(&digits, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or_else(|| {
                            RegexSyntaxError::new("invalid code point in pattern escape".to_owned())
                        })
                } else {
                    self.hex_escape(4)
                }
            }
            other => Ok(other),
        }
    }

    fn hex_escape(&mut self, digits: usize) -> Result<char, RegexSyntaxError> {
        let start = self.cursor;
        for _ in 0..digits {
            if !self.peek().is_some_and(|value| value.is_ascii_hexdigit()) {
                return Err(RegexSyntaxError::new(
                    "invalid hexadecimal escape in pattern".to_owned(),
                ));
            }
            self.cursor += 1;
        }
        let text: String = self.characters[start..self.cursor].iter().collect();
        let value = u32::from_str_radix(&text, 16)
            .map_err(|_| RegexSyntaxError::new("invalid escape value in pattern".to_owned()))?;
        if (0xd800..=0xdfff).contains(&value) {
            return char::from_u32(0xf_0000 + value - 0xd800).ok_or_else(|| {
                RegexSyntaxError::new("invalid escape value in pattern".to_owned())
            });
        }
        char::from_u32(value)
            .ok_or_else(|| RegexSyntaxError::new("invalid escape value in pattern".to_owned()))
    }
}

enum ClassEscape {
    Char(char),
    Shorthand(ClassItem),
}

fn shorthand_item(character: char) -> ClassItem {
    match character {
        'd' => ClassItem::Digit(true),
        'D' => ClassItem::Digit(false),
        'w' => ClassItem::Word(true),
        'W' => ClassItem::Word(false),
        's' => ClassItem::Space(true),
        'S' => ClassItem::Space(false),
        _ => unreachable!("callers only pass shorthand letters"),
    }
}

/// Unicode property escapes (`\p{…}`).
///
/// The standard library exposes only a few of the properties the specification
/// names, and this crate carries no Unicode database, so this is a deliberate
/// subset: the general categories and binary properties `std` can answer
/// exactly, a few that are answered by code-point ranges (documented below),
/// and the scripts that matter for the pages this engine is used on. A name
/// outside the table is a syntax error, not a silent non-match.
mod property {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Property {
        Any,
        Ascii,
        Letter,
        UppercaseLetter,
        LowercaseLetter,
        Number,
        Punctuation,
        Symbol,
        Separator,
        Alphabetic,
        Uppercase,
        Lowercase,
        WhiteSpace,
        Emoji,
        Script(Script),
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Script {
        Latin,
        Greek,
        Cyrillic,
        Han,
        Hiragana,
        Katakana,
        Hangul,
        Arabic,
        Hebrew,
        Thai,
        Devanagari,
    }

    impl Property {
        pub(super) fn parse(text: &str) -> Option<Self> {
            let (name, value) = match text.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (text, None),
            };
            match (name, value) {
                ("General_Category" | "gc", Some(value)) => Self::parse_category(value),
                ("Script" | "sc" | "Script_Extensions" | "scx", Some(value)) => {
                    Self::parse_script(value).map(Self::Script)
                }
                (name, None) => Self::parse_category(name).or(match name {
                    "Any" => Some(Self::Any),
                    "ASCII" => Some(Self::Ascii),
                    "Alphabetic" | "Alpha" => Some(Self::Alphabetic),
                    "Uppercase" | "Upper" => Some(Self::Uppercase),
                    "Lowercase" | "Lower" => Some(Self::Lowercase),
                    "White_Space" | "space" => Some(Self::WhiteSpace),
                    "Emoji" | "Emoji_Presentation" | "Extended_Pictographic" => Some(Self::Emoji),
                    _ => None,
                }),
                _ => None,
            }
        }

        fn parse_category(name: &str) -> Option<Self> {
            Some(match name {
                "L" | "Letter" => Self::Letter,
                "Lu" | "Uppercase_Letter" => Self::UppercaseLetter,
                "Ll" | "Lowercase_Letter" => Self::LowercaseLetter,
                "N" | "Number" => Self::Number,
                "P" | "Punctuation" | "punct" => Self::Punctuation,
                "S" | "Symbol" => Self::Symbol,
                "Z" | "Separator" | "Zs" | "Space_Separator" => Self::Separator,
                _ => return None,
            })
        }

        fn parse_script(name: &str) -> Option<Script> {
            Some(match name {
                "Latin" | "Latn" => Script::Latin,
                "Greek" | "Grek" => Script::Greek,
                "Cyrillic" | "Cyrl" => Script::Cyrillic,
                "Han" | "Hani" => Script::Han,
                "Hiragana" | "Hira" => Script::Hiragana,
                "Katakana" | "Kana" => Script::Katakana,
                "Hangul" | "Hang" => Script::Hangul,
                "Arabic" | "Arab" => Script::Arabic,
                "Hebrew" | "Hebr" => Script::Hebrew,
                "Thai" => Script::Thai,
                "Devanagari" | "Deva" => Script::Devanagari,
                _ => return None,
            })
        }

        pub(super) fn contains(self, character: char) -> bool {
            let code = u32::from(character);
            match self {
                Self::Any => true,
                Self::Ascii => character.is_ascii(),
                // `Letter` is Alphabetic without the number letters and the
                // combining marks `std`'s Alphabetic also admits.
                Self::Letter => {
                    character.is_alphabetic()
                        && !is_mark(code)
                        && !matches!(code, 0x2160..=0x2188 | 0x3007 | 0x3021..=0x3029)
                }
                Self::Alphabetic => character.is_alphabetic(),
                Self::UppercaseLetter | Self::Uppercase => character.is_uppercase(),
                Self::LowercaseLetter | Self::Lowercase => character.is_lowercase(),
                Self::Number => character.is_numeric(),
                Self::WhiteSpace => character.is_whitespace(),
                Self::Separator => {
                    character.is_whitespace() && !character.is_control() || code == 0x3000
                }
                Self::Punctuation => is_punctuation(character, code),
                Self::Symbol => is_symbol(character, code),
                Self::Emoji => is_emoji(code),
                Self::Script(script) => script.contains(code),
            }
        }
    }

    impl Script {
        fn contains(self, code: u32) -> bool {
            match self {
                Self::Latin => matches!(
                    code,
                    0x41..=0x5a | 0x61..=0x7a | 0xaa | 0xba | 0xc0..=0xd6 | 0xd8..=0xf6
                        | 0xf8..=0x2b8 | 0x1e00..=0x1eff | 0x2c60..=0x2c7f | 0xa720..=0xa7ff
                        | 0xff21..=0xff3a | 0xff41..=0xff5a
                ),
                Self::Greek => matches!(code, 0x370..=0x3ff | 0x1f00..=0x1fff),
                Self::Cyrillic => {
                    matches!(code, 0x400..=0x52f | 0x1c80..=0x1c8f | 0x2de0..=0x2dff | 0xa640..=0xa69f)
                }
                Self::Han => matches!(
                    code,
                    0x2e80..=0x2fdf | 0x3005 | 0x3007 | 0x3021..=0x3029 | 0x3038..=0x303b
                        | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff
                        | 0x20000..=0x2fa1f | 0x30000..=0x3134f
                ),
                Self::Hiragana => {
                    matches!(code, 0x3041..=0x3096 | 0x309d..=0x309f | 0x1b001..=0x1b11f)
                }
                Self::Katakana => matches!(
                    code,
                    0x30a1..=0x30fa | 0x30fd..=0x30ff | 0x31f0..=0x31ff | 0x32d0..=0x32fe
                        | 0x3300..=0x3357 | 0xff66..=0xff6f | 0xff71..=0xff9d
                ),
                Self::Hangul => matches!(
                    code,
                    0x1100..=0x11ff | 0x3131..=0x318e | 0xa960..=0xa97c | 0xac00..=0xd7a3 | 0xd7b0..=0xd7fb
                ),
                Self::Arabic => {
                    matches!(code, 0x600..=0x6ff | 0x750..=0x77f | 0x8a0..=0x8ff | 0xfb50..=0xfdff | 0xfe70..=0xfeff)
                }
                Self::Hebrew => matches!(code, 0x591..=0x5f4 | 0xfb1d..=0xfb4f),
                Self::Thai => matches!(code, 0xe01..=0xe3a | 0xe40..=0xe5b),
                Self::Devanagari => matches!(code, 0x900..=0x97f | 0xa8e0..=0xa8ff),
            }
        }
    }

    fn is_mark(code: u32) -> bool {
        matches!(
            code,
            0x300..=0x36f | 0x483..=0x489 | 0x591..=0x5bd | 0x610..=0x61a | 0x64b..=0x65f
                | 0x900..=0x903 | 0x93a..=0x94f | 0xe31 | 0xe34..=0xe3a | 0xe47..=0xe4e
                | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0x3099..=0x309a
                | 0xfe00..=0xfe0f | 0xfe20..=0xfe2f
        )
    }

    /// ASCII punctuation (not the ASCII symbols) plus the Latin-1, General
    /// Punctuation, CJK and fullwidth ranges that are punctuation.
    fn is_punctuation(character: char, code: u32) -> bool {
        if character.is_ascii() {
            return character.is_ascii_punctuation()
                && !matches!(
                    character,
                    '$' | '+' | '<' | '=' | '>' | '^' | '`' | '|' | '~'
                );
        }
        matches!(
            code,
            0xa1 | 0xa7 | 0xab | 0xb6 | 0xb7 | 0xbb | 0xbf | 0x37e | 0x387 | 0x55a..=0x55f
                | 0x589..=0x58a | 0x5be | 0x5c0 | 0x5c3 | 0x5c6 | 0x5f3..=0x5f4
                | 0x60c..=0x60d | 0x61b | 0x61e..=0x61f | 0x66a..=0x66d | 0x6d4
                | 0x2010..=0x2027 | 0x2030..=0x2043 | 0x2045..=0x2051 | 0x2053..=0x205e
                | 0x207d..=0x207e | 0x208d..=0x208e | 0x2308..=0x230b | 0x2329..=0x232a
                | 0x2768..=0x2775 | 0x27e6..=0x27ef | 0x2983..=0x2998 | 0x29d8..=0x29db
                | 0x29fc..=0x29fd | 0x2e00..=0x2e2e | 0x2e30..=0x2e4f
                | 0x3001..=0x3003 | 0x3008..=0x3011 | 0x3014..=0x301f | 0x3030 | 0x303d
                | 0x30a0 | 0x30fb | 0xfe10..=0xfe19 | 0xfe30..=0xfe52 | 0xfe54..=0xfe61
                | 0xfe63 | 0xfe68 | 0xfe6a..=0xfe6b | 0xff01..=0xff03 | 0xff05..=0xff0a
                | 0xff0c..=0xff0f | 0xff1a..=0xff1b | 0xff1f..=0xff20 | 0xff3b..=0xff3d
                | 0xff3f | 0xff5b | 0xff5d | 0xff5f..=0xff65
        )
    }

    fn is_symbol(character: char, code: u32) -> bool {
        if character.is_ascii() {
            return matches!(
                character,
                '$' | '+' | '<' | '=' | '>' | '^' | '`' | '|' | '~'
            );
        }
        matches!(
            code,
            0xa2..=0xa6 | 0xa8..=0xa9 | 0xac | 0xae..=0xb1 | 0xb4 | 0xb8 | 0xd7 | 0xf7
                | 0x2044 | 0x2052 | 0x207a..=0x207c | 0x208a..=0x208c | 0x20a0..=0x20c0
                | 0x2100..=0x214f | 0x2190..=0x2307 | 0x230c..=0x2328 | 0x232b..=0x2426
                | 0x2440..=0x244a | 0x249c..=0x24e9 | 0x2500..=0x2767 | 0x2794..=0x27c4
                | 0x27c7..=0x27e5 | 0x27f0..=0x2982 | 0x2999..=0x29d7 | 0x29dc..=0x29fb
                | 0x29fe..=0x2b73 | 0x2b76..=0x2bff | 0x3004 | 0x3012..=0x3013 | 0x3020
                | 0x3036..=0x3037 | 0x303e..=0x303f | 0xff04 | 0xff0b | 0xff1c..=0xff1e
                | 0xff3e | 0xff40 | 0xff5c | 0xff5e | 0xffe0..=0xffe6 | 0xffe8..=0xffee
                | 0x1f000..=0x1faff
        )
    }

    fn is_emoji(code: u32) -> bool {
        matches!(
            code,
            0x23 | 0x2a | 0x30..=0x39 | 0xa9 | 0xae | 0x203c | 0x2049 | 0x2122 | 0x2139
                | 0x2194..=0x21aa | 0x231a..=0x23ff | 0x24c2 | 0x25aa..=0x25fe
                | 0x2600..=0x27bf | 0x2934..=0x2935 | 0x2b05..=0x2b55 | 0x3030 | 0x303d
                | 0x3297 | 0x3299 | 0x1f000..=0x1faff
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Compiled, compile};

    fn matches(pattern: &str, flags: &str, input: &str) -> Option<(usize, usize)> {
        let compiled = compile(pattern, flags).expect("pattern should compile");
        let characters: Vec<char> = input.chars().collect();
        compiled
            .find(&characters, 0)
            .map(|found| (found.start, found.end))
    }

    fn groups(pattern: &str, flags: &str, input: &str) -> Vec<Option<String>> {
        let compiled = compile(pattern, flags).expect("pattern should compile");
        let characters: Vec<char> = input.chars().collect();
        compiled
            .find(&characters, 0)
            .map(|found| {
                found
                    .groups
                    .iter()
                    .map(|group| group.map(|(start, end)| characters[start..end].iter().collect()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn literals_classes_and_shorthands() {
        assert_eq!(matches("abc", "", "xxabcxx"), Some((2, 5)));
        assert_eq!(matches(r"\d+", "", "ab123"), Some((2, 5)));
        assert_eq!(matches(r"[a-c]+", "", "zzbaca"), Some((2, 6)));
        assert_eq!(matches(r"[^a-c]+", "", "abcd"), Some((3, 4)));
        assert_eq!(matches(r"\w\s\w", "", "ab c!"), Some((1, 4)));
        assert_eq!(matches(".", "", "\n"), None);
        assert_eq!(matches(".", "s", "\n"), Some((0, 1)));
    }

    #[test]
    fn quantifiers_are_greedy_lazy_or_bounded() {
        assert_eq!(matches("a*", "", "aaa"), Some((0, 3)));
        assert_eq!(matches("a+?", "", "aaa"), Some((0, 1)));
        assert_eq!(matches(r"\d{2,3}", "", "12345"), Some((0, 3)));
        assert_eq!(matches(r"\d{2,}", "", "1a22"), Some((2, 4)));
        assert_eq!(matches("colou?r", "", "color"), Some((0, 5)));
        assert_eq!(matches("colou?r", "", "colour"), Some((0, 6)));
    }

    #[test]
    fn alternation_groups_and_backreferences() {
        assert_eq!(matches("cat|dog", "", "hotdog"), Some((3, 6)));
        assert_eq!(
            groups(r"(\w+)-(\d+)", "", "item-42"),
            vec![Some("item".to_owned()), Some("42".to_owned())]
        );
        assert_eq!(matches(r"(\w)\1", "", "abb"), Some((1, 3)));
        assert_eq!(groups("(a)?(b)", "", "b"), vec![None, Some("b".to_owned())]);
    }

    #[test]
    fn anchors_boundaries_and_multiline() {
        assert_eq!(matches(r"^ab$", "", "ab"), Some((0, 2)));
        assert_eq!(matches(r"^b", "", "ab"), None);
        assert_eq!(matches(r"^b", "m", "ab\nbc"), Some((3, 4)));
        assert_eq!(matches(r"\bcat\b", "", "a cat!"), Some((2, 5)));
        assert_eq!(matches(r"\B\w", "", "ab"), Some((1, 2)));
    }

    #[test]
    fn lookahead_and_case_insensitivity() {
        assert_eq!(matches(r"foo(?=bar)", "", "foobar"), Some((0, 3)));
        assert_eq!(matches(r"foo(?=bar)", "", "foobaz"), None);
        assert_eq!(matches(r"foo(?!bar)", "", "foobaz"), Some((0, 3)));
        assert_eq!(matches("hello", "i", "say HELLO"), Some((4, 9)));
    }

    #[test]
    fn named_groups_lookbehind_and_properties() {
        let compiled = compile(r"(?<year>\d{4})-(?<month>\d\d)", "").expect("compiles");
        let input: Vec<char> = "on 2020-05-01".chars().collect();
        let found = compiled.find(&input, 0).expect("matches");
        assert_eq!(
            found
                .names
                .iter()
                .map(|(name, index)| (name.as_str(), *index))
                .collect::<Vec<_>>(),
            [("year", 1), ("month", 2)]
        );
        assert_eq!(matches(r"(?<q>['])x\k<q>", "", "'x'"), Some((0, 3)));
        assert_eq!(matches(r"(?<=\$)\d+", "", "cost $42"), Some((6, 8)));
        assert_eq!(matches(r"(?<!\$)\b\d+", "", "$42 17"), Some((4, 6)));
        assert_eq!(matches(r"(?<=ab|c)d", "", "abd"), Some((2, 3)));
        assert_eq!(matches(r"\p{L}+", "u", "12héllo!"), Some((2, 7)));
        assert_eq!(matches(r"\p{Lu}", "u", "abC"), Some((2, 3)));
        assert_eq!(matches(r"\P{L}+", "u", "ab12cd"), Some((2, 4)));
        assert_eq!(matches(r"\p{Script=Han}+", "u", "ab汉字cd"), Some((2, 4)));
        assert_eq!(matches(r"[\p{N}_]+", "u", "ab1_2c"), Some((2, 5)));
        assert_eq!(matches(r"\cJ", "", "a\nb"), Some((1, 2)));
    }

    #[test]
    fn sticky_and_from_respect_start_positions() {
        let compiled = compile("ab", "y").expect("compiles");
        let characters: Vec<char> = "xaby".chars().collect();
        assert_eq!(compiled.find(&characters, 0), None);
        assert_eq!(
            compiled.find(&characters, 1).map(|found| found.end),
            Some(3)
        );
    }

    #[test]
    fn unsupported_constructs_fail_with_clear_errors() {
        for pattern in [
            "(?<1a>a)",
            "(?<n>a)(?<n>b)",
            "\\p{NotAProperty}",
            "[a-",
            "(?<n>a)\\k<missing>",
        ] {
            assert!(
                compile(pattern, "").is_err(),
                "{pattern:?} should be rejected"
            );
        }
        assert!(compile("a", "q").is_err(), "unknown flag must be rejected");
    }

    #[test]
    fn parse_time_validation_rejects_early_errors_only() {
        use super::validate;
        for (pattern, flags) in [
            ("a", "gg"),
            ("a", "uv"),
            ("a", "q"),
            ("(?<=a)*", ""),
            ("{1}", ""),
            ("x|{2,3}", ""),
            ("a**", ""),
            ("[b-a]", ""),
            ("(", ""),
            ("(?<n>a)\\k<m>", ""),
            ("(?i-i:a)", ""),
            ("(?-:a)", ""),
            ("(?ii:a)", ""),
            ("(?\u{130}:a)", ""),
        ] {
            assert!(
                validate(pattern, flags).is_err(),
                "/{pattern}/{flags} is an early error"
            );
        }
        // Valid Annex B syntax, and valid syntax this engine does not implement
        // (deferred to evaluation), must not be rejected at parse time.
        for (pattern, flags) in [
            ("a{1}", ""),
            ("x{,2}", ""),
            ("(?=a)*", ""),
            ("a{", ""),
            ("\\k<m>", ""),
            ("\\p{ASCII_Hex_Digit}", "u"),
            ("(?i:a)", ""),
            ("(?-m:a)", ""),
            ("(?i-s:a)", ""),
        ] {
            assert!(
                validate(pattern, flags).is_ok(),
                "/{pattern}/{flags} must not be rejected at parse time"
            );
        }
    }

    #[test]
    fn pathological_patterns_stay_bounded() {
        // Catastrophic backtracking shape; the step cap keeps this finite.
        let compiled: Compiled = compile(r"(a+)+$", "").expect("compiles");
        let input: Vec<char> = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaab".chars().collect();
        assert_eq!(compiled.find(&input, 0), None);
    }
}
