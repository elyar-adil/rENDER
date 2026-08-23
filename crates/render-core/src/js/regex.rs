//! A compact backtracking regular-expression engine covering the subset of
//! ECMAScript `RegExp` syntax that real-world pages rely on.
//!
//! Supported: literals, `.`, character classes with ranges/negation/shorthand,
//! `\d \D \s \S \w \W \b \B`, escapes (`\f \n \r \t \v \0 \xHH \uHHHH \u{H+}`
//! plus escaped punctuators), capturing and `(?:)` groups, alternation,
//! greedy/lazy quantifiers (`* + ? {n} {n,} {n,m}`), anchors (`^ $` with `m`),
//! lookahead (`(?= )` `(?! )`), backreferences (`\1`–`\9`), and the `i m s g y`
//! flags (`u` is accepted for escape strictness parity but does not change
//! ASCII semantics).
//!
//! Explicitly rejected with a syntax error rather than misinterpreted:
//! lookbehind, named groups, unicode property escapes (`\p{…}`), and set
//! operations inside classes.

use std::fmt;

/// Why a pattern could not be compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexSyntaxError(pub String);

impl fmt::Display for RegexSyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
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
        for character in flags.chars() {
            match character {
                'g' => parsed.global = true,
                'i' => parsed.ignore_case = true,
                'm' => parsed.multiline = true,
                's' => parsed.dot_all = true,
                'y' => parsed.sticky = true,
                'd' | 'u' | 'v' => {}
                other => {
                    return Err(RegexSyntaxError(format!(
                        "unsupported regex flag {other:?}"
                    )));
                }
            }
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
    flags: Flags,
    source: String,
}

const MAX_MATCH_STEPS: u32 = 1_000_000;

/// One successful match: overall span plus per-group spans (character indices).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchRanges {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<(usize, usize)>>,
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
        let mut start = from.min(input.len());
        loop {
            let mut matcher = Matcher {
                input,
                flags: self.flags,
                captures: vec![None; self.group_count + 1],
                steps: 0,
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
            } => self.quantifier(*min, *max, *greedy, body, position, 0, next),
        }
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
    let mut parser = PatternParser {
        characters: &characters,
        cursor: 0,
        group_count: 0,
    };
    let root = parser.alternative(true)?;
    if parser.cursor != characters.len() {
        return Err(RegexSyntaxError("unexpected ')' in pattern".to_owned()));
    }
    Ok(Compiled {
        root,
        group_count: parser.group_count,
        flags: parsed_flags,
        source: pattern.to_owned(),
    })
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
            Node::AnchorStart | Node::AnchorEnd | Node::WordBoundary(_)
        ) {
            return Err(RegexSyntaxError(
                "quantifier applied to an anchor".to_owned(),
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
            return Err(RegexSyntaxError(
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
        let Some(character) = self.bump() else {
            return Err(RegexSyntaxError("unexpected end of pattern".to_owned()));
        };
        match character {
            '^' => Ok(Node::AnchorStart),
            '$' => Ok(Node::AnchorEnd),
            '.' => Ok(Node::AnyChar),
            '[' => self.class(),
            '(' => self.group(top_level),
            '\\' => self.escape(),
            '*' | '+' | '?' => Err(RegexSyntaxError(
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
                        return Err(RegexSyntaxError("unterminated lookahead".to_owned()));
                    }
                    return Ok(Node::Lookahead {
                        negated: false,
                        body: Box::new(body),
                    });
                }
                Some('!') => {
                    let body = self.alternative(false)?;
                    if !self.eat(')') {
                        return Err(RegexSyntaxError("unterminated lookahead".to_owned()));
                    }
                    return Ok(Node::Lookahead {
                        negated: true,
                        body: Box::new(body),
                    });
                }
                Some('<') => {
                    return Err(RegexSyntaxError(
                        "lookbehind and named groups are not supported".to_owned(),
                    ));
                }
                _ => {
                    return Err(RegexSyntaxError("invalid group modifier".to_owned()));
                }
            }
        } else {
            self.group_count += 1;
            index = Some(self.group_count);
        }
        let body = self.alternative(top_level)?;
        if !self.eat(')') {
            return Err(RegexSyntaxError("unterminated group".to_owned()));
        }
        Ok(Node::Group {
            index,
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
                            return Err(RegexSyntaxError(
                                "shorthand cannot bound a class range".to_owned(),
                            ));
                        }
                    }
                } else {
                    high_character
                };
                if high < low {
                    return Err(RegexSyntaxError("class range out of order".to_owned()));
                }
                items.push(ClassItem::Range(low, high));
            } else {
                items.push(ClassItem::Char(low));
            }
        }
        if !closed {
            return Err(RegexSyntaxError("unterminated character class".to_owned()));
        }
        Ok(Node::Class { negated, items })
    }

    fn escape(&mut self) -> Result<Node, RegexSyntaxError> {
        let Some(character) = self.bump() else {
            return Err(RegexSyntaxError(
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
            'p' | 'P' => Err(RegexSyntaxError(
                "unicode property escapes are not supported".to_owned(),
            )),
            '1'..='9' => Ok(Node::Backreference(
                character.to_digit(10).unwrap_or_default() as usize,
            )),
            'k' => Err(RegexSyntaxError(
                "named backreferences are not supported".to_owned(),
            )),
            other => Ok(Node::Literal(self.escape_char(other)?)),
        }
    }

    fn class_escape(&mut self) -> Result<ClassEscape, RegexSyntaxError> {
        let Some(character) = self.bump() else {
            return Err(RegexSyntaxError(
                "class ends with a lone backslash".to_owned(),
            ));
        };
        match character {
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                Ok(ClassEscape::Shorthand(shorthand_item(character)))
            }
            'b' => Ok(ClassEscape::Char('\u{0008}')),
            'p' | 'P' => Err(RegexSyntaxError(
                "unicode property escapes are not supported".to_owned(),
            )),
            other => Ok(ClassEscape::Char(self.escape_char(other)?)),
        }
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
            'c' => Err(RegexSyntaxError(
                "control escapes are not supported".to_owned(),
            )),
            'x' => self.hex_escape(2),
            'u' => {
                if self.eat('{') {
                    let start = self.cursor;
                    while self.peek().is_some_and(|value| value.is_ascii_hexdigit()) {
                        self.cursor += 1;
                    }
                    let digits: String = self.characters[start..self.cursor].iter().collect();
                    if !(1..=6).contains(&digits.len()) || !self.eat('}') {
                        return Err(RegexSyntaxError(
                            "invalid \\u{...} escape in pattern".to_owned(),
                        ));
                    }
                    u32::from_str_radix(&digits, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or_else(|| {
                            RegexSyntaxError("invalid code point in pattern escape".to_owned())
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
                return Err(RegexSyntaxError(
                    "invalid hexadecimal escape in pattern".to_owned(),
                ));
            }
            self.cursor += 1;
        }
        let text: String = self.characters[start..self.cursor].iter().collect();
        u32::from_str_radix(&text, 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| RegexSyntaxError("invalid escape value in pattern".to_owned()))
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
        for pattern in ["(?<name>a)", "(?=a)\\k<name>", "\\p{L}", "[a-"] {
            assert!(
                compile(pattern, "").is_err(),
                "{pattern:?} should be rejected"
            );
        }
        assert!(compile("a", "q").is_err(), "unknown flag must be rejected");
    }

    #[test]
    fn pathological_patterns_stay_bounded() {
        // Catastrophic backtracking shape; the step cap keeps this finite.
        let compiled: Compiled = compile(r"(a+)+$", "").expect("compiles");
        let input: Vec<char> = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaab".chars().collect();
        assert_eq!(compiled.find(&input, 0), None);
    }
}
