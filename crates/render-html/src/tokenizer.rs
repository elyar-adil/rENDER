use std::collections::HashMap;
use std::sync::OnceLock;

use entities::ENTITIES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeToken {
    pub name: String,
    pub value: String,
}

/// What ended an attribute, which is what decides whether the attribute after it
/// is missing its separator.
///
/// The standard raises `missing-whitespace-between-attributes` in exactly one
/// state — 13.2.5.39, "after attribute value (quoted)" — and every other state
/// that can follow an attribute has an explicit case for every character that
/// could be a separator. So the question is not "is there whitespace in the
/// stream" but "which state did this attribute leave the tokenizer in".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttributeEnd {
    /// The attribute's value was quoted, and this is the character after its
    /// closing quote — the character the "after attribute value (quoted)" state
    /// consumes. `None` at the end of the input.
    AfterQuotedValue(Option<char>),
    /// The attribute had no value, or an unquoted one. Whitespace, `/` and `>`
    /// all have explicit cases in the state such an attribute leaves the
    /// tokenizer in, and an unquoted value absorbs every other character, so
    /// nothing here can make the next attribute look unseparated.
    AfterPlainValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagToken {
    pub name: String,
    pub attributes: Vec<AttributeToken>,
    pub self_closing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoctypeToken {
    pub name: Option<String>,
    pub public_id: Option<String>,
    pub system_id: Option<String>,
    pub force_quirks: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Doctype(DoctypeToken),
    StartTag(TagToken),
    EndTag(TagToken),
    Comment(String),
    /// A processing instruction (13.2.5.72-76), the only token whose target
    /// keeps its case: the tokenizer compares the target against `xml` and
    /// `xml-stylesheet` case-insensitively but stores it as written.
    ProcessingInstruction {
        target: String,
        data: String,
    },
    Character(String),
    /// The characters of a CDATA section (13.2.5.69-71). A section is only
    /// tokenized when the tree builder reports that the adjusted current node is
    /// not an element in the HTML namespace; in the HTML namespace
    /// `<![CDATA[` is a `cdata-in-html-content` parse error whose content is
    /// comment tokens instead. Its characters are inserted as character data by
    /// the rules for parsing tokens in foreign content.
    Cdata(String),
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentModel {
    Data,
    Rcdata,
    RawText,
    ScriptData,
    Plaintext,
    Cdata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HtmlParseErrorCode {
    AbruptClosingOfEmptyComment,
    AbsenceOfDigitsInNumericCharacterReference,
    CharacterReferenceOutsideUnicodeRange,
    CdataInHtmlContent,
    ControlCharacterReference,
    DisallowedProcessingInstructionTarget,
    DuplicateAttribute,
    EofInProcessingInstruction,
    EofInScriptHtmlCommentLikeText,
    EndTagWithAttributes,
    EndTagWithTrailingSolidus,
    EofBeforeTagName,
    EofInCdata,
    EofInComment,
    EofInDoctype,
    EofInElementThatCanContainOnlyText,
    EofInTag,
    IncorrectlyOpenedComment,
    InvalidCharacterSequenceAfterDoctypeName,
    InvalidFirstCharacterOfProcessingInstructionTarget,
    InvalidFirstCharacterOfTagName,
    InvalidProcessingInstructionTarget,
    MissingAttributeValue,
    MissingDoctypeName,
    MissingEndTagName,
    MissingSemicolonAfterCharacterReference,
    MissingWhitespaceBeforeDoctypeName,
    MissingWhitespaceBetweenAttributes,
    NoncharacterCharacterReference,
    NonVoidHtmlElementStartTagWithTrailingSolidus,
    NullCharacterReference,
    SurrogateCharacterReference,
    UnexpectedCharacterInAttributeName,
    UnexpectedCharacterInUnquotedAttributeValue,
    UnexpectedEqualsSignBeforeAttributeName,
    UnexpectedNullCharacter,
    UnexpectedQuestionMarkInsteadOfTagName,
    UnexpectedSolidusInTag,
    UnknownNamedCharacterReference,
    MissingDoctype,
    UnexpectedToken,
}

impl HtmlParseErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AbruptClosingOfEmptyComment => "abrupt-closing-of-empty-comment",
            Self::AbsenceOfDigitsInNumericCharacterReference => {
                "absence-of-digits-in-numeric-character-reference"
            }
            Self::CharacterReferenceOutsideUnicodeRange => {
                "character-reference-outside-unicode-range"
            }
            Self::CdataInHtmlContent => "cdata-in-html-content",
            Self::DisallowedProcessingInstructionTarget => {
                "disallowed-processing-instruction-target"
            }
            Self::EofInProcessingInstruction => "eof-in-processing-instruction",
            Self::EofInScriptHtmlCommentLikeText => "eof-in-script-html-comment-like-text",
            Self::ControlCharacterReference => "control-character-reference",
            Self::DuplicateAttribute => "duplicate-attribute",
            Self::EndTagWithAttributes => "end-tag-with-attributes",
            Self::EndTagWithTrailingSolidus => "end-tag-with-trailing-solidus",
            Self::EofBeforeTagName => "eof-before-tag-name",
            Self::EofInCdata => "eof-in-cdata",
            Self::EofInComment => "eof-in-comment",
            Self::EofInDoctype => "eof-in-doctype",
            Self::EofInElementThatCanContainOnlyText => "eof-in-element-that-can-contain-only-text",
            Self::EofInTag => "eof-in-tag",
            Self::IncorrectlyOpenedComment => "incorrectly-opened-comment",
            Self::InvalidCharacterSequenceAfterDoctypeName => {
                "invalid-character-sequence-after-doctype-name"
            }
            Self::InvalidFirstCharacterOfProcessingInstructionTarget => {
                "invalid-first-character-of-processing-instruction-target"
            }
            Self::InvalidFirstCharacterOfTagName => "invalid-first-character-of-tag-name",
            Self::InvalidProcessingInstructionTarget => "invalid-processing-instruction-target",
            Self::MissingAttributeValue => "missing-attribute-value",
            Self::MissingDoctypeName => "missing-doctype-name",
            Self::MissingEndTagName => "missing-end-tag-name",
            Self::MissingSemicolonAfterCharacterReference => {
                "missing-semicolon-after-character-reference"
            }
            Self::MissingWhitespaceBeforeDoctypeName => "missing-whitespace-before-doctype-name",
            Self::MissingWhitespaceBetweenAttributes => "missing-whitespace-between-attributes",
            Self::NoncharacterCharacterReference => "noncharacter-character-reference",
            Self::NonVoidHtmlElementStartTagWithTrailingSolidus => {
                "non-void-html-element-start-tag-with-trailing-solidus"
            }
            Self::NullCharacterReference => "null-character-reference",
            Self::SurrogateCharacterReference => "surrogate-character-reference",
            Self::UnexpectedCharacterInAttributeName => "unexpected-character-in-attribute-name",
            Self::UnexpectedCharacterInUnquotedAttributeValue => {
                "unexpected-character-in-unquoted-attribute-value"
            }
            Self::UnexpectedEqualsSignBeforeAttributeName => {
                "unexpected-equals-sign-before-attribute-name"
            }
            Self::UnexpectedNullCharacter => "unexpected-null-character",
            Self::UnexpectedQuestionMarkInsteadOfTagName => {
                "unexpected-question-mark-instead-of-tag-name"
            }
            Self::UnexpectedSolidusInTag => "unexpected-solidus-in-tag",
            Self::UnknownNamedCharacterReference => "unknown-named-character-reference",
            Self::MissingDoctype => "missing-doctype",
            Self::UnexpectedToken => "unexpected-token",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtmlParseError {
    pub offset: usize,
    pub code: HtmlParseErrorCode,
}

/// The states of the script data state machine, 13.2.5.15 to 13.2.5.31.
///
/// The whole of script data's complication is the `<!-- ... -->` convention: a
/// script may wrap its body in an HTML-like comment, and inside that comment
/// the string `<script` opens a *nested* region in which `</script>` does **not**
/// close the element. Modelling that with a single "is the next thing the
/// appropriate end tag" test is not possible, which is why the states exist and
/// why a `<!--<script` in a script body is the classic real failure mode of a
/// page that lost its closing tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScriptDataState {
    Data,
    LessThan,
    EndTagOpen,
    EndTagName,
    EscapeStart,
    EscapeStartDash,
    Escaped,
    EscapedDash,
    EscapedDashDash,
    EscapedLessThan,
    EscapedEndTagOpen,
    EscapedEndTagName,
    DoubleEscapeStart,
    DoubleEscaped,
    DoubleEscapedDash,
    DoubleEscapedDashDash,
    DoubleEscapedLessThan,
    DoubleEscapeEnd,
}

impl ScriptDataState {
    /// Whether an end-of-file token in this state is the standard's
    /// `eof-in-script-html-comment-like-text` rather than the ordinary
    /// end-of-file-in-a-raw-text-element.
    const fn is_comment_like(self) -> bool {
        matches!(
            self,
            Self::Escaped
                | Self::EscapedDash
                | Self::EscapedDashDash
                | Self::DoubleEscaped
                | Self::DoubleEscapedDash
                | Self::DoubleEscapedDashDash
        )
    }
}

/// Incremental HTML tokenizer. The tree builder changes the content model after
/// inserting elements such as `title`, `textarea`, `style`, and `script`, just
/// as required by the HTML parsing algorithm.
pub struct Tokenizer<'a> {
    input: &'a str,
    offset: usize,
    content_model: ContentModel,
    appropriate_end_tag: Option<String>,
    errors: Vec<HtmlParseError>,
    emitted_eof: bool,
    allow_cdata: bool,
    /// Tokens produced ahead of the current input position.
    ///
    /// A queue rather than a single slot because the script data state machine
    /// can discover an end tag in the middle of a run of character data, and the
    /// text before it has to be delivered first: `<script>x</script>` is the
    /// text `x` and then the end tag, not the other way round.
    pending: Vec<Token>,
    script_state: ScriptDataState,
    /// The script data state machine's temporary buffer (13.2.5.17), which
    /// holds an end tag name and, in the double-escape states, the last matched
    /// `<script` or `</script`.
    script_buffer: String,
}

impl<'a> Tokenizer<'a> {
    #[must_use]
    pub const fn new(input: &'a str) -> Self {
        Self {
            input,
            offset: 0,
            content_model: ContentModel::Data,
            appropriate_end_tag: None,
            errors: Vec::new(),
            emitted_eof: false,
            allow_cdata: false,
            pending: Vec::new(),
            script_state: ScriptDataState::Data,
            script_buffer: String::new(),
        }
    }

    #[must_use]
    pub fn errors(&self) -> &[HtmlParseError] {
        &self.errors
    }

    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    #[must_use]
    pub fn into_errors(self) -> Vec<HtmlParseError> {
        self.errors
    }

    pub fn switch_to(&mut self, model: ContentModel, appropriate_end_tag: Option<&str>) {
        self.content_model = model;
        self.appropriate_end_tag = appropriate_end_tag.map(str::to_ascii_lowercase);
        self.script_state = ScriptDataState::Data;
        self.script_buffer.clear();
    }

    /// Report whether the tree builder's adjusted current node is an element
    /// outside the HTML namespace.
    ///
    /// The "markup declaration open state" (13.2.5.42) only enters the CDATA
    /// section state under that condition, and the tree builder owns the
    /// adjusted current node, so it pushes the answer in after every token.
    pub const fn set_allow_cdata(&mut self, allow_cdata: bool) {
        self.allow_cdata = allow_cdata;
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Token {
        // One input position can produce several tokens: the script data state
        // machine can emit a run of character data and then the end tag that
        // closes the element, and the `<?` EOF path emits its token before the
        // end-of-file token.
        if !self.pending.is_empty() {
            return self.pending.remove(0);
        }
        if self.emitted_eof {
            return Token::Eof;
        }
        // The raw-text and script-data content models have their own
        // end-of-input-stream clauses, and the tree builder's end tag can leave
        // the input position at the end with a half-built token still pending,
        // so the short-circuit applies only to the models that have nothing to
        // say at the end of the input.
        if self.offset >= self.input.len()
            && matches!(
                self.content_model,
                ContentModel::Data | ContentModel::Plaintext
            )
        {
            self.emitted_eof = true;
            return Token::Eof;
        }
        match self.content_model {
            ContentModel::Data => self.next_data_token(),
            ContentModel::Rcdata | ContentModel::RawText => self.next_text_content_token(),
            ContentModel::ScriptData => self.next_script_data_token(),
            ContentModel::Plaintext => self.next_plaintext_token(),
            ContentModel::Cdata => self.next_cdata_token(),
        }
    }

    fn next_data_token(&mut self) -> Token {
        match self.peek_char() {
            Some('&') => {
                let value = self
                    .consume_character_reference(false, None)
                    .unwrap_or_else(|| "&".to_owned());
                Token::Character(value)
            }
            Some('<') => self.consume_markup(),
            Some('\0') => {
                let offset = self.offset;
                self.bump_char();
                self.error_at(offset, HtmlParseErrorCode::UnexpectedNullCharacter);
                Token::Character("\u{fffd}".to_owned())
            }
            Some(_) => {
                let start = self.offset;
                while let Some(character) = self.peek_char() {
                    if matches!(character, '&' | '<' | '\0') {
                        break;
                    }
                    self.bump_char();
                }
                Token::Character(self.input[start..self.offset].to_owned())
            }
            None => self.emit_eof(),
        }
    }

    fn next_text_content_token(&mut self) -> Token {
        if self.is_appropriate_end_tag_at_current_offset() {
            self.offset += 2;
            let token = self.consume_tag(false);
            self.content_model = ContentModel::Data;
            self.appropriate_end_tag = None;
            return token;
        }

        match self.peek_char() {
            Some('&') if self.content_model == ContentModel::Rcdata => {
                let value = self
                    .consume_character_reference(false, None)
                    .unwrap_or_else(|| "&".to_owned());
                Token::Character(value)
            }
            Some('\0') => {
                let offset = self.offset;
                self.bump_char();
                self.error_at(offset, HtmlParseErrorCode::UnexpectedNullCharacter);
                Token::Character("\u{fffd}".to_owned())
            }
            Some(_) => {
                let start = self.offset;
                while self.offset < self.input.len() {
                    if self.is_appropriate_end_tag_at_current_offset()
                        || self.peek_char() == Some('\0')
                        || (self.content_model == ContentModel::Rcdata
                            && self.peek_char() == Some('&'))
                    {
                        break;
                    }
                    self.bump_char();
                }
                Token::Character(self.input[start..self.offset].to_owned())
            }
            None => {
                self.error_at(
                    self.offset,
                    HtmlParseErrorCode::EofInElementThatCanContainOnlyText,
                );
                self.emit_eof()
            }
        }
    }

    /// The script data state machine, 13.2.5.18 to 13.2.5.31.
    ///
    /// One character token is produced per run of ordinary characters; the
    /// states that only accumulate dashes and `<` characters are collapsed into
    /// the run because the tree builder inserts every character of a token at the
    /// same node anyway. What is **not** collapsed is the two decisions that
    /// change the shape of the tree:
    ///
    /// * a `</` followed by the appropriate end tag name ends the element, and
    /// * `<!--` followed by `<script` switches the machine into the "double
    ///   escaped" states, in which `</script>` is ordinary text until another
    ///   `</script>`-shaped sequence says the nesting is over.
    ///
    /// The second is the one that matters on real pages. A script that lost its
    /// closing tag leaves the rest of the document inside the element under the
    /// old rule that always looked for `</script>`, which is a different tree
    /// from the one the standard produces.
    fn next_script_data_token(&mut self) -> Token {
        let mut data = String::new();
        loop {
            let character = match self.peek_char() {
                Some(character) => character,
                None => return self.script_data_at_eof(data),
            };
            match self.script_state {
                ScriptDataState::Data => {
                    if character == '<' {
                        // The `<` is not character data yet: it may become an end
                        // tag or the start of an escape, and the standard only
                        // "emits" it in the states where neither happens.
                        self.bump_char();
                        self.script_state = ScriptDataState::LessThan;
                    } else {
                        self.push_script_character(&mut data, character);
                    }
                }
                ScriptDataState::LessThan => {
                    match character {
                        '/' => {
                            self.bump_char();
                            self.script_buffer.clear();
                            self.script_state = ScriptDataState::EndTagOpen;
                        }
                        '!' => {
                            // `<!` is character data whatever the escape states
                            // decide to do next; the two dashes are still in
                            // question, because only `--` opens the escaped
                            // states.
                            self.bump_char();
                            data.push_str("<!");
                            self.script_buffer.clear();
                            self.script_state = ScriptDataState::EscapeStart;
                        }
                        _ => {
                            data.push('<');
                            self.script_state = ScriptDataState::Data;
                        }
                    }
                }
                // "If the next input character is an ASCII alpha, then create a
                // new end tag token... Switch to the script data end tag name
                // state. Otherwise, this is an invalid-first-character-of-tag-name
                // parse error. Emit a U+003C character token and a U+002F
                // character token, switch to the data state, and reprocess the
                // current character."
                ScriptDataState::EndTagOpen => {
                    if character.is_ascii_alphabetic() {
                        // The character is not consumed: the state "reconsumes"
                        // it, so the first letter of the name is read by the
                        // name state like every other one.
                        self.script_buffer.clear();
                        self.script_state = ScriptDataState::EndTagName;
                    } else {
                        self.error_at(
                            self.offset,
                            HtmlParseErrorCode::InvalidFirstCharacterOfTagName,
                        );
                        data.push_str("</");
                        self.script_state = ScriptDataState::Data;
                    }
                }
                // "If the next input character is an ASCII alpha, append the
                // lowercase version to the tag token's tag name and append the
                // character to the temporary buffer. If the next input character is
                // one of tab, LF, FF, space, `/`, or `>` and the token is an
                // appropriate end tag, switch to the data state and emit the tag.
                // Otherwise, this is an invalid-first-character-of-tag-name parse
                // error: emit a U+003C character token, a U+002F character token,
                // and a character token for each of the characters in the temporary
                // buffer, switch to the script data state, and reprocess the
                // current character."
                //
                // The third clause is what makes a truncated `</script` at the end
                // of the input come out as the text `</script` rather than silently
                // closing the element. The temporary buffer keeps the characters
                // **as written**, so `</SCRIPT` that is not the right end tag comes
                // back as `</SCRIPT` and not as `</script`.
                ScriptDataState::EndTagName => {
                    if character.is_ascii_alphabetic() {
                        self.bump_char();
                        self.script_buffer.push(character);
                    } else if is_ascii_whitespace(character) || matches!(character, '/' | '>') {
                        let name = std::mem::take(&mut self.script_buffer);
                        self.script_state = ScriptDataState::Data;
                        if self.is_appropriate_end_tag_name(&name) {
                            // Re-read the end tag from its name so the tag
                            // tokenizer handles its attributes and its terminator.
                            self.offset -= name.len();
                            self.script_state = ScriptDataState::Data;
                            self.script_buffer.clear();
                            self.content_model = ContentModel::Data;
                            self.appropriate_end_tag = None;
                            let token = self.consume_tag(false);
                            return self.emit_after(data, token);
                        }
                        // A different end tag is not an error and does not close
                        // the element: its text stays script data.
                        data.push_str("</");
                        data.push_str(&name);
                    } else {
                        data.push_str("</");
                        let name = std::mem::take(&mut self.script_buffer);
                        data.push_str(&name);
                        self.script_state = ScriptDataState::Data;
                    }
                }
                ScriptDataState::EscapeStart => {
                    if character == '-' {
                        self.bump_char();
                        data.push('-');
                        self.script_state = ScriptDataState::EscapeStartDash;
                    } else {
                        self.script_state = ScriptDataState::Data;
                    }
                }
                ScriptDataState::EscapeStartDash => {
                    if character == '-' {
                        self.bump_char();
                        data.push('-');
                        // The second dash lands in the *dash dash* state, not the
                        // escaped state, which is why `<!--->` leaves the
                        // comment-like region rather than staying in it.
                        self.script_state = ScriptDataState::EscapedDashDash;
                    } else {
                        self.script_state = ScriptDataState::Data;
                    }
                }
                ScriptDataState::Escaped | ScriptDataState::DoubleEscaped => {
                    let escaped = self.script_state == ScriptDataState::Escaped;
                    match character {
                        '-' => {
                            self.bump_char();
                            data.push('-');
                            self.script_state = if escaped {
                                ScriptDataState::EscapedDash
                            } else {
                                ScriptDataState::DoubleEscapedDash
                            };
                        }
                        // The escaped state does not emit the `<`; the double
                        // escaped state does. That asymmetry is the standard's,
                        // and it is harmless because the escaped state's
                        // less-than sign state emits it on every path out.
                        '<' => {
                            self.bump_char();
                            if !escaped {
                                data.push('<');
                            }
                            self.script_state = if escaped {
                                ScriptDataState::EscapedLessThan
                            } else {
                                ScriptDataState::DoubleEscapedLessThan
                            };
                        }
                        _ => self.push_script_character(&mut data, character),
                    }
                }
                ScriptDataState::EscapedDash | ScriptDataState::DoubleEscapedDash => {
                    let escaped = self.script_state == ScriptDataState::EscapedDash;
                    match character {
                        '-' => {
                            self.bump_char();
                            data.push('-');
                            self.script_state = if escaped {
                                ScriptDataState::EscapedDashDash
                            } else {
                                ScriptDataState::DoubleEscapedDashDash
                            };
                        }
                        '<' => {
                            self.bump_char();
                            if !escaped {
                                data.push('<');
                            }
                            self.script_state = if escaped {
                                ScriptDataState::EscapedLessThan
                            } else {
                                ScriptDataState::DoubleEscapedLessThan
                            };
                        }
                        _ => {
                            self.script_state = if escaped {
                                ScriptDataState::Escaped
                            } else {
                                ScriptDataState::DoubleEscaped
                            };
                        }
                    }
                }
                ScriptDataState::EscapedDashDash | ScriptDataState::DoubleEscapedDashDash => {
                    let escaped = self.script_state == ScriptDataState::EscapedDashDash;
                    match character {
                        '-' => {
                            self.bump_char();
                            data.push('-');
                        }
                        '<' => {
                            self.bump_char();
                            if !escaped {
                                data.push('<');
                            }
                            self.script_state = if escaped {
                                ScriptDataState::EscapedLessThan
                            } else {
                                ScriptDataState::DoubleEscapedLessThan
                            };
                        }
                        // "Switch to the script data state. Emit a U+003E
                        // GREATER-THAN SIGN character token." The `>` of `-->`
                        // ends the comment-like region outright, in both the
                        // escaped and the double escaped states.
                        '>' => {
                            self.bump_char();
                            data.push('>');
                            self.script_state = ScriptDataState::Data;
                        }
                        _ => {
                            self.script_state = if escaped {
                                ScriptDataState::Escaped
                            } else {
                                ScriptDataState::DoubleEscaped
                            };
                        }
                    }
                }
                // "If the next input character is the U+002F SOLIDUS character
                // (`/`), set the temporary buffer to the empty string and switch to
                // the escaped end tag open state. If it is an ASCII alpha, set the
                // temporary buffer to the empty string, emit a U+003C character
                // token, and reconsume in the double escape start state. Otherwise,
                // this is a missing-`</script>`-code-point parse error, emit a
                // U+003C character token, and reconsume in the escaped state."
                ScriptDataState::EscapedLessThan => match character {
                    '/' => {
                        self.bump_char();
                        self.script_buffer.clear();
                        self.script_state = ScriptDataState::EscapedEndTagOpen;
                    }
                    _ if character.is_ascii_alphabetic() => {
                        self.script_buffer.clear();
                        data.push('<');
                        self.script_state = ScriptDataState::DoubleEscapeStart;
                    }
                    _ => {
                        data.push('<');
                        self.script_state = ScriptDataState::Escaped;
                    }
                },
                ScriptDataState::EscapedEndTagOpen => {
                    if character.is_ascii_alphabetic() {
                        self.script_buffer.clear();
                        self.script_state = ScriptDataState::EscapedEndTagName;
                    } else {
                        // "Emit a U+003C character token and a U+002F SOLIDUS
                        // character token." Neither was emitted on the way in.
                        data.push_str("</");
                        self.script_state = ScriptDataState::Escaped;
                    }
                }
                // "If the next input character is an ASCII alpha, append it to the
                // tag name and to the temporary buffer. If it is whitespace, `/`, or
                // `>` and the end tag is the appropriate one, emit it. Otherwise,
                // emit a U+003C character token, a U+002F character token, and a
                // start tag token whose tag name is the temporary buffer, switch to
                // the escaped state, and reprocess."
                // "If the current end tag token is an appropriate end tag token,
                // then switch to the data state and emit the current tag token.
                // Otherwise, treat it as per the 'anything else' entry below."
                //
                // "Appropriate" is a question about the parser, not about the
                // word `script`: the appropriate end tag is whatever the tree
                // builder last switched this content model to, and 13.4's
                // fragment case has none at all. Testing the name against the
                // literal `script` is invisible for a `script` element and wrong
                // for every other use of the script data state, which is how
                // `</script>` inside an HTML-like comment of a `script` fragment
                // used to close nothing and lose half the text.
                ScriptDataState::EscapedEndTagName => {
                    if character.is_ascii_alphabetic() {
                        self.bump_char();
                        self.script_buffer.push(character);
                    } else if is_ascii_whitespace(character) || matches!(character, '/' | '>') {
                        let name = std::mem::take(&mut self.script_buffer);
                        self.script_state = ScriptDataState::Escaped;
                        if self.is_appropriate_end_tag_name(&name) {
                            self.error_at(self.offset, HtmlParseErrorCode::UnexpectedToken);
                            self.offset -= name.len();
                            self.script_buffer.clear();
                            self.content_model = ContentModel::Data;
                            self.appropriate_end_tag = None;
                            let token = self.consume_tag(false);
                            return self.emit_after(data, token);
                        }
                        data.push_str("</");
                        data.push_str(&name);
                    } else {
                        data.push_str("</");
                        let name = std::mem::take(&mut self.script_buffer);
                        data.push_str(&name);
                        self.script_state = ScriptDataState::Escaped;
                    }
                }
                // "If the next input character is one of tab, LF, FF, space, `/`,
                // or `>`: if the temporary buffer is 'script', switch to the double
                // escaped state, otherwise to the escaped state, and emit the
                // current input character as a character token. If it is an ASCII
                // alpha, append it to the temporary buffer and emit it. Otherwise,
                // reconsume in the escaped state."
                //
                // The alpha characters are emitted *and* buffered, so `<sCrIpt`
                // inside an HTML-like comment stays in the text even though the
                // lowercase form decides which state comes next.
                ScriptDataState::DoubleEscapeStart => {
                    if is_ascii_whitespace(character) || matches!(character, '/' | '>') {
                        if self.script_buffer.eq_ignore_ascii_case("script") {
                            self.script_state = ScriptDataState::DoubleEscaped;
                        } else {
                            self.script_state = ScriptDataState::Escaped;
                        }
                        self.script_buffer.clear();
                        self.bump_char();
                        data.push(character);
                    } else if character.is_ascii_alphabetic() {
                        self.bump_char();
                        self.script_buffer.push(character.to_ascii_lowercase());
                        data.push(character);
                    } else {
                        self.script_state = ScriptDataState::Escaped;
                    }
                }
                // "If the next input character is the U+002F character, set the
                // temporary buffer to the empty string and switch to the double
                // escape end state. Otherwise, reconsume in the double escaped
                // state."
                ScriptDataState::DoubleEscapedLessThan => {
                    if character == '/' {
                        self.bump_char();
                        data.push('/');
                        self.script_buffer.clear();
                        self.script_state = ScriptDataState::DoubleEscapeEnd;
                    } else {
                        self.script_state = ScriptDataState::DoubleEscaped;
                    }
                }
                // "If the next input character is one of tab, LF, FF, space, `/`,
                // or `>`: if the temporary buffer is 'script', switch to the
                // escaped state, otherwise to the double escaped state, and emit
                // the current input character as a character token. If it is an
                // ASCII alpha, append it to the temporary buffer and emit it.
                // Otherwise, reconsume in the double escaped state."
                //
                // This is what turns `</script>` inside a `<!--<script>` region
                // back into a real end tag: the region is a comment-like wrapper
                // whose *inner* `</script>` only ends the inner region.
                ScriptDataState::DoubleEscapeEnd => {
                    if is_ascii_whitespace(character) || matches!(character, '/' | '>') {
                        if self.script_buffer.eq_ignore_ascii_case("script") {
                            self.script_state = ScriptDataState::Escaped;
                        } else {
                            self.script_state = ScriptDataState::DoubleEscaped;
                        }
                        self.script_buffer.clear();
                        self.bump_char();
                        data.push(character);
                    } else if character.is_ascii_alphabetic() {
                        self.bump_char();
                        self.script_buffer.push(character.to_ascii_lowercase());
                        data.push(character);
                    } else {
                        self.script_state = ScriptDataState::DoubleEscaped;
                    }
                }
            }
        }
    }

    /// The end of the input stream in a script data state.
    ///
    /// Six of the seventeen states give the end of the input stream its own
    /// case — the escaped and double escaped states report
    /// `eof-in-script-html-comment-like-text` and stop — and the rest fall
    /// through to their "anything else" clause, which reconsumes an imaginary
    /// character. That is not a formality: `<script><` is the text `<`, and
    /// `<script></` is the text `</`, and both are only right if the state the
    /// input ran out in gets to run its "anything else" branch before the parser
    /// stops.
    fn script_data_at_eof(&mut self, mut data: String) -> Token {
        let comment_like = self.script_state.is_comment_like();
        match self.script_state {
            ScriptDataState::Data => {}
            ScriptDataState::LessThan => {
                data.push('<');
                self.script_state = ScriptDataState::Data;
            }
            ScriptDataState::EndTagOpen => {
                // "This is an invalid-first-character-of-tag-name parse error."
                self.error_at(
                    self.offset,
                    HtmlParseErrorCode::InvalidFirstCharacterOfTagName,
                );
                data.push_str("</");
                self.script_state = ScriptDataState::Data;
            }
            ScriptDataState::EndTagName => {
                // The same "anything else" clause, reached with a half-built name.
                self.error_at(
                    self.offset,
                    HtmlParseErrorCode::InvalidFirstCharacterOfTagName,
                );
                data.push_str("</");
                data.push_str(&std::mem::take(&mut self.script_buffer));
                self.script_state = ScriptDataState::Data;
            }
            ScriptDataState::EscapeStart | ScriptDataState::EscapeStartDash => {
                self.script_state = ScriptDataState::Data;
            }
            ScriptDataState::Escaped
            | ScriptDataState::EscapedDash
            | ScriptDataState::EscapedDashDash
            | ScriptDataState::DoubleEscaped
            | ScriptDataState::DoubleEscapedDash
            | ScriptDataState::DoubleEscapedDashDash => {
                self.flush_then_eof(data, comment_like);
                return self.next();
            }
            ScriptDataState::EscapedLessThan => {
                data.push('<');
                self.script_state = ScriptDataState::Escaped;
            }
            ScriptDataState::EscapedEndTagOpen => {
                data.push_str("</");
                self.script_state = ScriptDataState::Escaped;
            }
            ScriptDataState::EscapedEndTagName => {
                data.push_str("</");
                data.push_str(&std::mem::take(&mut self.script_buffer));
                self.script_state = ScriptDataState::Escaped;
            }
            ScriptDataState::DoubleEscapeStart
            | ScriptDataState::DoubleEscapedLessThan
            | ScriptDataState::DoubleEscapeEnd => {
                self.script_state = if self.script_state == ScriptDataState::DoubleEscapeStart {
                    ScriptDataState::Escaped
                } else {
                    ScriptDataState::DoubleEscaped
                };
            }
        }
        if !data.is_empty() {
            self.pending.push(Token::Character(data));
            return self.next();
        }
        self.error_at(
            self.offset,
            if comment_like {
                HtmlParseErrorCode::EofInScriptHtmlCommentLikeText
            } else {
                HtmlParseErrorCode::EofInElementThatCanContainOnlyText
            },
        );
        self.emit_eof()
    }

    /// Report the end-of-input-stream parse error of a script data state and
    /// queue the end-of-file token behind whatever character data came first.
    fn flush_then_eof(&mut self, data: String, comment_like: bool) {
        if !data.is_empty() {
            self.pending.push(Token::Character(data));
        }
        self.error_at(
            self.offset,
            if comment_like {
                HtmlParseErrorCode::EofInScriptHtmlCommentLikeText
            } else {
                HtmlParseErrorCode::EofInElementThatCanContainOnlyText
            },
        );
        self.emitted_eof = true;
        self.pending.push(Token::Eof);
    }

    /// Emit `data` and then `token`, in that order, and return whichever comes
    /// first. The tree builder inserts every character of a character token at
    /// one node, so a run of script text only has to be delivered *before* the
    /// end tag that closes the element, not in one piece — but it does have to
    /// be in one piece in practice, because splitting it would make a test that
    /// asserts on tokens see a run-per-character.
    fn emit_after(&mut self, data: String, token: Token) -> Token {
        if data.is_empty() {
            return token;
        }
        self.pending.push(Token::Character(data));
        self.pending.push(token);
        self.next()
    }

    fn is_appropriate_end_tag_name(&self, name: &str) -> bool {
        self.appropriate_end_tag
            .as_deref()
            .is_some_and(|expected| expected.eq_ignore_ascii_case(name))
    }

    /// Append one character of script data, replacing U+0000 NULL with
    /// U+FFFD REPLACEMENT CHARACTER as the data state and the escaped states do.
    fn push_script_character(&mut self, data: &mut String, character: char) {
        self.bump_char();
        if character == '\0' {
            self.error_at(self.offset - 1, HtmlParseErrorCode::UnexpectedNullCharacter);
            data.push('\u{fffd}');
        } else {
            data.push(character);
        }
    }

    fn next_plaintext_token(&mut self) -> Token {
        if self.offset >= self.input.len() {
            return self.emit_eof();
        }
        let start = self.offset;
        let mut output = String::new();
        while let Some(character) = self.peek_char() {
            self.bump_char();
            if character == '\0' {
                self.error_at(self.offset - 1, HtmlParseErrorCode::UnexpectedNullCharacter);
                output.push('\u{fffd}');
            } else {
                output.push(character);
            }
        }
        if output.is_empty() {
            Token::Character(self.input[start..self.offset].to_owned())
        } else {
            Token::Character(output)
        }
    }

    fn consume_markup(&mut self) -> Token {
        let less_than_offset = self.offset;
        self.bump_char();
        if self.consume_char('!') {
            if self.remaining_starts_ascii_case_insensitive("doctype") {
                self.offset += "doctype".len();
                return self.consume_doctype();
            }
            if self.remaining().starts_with("--") {
                self.offset += 2;
                return self.consume_comment();
            }
            if self.remaining_starts_ascii_case_insensitive("[CDATA[") {
                return self.consume_cdata_start(less_than_offset);
            }
            self.error_at(
                less_than_offset,
                HtmlParseErrorCode::IncorrectlyOpenedComment,
            );
            return self.consume_bogus_comment("");
        }
        if self.consume_char('/') {
            match self.peek_char() {
                Some(character) if character.is_ascii_alphabetic() => {
                    return self.consume_tag(false);
                }
                Some('>') => {
                    self.bump_char();
                    self.error_at(less_than_offset, HtmlParseErrorCode::MissingEndTagName);
                    return self.next();
                }
                None => {
                    self.error_at(less_than_offset, HtmlParseErrorCode::EofBeforeTagName);
                    return Token::Character("</".to_owned());
                }
                Some(_) => {
                    self.error_at(
                        less_than_offset,
                        HtmlParseErrorCode::InvalidFirstCharacterOfTagName,
                    );
                    return Token::Character("</".to_owned());
                }
            }
        }
        if self
            .peek_char()
            .is_some_and(|character| character.is_ascii_alphabetic())
        {
            return self.consume_tag(true);
        }
        if self.peek_char() == Some('?') {
            return self.consume_processing_instruction();
        }
        if self.peek_char().is_none() {
            self.error_at(less_than_offset, HtmlParseErrorCode::EofBeforeTagName);
        } else {
            self.error_at(
                less_than_offset,
                HtmlParseErrorCode::InvalidFirstCharacterOfTagName,
            );
        }
        Token::Character("<".to_owned())
    }

    /// The processing-instruction states, 13.2.5.72 to 13.2.5.76.
    ///
    /// Reached from the tag open state on a `?`, which is the only way into a
    /// processing instruction: `<?` in a raw text or RCDATA element is character
    /// data, and `<?` inside a comment is comment data, so this runs only in the
    /// data state.
    ///
    /// Three of the five states can abandon the token and become a comment, and
    /// when they do the `?` is **prepended** to the comment's data: that is what
    /// "convert the temporary buffer to a comment" means, and it is why
    /// `<?xml version="1.0">` is one comment reading `?xml version="1.0"` and
    /// not a comment reading `xml version="1.0"`.
    fn consume_processing_instruction(&mut self) -> Token {
        // The tag open state consumes the `?` and sets the temporary buffer to
        // the empty string before switching here, so the instruction proper
        // starts at the character after it.
        let open_offset = self.offset;
        self.bump_char();
        // "Processing instruction open state": an ASCII alpha or `_` starts the
        // target; the end of the input stream is an error; anything else makes
        // the instruction a comment.
        match self.peek_char() {
            Some(character) if character.is_ascii_alphabetic() || character == '_' => {}
            None => {
                self.error_at(open_offset, HtmlParseErrorCode::EofInProcessingInstruction);
                return self.emit_eof();
            }
            Some(_) => {
                self.error_at(
                    open_offset,
                    HtmlParseErrorCode::InvalidFirstCharacterOfProcessingInstructionTarget,
                );
                return self.consume_bogus_comment("?");
            }
        }
        // "Processing instruction target state".
        let mut target = String::new();
        loop {
            match self.peek_char() {
                Some(character)
                    if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') =>
                {
                    self.bump_char();
                    target.push(character);
                }
                Some(character)
                    if is_ascii_whitespace(character) || matches!(character, '?' | '>') =>
                {
                    // "If target is an ASCII case-insensitive match for 'xml' or
                    // 'xml-stylesheet': this is a
                    // disallowed-processing-instruction-target parse error.
                    // Convert the temporary buffer to a comment. Reconsume in the
                    // bogus comment state."
                    if target.eq_ignore_ascii_case("xml")
                        || target.eq_ignore_ascii_case("xml-stylesheet")
                    {
                        self.error_at(
                            open_offset,
                            HtmlParseErrorCode::DisallowedProcessingInstructionTarget,
                        );
                        return self.consume_bogus_comment(&format!("?{target}"));
                    }
                    break;
                }
                None => {
                    self.error_at(open_offset, HtmlParseErrorCode::EofInProcessingInstruction);
                    return self.emit_eof();
                }
                Some(_) => {
                    self.error_at(
                        self.offset,
                        HtmlParseErrorCode::InvalidProcessingInstructionTarget,
                    );
                    return self.consume_bogus_comment(&format!("?{target}"));
                }
            }
        }
        // "After processing instruction target state": whitespace is dropped, so
        // the separator between the target and the data is not part of either.
        while self.peek_char().is_some_and(is_ascii_whitespace) {
            self.bump_char();
        }
        // "Processing instruction data state" and its questionable substate: a
        // `?` before a character that is not `>` belongs to the data, and only
        // `>` or the end of the input stream ends the instruction.
        let mut data = String::new();
        loop {
            match self.peek_char() {
                Some('>') => {
                    self.bump_char();
                    return Token::ProcessingInstruction { target, data };
                }
                None => {
                    self.error_at(self.offset, HtmlParseErrorCode::EofInProcessingInstruction);
                    self.emitted_eof = true;
                    self.pending
                        .push(Token::ProcessingInstruction { target, data });
                    return self.next();
                }
                Some('?') => {
                    self.bump_char();
                    if self.peek_char() == Some('>') {
                        self.bump_char();
                        return Token::ProcessingInstruction { target, data };
                    }
                    // "Append U+003F (?) to the data. Reconsume in the
                    // processing instruction data state."
                    data.push('?');
                }
                Some('\0') => {
                    self.bump_char();
                    data.push('\u{fffd}');
                }
                Some(_) => {
                    data.push(self.bump_char().unwrap_or('\0'));
                }
            }
        }
    }

    fn consume_tag(&mut self, start_tag: bool) -> Token {
        let tag_offset = self.offset;
        let mut name = String::new();
        while let Some(character) = self.peek_char() {
            if is_ascii_whitespace(character) || matches!(character, '/' | '>') {
                break;
            }
            self.bump_char();
            if character == '\0' {
                self.error_at(self.offset - 1, HtmlParseErrorCode::UnexpectedNullCharacter);
                name.push('\u{fffd}');
            } else {
                name.push(character.to_ascii_lowercase());
            }
        }
        if name.is_empty() {
            self.error_at(tag_offset, HtmlParseErrorCode::EofBeforeTagName);
        }

        if !start_tag {
            self.skip_ascii_whitespace();
            let mut self_closing = false;
            if self.peek_char() != Some('>') && self.peek_char().is_some() {
                self.error_at(self.offset, HtmlParseErrorCode::EndTagWithAttributes);
                while let Some(character) = self.peek_char() {
                    if character == '>' {
                        break;
                    }
                    if character == '/' {
                        self_closing = true;
                    }
                    self.bump_char();
                }
            }
            if self_closing {
                self.error_at(self.offset, HtmlParseErrorCode::EndTagWithTrailingSolidus);
            }
            if !self.consume_char('>') {
                self.error_at(self.offset, HtmlParseErrorCode::EofInTag);
            }
            return Token::EndTag(TagToken {
                name,
                attributes: Vec::new(),
                self_closing: false,
            });
        }

        let mut attributes = Vec::new();
        let mut self_closing = false;
        // What ended the previous attribute. Only a quoted value can leave the
        // next attribute missing its separator, so this starts as "no previous
        // attribute" and the first attribute never reports anything.
        let mut previous_end = None;
        loop {
            let _ = self.skip_ascii_whitespace();
            match self.peek_char() {
                Some('>') => {
                    self.bump_char();
                    break;
                }
                Some('/') => {
                    self.bump_char();
                    if self.consume_char('>') {
                        self_closing = true;
                        break;
                    }
                    self.error_at(self.offset - 1, HtmlParseErrorCode::UnexpectedSolidusInTag);
                }
                None => {
                    self.error_at(self.offset, HtmlParseErrorCode::EofInTag);
                    break;
                }
                Some(_) => {
                    if let Some(AttributeEnd::AfterQuotedValue(Some(character))) = previous_end
                        && !is_ascii_whitespace(character)
                        && character != '/'
                        && character != '>'
                    {
                        // "After attribute value (quoted) state" (13.2.5.39):
                        // "U+0009, U+000A, U+000C, U+0020: Switch to the before
                        // attribute name state. U+002F: Switch to the self-closing
                        // start tag state. U+003E: Switch to the data state. Emit
                        // the current tag token. EOF: This is an eof-in-tag parse
                        // error. Emit an end-of-file token. Anything else: This is
                        // a missing-whitespace-between-attributes parse error.
                        // Reconsume in the before attribute name state."
                        //
                        // `self.offset` is the character the state consumes, which
                        // is the first character of the following attribute's name:
                        // the error points at the gap, where the separator should
                        // have been, rather than at the attribute after it.
                        self.error_at(
                            self.offset,
                            HtmlParseErrorCode::MissingWhitespaceBetweenAttributes,
                        );
                    }
                    // The offset `consume_attribute` leaves behind is the one
                    // *after* the attribute, which for a tag whose last
                    // attribute is the duplicate is the end of the tag: an
                    // offset there names none of the markup that caused the
                    // error. The offending attribute is the one being consumed
                    // now, so the error is reported where its name starts, the
                    // same place the missing-whitespace error above points.
                    let attribute_start = self.offset;
                    let (attribute, end) = self.consume_attribute();
                    if attributes.iter().any(|existing: &AttributeToken| {
                        existing.name.eq_ignore_ascii_case(&attribute.name)
                    }) {
                        self.error_at(attribute_start, HtmlParseErrorCode::DuplicateAttribute);
                    } else {
                        attributes.push(attribute);
                    }
                    previous_end = Some(end);
                }
            }
        }

        Token::StartTag(TagToken {
            name,
            attributes,
            self_closing,
        })
    }

    /// Consume one attribute, and report what ended it.
    ///
    /// The second value is the whole point of splitting the return: whether the
    /// next attribute is missing its separator depends on **how this attribute
    /// ended**, not on whether whitespace happens to be left in the stream. This
    /// function skips the whitespace that follows an attribute name — 13.2.5.34
    /// moves to the after attribute value state on whitespace, which ignores it —
    /// so by the time the caller looks at the stream the separator is already
    /// gone and cannot be observed from there.
    fn consume_attribute(&mut self) -> (AttributeToken, AttributeEnd) {
        let mut name = String::new();
        if self.peek_char() == Some('=') {
            self.error_at(
                self.offset,
                HtmlParseErrorCode::UnexpectedEqualsSignBeforeAttributeName,
            );
            self.bump_char();
            name.push('=');
        }
        while let Some(character) = self.peek_char() {
            if is_ascii_whitespace(character) || matches!(character, '/' | '>' | '=') {
                break;
            }
            self.bump_char();
            match character {
                '\0' => {
                    self.error_at(self.offset - 1, HtmlParseErrorCode::UnexpectedNullCharacter);
                    name.push('\u{fffd}');
                }
                '"' | '\'' | '<' => {
                    self.error_at(
                        self.offset - 1,
                        HtmlParseErrorCode::UnexpectedCharacterInAttributeName,
                    );
                    name.push(character.to_ascii_lowercase());
                }
                _ => name.push(character.to_ascii_lowercase()),
            }
        }
        self.skip_ascii_whitespace();
        if self.consume_char('=') {
            self.skip_ascii_whitespace();
            let (value, quoted) = self.consume_attribute_value();
            let end = if quoted {
                // A quoted value's closing quote has just been consumed, so the
                // next input character is the one the "after attribute value
                // (quoted)" state would consume.
                AttributeEnd::AfterQuotedValue(self.peek_char())
            } else {
                // An unquoted value absorbs everything that is neither ASCII
                // whitespace nor `>` (13.2.5.38), so the only characters it can
                // end on are ones for which that state has an explicit case and
                // reports no error at all.
                AttributeEnd::AfterPlainValue
            };
            (AttributeToken { name, value }, end)
        } else {
            // A value-less attribute can only be ended by ASCII whitespace, `/`,
            // `>`, or `=`, and none of the states reachable from a value-less
            // attribute reports missing-whitespace-between-attributes. So nothing
            // about this end can make the next attribute look unseparated, and
            // the character is not recorded.
            (
                AttributeToken {
                    name,
                    value: String::new(),
                },
                AttributeEnd::AfterPlainValue,
            )
        }
    }

    fn consume_attribute_value(&mut self) -> (String, bool) {
        let quote = match self.peek_char() {
            Some('"' | '\'') => self.bump_char(),
            Some('>') | None => {
                self.error_at(self.offset, HtmlParseErrorCode::MissingAttributeValue);
                return (String::new(), false);
            }
            _ => None,
        };
        let mut value = String::new();
        loop {
            let Some(character) = self.peek_char() else {
                self.error_at(self.offset, HtmlParseErrorCode::EofInTag);
                break;
            };
            if quote == Some(character) {
                self.bump_char();
                break;
            }
            if quote.is_none() && (is_ascii_whitespace(character) || character == '>') {
                break;
            }
            if character == '&' {
                if let Some(decoded) = self.consume_character_reference(true, quote) {
                    value.push_str(&decoded);
                } else {
                    value.push('&');
                }
                continue;
            }
            self.bump_char();
            match character {
                '\0' => {
                    self.error_at(self.offset - 1, HtmlParseErrorCode::UnexpectedNullCharacter);
                    value.push('\u{fffd}');
                }
                '"' | '\'' | '<' | '=' | '`' if quote.is_none() => {
                    self.error_at(
                        self.offset - 1,
                        HtmlParseErrorCode::UnexpectedCharacterInUnquotedAttributeValue,
                    );
                    value.push(character);
                }
                _ => value.push(character),
            }
        }
        (value, quote.is_some())
    }

    fn consume_comment(&mut self) -> Token {
        let start = self.offset;
        if self.consume_char('>') {
            self.error_at(start, HtmlParseErrorCode::AbruptClosingOfEmptyComment);
            return Token::Comment(String::new());
        }
        Token::Comment(self.consume_comment_body())
    }

    /// The comment body of a comment whose opening has already been consumed:
    /// everything up to the `-->` that closes it, or up to the end of the input
    /// stream, which is an `eof-in-comment` parse error.
    fn consume_comment_body(&mut self) -> String {
        if let Some(relative_end) = self.remaining().find("-->") {
            let end = self.offset + relative_end;
            let data = self.input[self.offset..end].replace('\0', "\u{fffd}");
            if self.input[self.offset..end].contains('\0') {
                self.error_at(self.offset, HtmlParseErrorCode::UnexpectedNullCharacter);
            }
            self.offset = end + 3;
            return data;
        }
        let data = self.remaining().replace('\0', "\u{fffd}");
        if self.remaining().contains('\0') {
            self.error_at(self.offset, HtmlParseErrorCode::UnexpectedNullCharacter);
        }
        self.offset = self.input.len();
        self.error_at(self.offset, HtmlParseErrorCode::EofInComment);
        data
    }

    /// "Bogus comment state" (13.2.5.41), entered with `initial` already in the
    /// comment token's data.
    ///
    /// Only two things end this state: a `>` closes the empty comment, and the
    /// end of the input stream closes whatever there is. **Anything else is
    /// appended to the comment's data and the state changes to the comment
    /// state**, so the comment is ended by the next `-->`, not by the next `>`.
    ///
    /// That distinction is the whole state. Scanning to the next `>` instead
    /// ends the comment early, and on real markup that misreads the rest of the
    /// declaration: a bare `<![CDATA[ ... ]]>` in a document, which is what a
    /// page with a missing `<script>` start tag emits, loses everything up to the
    /// first `>` and then reports the remainder as page content.
    /// "Bogus comment state" (13.2.5.41), entered with `initial` already in the
    /// comment token's data.
    ///
    /// The state ends on a `>` or on the end of the input stream, and **anything
    /// else is appended to the comment's data with the state left where it is**:
    /// the character is not handed on, so the comment ends at the next `>` and
    /// not at the next `-->`.
    ///
    /// That is the whole state, and it is why the closing `>` is not part of the
    /// data. An earlier revision of the standard sent everything else to the
    /// comment state instead, so a declaration such as `<![CDATA[ ... ]]>` used
    /// to run to the next `-->`; on a page with a missing `<script>` start tag
    /// that turned a 92-character comment into one swallowing the rest of the
    /// document. The current specification reversed that, and the
    /// tree-construction suite agrees with it.
    fn consume_bogus_comment(&mut self, initial: &str) -> Token {
        let start = self.offset;
        if self.consume_char('>') {
            self.error_at(start, HtmlParseErrorCode::AbruptClosingOfEmptyComment);
            return Token::Comment(initial.to_owned());
        }
        let mut data = String::from(initial);
        loop {
            let Some(character) = self.bump_char() else {
                self.error_at(start, HtmlParseErrorCode::EofInComment);
                return Token::Comment(data);
            };
            if character == '>' {
                return Token::Comment(data);
            }
            if character == '\0' {
                self.error_at(self.offset - 1, HtmlParseErrorCode::UnexpectedNullCharacter);
                data.push('\u{fffd}');
            } else {
                data.push(character);
            }
        }
    }

    /// "Markup declaration open state": the input starts with `[CDATA[`.
    ///
    /// If the tree builder reports that the adjusted current node is not an
    /// element in the HTML namespace, consume the seven characters and switch
    /// to the CDATA section state. Otherwise this is a `cdata-in-html-content`
    /// parse error: create a comment token whose data is the string `[CDATA[`
    /// and switch to the bogus comment state, which continues the same token.
    fn consume_cdata_start(&mut self, less_than_offset: usize) -> Token {
        self.offset += "[CDATA[".len();
        if self.allow_cdata {
            self.content_model = ContentModel::Cdata;
            return self.next();
        }
        self.error_at(less_than_offset, HtmlParseErrorCode::CdataInHtmlContent);
        // The bogus comment state continues the token the markup declaration
        // open state created, so `[CDATA[` is the comment's initial data rather
        // than a comment of its own. Emitting it separately would put two
        // adjacent comment nodes in the tree where the standard's one comment
        // token belongs.
        self.consume_bogus_comment("[CDATA[")
    }

    /// "CDATA section state" (13.2.5.69) and its bracket and end substates.
    ///
    /// Every input character there becomes a character token, so a run up to
    /// the closing `]]>` is collected into a single token: the tree builder
    /// inserts all of a character token's characters at the current node
    /// anyway. U+0000 NULL is emitted as-is because the replacement with
    /// U+FFFD REPLACEMENT CHARACTER belongs to the rules for parsing tokens in
    /// foreign content.
    ///
    /// The `]]>` delimiter is not character data, and a run of more than two
    /// U+005D characters yields all but the final two as data, so the run is
    /// counted rather than scanned for.
    fn next_cdata_token(&mut self) -> Token {
        let mut data = String::new();
        // U+005D characters consumed since the last emitted character, still
        // owed as character data.
        let mut owed_brackets = 0_usize;
        loop {
            let Some(character) = self.peek_char() else {
                if data.is_empty() && owed_brackets == 0 {
                    // "This is an eof-in-cdata parse error. Emit an end-of-file
                    // token."
                    self.content_model = ContentModel::Data;
                    self.error_at(self.offset, HtmlParseErrorCode::EofInCdata);
                    return self.emit_eof();
                }
                // The characters collected so far are a token of their own, so
                // the end-of-file error is reported by the next call.
                data.push_str(&"]".repeat(owed_brackets));
                return Token::Cdata(data);
            };
            if character == ']' {
                self.bump_char();
                owed_brackets += 1;
                if owed_brackets >= 2 && self.peek_char() == Some('>') {
                    // "U+003E GREATER-THAN SIGN: Switch to the data state."
                    data.push_str(&"]".repeat(owed_brackets - 2));
                    self.bump_char();
                    self.content_model = ContentModel::Data;
                    return Token::Cdata(data);
                }
                continue;
            }
            data.push_str(&"]".repeat(owed_brackets));
            owed_brackets = 0;
            self.bump_char();
            data.push(character);
        }
    }

    fn consume_doctype(&mut self) -> Token {
        let mut token = DoctypeToken {
            name: None,
            public_id: None,
            system_id: None,
            force_quirks: false,
        };
        if !self.skip_ascii_whitespace() {
            self.error_at(
                self.offset,
                HtmlParseErrorCode::MissingWhitespaceBeforeDoctypeName,
            );
        }
        if self.peek_char() == Some('>') {
            self.bump_char();
            token.force_quirks = true;
            self.error_at(self.offset, HtmlParseErrorCode::MissingDoctypeName);
            return Token::Doctype(token);
        }
        if self.peek_char().is_none() {
            token.force_quirks = true;
            self.error_at(self.offset, HtmlParseErrorCode::EofInDoctype);
            return Token::Doctype(token);
        }

        let mut name = String::new();
        while let Some(character) = self.peek_char() {
            if is_ascii_whitespace(character) || character == '>' {
                break;
            }
            self.bump_char();
            if character == '\0' {
                self.error_at(self.offset - 1, HtmlParseErrorCode::UnexpectedNullCharacter);
                name.push('\u{fffd}');
            } else {
                name.push(character.to_ascii_lowercase());
            }
        }
        token.name = Some(name);
        self.skip_ascii_whitespace();
        if self.consume_char('>') {
            return Token::Doctype(token);
        }
        if self.remaining_starts_ascii_case_insensitive("public") {
            self.offset += "public".len();
            if !self.skip_ascii_whitespace() {
                token.force_quirks = true;
                self.error_at(
                    self.offset,
                    HtmlParseErrorCode::InvalidCharacterSequenceAfterDoctypeName,
                );
            }
            token.public_id = self.consume_doctype_identifier(&mut token.force_quirks);
            self.skip_ascii_whitespace();
            if matches!(self.peek_char(), Some('"' | '\'')) {
                token.system_id = self.consume_doctype_identifier(&mut token.force_quirks);
            }
        } else if self.remaining_starts_ascii_case_insensitive("system") {
            self.offset += "system".len();
            if !self.skip_ascii_whitespace() {
                token.force_quirks = true;
                self.error_at(
                    self.offset,
                    HtmlParseErrorCode::InvalidCharacterSequenceAfterDoctypeName,
                );
            }
            token.system_id = self.consume_doctype_identifier(&mut token.force_quirks);
        } else {
            token.force_quirks = true;
            self.error_at(
                self.offset,
                HtmlParseErrorCode::InvalidCharacterSequenceAfterDoctypeName,
            );
        }
        while let Some(character) = self.peek_char() {
            self.bump_char();
            if character == '>' {
                break;
            }
        }
        if self.offset >= self.input.len() && !self.input.ends_with('>') {
            token.force_quirks = true;
            self.error_at(self.offset, HtmlParseErrorCode::EofInDoctype);
        }
        Token::Doctype(token)
    }

    fn consume_doctype_identifier(&mut self, force_quirks: &mut bool) -> Option<String> {
        let Some(quote @ ('"' | '\'')) = self.peek_char() else {
            *force_quirks = true;
            self.error_at(
                self.offset,
                HtmlParseErrorCode::InvalidCharacterSequenceAfterDoctypeName,
            );
            return None;
        };
        self.bump_char();
        let mut value = String::new();
        while let Some(character) = self.peek_char() {
            self.bump_char();
            if character == quote {
                return Some(value);
            }
            if character == '\0' {
                self.error_at(self.offset - 1, HtmlParseErrorCode::UnexpectedNullCharacter);
                value.push('\u{fffd}');
            } else {
                value.push(character);
            }
        }
        *force_quirks = true;
        self.error_at(self.offset, HtmlParseErrorCode::EofInDoctype);
        Some(value)
    }

    fn consume_character_reference(
        &mut self,
        in_attribute: bool,
        additional_allowed: Option<char>,
    ) -> Option<String> {
        let ampersand_offset = self.offset;
        if !self.consume_char('&') {
            return None;
        }
        if self.peek_char().is_none_or(|character| {
            is_ascii_whitespace(character)
                || matches!(character, '<' | '&')
                || additional_allowed == Some(character)
        }) {
            return None;
        }
        if self.consume_char('#') {
            return self.consume_numeric_character_reference(ampersand_offset);
        }

        let after_ampersand = self.offset;
        let map = named_entities();
        let remaining = self.remaining();
        let max_len = remaining.len().min(max_named_entity_len());
        for length in (1..=max_len).rev() {
            let Some(candidate) = remaining.get(..length) else {
                continue;
            };
            let Some(value) = map.get(candidate) else {
                continue;
            };
            let has_semicolon = candidate.ends_with(';');
            let next = remaining
                .get(length..)
                .and_then(|suffix| suffix.chars().next());
            if in_attribute
                && !has_semicolon
                && next
                    .is_some_and(|character| character.is_ascii_alphanumeric() || character == '=')
            {
                self.offset = after_ampersand;
                return None;
            }
            self.offset += length;
            if !has_semicolon {
                self.error_at(
                    ampersand_offset,
                    HtmlParseErrorCode::MissingSemicolonAfterCharacterReference,
                );
            }
            return Some((*value).to_owned());
        }

        if self
            .peek_char()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        {
            self.error_at(
                ampersand_offset,
                HtmlParseErrorCode::UnknownNamedCharacterReference,
            );
        }
        self.offset = after_ampersand;
        None
    }

    fn consume_numeric_character_reference(&mut self, ampersand_offset: usize) -> Option<String> {
        let hexadecimal = matches!(self.peek_char(), Some('x' | 'X'));
        if hexadecimal {
            self.bump_char();
        }
        let digits_start = self.offset;
        while self.peek_char().is_some_and(|character| {
            if hexadecimal {
                character.is_ascii_hexdigit()
            } else {
                character.is_ascii_digit()
            }
        }) {
            self.bump_char();
        }
        if self.offset == digits_start {
            self.error_at(
                ampersand_offset,
                HtmlParseErrorCode::AbsenceOfDigitsInNumericCharacterReference,
            );
            self.offset = ampersand_offset + 1;
            return None;
        }
        let digits = &self.input[digits_start..self.offset];
        let value =
            u32::from_str_radix(digits, if hexadecimal { 16 } else { 10 }).unwrap_or(u32::MAX);
        if !self.consume_char(';') {
            self.error_at(
                ampersand_offset,
                HtmlParseErrorCode::MissingSemicolonAfterCharacterReference,
            );
        }
        let scalar = sanitize_numeric_reference(value, ampersand_offset, &mut self.errors);
        Some(scalar.to_string())
    }

    fn is_appropriate_end_tag_at_current_offset(&self) -> bool {
        let Some(name) = &self.appropriate_end_tag else {
            return false;
        };
        let remaining = self.remaining();
        if !remaining.starts_with("</") {
            return false;
        }
        let after_open = &remaining[2..];
        let Some(candidate_name) = after_open.get(..name.len()) else {
            return false;
        };
        if !candidate_name.eq_ignore_ascii_case(name) {
            return false;
        }
        after_open[name.len()..]
            .chars()
            .next()
            .is_none_or(|character| {
                is_ascii_whitespace(character) || matches!(character, '/' | '>')
            })
    }

    fn remaining_starts_ascii_case_insensitive(&self, expected: &str) -> bool {
        self.remaining()
            .get(..expected.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(expected))
    }

    fn skip_ascii_whitespace(&mut self) -> bool {
        let start = self.offset;
        while self.peek_char().is_some_and(is_ascii_whitespace) {
            self.bump_char();
        }
        self.offset > start
    }

    fn peek_char(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn bump_char(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.offset += character.len_utf8();
        Some(character)
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.bump_char();
            true
        } else {
            false
        }
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.offset..]
    }

    fn error_at(&mut self, offset: usize, code: HtmlParseErrorCode) {
        self.errors.push(HtmlParseError { offset, code });
    }

    fn emit_eof(&mut self) -> Token {
        self.emitted_eof = true;
        Token::Eof
    }
}

const fn is_ascii_whitespace(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\u{000c}' | '\r' | ' ')
}

fn named_entities() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        ENTITIES
            .iter()
            .filter_map(|entity| {
                entity
                    .entity
                    .strip_prefix('&')
                    .map(|name| (name, entity.characters))
            })
            .collect()
    })
}

fn max_named_entity_len() -> usize {
    static MAX: OnceLock<usize> = OnceLock::new();
    *MAX.get_or_init(|| {
        ENTITIES
            .iter()
            .filter_map(|entity| entity.entity.len().checked_sub(1))
            .max()
            .unwrap_or(0)
    })
}

fn sanitize_numeric_reference(value: u32, offset: usize, errors: &mut Vec<HtmlParseError>) -> char {
    let mut error = |code| errors.push(HtmlParseError { offset, code });
    if value == 0 {
        error(HtmlParseErrorCode::NullCharacterReference);
        return '\u{fffd}';
    }
    if value > 0x10_ffff {
        error(HtmlParseErrorCode::CharacterReferenceOutsideUnicodeRange);
        return '\u{fffd}';
    }
    if (0xd800..=0xdfff).contains(&value) {
        error(HtmlParseErrorCode::SurrogateCharacterReference);
        return '\u{fffd}';
    }
    if is_noncharacter(value) {
        error(HtmlParseErrorCode::NoncharacterCharacterReference);
    }
    let mapped = match value {
        0x80 => 0x20ac,
        0x82 => 0x201a,
        0x83 => 0x0192,
        0x84 => 0x201e,
        0x85 => 0x2026,
        0x86 => 0x2020,
        0x87 => 0x2021,
        0x88 => 0x02c6,
        0x89 => 0x2030,
        0x8a => 0x0160,
        0x8b => 0x2039,
        0x8c => 0x0152,
        0x8e => 0x017d,
        0x91 => 0x2018,
        0x92 => 0x2019,
        0x93 => 0x201c,
        0x94 => 0x201d,
        0x95 => 0x2022,
        0x96 => 0x2013,
        0x97 => 0x2014,
        0x98 => 0x02dc,
        0x99 => 0x2122,
        0x9a => 0x0161,
        0x9b => 0x203a,
        0x9c => 0x0153,
        0x9e => 0x017e,
        0x9f => 0x0178,
        _ => value,
    };
    if mapped != value || is_control(value) {
        error(HtmlParseErrorCode::ControlCharacterReference);
    }
    char::from_u32(mapped).unwrap_or('\u{fffd}')
}

const fn is_noncharacter(value: u32) -> bool {
    (value >= 0xfdd0 && value <= 0xfdef) || (value & 0xffff == 0xfffe) || (value & 0xffff == 0xffff)
}

const fn is_control(value: u32) -> bool {
    (value >= 0x0001 && value <= 0x0008)
        || value == 0x000b
        || (value >= 0x000d && value <= 0x001f)
        || (value >= 0x007f && value <= 0x009f)
}

#[cfg(test)]
mod tests {
    use super::{ContentModel, HtmlParseErrorCode, TagToken, Token, Tokenizer};

    fn tokenize(input: &str) -> (Vec<Token>, Vec<HtmlParseErrorCode>) {
        let mut tokenizer = Tokenizer::new(input);
        let mut tokens = Vec::new();
        loop {
            let token = tokenizer.next();
            let eof = token == Token::Eof;
            tokens.push(token);
            if eof {
                break;
            }
        }
        let errors = tokenizer.errors().iter().map(|error| error.code).collect();
        (tokens, errors)
    }

    #[test]
    fn a_named_reference_without_a_semicolon_is_reported() {
        // "This error occurs if the parser encounters a character reference that
        // is not terminated by a U+003B (;) code point. The parser behaves the
        // same as if the character reference is terminated by the U+003B (;) code
        // point." (13.2.2, missing-semicolon-after-character-reference)
        //
        // The suite's `entities01.dat` case for `FOO&gtBAR` lists this under both
        // its legacy name `named-entity-without-semicolon` and its current one,
        // which is the corpus spelling one condition two ways rather than
        // reporting it twice.
        let (tokens, errors) = tokenize("FOO&gtBAR");
        assert_eq!(
            errors,
            vec![HtmlParseErrorCode::MissingSemicolonAfterCharacterReference],
            "one reference, one diagnostic: {errors:?}"
        );
        // "The parser behaves the same as if the character reference is
        // terminated by the U+003B (;)", so the reference resolves rather than
        // being left in the input. The tokenizer emits the run before and after
        // it as separate character tokens; "When the steps below require the
        // user agent to insert a character" (13.2.6.1) appends to the previous
        // text node, so the *tree* sees one `FOO>BAR`. That merge is the tree
        // builder's, not this test's, so only the token stream is asserted here.
        assert_eq!(
            tokens[0..3],
            [
                Token::Character("FOO".to_owned()),
                Token::Character(">".to_owned()),
                Token::Character("BAR".to_owned()),
            ]
        );
        // The semicolon-terminated form is the same text with no diagnostic,
        // which is what makes the first assertion about the reference and not
        // about the document.
        let (_, terminated) = tokenize("FOO&gt;BAR");
        assert!(terminated.is_empty(), "a terminated reference is silent");
    }

    #[test]
    fn tokenizes_tags_attributes_and_first_duplicate_wins() {
        let (tokens, errors) = tokenize("<DIV ID=first id='second' disabled></DIV>");
        let Token::StartTag(TagToken {
            name, attributes, ..
        }) = &tokens[0]
        else {
            panic!("expected start tag");
        };
        assert_eq!(name, "div");
        assert_eq!(attributes.len(), 2);
        assert_eq!(attributes[0].name, "id");
        assert_eq!(attributes[0].value, "first");
        assert_eq!(attributes[1].name, "disabled");
        assert_eq!(errors, vec![HtmlParseErrorCode::DuplicateAttribute]);
    }

    /// A diagnostic has to name the markup that caused it, not the markup after
    /// it. A duplicate is the last attribute of its tag often enough that
    /// reporting the offset the tokenizer has reached puts the error past the
    /// end of the tag, where it names nothing at all — which is the failure this
    /// pins. The shape is one a server-rendered page emits when a template
    /// appends an attribute to a tag that already has one.
    #[test]
    fn a_duplicate_attribute_is_reported_at_the_duplicate() {
        let input = "<a class=\"icon\" class=\"/article/1\" title=\"t\">";
        let mut tokenizer = Tokenizer::new(input);
        let _ = tokenizer.next();
        let error = tokenizer
            .errors()
            .iter()
            .find(|error| error.code == HtmlParseErrorCode::DuplicateAttribute)
            .expect("a duplicate attribute error");
        // The offset is where the second `class` starts, so the markup from
        // there on is the attribute that was thrown away.
        assert_eq!(&input[error.offset..error.offset + 5], "class");
        assert_eq!(&input[error.offset..], "class=\"/article/1\" title=\"t\">");

        // The offset is the attribute's own, not the end of the tag: with the
        // duplicate last, the end of the tag is a different offset entirely.
        assert_ne!(error.offset, input.len() - 1);
    }

    #[test]
    fn equals_sign_before_attribute_name_is_preserved_by_error_recovery() {
        let (tokens, errors) = tokenize("<div =foo>");
        let Token::StartTag(tag) = &tokens[0] else {
            panic!("expected start tag");
        };
        assert_eq!(tag.attributes[0].name, "=foo");
        assert_eq!(tag.attributes[0].value, "");
        assert!(errors.contains(&HtmlParseErrorCode::UnexpectedEqualsSignBeforeAttributeName));
    }

    #[test]
    fn decodes_named_numeric_and_legacy_control_references() {
        let (tokens, errors) = tokenize("&copy; &#x1f642; &#128; &NotEqualTilde;");
        let text: String = tokens
            .iter()
            .filter_map(|token| match token {
                Token::Character(value) => Some(value.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "© 🙂 € ≂̸");
        assert!(errors.contains(&HtmlParseErrorCode::ControlCharacterReference));
    }

    #[test]
    fn attribute_legacy_reference_obeys_ambiguous_ampersand_rule() {
        let (tokens, _) = tokenize("<a x='&copycat' y='&copy;'>");
        let Token::StartTag(tag) = &tokens[0] else {
            panic!("expected start tag");
        };
        assert_eq!(tag.attributes[0].value, "&copycat");
        assert_eq!(tag.attributes[1].value, "©");
    }

    #[test]
    fn tokenizes_comments_and_doctype_identifiers() {
        let (tokens, errors) = tokenize(
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"legacy.dtd\"><!--ok-->",
        );
        let Token::Doctype(doctype) = &tokens[0] else {
            panic!("expected doctype");
        };
        assert_eq!(doctype.name.as_deref(), Some("html"));
        assert_eq!(
            doctype.public_id.as_deref(),
            Some("-//W3C//DTD HTML 4.01//EN")
        );
        assert_eq!(doctype.system_id.as_deref(), Some("legacy.dtd"));
        assert_eq!(tokens[1], Token::Comment("ok".to_owned()));
        assert!(errors.is_empty());
    }

    #[test]
    fn raw_text_only_recognizes_the_appropriate_end_tag() {
        let mut tokenizer = Tokenizer::new("a<div>b</script>tail");
        tokenizer.switch_to(ContentModel::ScriptData, Some("script"));
        assert_eq!(tokenizer.next(), Token::Character("a<div>b".to_owned()));
        let Token::EndTag(tag) = tokenizer.next() else {
            panic!("expected script end tag");
        };
        assert_eq!(tag.name, "script");
        assert_eq!(tokenizer.next(), Token::Character("tail".to_owned()));
    }

    #[test]
    fn script_data_html_like_comments_nest_a_second_script_region() {
        // `<!--<script ` inside a script switches the tokenizer into the "double
        // escaped" states, in which `</script>` is *text*. A `</script>` after
        // the region's own `-->` is what actually closes the element.
        //
        // This is the shape a real page has when a `//<![CDATA[ ... //]]>` script
        // wrapper loses its opening tag, and treating the first `</script>` as
        // the end of the element is what makes such a page come out as a single
        // enormous script element.
        let mut tokenizer = Tokenizer::new("<!--<script </script><p>x</script>tail");
        tokenizer.switch_to(ContentModel::ScriptData, Some("script"));
        let mut text = String::new();
        loop {
            match tokenizer.next() {
                Token::Character(value) => text.push_str(&value),
                Token::EndTag(tag) => {
                    assert_eq!(tag.name, "script");
                    break;
                }
                token => panic!("unexpected token: {token:?}"),
            }
        }
        assert_eq!(text, "<!--<script </script><p>x");
        assert_eq!(tokenizer.next(), Token::Character("tail".to_owned()));
    }

    #[test]
    fn script_data_does_not_lose_a_truncated_end_tag() {
        // A `</script` that the input stream ends in the middle of is not an end
        // tag: the end tag name state's "anything else" clause turns the
        // half-built name into character data, so the text of the script is
        // `</script` and the element is still open at the end of the input.
        for (input, expected) in [
            ("</script", "</script"),
            ("</scriptX", "</scriptX"),
            ("<", "<"),
            ("</", "</"),
        ] {
            let mut tokenizer = Tokenizer::new(input);
            tokenizer.switch_to(ContentModel::ScriptData, Some("script"));
            assert_eq!(
                tokenizer.next(),
                Token::Character(expected.to_owned()),
                "{input}"
            );
            let (tokens, errors) = tokenize_script(input);
            assert_eq!(
                tokens,
                vec![Token::Character(expected.to_owned()), Token::Eof],
                "{input}"
            );
            // A truncated end tag is a parse error however it runs out: the end
            // tag name state's "anything else" clause is
            // `invalid-first-character-of-tag-name` for `</scriptX` and the
            // ordinary end-of-input-stream error for `</script`, whose last
            // character *is* a valid tag name character.
            assert!(!errors.is_empty(), "{input} reports a parse error");
        }
    }

    #[test]
    fn an_end_tag_that_is_not_the_right_one_stays_script_data() {
        // `</div>` in script data is not an error and does not close the element:
        // the name is emitted as character data, characters as written.
        let mut tokenizer = Tokenizer::new("a</div>b</script>");
        tokenizer.switch_to(ContentModel::ScriptData, Some("script"));
        assert_eq!(tokenizer.next(), Token::Character("a</div>b".to_owned()));
        let Token::EndTag(tag) = tokenizer.next() else {
            panic!("expected script end tag");
        };
        assert_eq!(tag.name, "script");
    }

    fn tokenize_script(input: &str) -> (Vec<Token>, Vec<HtmlParseErrorCode>) {
        let mut tokenizer = Tokenizer::new(input);
        tokenizer.switch_to(ContentModel::ScriptData, Some("script"));
        let mut tokens = Vec::new();
        loop {
            let token = tokenizer.next();
            let eof = token == Token::Eof;
            tokens.push(token);
            if eof {
                break;
            }
        }
        let errors = tokenizer.errors().iter().map(|error| error.code).collect();
        (tokens, errors)
    }

    #[test]
    fn rcdata_decodes_entities_but_preserves_markup() {
        let mut tokenizer = Tokenizer::new("a&amp;<b></textarea>");
        tokenizer.switch_to(ContentModel::Rcdata, Some("textarea"));
        let mut text = String::new();
        loop {
            match tokenizer.next() {
                Token::Character(value) => text.push_str(&value),
                Token::EndTag(tag) => {
                    assert_eq!(tag.name, "textarea");
                    break;
                }
                token => panic!("unexpected token: {token:?}"),
            }
        }
        assert_eq!(text, "a&<b>");
    }

    #[test]
    fn malformed_input_reports_errors_without_dropping_literal_text() {
        let (tokens, errors) = tokenize("a<1 b&#0;");
        let text: String = tokens
            .iter()
            .filter_map(|token| match token {
                Token::Character(value) => Some(value.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "a<1 b�");
        assert!(errors.contains(&HtmlParseErrorCode::InvalidFirstCharacterOfTagName));
        assert!(errors.contains(&HtmlParseErrorCode::NullCharacterReference));
    }

    fn tokenize_with_cdata(input: &str) -> (Vec<Token>, Vec<HtmlParseErrorCode>) {
        let mut tokenizer = Tokenizer::new(input);
        tokenizer.set_allow_cdata(true);
        let mut tokens = Vec::new();
        loop {
            let token = tokenizer.next();
            let eof = token == Token::Eof;
            tokens.push(token);
            if eof {
                break;
            }
        }
        let errors = tokenizer.errors().iter().map(|error| error.code).collect();
        (tokens, errors)
    }

    #[test]
    fn a_cdata_section_is_character_data_outside_the_html_namespace() {
        let (tokens, errors) = tokenize_with_cdata("<text><![CDATA[a<b&c]]>d");
        assert!(matches!(&tokens[0], Token::StartTag(tag) if tag.name == "text"));
        assert_eq!(tokens[1], Token::Cdata("a<b&c".to_owned()));
        // The section ends at the data state, so what follows is ordinary
        // character data with character references.
        assert_eq!(tokens[2], Token::Character("d".to_owned()));
        assert!(errors.is_empty());
    }

    #[test]
    fn a_cdata_section_in_the_html_namespace_is_one_bogus_comment() {
        // The markup declaration open state creates one comment token with the
        // data `[CDATA[` and the bogus comment state continues it, so the whole
        // declaration is a single comment. The bogus comment state ends on the
        // `>` of `]]>`, and that `>` is not part of the data, so `tail` after it
        // is ordinary character data again.
        let (tokens, errors) = tokenize("<![CDATA[x]]>tail");
        assert_eq!(tokens[0], Token::Comment("[CDATA[x]]".to_owned()));
        assert_eq!(tokens[1], Token::Character("tail".to_owned()));
        assert_eq!(tokens[2], Token::Eof);
        assert_eq!(errors, vec![HtmlParseErrorCode::CdataInHtmlContent]);
    }

    #[test]
    fn a_bogus_comment_ends_at_the_next_greater_than_sign() {
        // The bogus comment state appends every character to the comment's data
        // and stays in that state, so the comment ends at the first `>` after it
        // and the `>` is not part of the data (13.2.5.41).
        //
        // This test previously asserted the opposite rule — that a bogus comment
        // is ended by `-->` and not by `>` — which was the behaviour of a
        // superseded revision of the standard and is wrong against both the
        // current text and the html5lib tree-construction suite. The change was
        // forced by conformance evidence, not chosen.
        let (tokens, errors) = tokenize("<![CDATA[a>b]]>c-->d");
        assert_eq!(
            tokens,
            vec![
                Token::Comment("[CDATA[a".to_owned()),
                Token::Character("b]]>c-->d".to_owned()),
                Token::Eof,
            ]
        );
        assert_eq!(errors, vec![HtmlParseErrorCode::CdataInHtmlContent]);
        // The same rule for the other two ways into the state: a `<!` that is
        // neither a comment nor a DOCTYPE, which is the standard's own example of
        // an XML markup declaration written in HTML, and a `<?` whose target does
        // not start with an ASCII alpha or `_`, which the processing instruction
        // open state converts into a comment. The `?` is the first character of
        // the comment's data in both, and the closing `>` is in neither.
        for (input, data) in [
            ("<!ELEMENT br EMPTY>x", "ELEMENT br EMPTY"),
            ("<?-1>x", "?-1"),
        ] {
            let (tokens, errors) = tokenize(input);
            assert_eq!(
                tokens,
                vec![
                    Token::Comment(data.to_owned()),
                    Token::Character("x".to_owned()),
                    Token::Eof,
                ],
                "{input}"
            );
            assert!(!errors.is_empty(), "{input} reports a parse error");
        }
        // A `<?` whose target *is* well formed is a processing instruction, not a
        // comment. The space before the closing `?` is data: the questionable
        // state consumes the `?` without appending it, and its `>` ends the
        // token without adding anything either.
        let (tokens, _) = tokenize("<?php echo 1; ?>x");
        assert_eq!(
            tokens,
            vec![
                Token::ProcessingInstruction {
                    target: "php".to_owned(),
                    data: "echo 1; ".to_owned(),
                },
                Token::Character("x".to_owned()),
                Token::Eof,
            ]
        );
        // A `>` immediately after `<!` closes an empty comment, which is the one
        // place the state ends on a greater-than sign. Reaching the state at all
        // is itself the `incorrectly-opened-comment` error.
        let (tokens, errors) = tokenize("<!>");
        assert_eq!(tokens[0], Token::Comment(String::new()));
        assert_eq!(
            errors,
            vec![
                HtmlParseErrorCode::IncorrectlyOpenedComment,
                HtmlParseErrorCode::AbruptClosingOfEmptyComment,
            ]
        );
        // A bare `<!` at the end of the input stream is a comment that ran out.
        let (tokens, errors) = tokenize("<!");
        assert_eq!(tokens, vec![Token::Comment(String::new()), Token::Eof]);
        assert_eq!(
            errors,
            vec![
                HtmlParseErrorCode::IncorrectlyOpenedComment,
                HtmlParseErrorCode::EofInComment,
            ]
        );
    }

    #[test]
    fn cdata_bracket_runs_follow_the_bracket_and_end_states() {
        // Two brackets followed by '>' end the section; a surplus bracket in the
        // run is character data, and a single bracket is not a delimiter. An
        // unterminated run runs into the end of the input stream.
        for (input, expected, expected_errors) in [
            (
                "<![CDATA[]]>x",
                vec![
                    Token::Cdata(String::new()),
                    Token::Character("x".into()),
                    Token::Eof,
                ],
                vec![],
            ),
            (
                "<![CDATA[]]]>x",
                vec![
                    Token::Cdata("]".into()),
                    Token::Character("x".into()),
                    Token::Eof,
                ],
                vec![],
            ),
            (
                "<![CDATA[]]]]>x",
                vec![
                    Token::Cdata("]]".into()),
                    Token::Character("x".into()),
                    Token::Eof,
                ],
                vec![],
            ),
            (
                "<![CDATA[]]x",
                vec![Token::Cdata("]]x".into()), Token::Eof],
                vec![HtmlParseErrorCode::EofInCdata],
            ),
            (
                "<![CDATA[>x",
                vec![Token::Cdata(">x".into()), Token::Eof],
                vec![HtmlParseErrorCode::EofInCdata],
            ),
        ] {
            let (tokens, errors) = tokenize_with_cdata(input);
            assert_eq!(tokens, expected, "{input}");
            assert_eq!(errors, expected_errors, "{input}");
        }
    }

    #[test]
    fn a_cdata_section_passes_null_through_for_the_tree_builder() {
        let (tokens, errors) = tokenize_with_cdata("<![CDATA[a\0b]]>");
        assert_eq!(tokens[0], Token::Cdata("a\0b".to_owned()));
        // 13.2.5.69: "U+0000 NULL characters are handled in the tree
        // construction stage", so the tokenizer must not report one here.
        assert!(errors.is_empty());
    }

    #[test]
    fn a_truncated_cdata_section_reports_the_end_of_file() {
        let (tokens, errors) = tokenize_with_cdata("<![CDATA[ab]]");
        assert_eq!(tokens[0], Token::Cdata("ab]]".to_owned()));
        assert_eq!(tokens[1], Token::Eof);
        assert_eq!(errors, vec![HtmlParseErrorCode::EofInCdata]);
    }
    /// The standard raises `missing-whitespace-between-attributes` in exactly one
    /// state — 13.2.5.39, "after attribute value (quoted)" — so a value-less
    /// attribute can never be the one that is missing a separator. These are the
    /// shapes that used to be reported and must not be, including the real-world
    /// `<script defer src=...>` shape. The attribute lists are asserted too, so
    /// that suppressing the diagnostic cannot quietly change the parse.
    #[test]
    fn a_value_less_attribute_never_reports_a_missing_separator() {
        for (markup, expected) in [
            // The rows from the report.
            ("<script defer src=\"a.js\">", "defer=,src=a.js"),
            ("<script src=\"a.js\" defer>", "src=a.js,defer="),
            ("<div a b>", "a=,b="),
            ("<div a=\"1\" b=\"2\">", "a=1,b=2"),
            // The rest of the value-less shapes.
            ("<div a b=\"1\">", "a=,b=1"),
            ("<div a=\"1\" b>", "a=1,b="),
            ("<div a=\"1\" b=\"2\" c>", "a=1,b=2,c="),
            ("<div a b c d>", "a=,b=,c=,d="),
            // Markup of the kind that is on almost every page.
            (
                "<script defer async type=module src=a.js>",
                "defer=,async=,type=module,src=a.js",
            ),
            ("<input disabled required>", "disabled=,required="),
            (
                "<img src=a alt=b loading=lazy decoding=async width=10>",
                "src=a,alt=b,loading=lazy,decoding=async,width=10",
            ),
            // A value-less attribute at the end of the tag.
            ("<div a >", "a="),
            ("<div a/>", "a="),
        ] {
            let (tokens, errors) = tokenize(markup);
            assert!(
                !errors.contains(&HtmlParseErrorCode::MissingWhitespaceBetweenAttributes),
                "{markup} reported a missing separator: {errors:?}"
            );
            let Token::StartTag(TagToken { attributes, .. }) = &tokens[0] else {
                panic!("expected a start tag for {markup}");
            };
            let parsed: Vec<String> = attributes
                .iter()
                .map(|attribute| format!("{}={}", attribute.name, attribute.value))
                .collect();
            assert_eq!(parsed.join(","), expected, "{markup}");
        }
    }

    /// The genuine class: a quoted value followed immediately by another
    /// attribute, which is the standard's own example. Reported once per gap, and
    /// at the character where the separator should have been — the first character
    /// of the attribute that follows.
    #[test]
    fn a_missing_separator_is_reported_after_a_quoted_value() {
        for markup in [
            "<div id=\"foo\"class=\"bar\">",
            "<div id='foo'class='bar'>",
            "<div id=\"foo\"class='bar'>",
        ] {
            let (_, errors) = tokenize(markup);
            assert_eq!(
                errors
                    .iter()
                    .filter(|code| {
                        **code == HtmlParseErrorCode::MissingWhitespaceBetweenAttributes
                    })
                    .count(),
                1,
                "{markup}: {errors:?}"
            );
        }
        // Two gaps, two errors, each at its own gap.
        let mut tokenizer = Tokenizer::new("<div a=\"1\"b=\"2\"c=\"3\">");
        loop {
            let token = tokenizer.next();
            if token == Token::Eof {
                break;
            }
        }
        let offsets: Vec<usize> = tokenizer
            .errors()
            .iter()
            .filter(|error| error.code == HtmlParseErrorCode::MissingWhitespaceBetweenAttributes)
            .map(|error| error.offset)
            .collect();
        // `<div a="1"` is ten characters, so the first `b` is at 10 and the second
        // `c` at 15: the offsets are the gaps, not the ends of the attributes
        // that follow them.
        assert_eq!(offsets, vec![10, 15]);

        // The parse is unaffected: the parser behaves as if the whitespace were
        // present.
        let (tokens, _) = tokenize("<div id=\"foo\"class=\"bar\">");
        let Token::StartTag(TagToken { attributes, .. }) = &tokens[0] else {
            panic!("expected a start tag");
        };
        let parsed: Vec<String> = attributes
            .iter()
            .map(|attribute| format!("{}={}", attribute.name, attribute.value))
            .collect();
        assert_eq!(parsed, vec!["id=foo", "class=bar"]);
    }

    /// The three characters the "after attribute value (quoted)" state has an
    /// explicit case for are separators, not gaps: ASCII whitespace, `/`, and `>`.
    /// A quoted value followed by any of them is clean.
    #[test]
    fn a_quoted_value_may_be_followed_by_whitespace_a_solidus_or_the_end_of_the_tag() {
        for markup in [
            "<div a=\"1\" b=\"2\">",
            "<div a=\"1\"\tb=\"2\">",
            "<div a=\"1\"\nb=\"2\">",
            "<div a=\"1\"\rb=\"2\">",
            "<div a=\"1\"\u{0c}b=\"2\">",
            "<div a=\"1\"  b=\"2\">",
            // A solidus is the self-closing start tag state's business, and `>`
            // ends the tag.
            "<div a=\"1\"/>",
            "<div a=\"1\" b=\"2\"/>",
            "<div a=\"1\">",
        ] {
            let (_, errors) = tokenize(markup);
            assert!(
                !errors.contains(&HtmlParseErrorCode::MissingWhitespaceBetweenAttributes),
                "{markup} reported a missing separator: {errors:?}"
            );
        }
    }

    /// An unquoted value absorbs every character that is neither ASCII whitespace
    /// nor `>` (13.2.5.38), so a quote inside one cannot leave a gap: it is part
    /// of the value, and reported there.
    #[test]
    fn an_unquoted_value_absorbs_a_quote_so_no_separator_is_missing() {
        for markup in ["<div a=1\"b\">", "<div a=1'c>", "<div a=x=y>"] {
            let (_, errors) = tokenize(markup);
            assert!(
                !errors.contains(&HtmlParseErrorCode::MissingWhitespaceBetweenAttributes),
                "{markup} reported a missing separator: {errors:?}"
            );
        }
        // Both quotes are kept in the value, and each is reported there.
        let (tokens, errors) = tokenize("<div a=1\"b\">");
        let Token::StartTag(TagToken { attributes, .. }) = &tokens[0] else {
            panic!("expected a start tag");
        };
        assert_eq!(attributes[0].value, "1\"b\"");
        assert_eq!(
            errors,
            vec![
                HtmlParseErrorCode::UnexpectedCharacterInUnquotedAttributeValue,
                HtmlParseErrorCode::UnexpectedCharacterInUnquotedAttributeValue
            ]
        );
    }

    /// A solidus after a quoted value belongs to the self-closing start tag state
    /// (13.2.5.40), which reports `unexpected-solidus-in-tag` and reconsumes in
    /// the before attribute name state — so it must not also be reported as a
    /// missing separator.
    #[test]
    fn a_solidus_after_a_quoted_value_is_a_solidus_error_not_a_missing_separator() {
        let (_, errors) = tokenize("<div a=\"1\"/b=\"2\">");
        assert!(
            !errors.contains(&HtmlParseErrorCode::MissingWhitespaceBetweenAttributes),
            "{errors:?}"
        );
        assert_eq!(
            errors,
            vec![HtmlParseErrorCode::UnexpectedSolidusInTag],
            "the solidus is the only thing wrong with this markup"
        );
    }
}
