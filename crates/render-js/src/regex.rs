//! A compact backtracking regular-expression engine covering the subset of
//! ECMAScript `RegExp` syntax that real-world pages rely on.
//!
//! The matcher reads UTF-16 code units, the unit a String is addressed in.
//! Without the `u` flag each unit is one character; with it a surrogate pair is
//! one code point and the pattern is read as code points. Match positions and
//! capture spans are code-unit indices, so callers need no conversion.
//!
//! Supported: literals, `.`, character classes with ranges/negation/shorthand,
//! `\d \D \s \S \w \W \b \B`, escapes (`\f \n \r \t \v \0 \xHH \uHHHH \u{H+}`,
//! control escapes, legacy octal escapes and escaped punctuators), capturing and
//! `(?:)` groups, alternation, greedy/lazy quantifiers (`* + ? {n} {n,} {n,m}`),
//! anchors (`^ $` with `m`), lookahead (`(?= )` `(?! )`), lookbehind
//! (`(?<= )` `(?<! )`), named groups (`(?<name> )`, `\k<name>`, repeated names in
//! different branches), backreferences (`\1`–`\9`), a documented subset of
//! unicode property escapes (`\p{…}` under `u`), the `u` syntax restrictions,
//! and the `i m s g y d` flags.
//!
//! Lookbehind is matched by trying each start position at or before the
//! current one and requiring the body to end exactly there, rather than by
//! matching right to left. The two agree on whether a lookbehind succeeds;
//! they can differ in what a capture group inside one captures.
//!
//! Explicitly rejected with a syntax error rather than misinterpreted: a
//! property name outside [`property`]'s table, and set operations inside
//! classes (the `v` flag is accepted but its syntax is not implemented).

use std::fmt;

use crate::utf16;

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
    /// `u`: the pattern and the input are read as code points.
    pub unicode: bool,
    /// `d`: a match result carries the spans of its groups.
    pub has_indices: bool,
    /// `v`: accepted and recorded, but its set-notation syntax is not
    /// implemented, so it reads the pattern without `u`.
    pub unicode_sets: bool,
}

impl Flags {
    /// Parse the flag letters. Duplicates, unknown letters and `u` with `v` are
    /// early errors.
    ///
    /// # Errors
    ///
    /// Returns an error for a repeated letter, a letter outside the supported
    /// set, or the combination `uv`.
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
                'd' => parsed.has_indices = true,
                'g' => parsed.global = true,
                'i' => parsed.ignore_case = true,
                'm' => parsed.multiline = true,
                's' => parsed.dot_all = true,
                'u' => parsed.unicode = true,
                // `v` reads patterns with the `u` semantics; its set notation
                // is not implemented (see `compile`).
                'v' => {
                    parsed.unicode = true;
                    parsed.unicode_sets = true;
                }
                'y' => parsed.sticky = true,
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

    /// The flags in the order `RegExp.prototype.flags` reports them.
    #[must_use]
    pub fn describe(self) -> String {
        let mut text = String::new();
        for (set, letter) in [
            (self.has_indices, 'd'),
            (self.global, 'g'),
            (self.ignore_case, 'i'),
            (self.multiline, 'm'),
            (self.dot_all, 's'),
            (self.unicode && !self.unicode_sets, 'u'),
            (self.unicode_sets, 'v'),
            (self.sticky, 'y'),
        ] {
            if set {
                text.push(letter);
            }
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
    /// A pattern character: a code unit without `u`, a code point with it.
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
    /// A reference to one or more groups. Duplicate names share one reference,
    /// and the group that took part in the match is the one referred to.
    Backreference(Vec<usize>),
    Quantifier {
        min: u32,
        max: Option<u32>,
        greedy: bool,
        body: Box<Node>,
        /// 1-based capture indices inside `body`, cleared at the start of every
        /// iteration (ECMA-262 `RepeatMatcher` step 4).
        reset: Option<(usize, usize)>,
    },
    AnchorStart,
    AnchorEnd,
    WordBoundary(bool),
    /// `(?ims-ims:…)`: the body matches with each named modifier set or cleared
    /// (`Some`), and leaves the flags in effect around the group alone (`None`).
    /// The continuation after the group runs under the flags around it again.
    Modifiers {
        ignore_case: Option<bool>,
        multiline: Option<bool>,
        dot_all: Option<bool>,
        body: Box<Node>,
    },
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

/// One successful match: overall span plus per-group spans (code-unit indices).
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

    /// Find the leftmost match starting at or after `from`, both in code units
    /// of `input`.
    #[must_use]
    pub fn find(&self, input: &[u16], from: usize) -> Option<MatchRanges> {
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

    fn find_with_depth(&self, input: &[u16], from: usize, max_depth: u32) -> Option<MatchRanges> {
        let unicode = self.flags.unicode;
        let mut start = from.min(input.len());
        // A start inside a surrogate pair begins at the pair's lead unit.
        if unicode && is_trail_inside_pair(input, start) {
            start -= 1;
        }
        loop {
            let mut matcher = Matcher {
                input,
                flags: self.flags,
                captures: vec![None; self.group_count + 1],
                steps: 0,
                depth: 0,
                max_depth,
            };
            let end = std::cell::Cell::new(None);
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
            if self.flags.sticky || start >= input.len() {
                return None;
            }
            start += if unicode {
                code_point_at(input, start, true).map_or(1, |(_, length)| length)
            } else {
                1
            };
        }
    }
}

struct Matcher<'a> {
    input: &'a [u16],
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
            Node::Literal(_) | Node::AnyChar | Node::Class { .. } => {
                let length = self.single_length(node, position)?;
                next(self, position + length)
            }
            Node::AnchorStart => {
                let at_start = position == 0
                    || (self.flags.multiline
                        && is_line_terminator(u32::from(self.input[position - 1])));
                if at_start { next(self, position) } else { None }
            }
            Node::AnchorEnd => {
                let at_end = position == self.input.len()
                    || (self.flags.multiline
                        && is_line_terminator(u32::from(self.input[position])));
                if at_end { next(self, position) } else { None }
            }
            Node::WordBoundary(expected) => {
                let before = position
                    .checked_sub(1)
                    .and_then(|index| self.input.get(index))
                    .is_some_and(|unit| is_word(u32::from(*unit), self.flags));
                let after = self
                    .input
                    .get(position)
                    .is_some_and(|unit| is_word(u32::from(*unit), self.flags));
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
            Node::Modifiers {
                ignore_case,
                multiline,
                dot_all,
                body,
            } => {
                let outer = self.flags;
                let mut inner = outer;
                if let Some(value) = ignore_case {
                    inner.ignore_case = *value;
                }
                if let Some(value) = multiline {
                    inner.multiline = *value;
                }
                if let Some(value) = dot_all {
                    inner.dot_all = *value;
                }
                self.flags = inner;
                let matched = self.node(body, position, &mut |matcher, end_position| {
                    matcher.flags = outer;
                    let result = next(matcher, end_position);
                    matcher.flags = inner;
                    result
                });
                self.flags = outer;
                matched
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
                // body that ends exactly at `position`. Under `u` a start inside
                // a surrogate pair is not a character boundary.
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
                    if self.flags.unicode && is_trail_inside_pair(self.input, start) {
                        continue;
                    }
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
            Node::Backreference(indices) => {
                let captured = indices
                    .iter()
                    .find_map(|index| self.captures.get(*index).copied().flatten());
                let Some((start, end)) = captured else {
                    return next(self, position);
                };
                let length = end - start;
                if position + length > self.input.len() {
                    return None;
                }
                // Under `u` a back-reference matches whole code points, so it can
                // neither start nor end inside a surrogate pair of the input.
                if self.flags.unicode
                    && (is_trail_inside_pair(self.input, position)
                        || is_trail_inside_pair(self.input, position + length))
                {
                    return None;
                }
                for offset in 0..length {
                    let expected = u32::from(self.input[start + offset]);
                    let actual = u32::from(self.input[position + offset]);
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
                reset,
            } => {
                if matches!(
                    body.as_ref(),
                    Node::Literal(_) | Node::AnyChar | Node::Class { .. }
                ) {
                    return self.simple_repeat(*min, *max, *greedy, body, position, next);
                }
                self.quantifier(*min, *max, *greedy, body, *reset, position, 0, next)
            }
        }
    }

    /// The length of the single character `node` matches at `position`, if it
    /// matches there.
    fn single_length(&self, node: &Node, position: usize) -> Option<usize> {
        let (actual, length) = code_point_at(self.input, position, self.flags.unicode)?;
        let matched = match node {
            Node::Literal(expected) => self.chars_match(code_value(*expected), actual),
            Node::AnyChar => self.flags.dot_all || !is_line_terminator(actual),
            Node::Class { negated, items } => class_contains(items, actual, self.flags) != *negated,
            _ => false,
        };
        matched.then_some(length)
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
        // `ends[k]` is the position after `k` repetitions.
        let mut ends = vec![position];
        while max.is_none_or(|limit| ends.len() - 1 < limit as usize) {
            let last = ends[ends.len() - 1];
            let Some(length) = self.single_length(body, last) else {
                break;
            };
            ends.push(last + length);
            self.tick()?;
        }
        let count = ends.len() - 1;
        let minimum = min as usize;
        if count < minimum {
            return None;
        }
        if greedy {
            for repetitions in (minimum..=count).rev() {
                self.tick()?;
                if let Some(result) = next(self, ends[repetitions]) {
                    return Some(result);
                }
            }
        } else {
            for &end in &ends[minimum..=count] {
                self.tick()?;
                if let Some(result) = next(self, end) {
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
        reset: Option<(usize, usize)>,
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
            // Each iteration starts with the groups inside the body unset; they
            // are put back if the iteration fails, so backtracking sees them.
            let saved = reset.map(|(first, last)| {
                let saved = matcher.captures[first..=last].to_vec();
                matcher.captures[first..=last].fill(None);
                saved
            });
            let result = matcher.node(body, position, &mut |inner, advanced| {
                if advanced == position && min <= count + 1 {
                    // An empty-body repetition would loop forever.
                    return next(inner, advanced);
                }
                inner.quantifier(min, max, greedy, body, reset, advanced, count + 1, next)
            });
            if result.is_none()
                && let (Some((first, last)), Some(saved)) = (reset, saved)
            {
                matcher.captures[first..=last].copy_from_slice(&saved);
            }
            result
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

    /// Whether the code unit or code point `actual` matches `expected`, with
    /// case folding when the `i` flag is set.
    fn chars_match(&self, expected: u32, actual: u32) -> bool {
        expected == actual
            || (self.flags.ignore_case
                && canonicalize(expected, self.flags.unicode)
                    == canonicalize(actual, self.flags.unicode))
    }
}

/// The code unit or code point a pattern character stands for.
fn code_value(character: char) -> u32 {
    utf16::placeholder_unit(character).map_or(u32::from(character), u32::from)
}

/// The pattern character for a code point or code unit, where a surrogate is
/// the placeholder that stands for one unpaired surrogate.
fn value_char(value: u32) -> Option<char> {
    if (0xd800..=0xdfff).contains(&value) {
        Some(crate::lexer::surrogate_placeholder(value))
    } else {
        char::from_u32(value)
    }
}

fn is_lead(unit: u16) -> bool {
    (0xd800..=0xdbff).contains(&unit)
}

fn is_trail(unit: u16) -> bool {
    (0xdc00..=0xdfff).contains(&unit)
}

/// Whether `position` falls on the trail unit of a surrogate pair.
fn is_trail_inside_pair(input: &[u16], position: usize) -> bool {
    position > 0
        && position < input.len()
        && is_trail(input[position])
        && is_lead(input[position - 1])
}

/// The code point at `position` and its length in code units. Without `u`, or
/// for an unpaired surrogate, a code point is one unit.
fn code_point_at(input: &[u16], position: usize, unicode: bool) -> Option<(u32, usize)> {
    let unit = *input.get(position)?;
    if unicode
        && is_lead(unit)
        && let Some(&trail) = input.get(position + 1)
        && is_trail(trail)
    {
        let value = 0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(trail) - 0xdc00);
        return Some((value, 2));
    }
    Some((u32::from(unit), 1))
}

/// ECMA-262 `LineTerminator`.
fn is_line_terminator(value: u32) -> bool {
    matches!(value, 0x0a | 0x0d | 0x2028 | 0x2029)
}

/// ECMA-262 `\s`: `WhiteSpace` and `LineTerminator`.
fn is_space(value: u32) -> bool {
    matches!(
        value,
        0x09..=0x0d
            | 0x20
            | 0xa0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

/// ECMA-262 `\w`, with the two extra characters that case-fold into it under
/// `u` and `i`.
fn is_word(value: u32, flags: Flags) -> bool {
    let basic = value < 128
        && char::from_u32(value)
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_');
    basic || (flags.unicode && flags.ignore_case && matches!(value, 0x17f | 0x212a))
}

/// The canonical form that case-insensitive matching compares: the single
/// uppercase mapping without `u`, and the single lowercase mapping (an
/// approximation of simple case folding) with `u`.
fn canonicalize(value: u32, unicode: bool) -> u32 {
    let Some(character) = char::from_u32(value) else {
        return value;
    };
    if unicode {
        return single_char(character.to_lowercase()).unwrap_or(value);
    }
    match single_char(character.to_uppercase()) {
        Some(upper) if upper <= 0xffff && !(value >= 128 && upper < 128) => upper,
        _ => value,
    }
}

/// The character itself and its single-character case mappings: the values a
/// case-insensitive class has to test.
fn case_variants(value: u32) -> [u32; 3] {
    let Some(character) = char::from_u32(value) else {
        return [value; 3];
    };
    [
        value,
        single_char(character.to_lowercase()).unwrap_or(value),
        single_char(character.to_uppercase()).unwrap_or(value),
    ]
}

fn single_char(mut mapping: impl Iterator<Item = char>) -> Option<u32> {
    match (mapping.next(), mapping.next()) {
        (Some(only), None) => Some(u32::from(only)),
        _ => None,
    }
}

fn class_contains(items: &[ClassItem], value: u32, flags: Flags) -> bool {
    if flags.ignore_case {
        // A member matches when it canonicalizes to the same character as the
        // input, so only variants with that canonical form may match.
        let canonical = canonicalize(value, flags.unicode);
        case_variants(value).into_iter().any(|candidate| {
            canonicalize(candidate, flags.unicode) == canonical
                && items
                    .iter()
                    .any(|item| item_matches(item, candidate, flags))
        })
    } else {
        items.iter().any(|item| item_matches(item, value, flags))
    }
}

fn item_matches(item: &ClassItem, value: u32, flags: Flags) -> bool {
    match item {
        ClassItem::Char(expected) => code_value(*expected) == value,
        ClassItem::Range(start, end) => code_value(*start) <= value && value <= code_value(*end),
        ClassItem::Digit(positive) => {
            (u32::from('0')..=u32::from('9')).contains(&value) == *positive
        }
        ClassItem::Word(positive) => is_word(value, flags) == *positive,
        ClassItem::Space(positive) => is_space(value) == *positive,
        ClassItem::Property(property, positive) => property.contains(value) == *positive,
    }
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

/// Find the capturing groups of `characters` and number them the way the
/// parser will, without building anything. Escapes and character classes are
/// skipped so a `(` inside them is not mistaken for a group. Returns the named
/// groups and the number of capturing groups.
fn scan_group_names(characters: &[char]) -> (Vec<(String, usize)>, usize) {
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
                    if let Some((name, _)) = read_group_name(characters, cursor + 3) {
                        names.push((name, count));
                    }
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    (names, count)
}

/// Read a `GroupName` whose `<` is already consumed, from `cursor` through its
/// closing `>`. Returns the decoded name and the cursor after the `>`. A `\u`
/// escape is decoded, and each character must be an identifier start (first)
/// or identifier part (later).
fn read_group_name(characters: &[char], mut cursor: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    loop {
        let character = *characters.get(cursor)?;
        if character == '>' {
            break;
        }
        let (value, next) = if character == '\\' {
            if characters.get(cursor + 1) != Some(&'u') {
                return None;
            }
            unicode_escape_value(characters, cursor + 2)?
        } else if let (Some(lead), Some(trail)) = (
            utf16::placeholder_unit(character),
            characters
                .get(cursor + 1)
                .and_then(|next| utf16::placeholder_unit(*next)),
        ) && is_lead(lead)
            && is_trail(trail)
        {
            // A literal surrogate pair, read as two characters without `u`.
            let value = 0x1_0000 + ((u32::from(lead) - 0xd800) << 10) + (u32::from(trail) - 0xdc00);
            (value, cursor + 2)
        } else {
            (code_value(character), cursor + 1)
        };
        let decoded = char::from_u32(value)?;
        let valid = if name.is_empty() {
            is_identifier_start(decoded)
        } else {
            is_identifier_part(decoded)
        };
        if !valid {
            return None;
        }
        name.push(decoded);
        cursor = next;
    }
    if name.is_empty() {
        return None;
    }
    Some((name, cursor + 1))
}

/// The code point of a `\u` escape whose `u` is already consumed: `{X…}`, or
/// four hex digits joined with a following `\uXXXX` trail surrogate. Returns the
/// code point and the cursor after the escape.
fn unicode_escape_value(characters: &[char], cursor: usize) -> Option<(u32, usize)> {
    if characters.get(cursor) == Some(&'{') {
        let close = cursor + 1 + characters[cursor + 1..].iter().position(|c| *c == '}')?;
        let digits: String = characters[cursor + 1..close].iter().collect();
        if digits.is_empty() || !digits.chars().all(|digit| digit.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(&digits, 16).ok()?;
        return (value <= 0x10_ffff).then_some((value, close + 1));
    }
    let lead = hex_value(characters, cursor, 4)?;
    if (0xd800..=0xdbff).contains(&lead)
        && characters.get(cursor + 4) == Some(&'\\')
        && characters.get(cursor + 5) == Some(&'u')
        && let Some(trail) = hex_value(characters, cursor + 6, 4)
        && (0xdc00..=0xdfff).contains(&trail)
    {
        let value = 0x1_0000 + ((lead - 0xd800) << 10) + (trail - 0xdc00);
        return Some((value, cursor + 10));
    }
    Some((lead, cursor + 4))
}

/// `digits` hexadecimal digits starting at `cursor`, if all of them are present.
fn hex_value(characters: &[char], cursor: usize, digits: usize) -> Option<u32> {
    let text: String = characters.get(cursor..cursor + digits)?.iter().collect();
    if !text.chars().all(|digit| digit.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(&text, 16).ok()
}

fn is_identifier_start(character: char) -> bool {
    character == '$' || character == '_' || unicode_ident::is_xid_start(character)
}

fn is_identifier_part(character: char) -> bool {
    is_identifier_start(character)
        || character == '\u{200c}'
        || character == '\u{200d}'
        || unicode_ident::is_xid_continue(character)
}

/// Whether two groups share a disjunction and sit in different branches of it.
fn in_different_branches(left: &[(usize, usize)], right: &[(usize, usize)]) -> bool {
    left.iter().any(|(disjunction, branch)| {
        right
            .iter()
            .any(|(other, other_branch)| disjunction == other && branch != other_branch)
    })
}

/// The pattern as the characters the parser reads: code points with the `u`
/// flag, code units without it (a surrogate unit is a placeholder character).
fn pattern_characters(pattern: &str, unicode: bool) -> Vec<char> {
    let units = utf16::utf16_units(pattern);
    let mut characters = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if unicode
            && is_lead(unit)
            && let Some(&trail) = units.get(index + 1)
            && is_trail(trail)
        {
            let value = 0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(trail) - 0xdc00);
            characters.push(char::from_u32(value).unwrap_or('\u{fffd}'));
            index += 2;
        } else {
            characters.push(value_char(u32::from(unit)).unwrap_or('\u{fffd}'));
            index += 1;
        }
    }
    characters
}

/// ECMA-262 §22.2.3 `ClassRanges` atom: one character, or a class escape.
enum ClassAtom {
    Char(char),
    Item(ClassItem),
}

impl ClassAtom {
    fn into_item(self) -> ClassItem {
        match self {
            Self::Char(character) => ClassItem::Char(character),
            Self::Item(item) => item,
        }
    }
}

/// Type of a named group as parsed: its name, its capture index, and the
/// enclosing `(disjunction, branch)` path.
type NamedGroup = (String, usize, Vec<(usize, usize)>);

/// Compile a pattern with the given flags.
///
/// # Errors
///
/// Returns [`RegexSyntaxError`] for malformed patterns and for constructs the
/// engine deliberately does not support.
pub fn compile(pattern: &str, flags: &str) -> Result<Compiled, RegexSyntaxError> {
    let parsed_flags = Flags::parse(flags)?;
    match compile_flags(pattern, parsed_flags) {
        Ok(compiled) => Ok(compiled),
        // Set notation under `v` is not implemented. A pattern that is not valid
        // `u` syntax may still be valid `v` syntax, so it keeps the reading it
        // had before `v` took `u` semantics; a pattern invalid under both is
        // deferred to evaluation, not rejected at parse time.
        Err(_) if parsed_flags.unicode_sets => compile_flags(
            pattern,
            Flags {
                unicode: false,
                ..parsed_flags
            },
        )
        .map_err(|error| RegexSyntaxError::unsupported(error.message)),
        Err(error) => Err(error),
    }
}

fn compile_flags(pattern: &str, parsed_flags: Flags) -> Result<Compiled, RegexSyntaxError> {
    let characters = pattern_characters(pattern, parsed_flags.unicode);
    let (names, total_groups) = scan_group_names(&characters);
    let mut parser = PatternParser {
        characters: &characters,
        cursor: 0,
        group_count: 0,
        total_groups,
        names: names.clone(),
        named_groups: Vec::new(),
        path: Vec::new(),
        disjunctions: 0,
        unicode: parsed_flags.unicode,
    };
    let root = parser.alternative(true)?;
    if parser.cursor != characters.len() {
        return Err(RegexSyntaxError::new(
            "unexpected ')' in pattern".to_owned(),
        ));
    }
    // ES2025: a name may repeat only in different branches of a disjunction.
    let groups = &parser.named_groups;
    for (position, (name, _, path)) in groups.iter().enumerate() {
        let repeats = groups[position + 1..].iter().any(|(other, _, other_path)| {
            other == name && !in_different_branches(path, other_path)
        });
        if repeats {
            return Err(RegexSyntaxError::new(
                "duplicate capture group name".to_owned(),
            ));
        }
    }
    Ok(Compiled {
        root,
        group_count: parser.group_count,
        group_names: names,
        flags: parsed_flags,
        source: pattern.to_owned(),
    })
}

/// ECMA-262 `AdvanceStringIndex`: the index after `index`. Under `u` a surrogate
/// pair is one step, so an empty match never lands between its two halves.
#[must_use]
pub fn advance_index(input: &[u16], index: usize, unicode: bool) -> usize {
    if unicode && let Some((_, length)) = code_point_at(input, index, true) {
        return index + length;
    }
    index + 1
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

struct PatternParser<'a> {
    characters: &'a [char],
    cursor: usize,
    group_count: usize,
    /// Capturing groups in the whole pattern, found before parsing so a decimal
    /// escape can be told apart from a legacy octal escape.
    total_groups: usize,
    /// Names found by [`scan_group_names`], so `\k<name>` can refer forward.
    names: Vec<(String, usize)>,
    /// Each named group as parsed, with the path that encloses it. A name may
    /// repeat only in different branches.
    named_groups: Vec<NamedGroup>,
    /// The `(disjunction, branch)` pairs enclosing the position being parsed.
    path: Vec<(usize, usize)>,
    disjunctions: usize,
    /// Parsing under `u`: the stricter grammar of ECMA-262 §22.2.1.
    unicode: bool,
}

/// The syntax characters, and `/`, that an identity escape may name under `u`.
fn is_identity_escapable(character: char, in_class: bool) -> bool {
    matches!(
        character,
        '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '/'
    ) || (in_class && character == '-')
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
        let disjunction = self.disjunctions;
        self.disjunctions += 1;
        let mut branches = Vec::new();
        loop {
            self.path.push((disjunction, branches.len()));
            let branch = self.sequence(top_level)?;
            self.path.pop();
            branches.push(branch);
            if self.peek() != Some('|') {
                break;
            }
            self.cursor += 1;
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
            let groups_before = self.group_count;
            let atom = self.atom(top_level)?;
            let reset =
                (self.group_count > groups_before).then_some((groups_before + 1, self.group_count));
            let atom = self.maybe_quantifier(atom, reset)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.into_iter().next().unwrap_or(Node::Empty),
            _ => Node::Sequence(items),
        })
    }

    fn maybe_quantifier(
        &mut self,
        atom: Node,
        reset: Option<(usize, usize)>,
    ) -> Result<Node, RegexSyntaxError> {
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
        // Lookbehinds and anchors are never quantified; under `u` neither are
        // lookaheads (Annex B allows them only without `u`).
        let assertion = matches!(
            atom,
            Node::AnchorStart | Node::AnchorEnd | Node::WordBoundary(_) | Node::Lookbehind { .. }
        ) || (self.unicode && matches!(atom, Node::Lookahead { .. }));
        if assertion {
            return Err(RegexSyntaxError::new(
                "quantifier applied to an assertion".to_owned(),
            ));
        }
        Ok(Node::Quantifier {
            min,
            max,
            greedy,
            body: Box::new(atom),
            reset,
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
        // The engine counts repetitions in u32; no input is long enough to tell
        // a larger bound apart from u32::MAX.
        let clamp = |count: u64| u32::try_from(count).unwrap_or(u32::MAX);
        Ok(Some((clamp(min), max.map(clamp))))
    }

    /// A decimal count. A count beyond `u64` saturates: no input is that long.
    fn digits(&mut self) -> Option<u64> {
        let start = self.cursor;
        while self.peek().is_some_and(|value| value.is_ascii_digit()) {
            self.cursor += 1;
        }
        if self.cursor == start {
            return None;
        }
        Some(
            self.characters[start..self.cursor]
                .iter()
                .fold(0u64, |count, digit| {
                    count
                        .saturating_mul(10)
                        .saturating_add(u64::from(digit.to_digit(10).unwrap_or_default()))
                }),
        )
    }

    fn atom(&mut self, top_level: bool) -> Result<Node, RegexSyntaxError> {
        // Annex B: a braced quantifier with nothing before it is an early error,
        // while a `{` that cannot be a quantifier is an ordinary character.
        if self.peek() == Some('{') && self.try_bounds()?.is_some() {
            return Err(RegexSyntaxError::new(
                "quantifier has nothing to repeat".to_owned(),
            ));
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
            // A lone bracket is a character only without `u`.
            '{' | '}' | ']' if self.unicode => Err(RegexSyntaxError::new(
                "lone quantifier bracket with the u flag".to_owned(),
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
                    let Some((name, next)) = read_group_name(self.characters, self.cursor) else {
                        return Err(RegexSyntaxError::new(
                            "invalid capture group name".to_owned(),
                        ));
                    };
                    self.cursor = next;
                    self.group_count += 1;
                    index = Some(self.group_count);
                    self.named_groups
                        .push((name, self.group_count, self.path.clone()));
                }
                Some('i' | 'm' | 's' | '-') => {
                    self.cursor -= 1;
                    return self.modifier_group(top_level);
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

    /// `(?ims-ims:…)`: the modifiers group of ECMA-262 22.2.2.1 (the `RegExp`
    /// modifiers proposal). Malformed modifier syntax (an unknown or repeated
    /// letter, a letter both added and removed, or an empty `(?-:`) is an early
    /// error.
    fn modifier_group(&mut self, top_level: bool) -> Result<Node, RegexSyntaxError> {
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
        let setting = |letter: char| {
            if add.contains(letter) {
                Some(true)
            } else if remove.contains(letter) {
                Some(false)
            } else {
                None
            }
        };
        let (ignore_case, multiline, dot_all) = (setting('i'), setting('m'), setting('s'));
        let body = self.alternative(top_level)?;
        if !self.eat(')') {
            return Err(RegexSyntaxError::new("unterminated group".to_owned()));
        }
        Ok(Node::Modifiers {
            ignore_case,
            multiline,
            dot_all,
            body: Box::new(body),
        })
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
                self.class_escape()?
            } else {
                ClassAtom::Char(character)
            };
            let is_range = self.peek() == Some('-')
                && self
                    .characters
                    .get(self.cursor + 1)
                    .is_some_and(|next| *next != ']');
            if !is_range {
                items.push(low.into_item());
                continue;
            }
            self.cursor += 1;
            let high = match self.bump() {
                Some('\\') => self.class_escape()?,
                Some(character) => ClassAtom::Char(character),
                None => break,
            };
            match (low, high) {
                (ClassAtom::Char(low), ClassAtom::Char(high)) => {
                    if code_value(high) < code_value(low) {
                        return Err(RegexSyntaxError::new("class range out of order".to_owned()));
                    }
                    items.push(ClassItem::Range(low, high));
                }
                (low, high) => {
                    if self.unicode {
                        return Err(RegexSyntaxError::new(
                            "class escape cannot bound a range with the u flag".to_owned(),
                        ));
                    }
                    // Annex B: the `-` is an ordinary character.
                    items.push(low.into_item());
                    items.push(ClassItem::Char('-'));
                    items.push(high.into_item());
                }
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
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => Ok(Node::Class {
                negated: false,
                items: vec![shorthand_item(character)],
            }),
            'p' | 'P' if self.unicode => {
                let property = self.property_escape()?;
                Ok(Node::Class {
                    negated: false,
                    items: vec![ClassItem::Property(property, character == 'p')],
                })
            }
            '1'..='9' => self.decimal_escape(character),
            '0' => {
                if self.unicode && self.peek().is_some_and(|value| value.is_ascii_digit()) {
                    return Err(RegexSyntaxError::new(
                        "decimal digit after \\0 with the u flag".to_owned(),
                    ));
                }
                Ok(Node::Literal(self.octal_escape(0)))
            }
            // `\k` names a group when the pattern has any, and always under `u`.
            'k' if self.unicode || !self.names.is_empty() => {
                if !self.eat('<') {
                    return Err(RegexSyntaxError::new("invalid named reference".to_owned()));
                }
                let Some((name, next)) = read_group_name(self.characters, self.cursor) else {
                    return Err(RegexSyntaxError::new("invalid named reference".to_owned()));
                };
                self.cursor = next;
                let indices: Vec<usize> = self
                    .names
                    .iter()
                    .filter(|(candidate, _)| *candidate == name)
                    .map(|(_, index)| *index)
                    .collect();
                if indices.is_empty() {
                    return Err(RegexSyntaxError::new(
                        "undefined named reference".to_owned(),
                    ));
                }
                Ok(Node::Backreference(indices))
            }
            other => Ok(Node::Literal(self.escape_char(other, false)?)),
        }
    }

    /// A decimal escape is a backreference only when it names a group of the
    /// pattern. Otherwise Annex B reads an octal escape, or for 8 and 9 the digit
    /// itself; under `u` it is an error.
    fn decimal_escape(&mut self, first_character: char) -> Result<Node, RegexSyntaxError> {
        let first = first_character.to_digit(10).unwrap_or_default();
        let after_first = self.cursor;
        let mut number = first as usize;
        while let Some(digit) = self.peek().and_then(|value| value.to_digit(10)) {
            number = number.saturating_mul(10).saturating_add(digit as usize);
            self.cursor += 1;
        }
        if number <= self.total_groups {
            return Ok(Node::Backreference(vec![number]));
        }
        if self.unicode {
            return Err(RegexSyntaxError::new(
                "backreference to a group that does not exist".to_owned(),
            ));
        }
        self.cursor = after_first;
        if first >= 8 {
            Ok(Node::Literal(first_character))
        } else {
            Ok(Node::Literal(self.octal_escape(first)))
        }
    }

    /// Annex B `LegacyOctalEscapeSequence` whose first digit `first` (0 to 7) is
    /// already consumed. `\0` not followed by an octal digit is NUL.
    fn octal_escape(&mut self, first: u32) -> char {
        if first == 0 && !self.peek().is_some_and(|value| value.is_digit(8)) {
            return '\0';
        }
        // Up to two more digits after 0 to 3, and one more after 4 to 7.
        let extra = if first <= 3 { 2 } else { 1 };
        let mut value = first;
        for _ in 0..extra {
            let Some(digit) = self.peek().and_then(|character| character.to_digit(8)) else {
                break;
            };
            value = value * 8 + digit;
            self.cursor += 1;
        }
        char::from_u32(value).unwrap_or('\0')
    }

    fn class_escape(&mut self) -> Result<ClassAtom, RegexSyntaxError> {
        let Some(character) = self.bump() else {
            return Err(RegexSyntaxError::new(
                "class ends with a lone backslash".to_owned(),
            ));
        };
        Ok(match character {
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => ClassAtom::Item(shorthand_item(character)),
            'b' => ClassAtom::Char('\u{0008}'),
            'B' if self.unicode => {
                return Err(RegexSyntaxError::new(
                    "\\B is not allowed in a class with the u flag".to_owned(),
                ));
            }
            'p' | 'P' if self.unicode => {
                let property = self.property_escape()?;
                ClassAtom::Item(ClassItem::Property(property, character == 'p'))
            }
            '0'..='7' => {
                let first = character.to_digit(8).unwrap_or_default();
                if self.unicode {
                    if first != 0 || self.peek().is_some_and(|value| value.is_ascii_digit()) {
                        return Err(RegexSyntaxError::new(
                            "octal escape in a class with the u flag".to_owned(),
                        ));
                    }
                    ClassAtom::Char('\0')
                } else {
                    ClassAtom::Char(self.octal_escape(first))
                }
            }
            '8' | '9' if self.unicode => {
                return Err(RegexSyntaxError::new(
                    "decimal escape in a class with the u flag".to_owned(),
                ));
            }
            // Annex B ClassControlLetter: `\c` followed by a digit or `_` in a class.
            'c' if !self.unicode
                && self
                    .peek()
                    .is_some_and(|value| value.is_ascii_digit() || value == '_') =>
            {
                let letter = self.bump().unwrap_or('_');
                ClassAtom::Char(char::from(letter as u8 % 32))
            }
            other => ClassAtom::Char(self.escape_char(other, true)?),
        })
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
        // The table is the specification's complete list, so a name it does not
        // have is an early error rather than something to defer.
        property::Property::parse(&name)
            .ok_or_else(|| RegexSyntaxError::new(format!("invalid unicode property {name:?}")))
    }

    /// The character that an escape other than the class, decimal and `\k`
    /// forms stands for. `character` is already consumed. Under `u` an identity
    /// escape must name a syntax character.
    fn escape_char(&mut self, character: char, in_class: bool) -> Result<char, RegexSyntaxError> {
        match character {
            'f' => Ok('\u{000c}'),
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            'v' => Ok('\u{000b}'),
            // §22.2.1 ControlEscape: `\cA` is U+0001 … `\cZ` is U+001A.
            'c' => match self.peek() {
                Some(letter) if letter.is_ascii_alphabetic() => {
                    self.cursor += 1;
                    Ok(char::from(letter as u8 % 32))
                }
                _ if self.unicode => Err(RegexSyntaxError::new(
                    "invalid control escape with the u flag".to_owned(),
                )),
                _ => {
                    // Annex B: the backslash is a character, and `c` is read again.
                    self.cursor -= 1;
                    Ok('\\')
                }
            },
            'x' => match hex_value(self.characters, self.cursor, 2) {
                Some(value) => {
                    self.cursor += 2;
                    Ok(value_char(value).unwrap_or('\u{fffd}'))
                }
                None if self.unicode => Err(RegexSyntaxError::new(
                    "invalid \\x escape in pattern".to_owned(),
                )),
                None => Ok('x'),
            },
            'u' => {
                if self.unicode {
                    let Some((value, next)) = unicode_escape_value(self.characters, self.cursor)
                    else {
                        return Err(RegexSyntaxError::new(
                            "invalid \\u escape in pattern".to_owned(),
                        ));
                    };
                    self.cursor = next;
                    Ok(value_char(value).unwrap_or('\u{fffd}'))
                } else {
                    match hex_value(self.characters, self.cursor, 4) {
                        Some(value) => {
                            self.cursor += 4;
                            Ok(value_char(value).unwrap_or('\u{fffd}'))
                        }
                        // Annex B: `\u` without four hex digits is the letter.
                        None => Ok('u'),
                    }
                }
            }
            other => {
                if self.unicode && !is_identity_escapable(other, in_class) {
                    Err(RegexSyntaxError::new(
                        "invalid identity escape with the u flag".to_owned(),
                    ))
                } else {
                    Ok(other)
                }
            }
        }
    }
}

mod property {
    //! The `\p{…}` / `\P{…}` property table (ECMA-262 22.2.2.9 and Table 67 to
    //! Table 69). Every name the specification lists is answered by the Unicode
    //! data in `icu_properties`; every other name is a syntax error, because
    //! the table is complete rather than a subset of it.

    use icu_properties::props::{GeneralCategory, GeneralCategoryGroup, Script, WhiteSpace};
    use icu_properties::script::ScriptWithExtensions;
    use icu_properties::{
        CodePointMapData, CodePointSetData, CodePointSetDataBorrowed, PropertyParser,
    };

    #[derive(Clone, Copy, Debug)]
    pub(super) enum Property {
        /// `Any`: every code point.
        Any,
        /// `ASCII`: U+0000 to U+007F.
        Ascii,
        /// `Assigned`: every code point whose `General_Category` is not `Cn`.
        Assigned,
        /// `General_Category` values, including the grouped ones (`L`, `Letter`).
        Category(GeneralCategoryGroup),
        /// `Script=`, and the Script property a value of `Script_Extensions=`
        /// names by its primary script.
        Script(Script),
        /// `Script_Extensions=`: every code point that has the script among its
        /// extensions.
        ScriptExtensions(Script),
        /// A binary property from the specification's table of binary properties.
        Binary(CodePointSetDataBorrowed<'static>),
    }

    impl Property {
        /// The property `text` names, as the body of `\p{text}`. `None` for a
        /// name the specification does not define.
        pub(super) fn parse(text: &str) -> Option<Self> {
            match text.split_once('=') {
                Some((name, value)) => match name {
                    "General_Category" | "gc" => parse_category(value).map(Self::Category),
                    "Script" | "sc" => PropertyParser::<Script>::new()
                        .get_strict(value)
                        .map(Self::Script),
                    "Script_Extensions" | "scx" => PropertyParser::<Script>::new()
                        .get_strict(value)
                        .map(Self::ScriptExtensions),
                    _ => None,
                },
                None => match text {
                    "Any" => Some(Self::Any),
                    "ASCII" => Some(Self::Ascii),
                    "Assigned" => Some(Self::Assigned),
                    // ECMA-262 Table 67 gives White_Space the short alias `space`,
                    // which ICU's ECMA-262 lookup does not recognise.
                    "space" => Some(Self::Binary(CodePointSetData::new::<WhiteSpace>())),
                    _ => CodePointSetData::new_for_ecma262(text.as_bytes())
                        .map(Self::Binary)
                        .or_else(|| parse_category(text).map(Self::Category)),
                },
            }
        }

        /// Whether the code point `value` (a code unit when the pattern is read
        /// without `u`, and a code point otherwise) has the property.
        pub(super) fn contains(self, value: u32) -> bool {
            match self {
                Self::Any => true,
                Self::Ascii => value < 0x80,
                Self::Assigned => {
                    CodePointMapData::<GeneralCategory>::new().get32(value)
                        != GeneralCategory::Unassigned
                }
                Self::Category(group) => {
                    group.contains(CodePointMapData::<GeneralCategory>::new().get32(value))
                }
                Self::Script(script) => CodePointMapData::<Script>::new().get32(value) == script,
                Self::ScriptExtensions(script) => {
                    ScriptWithExtensions::new().has_script32(value, script)
                }
                Self::Binary(set) => set.contains32(value),
            }
        }
    }

    /// The `General_Category` value or group a name or `name=value` pair names.
    fn parse_category(name: &str) -> Option<GeneralCategoryGroup> {
        let value = name
            .strip_prefix("General_Category=")
            .or_else(|| name.strip_prefix("gc="))
            .unwrap_or(name);
        PropertyParser::<GeneralCategory>::new()
            .get_strict(value)
            .map(GeneralCategoryGroup::from)
            .or_else(|| grouped_category(value))
    }

    /// The grouped `General_Category` names (`L`, `Letter`, `LC`, `Cased_Letter`,
    /// and so on), which name a set of values rather than one.
    fn grouped_category(name: &str) -> Option<GeneralCategoryGroup> {
        let members: &[GeneralCategory] = match name {
            "L" | "Letter" => &[
                GeneralCategory::UppercaseLetter,
                GeneralCategory::LowercaseLetter,
                GeneralCategory::TitlecaseLetter,
                GeneralCategory::ModifierLetter,
                GeneralCategory::OtherLetter,
            ],
            "LC" | "Cased_Letter" => &[
                GeneralCategory::UppercaseLetter,
                GeneralCategory::LowercaseLetter,
                GeneralCategory::TitlecaseLetter,
            ],
            "M" | "Mark" | "Combining_Mark" => &[
                GeneralCategory::NonspacingMark,
                GeneralCategory::SpacingMark,
                GeneralCategory::EnclosingMark,
            ],
            "N" | "Number" => &[
                GeneralCategory::DecimalNumber,
                GeneralCategory::LetterNumber,
                GeneralCategory::OtherNumber,
            ],
            "P" | "Punctuation" | "punct" => &[
                GeneralCategory::ConnectorPunctuation,
                GeneralCategory::DashPunctuation,
                GeneralCategory::OpenPunctuation,
                GeneralCategory::ClosePunctuation,
                GeneralCategory::InitialPunctuation,
                GeneralCategory::FinalPunctuation,
                GeneralCategory::OtherPunctuation,
            ],
            "S" | "Symbol" => &[
                GeneralCategory::MathSymbol,
                GeneralCategory::CurrencySymbol,
                GeneralCategory::ModifierSymbol,
                GeneralCategory::OtherSymbol,
            ],
            "Z" | "Separator" => &[
                GeneralCategory::SpaceSeparator,
                GeneralCategory::LineSeparator,
                GeneralCategory::ParagraphSeparator,
            ],
            "C" | "Other" => &[
                GeneralCategory::Control,
                GeneralCategory::Format,
                GeneralCategory::Surrogate,
                GeneralCategory::PrivateUse,
                GeneralCategory::Unassigned,
            ],
            _ => return None,
        };
        // One bit per value, the layout `GeneralCategoryGroup` itself uses.
        let mask = members
            .iter()
            .fold(0_u32, |mask, category| mask | (1 << (*category as u32)));
        Some(GeneralCategoryGroup::from(mask))
    }
}

#[cfg(test)]
mod tests {
    use super::{Compiled, Flags, compile, validate};

    fn units(input: &str) -> Vec<u16> {
        input.encode_utf16().collect()
    }

    fn matches(pattern: &str, flags: &str, input: &str) -> Option<(usize, usize)> {
        let compiled = compile(pattern, flags).expect("pattern should compile");
        compiled
            .find(&units(input), 0)
            .map(|found| (found.start, found.end))
    }

    fn groups(pattern: &str, flags: &str, input: &str) -> Vec<Option<String>> {
        let compiled = compile(pattern, flags).expect("pattern should compile");
        let input = units(input);
        compiled
            .find(&input, 0)
            .map(|found| {
                found
                    .groups
                    .iter()
                    .map(|group| {
                        group.map(|(start, end)| String::from_utf16_lossy(&input[start..end]))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn modifier_groups_change_flags_for_their_body_only() {
        // `(?i:…)` makes case folding local to the group, and the continuation
        // after the group reads the flags around it again.
        assert!(matches("(?i:a)b", "", "Ab").is_some());
        assert!(matches("(?i:a)b", "", "AB").is_none());
        // A removal inside an `i` pattern is local too.
        assert!(matches("a(?-i:b)", "i", "Ab").is_some());
        assert!(matches("a(?-i:b)", "i", "AB").is_none());
        // `s` and `m` change `.` and the anchors for their body alone.
        assert!(matches("(?s:.)", "", "\n").is_some());
        assert!(matches(".", "", "\n").is_none());
        assert!(matches("(?m:^b)", "", "a\nb").is_some());
        assert!(matches("^b", "", "a\nb").is_none());
        // Malformed modifiers are early errors.
        for pattern in ["(?ii:a)", "(?-:a)", "(?i-i:a)", "(?x:a)"] {
            let error = compile(pattern, "").expect_err(pattern);
            assert!(!error.unsupported, "{pattern} must be a syntax error");
        }
    }

    #[test]
    fn property_escapes_use_the_specification_table_exactly() {
        // Names the table lists match, including the aliases and the grouped
        // General_Category values.
        assert!(matches("\\p{Lu}", "u", "A").is_some());
        assert!(matches("\\p{Letter}", "u", "a").is_some());
        assert!(matches("\\p{scx=Thai}", "u", "\u{0e01}").is_some());
        assert!(matches("\\p{Script=Tolong_Siki}", "u", "\u{11db0}").is_some());
        assert!(matches("\\P{Lu}", "u", "a").is_some());
        // Names outside the table are early errors, not deferred ones: loose
        // matching, a removed binary property, and a property that is not one
        // of the specification's.
        for pattern in [
            "\\p{ascii}",
            "\\p{Hyphen}",
            "\\p{Line_Break}",
            "\\p{Script=Foo}",
            "\\p{IsScript=Adlam}",
        ] {
            let error = validate(pattern, "u").expect_err(pattern);
            assert!(!error.unsupported, "{pattern} must be a syntax error");
        }
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
        let input = units("on 2020-05-01");
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
        let input = units("xaby");
        assert_eq!(compiled.find(&input, 0), None);
        assert_eq!(compiled.find(&input, 1).map(|found| found.end), Some(3));
    }

    #[test]
    fn unsupported_constructs_fail_with_clear_errors() {
        for (pattern, flags) in [
            ("(?<1a>a)", ""),
            ("(?<n>a)(?<n>b)", ""),
            ("\\p{NotAProperty}", "u"),
            ("[a-", ""),
            ("(?<n>a)\\k<missing>", ""),
        ] {
            assert!(
                compile(pattern, flags).is_err(),
                "/{pattern}/{flags} should be rejected"
            );
        }
        assert!(compile("a", "q").is_err(), "unknown flag must be rejected");
    }

    #[test]
    fn parse_time_validation_rejects_early_errors_only() {
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
    fn named_groups_share_names_across_branches_and_decode_escapes() {
        // A name may repeat only in different branches of one disjunction.
        assert!(validate("(?<x>a)|(?<x>b)", "").is_ok());
        assert!(validate("(?<x>a)(?<x>b)", "").is_err());
        // The duplicate that took part is the one a backreference refers to.
        assert_eq!(matches(r"(?:(?<x>a)|(?<x>b))\k<x>", "", "bb"), Some((0, 2)));
        assert_eq!(matches(r"(?:(?<x>a)|(?<x>b))\k<x>", "", "aa"), Some((0, 2)));
        assert_eq!(matches(r"(?:(?<x>a)|(?<x>b))\k<x>", "", "ab"), None);
        // Each iteration of a quantifier starts its groups unset.
        assert_eq!(
            matches(r"(?:(?:(?<x>a)|(?<x>b))\k<x>){2}", "", "aabb"),
            Some((0, 4))
        );
        assert_eq!(
            matches(r"(?:(?:(?<x>a)|(?<x>b))\k<x>){2}", "", "abab"),
            None
        );
        assert_eq!(groups(r"(?:(a)|b)+", "", "ab"), vec![None]);
        // Group names decode identifier escapes, including surrogate pairs.
        let spelled = compile(r"(?<ab>x)\k<\u{61}b>", "").expect("compiles");
        assert_eq!(spelled.group_names[0].0, "ab");
        assert_eq!(matches(r"(?<ab>x)\k<\u{61}b>", "", "xx"), Some((0, 2)));
        let astral = compile(r"(?<𝒜>a)", "").expect("compiles");
        assert_eq!(astral.group_names[0].0, "\u{1d49c}");
        assert!(validate(r"(?<\uD835>a)", "").is_err());
        assert!(validate(r"(?<1a>a)", "").is_err());
        // Annex B: a decimal escape is a backreference only to an existing group.
        assert_eq!(matches(r"\1", "", "\u{1}"), Some((0, 1)));
        assert_eq!(matches(r"(a)\2", "", "a\u{2}"), Some((0, 2)));
        assert_eq!(matches(r"\8", "", "8"), Some((0, 1)));
        assert_eq!(matches(r"\012", "", "\n"), Some((0, 1)));
        assert_eq!(matches(r"[\1]", "", "\u{1}"), Some((0, 1)));
        assert_eq!(matches(r"\0", "", "\u{0}"), Some((0, 1)));
    }

    #[test]
    fn unicode_flag_reads_code_points_and_other_flags_read_units() {
        // Without u, `.` and a class match one code unit; with u, a surrogate pair.
        assert_eq!(matches(".", "", "😀"), Some((0, 1)));
        assert_eq!(matches(".", "u", "😀"), Some((0, 2)));
        assert_eq!(matches(r"\uD83D", "", "😀"), Some((0, 1)));
        assert_eq!(matches(r"😀", "", "😀"), Some((0, 2)));
        assert_eq!(matches(r"😀", "u", "😀"), Some((0, 2)));
        assert_eq!(matches(r"\u{1F600}", "u", "x😀"), Some((1, 3)));
        assert_eq!(matches(r"[😀]", "u", "😀"), Some((0, 2)));
        // A trail surrogate is not a character under u, but is one without it.
        assert_eq!(matches(r"\udf06", "u", "𝌆"), None);
        assert_eq!(matches(r"\udf06", "", "𝌆"), Some((1, 2)));
        assert_eq!(matches(r"(?<=😀)a", "u", "😀a"), Some((2, 3)));
        // `v` reads code points too; its set notation is not implemented.
        assert_eq!(matches(r"\p{Script=Han}", "v", "𠮷"), Some((0, 2)));
        assert_eq!(
            Flags::parse("v").map(Flags::describe).ok().as_deref(),
            Some("v")
        );
        // A count beyond u32 is a valid, saturated bound.
        assert!(validate("b{9007199254740991}", "u").is_ok());
        assert_eq!(matches("a{4294967296}", "", "a"), None);
    }

    #[test]
    fn line_terminators_and_space_follow_ecmascript() {
        assert_eq!(matches(".", "", "\u{2028}"), None);
        assert_eq!(matches(".", "", "\r"), None);
        assert_eq!(matches(".", "s", "\u{2028}"), Some((0, 1)));
        assert_eq!(matches("^b", "m", "a\rb"), Some((2, 3)));
        assert_eq!(matches("^b", "m", "a\u{2029}b"), Some((2, 3)));
        assert_eq!(matches(r"\s", "", "\u{85}"), None);
        assert_eq!(matches(r"\s", "", "\u{feff}"), Some((0, 1)));
        // With u and i, `\w` also takes the two characters that fold into it.
        assert_eq!(matches(r"\w", "ui", "\u{17f}"), Some((0, 1)));
        assert_eq!(matches(r"\w", "i", "\u{17f}"), None);
    }

    #[test]
    fn unicode_mode_rejects_annex_b_syntax() {
        for (pattern, flags) in [
            (r"\1", "u"),
            (r"\-", "u"),
            (r"[\d-a]", "u"),
            (r"\c", "u"),
            (r"\c1", "u"),
            ("{", "u"),
            ("}", "u"),
            ("]", "u"),
            ("a{", "u"),
            (r"\a", "u"),
            (r"\u12", "u"),
            (r"\x1", "u"),
            (r"[\B]", "u"),
            (r"(?=a)*", "u"),
            (r"\k<a>", "u"),
            (r"\00", "u"),
            (r"[\1]", "u"),
            (r"\p{L", "u"),
        ] {
            assert!(
                compile(pattern, flags).is_err(),
                "/{pattern}/{flags} must be rejected"
            );
        }
        // Annex B keeps these without u.
        assert_eq!(matches(r"\a", "", "a"), Some((0, 1)));
        assert_eq!(matches(r"\u12", "", "u12"), Some((0, 3)));
        assert_eq!(matches(r"\x1", "", "x1"), Some((0, 2)));
        assert_eq!(matches(r"(?=a)*a", "", "a"), Some((0, 1)));
        assert_eq!(matches(r"\k<a>", "", "k<a>"), Some((0, 4)));
        assert_eq!(matches(r"[\d-a]", "", "-"), Some((0, 1)));
        assert_eq!(matches(r"\p{L}", "", "p{L}"), Some((0, 4)));
        assert_eq!(matches(r"a{", "", "a{"), Some((0, 2)));
    }

    #[test]
    fn pathological_patterns_stay_bounded() {
        // Catastrophic backtracking shape; the step cap keeps this finite.
        let compiled: Compiled = compile(r"(a+)+$", "").expect("compiles");
        let input = units("aaaaaaaaaaaaaaaaaaaaaaaaaaaaab");
        assert_eq!(compiled.find(&input, 0), None);
    }
}
