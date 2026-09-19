use std::collections::HashMap;
use std::sync::OnceLock;

use entities::ENTITIES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeToken {
    pub name: String,
    pub value: String,
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
    Character(String),
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentModel {
    Data,
    Rcdata,
    RawText,
    ScriptData,
    Plaintext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HtmlParseErrorCode {
    AbruptClosingOfEmptyComment,
    AbsenceOfDigitsInNumericCharacterReference,
    CharacterReferenceOutsideUnicodeRange,
    ControlCharacterReference,
    DuplicateAttribute,
    EndTagWithAttributes,
    EndTagWithTrailingSolidus,
    EofBeforeTagName,
    EofInComment,
    EofInDoctype,
    EofInElementThatCanContainOnlyText,
    EofInTag,
    IncorrectlyOpenedComment,
    InvalidCharacterSequenceAfterDoctypeName,
    InvalidFirstCharacterOfTagName,
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
            Self::ControlCharacterReference => "control-character-reference",
            Self::DuplicateAttribute => "duplicate-attribute",
            Self::EndTagWithAttributes => "end-tag-with-attributes",
            Self::EndTagWithTrailingSolidus => "end-tag-with-trailing-solidus",
            Self::EofBeforeTagName => "eof-before-tag-name",
            Self::EofInComment => "eof-in-comment",
            Self::EofInDoctype => "eof-in-doctype",
            Self::EofInElementThatCanContainOnlyText => "eof-in-element-that-can-contain-only-text",
            Self::EofInTag => "eof-in-tag",
            Self::IncorrectlyOpenedComment => "incorrectly-opened-comment",
            Self::InvalidCharacterSequenceAfterDoctypeName => {
                "invalid-character-sequence-after-doctype-name"
            }
            Self::InvalidFirstCharacterOfTagName => "invalid-first-character-of-tag-name",
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
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Token {
        if self.emitted_eof {
            return Token::Eof;
        }
        if self.offset >= self.input.len() {
            self.emitted_eof = true;
            return Token::Eof;
        }
        match self.content_model {
            ContentModel::Data => self.next_data_token(),
            ContentModel::Rcdata | ContentModel::RawText | ContentModel::ScriptData => {
                self.next_text_content_token()
            }
            ContentModel::Plaintext => self.next_plaintext_token(),
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
            if self.remaining().starts_with("--") {
                self.offset += 2;
                return self.consume_comment();
            }
            if self.remaining_starts_ascii_case_insensitive("doctype") {
                self.offset += "doctype".len();
                return self.consume_doctype();
            }
            self.error_at(
                less_than_offset,
                HtmlParseErrorCode::IncorrectlyOpenedComment,
            );
            return self.consume_bogus_comment();
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
            self.error_at(
                less_than_offset,
                HtmlParseErrorCode::UnexpectedQuestionMarkInsteadOfTagName,
            );
            return self.consume_bogus_comment();
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
        loop {
            let had_whitespace = self.skip_ascii_whitespace();
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
                    if !had_whitespace && !attributes.is_empty() {
                        self.error_at(
                            self.offset,
                            HtmlParseErrorCode::MissingWhitespaceBetweenAttributes,
                        );
                    }
                    let attribute = self.consume_attribute();
                    if attributes.iter().any(|existing: &AttributeToken| {
                        existing.name.eq_ignore_ascii_case(&attribute.name)
                    }) {
                        self.error_at(self.offset, HtmlParseErrorCode::DuplicateAttribute);
                    } else {
                        attributes.push(attribute);
                    }
                }
            }
        }

        Token::StartTag(TagToken {
            name,
            attributes,
            self_closing,
        })
    }

    fn consume_attribute(&mut self) -> AttributeToken {
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
        let value = if self.consume_char('=') {
            self.skip_ascii_whitespace();
            self.consume_attribute_value()
        } else {
            String::new()
        };
        AttributeToken { name, value }
    }

    fn consume_attribute_value(&mut self) -> String {
        let quote = match self.peek_char() {
            Some('"' | '\'') => self.bump_char(),
            Some('>') | None => {
                self.error_at(self.offset, HtmlParseErrorCode::MissingAttributeValue);
                return String::new();
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
        value
    }

    fn consume_comment(&mut self) -> Token {
        let start = self.offset;
        if self.consume_char('>') {
            self.error_at(start, HtmlParseErrorCode::AbruptClosingOfEmptyComment);
            return Token::Comment(String::new());
        }
        if let Some(relative_end) = self.remaining().find("-->") {
            let end = self.offset + relative_end;
            let data = self.input[self.offset..end].replace('\0', "\u{fffd}");
            if self.input[self.offset..end].contains('\0') {
                self.error_at(self.offset, HtmlParseErrorCode::UnexpectedNullCharacter);
            }
            self.offset = end + 3;
            return Token::Comment(data);
        }
        let data = self.remaining().replace('\0', "\u{fffd}");
        if self.remaining().contains('\0') {
            self.error_at(self.offset, HtmlParseErrorCode::UnexpectedNullCharacter);
        }
        self.offset = self.input.len();
        self.error_at(self.offset, HtmlParseErrorCode::EofInComment);
        Token::Comment(data)
    }

    fn consume_bogus_comment(&mut self) -> Token {
        let start = self.offset;
        while let Some(character) = self.peek_char() {
            if character == '>' {
                break;
            }
            self.bump_char();
        }
        let data = self.input[start..self.offset].replace('\0', "\u{fffd}");
        self.consume_char('>');
        Token::Comment(data)
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
}
