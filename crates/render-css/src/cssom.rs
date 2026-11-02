//! Helpers for the CSS Object Model that the JavaScript host exposes.
//!
//! The engine's cascade consumes a parsed [`crate::stylesheet::StyleSheet`],
//! but the CSSOM exposes the sheet's rules as text (`CSSRule.cssText`) and
//! lets script insert and delete individual top-level rules. Keeping the
//! element's text as the single source of truth means a script edit flows
//! through the ordinary style recalculation path instead of a second,
//! parallel rule store.

/// Split `source` into its top-level rules, in source order.
///
/// Each returned string is the rule's original text without leading or
/// trailing whitespace and comments. A qualified rule extends through its
/// balanced `{}` block; an at-rule that has no block (`@import`,
/// `@charset`, `@layer a, b;`) ends at its top-level `;`. Braces, brackets,
/// parentheses, strings, escapes and comments inside a rule do not split it.
///
/// This is the CSS Syntax §5 "consume a list of rules" boundary at the depth
/// CSSOM observes: the contents of a group rule (`@media`, `@supports`,
/// `@keyframes`) stay inside that rule's text.
#[must_use]
pub fn split_rule_list(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut rules = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        // Whitespace and top-level comments between rules.
        index = skip_trivia(source, index);
        if index >= bytes.len() {
            break;
        }
        if source[index..].starts_with("<!--") {
            index += 4;
            continue;
        }
        if source[index..].starts_with("-->") {
            index += 3;
            continue;
        }
        let start = index;
        let mut braces = 0usize;
        let mut brackets = 0usize;
        let mut parentheses = 0usize;
        let mut quote = None;
        let mut saw_block = false;
        while index < bytes.len() {
            let byte = bytes[index];
            if let Some(active) = quote {
                if byte == b'\\' {
                    index += 2;
                    continue;
                }
                if byte == active {
                    quote = None;
                }
                index += 1;
                continue;
            }
            if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
                index = skip_comment(bytes, index);
                continue;
            }
            match byte {
                b'\'' | b'"' => {
                    quote = Some(byte);
                    index += 1;
                }
                b'\\' => index += 2,
                b'{' => {
                    braces += 1;
                    saw_block = true;
                    index += 1;
                }
                b'}' => {
                    if braces == 0 {
                        // Stray closing brace: CSS Syntax consumes it as a
                        // parse error and starts a fresh rule after it.
                        break;
                    }
                    braces -= 1;
                    index += 1;
                    if braces == 0 && brackets == 0 && parentheses == 0 {
                        break;
                    }
                }
                b'[' => {
                    brackets += 1;
                    index += 1;
                }
                b']' => {
                    brackets = brackets.saturating_sub(1);
                    index += 1;
                }
                b'(' => {
                    parentheses += 1;
                    index += 1;
                }
                b')' => {
                    parentheses = parentheses.saturating_sub(1);
                    index += 1;
                }
                b';' if !saw_block && braces == 0 && brackets == 0 && parentheses == 0 => {
                    index += 1;
                    break;
                }
                _ => index += 1,
            }
        }
        let rule = source[start..index].trim();
        if !rule.is_empty() {
            rules.push(rule.to_owned());
        }
        if index == start {
            // A stray `}` cannot make progress; consume it and continue.
            index += 1;
        }
    }
    rules
}

/// Skip whitespace and `/* … */` comments at the top level.
fn skip_trivia(source: &str, mut index: usize) -> usize {
    let bytes = source.as_bytes();
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index) == Some(&b'/') && bytes.get(index + 1) == Some(&b'*') {
            index = skip_comment(bytes, index);
            continue;
        }
        return index;
    }
}

/// The index just past the comment that starts at `index` (`/*`), or the end
/// of input when it is unterminated.
fn skip_comment(bytes: &[u8], mut index: usize) -> usize {
    index += 2;
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::split_rule_list;

    #[test]
    fn splits_top_level_style_rules() {
        assert_eq!(
            split_rule_list("a { color: red } b{color:blue}"),
            ["a { color: red }", "b{color:blue}"]
        );
    }

    #[test]
    fn keeps_group_rule_blocks_together() {
        assert_eq!(
            split_rule_list(
                "@media (min-width: 1px) { a { color: red } b { color: blue } }\n\
                 @supports (display: grid) { c { display: grid } }"
            ),
            [
                "@media (min-width: 1px) { a { color: red } b { color: blue } }",
                "@supports (display: grid) { c { display: grid } }"
            ]
        );
    }

    #[test]
    fn at_rules_without_blocks_end_at_a_semicolon() {
        assert_eq!(
            split_rule_list("@import url(a.css); @layer base, theme; p { color: red }"),
            [
                "@import url(a.css);",
                "@layer base, theme;",
                "p { color: red }"
            ]
        );
    }

    #[test]
    fn braces_and_semicolons_in_strings_and_comments_do_not_split() {
        assert_eq!(
            split_rule_list(
                "a::before { content: \"}\" } /* } not a rule */ b { background: url('x;y') }"
            ),
            [
                "a::before { content: \"}\" }",
                "b { background: url('x;y') }"
            ]
        );
    }

    #[test]
    fn comments_between_rules_are_not_part_of_a_rule() {
        assert_eq!(
            split_rule_list("/* lead */ a { color: red } /* middle */ /* tail */"),
            ["a { color: red }"]
        );
    }

    #[test]
    fn empty_and_comment_only_sources_have_no_rules() {
        assert!(split_rule_list("").is_empty());
        assert!(split_rule_list(" \n\t").is_empty());
        assert!(split_rule_list("/* only a comment */").is_empty());
    }

    #[test]
    fn unbalanced_input_still_yields_recoverable_rules() {
        assert_eq!(
            split_rule_list("a { color: red } } b { color: blue }"),
            ["a { color: red }", "b { color: blue }"]
        );
        assert_eq!(
            split_rule_list("@media { a { color: red }"),
            ["@media { a { color: red }"]
        );
    }
}
