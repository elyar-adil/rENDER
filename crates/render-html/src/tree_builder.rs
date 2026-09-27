use render_dom::{Dom, Namespace, NodeId, NodeKind};

use super::tokenizer::{
    AttributeToken, ContentModel, DoctypeToken, HtmlParseError, HtmlParseErrorCode, TagToken,
    Token, Tokenizer,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuirksMode {
    NoQuirks,
    LimitedQuirks,
    Quirks,
}

impl QuirksMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoQuirks => "no-quirks",
            Self::LimitedQuirks => "limited-quirks",
            Self::Quirks => "quirks",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParseOutput {
    pub dom: Dom,
    pub errors: Vec<HtmlParseError>,
    pub quirks_mode: QuirksMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InsertionMode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InTemplate,
    AfterBody,
    AfterAfterBody,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Consumed,
    Reprocess,
}

/// Parse an HTML document into the Rust DOM.
///
/// The scripting mode is `Normal`, because rENDER is a user agent with a script
/// execution engine and therefore a scripting-enabled one (13.2.4.5). Use
/// [`parse_document_with_scripting`] to parse as a user agent with scripting
/// turned off, which is what makes `noscript` content parse as markup.
#[must_use]
pub fn parse_document(input: &str) -> ParseOutput {
    parse_document_with_scripting(input, true)
}

/// Parse an HTML document into the Rust DOM, with the scripting flag chosen by
/// the caller.
///
/// `scripting_enabled` is the tree builder's half of the parser's scripting mode
/// (13.2.4.5): the spec's mode has four values, of which the tree builder only
/// distinguishes `Disabled` from the rest, and this is that distinction. With
/// scripting disabled, a `noscript` element's contents are parsed as markup,
/// which is the whole point of the element.
#[must_use]
pub fn parse_document_with_scripting(input: &str, scripting_enabled: bool) -> ParseOutput {
    let mut builder = TreeBuilder::new(input);
    builder.scripting_disabled = !scripting_enabled;
    builder.parse()
}

struct TreeBuilder<'a> {
    tokenizer: Tokenizer<'a>,
    dom: Dom,
    open_elements: Vec<NodeId>,
    mode: InsertionMode,
    original_mode: InsertionMode,
    /// The stack of template insertion modes (13.2.4.1). It is initially
    /// empty, and a `template` start tag pushes onto it while its end tag pops
    /// from it; the current template insertion mode is the one most recently
    /// pushed. It is only used to parse the contents of a template element and
    /// to leave them again.
    template_modes: Vec<InsertionMode>,
    /// The list of active formatting elements (13.2.4.3). It is initially
    /// empty and is used to handle mis-nested formatting element tags. It is
    /// parser state, not DOM state: it refers to elements by node and keeps
    /// the token each was created for, so nothing in `render-dom` needs to
    /// know it exists.
    active_formatting: Vec<ActiveFormattingEntry>,
    /// The pending table character tokens of the "in table text" insertion
    /// mode (13.2.6.4.10), emptied on every entry into that mode.
    pending_table_characters: String,
    /// Whether the parser's scripting mode is `Disabled` (13.2.4.5). The spec's
    /// mode has four values and the tree builder only needs this one
    /// distinction: with scripting not disabled, a `noscript` element's contents
    /// are raw text, and with it disabled they are markup and a `noscript` in the
    /// head switches to the "in head noscript" insertion mode.
    scripting_disabled: bool,
    /// The form element pointer (13.2.4.4): "the last `form` element that was
    /// opened and whose end tag has not yet been seen", initially null.
    ///
    /// It exists so that form controls associate with forms "in the face of
    /// dramatically bad markup, for historical reasons", which means it can name
    /// a form that is not an element's ancestor. That association cannot be
    /// derived from the finished tree, so it is recorded on the element as it is
    /// created; see `Dom::form_owner`. The pointer is ignored while parsing
    /// template contents.
    form_element_pointer: Option<NodeId>,
    head_element: Option<NodeId>,
    body_element: Option<NodeId>,
    tree_errors: Vec<HtmlParseError>,
    quirks_mode: QuirksMode,
    foster_parenting: bool,
    ignore_next_line_feed: bool,
    temporary_head: Option<NodeId>,
}

/// An entry in the list of active formatting elements: either a marker, or a
/// formatting element together with the token it was created for. Keeping the
/// token is what lets "reconstruct the active formatting elements" and the
/// adoption agency algorithm create a replacement element for an entry whose
/// element is no longer open (13.2.4.3).
#[derive(Clone, Debug)]
enum ActiveFormattingEntry {
    Marker,
    Element { node: NodeId, token: TagToken },
}

impl ActiveFormattingEntry {
    fn node(&self) -> Option<NodeId> {
        match self {
            Self::Marker => None,
            Self::Element { node, .. } => Some(*node),
        }
    }
}

impl<'a> TreeBuilder<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            tokenizer: Tokenizer::new(input),
            dom: Dom::new(),
            open_elements: Vec::new(),
            mode: InsertionMode::Initial,
            original_mode: InsertionMode::Initial,
            template_modes: Vec::new(),
            active_formatting: Vec::new(),
            pending_table_characters: String::new(),
            scripting_disabled: false,
            form_element_pointer: None,
            head_element: None,
            body_element: None,
            tree_errors: Vec::new(),
            quirks_mode: QuirksMode::NoQuirks,
            foster_parenting: false,
            ignore_next_line_feed: false,
            temporary_head: None,
        }
    }

    fn parse(mut self) -> ParseOutput {
        loop {
            let token = self.tokenizer.next();
            let is_eof = token == Token::Eof;
            loop {
                if self.process(&token) == Action::Consumed {
                    break;
                }
            }
            // The "markup declaration open state" (13.2.5.42) needs the
            // adjusted current node, which the tree builder owns, to decide
            // whether `<![CDATA[` opens a CDATA section. The tree is only
            // queried for the next token once the current one is fully
            // processed, so that is where the answer is pushed in.
            self.tokenizer.set_allow_cdata(self.allows_cdata_section());
            if is_eof {
                break;
            }
        }
        let mut errors = self.tokenizer.into_errors();
        errors.extend(self.tree_errors);
        ParseOutput {
            dom: self.dom,
            errors,
            quirks_mode: self.quirks_mode,
        }
    }

    /// The "tree construction dispatcher" (13.2.6).
    ///
    /// The rules for parsing tokens in foreign content are not an insertion
    /// mode: the dispatcher selects them independently of the current insertion
    /// mode, which is left unchanged while they are in use.
    fn in_foreign_content(&self, token: &Token) -> bool {
        let Some(current) = self.open_elements.last().copied() else {
            // "If the stack of open elements is empty".
            return false;
        };
        // "The adjusted current node" is the current node. This tree builder
        // parses whole documents only, so there is no fragment context element.
        if self.is_html_element(current) {
            return false;
        }
        // A CDATA section (13.2.5.69) is character data, so it takes the same
        // character-token exceptions below that a character token does.
        let (start_tag_name, is_character) = match token {
            Token::StartTag(tag) => (Some(tag.name.as_str()), false),
            Token::Character(_) | Token::Cdata(_) => (None, true),
            _ => (None, false),
        };
        if self.is_mathml_text_integration_point(current)
            && (is_character
                || start_tag_name.is_some_and(|name| !matches!(name, "mglyph" | "malignmark")))
        {
            return false;
        }
        if start_tag_name == Some("svg") && self.is_mathml_annotation_xml(current) {
            return false;
        }
        if self.is_html_integration_point(current) && (is_character || start_tag_name.is_some()) {
            return false;
        }
        !matches!(token, Token::Eof)
    }

    /// Whether the tokenizer may enter the CDATA section state (13.2.5.42):
    /// only when there is an adjusted current node and it is not an element in
    /// the HTML namespace.
    fn allows_cdata_section(&self) -> bool {
        self.open_elements
            .last()
            .is_some_and(|current| !self.is_html_element(*current))
    }

    fn process(&mut self, token: &Token) -> Action {
        if let Token::Cdata(data) = token
            && !self.in_foreign_content(token)
        {
            // A CDATA section is only tokenized outside the HTML namespace, so
            // the dispatcher reaching an HTML insertion mode means the section
            // is at an integration point, whose character-token rules insert
            // exactly the same text at exactly the same node. Route the
            // characters there rather than dropping them.
            return self.process(&Token::Character(data.clone()));
        }
        if self.in_foreign_content(token) {
            return self.process_in_foreign_content(token);
        }
        self.process_in_html_content(token)
    }

    /// "Process the token according to the rules given in the section
    /// corresponding to the current insertion mode in HTML content" (13.2.6).
    ///
    /// The rules for parsing tokens in foreign content reprocess a token this
    /// way after they have already made that decision, so the dispatcher is not
    /// consulted again here: the current node can still be a MathML text
    /// integration point, which the dispatcher would send straight back.
    fn process_in_html_content(&mut self, token: &Token) -> Action {
        match self.mode {
            InsertionMode::Initial => self.process_initial(token),
            InsertionMode::BeforeHtml => self.process_before_html(token),
            InsertionMode::BeforeHead => self.process_before_head(token),
            InsertionMode::InHead => self.process_in_head(token),
            InsertionMode::InHeadNoscript => self.process_in_head_noscript(token),
            InsertionMode::AfterHead => self.process_after_head(token),
            InsertionMode::InBody => self.process_in_body(token),
            InsertionMode::Text => self.process_text(token),
            InsertionMode::InTable => self.process_in_table(token),
            InsertionMode::InTableText => self.process_in_table_text(token),
            InsertionMode::InCaption => self.process_in_caption(token),
            InsertionMode::InColumnGroup => self.process_in_column_group(token),
            InsertionMode::InTableBody => self.process_in_table_body(token),
            InsertionMode::InRow => self.process_in_row(token),
            InsertionMode::InCell => self.process_in_cell(token),
            InsertionMode::InTemplate => self.process_in_template(token),
            InsertionMode::AfterBody => self.process_after_body(token),
            InsertionMode::AfterAfterBody => self.process_after_after_body(token),
        }
    }

    fn process_initial(&mut self, token: &Token) -> Action {
        match token {
            Token::Character(data) if is_all_html_whitespace(data) => Action::Consumed,
            Token::Comment(data) => {
                self.insert_comment(self.dom.document(), data);
                Action::Consumed
            }
            Token::Doctype(doctype) => {
                self.insert_doctype(doctype);
                self.quirks_mode = doctype_quirks_mode(doctype);
                self.mode = InsertionMode::BeforeHtml;
                Action::Consumed
            }
            _ => {
                self.parse_error(HtmlParseErrorCode::MissingDoctype);
                self.quirks_mode = QuirksMode::Quirks;
                self.mode = InsertionMode::BeforeHtml;
                Action::Reprocess
            }
        }
    }

    fn process_before_html(&mut self, token: &Token) -> Action {
        match token {
            Token::Character(data) if is_all_html_whitespace(data) => Action::Consumed,
            Token::Comment(data) => {
                self.insert_comment(self.dom.document(), data);
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "html" => {
                self.insert_html_root(tag);
                self.mode = InsertionMode::BeforeHead;
                Action::Consumed
            }
            Token::EndTag(tag) if !matches!(tag.name.as_str(), "head" | "body" | "html" | "br") => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            _ => {
                self.insert_html_root(&empty_tag("html"));
                self.mode = InsertionMode::BeforeHead;
                Action::Reprocess
            }
        }
    }

    fn process_before_head(&mut self, token: &Token) -> Action {
        match token {
            Token::Character(data) if is_all_html_whitespace(data) => Action::Consumed,
            Token::Comment(data) => {
                self.insert_comment(self.current_node(), data);
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "html" => self.process_in_body(token),
            Token::StartTag(tag) if tag.name == "head" => {
                self.head_element = self.insert_element(tag, true);
                self.mode = InsertionMode::InHead;
                Action::Consumed
            }
            Token::EndTag(tag) if !matches!(tag.name.as_str(), "head" | "body" | "html" | "br") => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            _ => {
                self.head_element = self.insert_element(&empty_tag("head"), true);
                self.mode = InsertionMode::InHead;
                Action::Reprocess
            }
        }
    }

    fn process_in_head(&mut self, token: &Token) -> Action {
        match token {
            Token::Character(data) if is_all_html_whitespace(data) => {
                self.insert_text(data);
                Action::Consumed
            }
            Token::Comment(data) => {
                self.insert_comment(self.current_node(), data);
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "html" => self.process_in_body(token),
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "base" | "basefont" | "bgsound" | "link" | "meta"
                ) =>
            {
                self.insert_element(tag, false);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "title" => {
                self.enter_text_element(tag, ContentModel::Rcdata);
                Action::Consumed
            }
            // "A start tag whose tag name is 'noscript', if scripting mode is not
            // Disabled" and "A start tag whose tag name is 'noscript', if
            // scripting mode is Disabled" (13.2.6.4.4). With scripting enabled a
            // `noscript` element's contents are raw text, so a `style` or a
            // `link` written inside one is inert text rather than an element the
            // engine would act on. With scripting disabled the element's contents
            // are markup, parsed by the "in head noscript" mode.
            Token::StartTag(tag) if tag.name == "noscript" => {
                if self.scripting_disabled {
                    self.insert_element(tag, true);
                    self.mode = InsertionMode::InHeadNoscript;
                } else {
                    self.enter_text_element(tag, ContentModel::RawText);
                }
                Action::Consumed
            }
            Token::StartTag(tag) if matches!(tag.name.as_str(), "style" | "noframes") => {
                self.enter_text_element(tag, ContentModel::RawText);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "script" => {
                self.enter_text_element(tag, ContentModel::ScriptData);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "template" => {
                self.enter_template(tag);
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "template" => self.close_template(),
            Token::EndTag(tag) if tag.name == "head" => {
                self.pop_current();
                self.mode = InsertionMode::AfterHead;
                Action::Consumed
            }
            Token::EndTag(tag) if !matches!(tag.name.as_str(), "body" | "html" | "br") => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "head" => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            _ => {
                self.pop_current();
                self.mode = InsertionMode::AfterHead;
                Action::Reprocess
            }
        }
    }

    /// The "in head noscript" insertion mode (13.2.6.4.5).
    ///
    /// Only a parser whose scripting mode is `Disabled` can reach this mode: with
    /// scripting enabled, a `noscript` start tag is a raw text element instead
    /// (13.2.6.4.4), so the mode is where a `noscript` element's contents are
    /// parsed as markup. A `style`, `link`, `meta` or `noframes` start tag inside
    /// one is still handled by the "in head" rules, so the element can carry
    /// fallback stylesheets and metadata; anything else ends the element and is
    /// reprocessed by "in head".
    fn process_in_head_noscript(&mut self, token: &Token) -> Action {
        /// "Anything else": parse error, pop the `noscript` element, switch the
        /// insertion mode to "in head", and reprocess the token. Shared with the
        /// `</br>` arm, which the spec sends here explicitly. The token itself is
        /// not needed: returning `Reprocess` hands it back to the dispatcher,
        /// which runs it again in the new insertion mode.
        fn leave_noscript(builder: &mut TreeBuilder<'_>) -> Action {
            builder.parse_error(HtmlParseErrorCode::UnexpectedToken);
            // "Pop the current node (which will be a noscript element) from the
            // stack of open elements; the new current node will be a head
            // element."
            builder.pop_current();
            builder.mode = InsertionMode::InHead;
            Action::Reprocess
        }
        match token {
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "html" => self.process_in_body(token),
            // "Pop the current node (which will be a noscript element) from the
            // stack of open elements; the new current node will be a head
            // element. Switch the insertion mode to 'in head'."
            Token::EndTag(tag) if tag.name == "noscript" => {
                self.pop_current();
                self.mode = InsertionMode::InHead;
                Action::Consumed
            }
            // "A character token that is one of [ASCII whitespace], a comment
            // token, a processing instruction token, [or] a start tag whose tag
            // name is one of: 'basefont', 'bgsound', 'link', 'meta', 'noframes',
            // 'style': process the token using the rules for the 'in head'
            // insertion mode."
            Token::Character(data) if is_all_html_whitespace(data) => self.process_in_head(token),
            Token::Comment(_) => self.process_in_head(token),
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"
                ) =>
            {
                self.process_in_head(token)
            }
            // "An end tag whose tag name is 'br': act as described in the
            // 'anything else' entry below."
            Token::EndTag(tag) if tag.name == "br" => leave_noscript(self),
            // "A start tag whose tag name is one of: 'head', 'noscript'" and
            // "any other end tag": parse error, ignore the token.
            Token::StartTag(tag) if matches!(tag.name.as_str(), "head" | "noscript") => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::EndTag(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            _ => leave_noscript(self),
        }
    }

    /// The "after head" insertion mode.
    fn process_after_head(&mut self, token: &Token) -> Action {
        match token {
            Token::Character(data) if is_all_html_whitespace(data) => {
                self.insert_text(data);
                Action::Consumed
            }
            Token::Comment(data) => {
                self.insert_comment(self.current_node(), data);
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "html" => self.process_in_body(token),
            Token::StartTag(tag) if tag.name == "body" => {
                self.body_element = self.insert_element(tag, true);
                self.mode = InsertionMode::InBody;
                Action::Consumed
            }
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "base"
                        | "basefont"
                        | "bgsound"
                        | "link"
                        | "meta"
                        | "noframes"
                        | "script"
                        | "style"
                        | "template"
                        | "title"
                ) =>
            {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                let Some(head) = self.head_element else {
                    return Action::Consumed;
                };
                self.open_elements.push(head);
                let result = self.process_in_head(token);
                if self.mode == InsertionMode::Text {
                    self.temporary_head = Some(head);
                } else if let Some(index) =
                    self.open_elements.iter().rposition(|node| *node == head)
                {
                    self.open_elements.remove(index);
                }
                result
            }
            Token::EndTag(tag) if tag.name == "template" => self.process_in_head(token),
            Token::EndTag(tag) if !matches!(tag.name.as_str(), "body" | "html" | "br") => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "head" => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            _ => {
                self.body_element = self.insert_element(&empty_tag("body"), true);
                self.mode = InsertionMode::InBody;
                Action::Reprocess
            }
        }
    }

    fn process_text(&mut self, token: &Token) -> Action {
        match token {
            Token::Character(data) => {
                let data = if self.ignore_next_line_feed {
                    self.ignore_next_line_feed = false;
                    data.strip_prefix('\n').unwrap_or(data)
                } else {
                    data
                };
                if !data.is_empty() {
                    self.insert_text(data);
                }
                Action::Consumed
            }
            Token::EndTag(_) => {
                self.pop_current();
                self.remove_temporary_head();
                self.mode = self.original_mode;
                Action::Consumed
            }
            Token::Eof => {
                self.parse_error(HtmlParseErrorCode::EofInElementThatCanContainOnlyText);
                self.pop_current();
                self.remove_temporary_head();
                self.mode = self.original_mode;
                Action::Reprocess
            }
            _ => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn process_in_body(&mut self, token: &Token) -> Action {
        match token {
            // A CDATA section is character data (13.2.5.69); `process` maps one
            // that reaches an HTML insertion mode onto the character-token
            // rules, so the arm is a spelling of the one below.
            //
            // "A character token that is one of U+0009 CHARACTER TABULATION,
            // U+000A LINE FEED (LF), U+000C FORM FEED (FF), U+000D CARRIAGE
            // RETURN (CR), or U+0020 SPACE: Reconstruct the active formatting
            // elements, if any. Insert the token's character." and "Any other
            // character token: Reconstruct the active formatting elements, if any.
            // Insert the token's character."
            Token::Character(data) | Token::Cdata(data) => {
                let data = if self.ignore_next_line_feed {
                    self.ignore_next_line_feed = false;
                    data.strip_prefix('\n').unwrap_or(data)
                } else {
                    data
                };
                if !data.is_empty() {
                    self.reconstruct_active_formatting_elements();
                    self.insert_text(data);
                }
                Action::Consumed
            }
            Token::Comment(data) => {
                self.insert_comment(self.current_node(), data);
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "html" => {
                // "If there is a template element on the stack of open
                // elements, then ignore the token."
                if self.has_open_template() {
                    return Action::Consumed;
                }
                if let Some(html) = self.open_elements.first().copied() {
                    self.merge_attributes(html, tag);
                }
                Action::Consumed
            }
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "base"
                        | "basefont"
                        | "bgsound"
                        | "link"
                        | "meta"
                        | "noframes"
                        | "script"
                        | "style"
                        | "template"
                        | "title"
                ) =>
            {
                self.process_in_head(token)
            }
            Token::EndTag(tag) if tag.name == "template" => self.process_in_head(token),
            Token::StartTag(tag) if tag.name == "body" => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                // "If the stack of open elements has only one node on it, or if
                // the second element on the stack of open elements is not a body
                // element, or if there is a template element on the stack of
                // open elements, then ignore the token."
                if self.has_open_template() {
                    return Action::Consumed;
                }
                if let Some(body) = self.body_element {
                    self.merge_attributes(body, tag);
                }
                Action::Consumed
            }
            // "A start tag whose tag name is one of: 'address', 'article',
            // 'aside', 'blockquote', 'center', 'details', 'dialog', 'dir',
            // 'div', 'dl', 'fieldset', 'figcaption', 'figure', 'footer',
            // 'header', 'hgroup', 'main', 'menu', 'nav', 'ol', 'p', 'search',
            // 'section', 'summary', 'ul'": close a p element, then insert. The
            // spec reconstructs the active formatting elements neither here nor
            // for a block, because a block element itself stops them: the
            // formatting elements stay open underneath it and are re-opened by
            // the next reconstruction.
            Token::StartTag(tag) if tag.name == "p" => {
                self.close_p_if_open();
                self.insert_element(tag, true);
                Action::Consumed
            }
            Token::StartTag(tag) if is_block_start(&tag.name) => {
                self.close_p_if_open();
                self.insert_element(tag, true);
                Action::Consumed
            }
            Token::StartTag(tag) if is_heading(&tag.name) => {
                self.close_p_if_open();
                if self.current_tag().is_some_and(is_heading) {
                    self.pop_current();
                }
                self.insert_element(tag, true);
                Action::Consumed
            }
            // "A start tag whose tag name is one of: 'pre', 'listing'"
            Token::StartTag(tag) if matches!(tag.name.as_str(), "pre" | "listing") => {
                self.close_p_if_open();
                self.insert_element(tag, true);
                self.ignore_next_line_feed = true;
                Action::Consumed
            }
            // "A start tag whose tag name is 'form'" (13.2.6.4.7). If the form
            // element pointer is not null and the parser is not parsing template
            // contents, this is a parse error and the token is ignored, which is
            // what keeps a second `form` from opening inside the first. Otherwise
            // close a p element, insert, and point the form element pointer at the
            // element created.
            //
            // The current standard does not pop the form off the stack of open
            // elements here; the "in table" rules do that, and the end tag below
            // removes it.
            Token::StartTag(tag) if tag.name == "form" => {
                if self.form_element_pointer.is_some() && !self.has_open_template() {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                } else {
                    self.close_p_if_open();
                    if let Some(form) = self.insert_element(tag, true)
                        && !self.has_open_template()
                    {
                        self.form_element_pointer = Some(form);
                    }
                }
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "li" => {
                self.close_matching_list_item("li");
                self.close_p_if_open();
                self.insert_element(tag, true);
                Action::Consumed
            }
            Token::StartTag(tag) if matches!(tag.name.as_str(), "dd" | "dt") => {
                self.close_definition_item();
                self.close_p_if_open();
                self.insert_element(tag, true);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "plaintext" => {
                self.close_p_if_open();
                self.insert_element(tag, true);
                self.tokenizer.switch_to(ContentModel::Plaintext, None);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "table" => {
                self.close_p_if_open();
                self.insert_element(tag, true);
                self.mode = InsertionMode::InTable;
                Action::Consumed
            }
            Token::StartTag(tag) if matches!(tag.name.as_str(), "textarea" | "title") => {
                self.enter_text_element(tag, ContentModel::Rcdata);
                self.ignore_next_line_feed = tag.name == "textarea";
                Action::Consumed
            }
            // "A start tag whose tag name is 'xmp'": close a p element,
            // reconstruct the active formatting elements, then follow the generic
            // raw text element parsing algorithm.
            Token::StartTag(tag) if matches!(tag.name.as_str(), "xmp" | "iframe" | "noembed") => {
                self.close_p_if_open();
                if tag.name == "xmp" {
                    self.reconstruct_active_formatting_elements();
                }
                self.enter_text_element(tag, ContentModel::RawText);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "script" => {
                self.enter_text_element(tag, ContentModel::ScriptData);
                Action::Consumed
            }
            // "A start tag whose tag name is 'select'": if the stack of open
            // elements has a select element in scope, this is a parse error and
            // the token is ignored; otherwise reconstruct the active formatting
            // elements and insert. There is no separate "in select" mode in the
            // current standard, so `option` and `optgroup` are handled below
            // rather than by a mode of their own.
            Token::StartTag(tag) if tag.name == "select" => {
                if self.has_select_in_scope() {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                } else {
                    self.reconstruct_active_formatting_elements();
                    self.insert_element(tag, true);
                }
                Action::Consumed
            }
            // "A start tag whose tag name is 'option'": if the stack of open
            // elements has a select element in scope, then ... [if the stack of
            // open elements has an option element in scope, then this is a parse
            // error.] Otherwise, if the current node is an option element, then
            // pop the current node off the stack of open elements. Reconstruct the
            // active formatting elements, if any. Insert an HTML element for the
            // token.
            Token::StartTag(tag) if tag.name == "option" => {
                if self.has_select_in_scope() {
                    // "Generate implied end tags except for optgroup elements."
                    self.generate_implied_end_tags_except(Some("optgroup"));
                    if self.has_element_name_in_scope("option") {
                        self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    }
                } else if self.current_tag() == Some("option") {
                    self.pop_current();
                }
                self.reconstruct_active_formatting_elements();
                self.insert_element(tag, true);
                Action::Consumed
            }
            // "A start tag whose tag name is 'optgroup'", with the same shape.
            Token::StartTag(tag) if tag.name == "optgroup" => {
                if self.has_select_in_scope() {
                    self.generate_implied_end_tags_except(None);
                    if self.has_element_name_in_scope("option")
                        || self.has_element_name_in_scope("optgroup")
                    {
                        self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    }
                } else if self.current_tag() == Some("option") {
                    self.pop_current();
                }
                self.reconstruct_active_formatting_elements();
                self.insert_element(tag, true);
                Action::Consumed
            }
            // "A start tag whose tag name is 'math'" and "'svg'": reconstruct the
            // active formatting elements first. This is what keeps `<b><svg>` from
            // leaving the `b` stranded outside the foreign subtree.
            Token::StartTag(tag) if tag.name == "math" => {
                self.reconstruct_active_formatting_elements();
                self.insert_foreign_start_tag(tag, &Namespace::MathMl);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "svg" => {
                self.reconstruct_active_formatting_elements();
                self.insert_foreign_start_tag(tag, &Namespace::Svg);
                Action::Consumed
            }
            // "A start tag whose tag name is one of: 'area', 'br', 'embed',
            // 'img', 'keygen', 'wbr'": reconstruct the active formatting
            // elements, insert, and pop immediately.
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "area" | "br" | "embed" | "img" | "keygen" | "wbr"
                ) =>
            {
                self.reconstruct_active_formatting_elements();
                self.insert_element(tag, false);
                Action::Consumed
            }
            // "A start tag whose tag name is 'input'": if the stack of open
            // elements has a select element in scope, this is a parse error and the
            // parser pops elements until a select element has been popped. Then
            // reconstruct the active formatting elements, insert, and pop
            // immediately.
            Token::StartTag(tag) if tag.name == "input" => {
                if self.has_select_in_scope() {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    self.pop_through("select");
                }
                self.reconstruct_active_formatting_elements();
                self.insert_element(tag, false);
                Action::Consumed
            }
            // "A start tag whose tag name is one of: 'param', 'source',
            // 'track'": insert an HTML element for the token, then immediately
            // pop the current node off the stack of open elements. Unlike the
            // "area, br, embed, img, keygen, wbr" arm there is no reconstruction
            // step, because these are children of a replaced element rather than
            // content of the document.
            Token::StartTag(tag) if matches!(tag.name.as_str(), "param" | "source" | "track") => {
                self.insert_element(tag, false);
                Action::Consumed
            }
            // "A start tag whose tag name is 'hr'": close a p element, and if
            // there is a select element in scope generate implied end tags and
            // report a parse error if there is an option or optgroup in scope.
            // Then insert an HTML element for the token and immediately pop the
            // current node.
            Token::StartTag(tag) if tag.name == "hr" => {
                self.close_p_if_open();
                if self.has_select_in_scope() {
                    self.generate_implied_end_tags_except(None);
                    if self.has_element_name_in_scope("option")
                        || self.has_element_name_in_scope("optgroup")
                    {
                        self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    }
                }
                self.insert_element(tag, false);
                Action::Consumed
            }
            // "A start tag whose tag name is 'a'": if the list of active
            // formatting elements contains an a element between the end of the
            // list and the last marker, run the adoption agency algorithm for the
            // token, then remove that element from the list and the stack of open
            // elements if the algorithm did not already.
            Token::StartTag(tag) if tag.name == "a" => {
                if let Some(index) = self.find_formatting_element_since_marker("a") {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    self.run_adoption_agency_algorithm("a");
                    // The algorithm may have removed the element, or replaced it
                    // with a new one at the same place.
                    if index < self.active_formatting.len()
                        && self.active_formatting[index]
                            .node()
                            .is_some_and(|node| self.element_name(node) == Some("a"))
                    {
                        let entry = self.active_formatting.remove(index);
                        if let Some(node) = entry.node()
                            && let Some(stack) = self.open_elements.iter().position(|n| *n == node)
                        {
                            self.open_elements.remove(stack);
                        }
                    }
                }
                self.reconstruct_active_formatting_elements();
                if let Some(node) = self.insert_element(tag, true) {
                    self.push_active_formatting_element(node, tag);
                }
                Action::Consumed
            }
            // "A start tag whose tag name is one of: 'b', 'big', 'code', 'em',
            // 'font', 'i', 's', 'small', 'strike', 'strong', 'tt', 'u'".
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "b" | "big"
                        | "code"
                        | "em"
                        | "font"
                        | "i"
                        | "s"
                        | "small"
                        | "strike"
                        | "strong"
                        | "tt"
                        | "u"
                ) =>
            {
                self.reconstruct_active_formatting_elements();
                if let Some(node) = self.insert_element(tag, true) {
                    self.push_active_formatting_element(node, tag);
                }
                Action::Consumed
            }
            // "A start tag whose tag name is 'nobr'": reconstruct, and if there is
            // a nobr element in scope, run the adoption agency algorithm and
            // reconstruct once again.
            Token::StartTag(tag) if tag.name == "nobr" => {
                self.reconstruct_active_formatting_elements();
                if self.has_element_name_in_scope("nobr") {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    self.run_adoption_agency_algorithm("nobr");
                    self.reconstruct_active_formatting_elements();
                }
                if let Some(node) = self.insert_element(tag, true) {
                    self.push_active_formatting_element(node, tag);
                }
                Action::Consumed
            }
            // "A start tag whose tag name is one of: 'applet', 'marquee',
            // 'object'": reconstruct, insert, and insert a marker.
            Token::StartTag(tag)
                if matches!(tag.name.as_str(), "applet" | "marquee" | "object") =>
            {
                self.reconstruct_active_formatting_elements();
                self.insert_element(tag, true);
                self.push_active_formatting_marker();
                Action::Consumed
            }
            // "An end tag token whose tag name is one of: 'applet', 'marquee',
            // 'object'": if there is no such element in scope, ignore the token.
            // Otherwise generate implied end tags, pop until it has been popped,
            // and clear the list of active formatting elements up to the last
            // marker, which removes the marker this element pushed.
            Token::EndTag(tag) if matches!(tag.name.as_str(), "applet" | "marquee" | "object") => {
                if !self.has_element_name_in_scope(&tag.name) {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    return Action::Consumed;
                }
                self.close_through_implied_end_tags(&tag.name);
                self.clear_active_formatting_up_to_last_marker();
                Action::Consumed
            }
            // "A start tag whose tag name is 'noscript', if scripting mode is not
            // Disabled": follow the generic raw text element parsing algorithm
            // (13.2.6.4.7).
            //
            // With scripting disabled there is no such rule, so the tag falls
            // through to the "any other start tag" arm below and the element's
            // contents are parsed as markup — which is the fallback content the
            // element exists to provide.
            Token::StartTag(tag) if tag.name == "noscript" && !self.scripting_disabled => {
                self.enter_text_element(tag, ContentModel::RawText);
                Action::Consumed
            }
            // "A start tag whose tag name is one of: 'caption', 'col',
            // 'colgroup', 'frame', 'head', 'tbody', 'td', 'tfoot', 'th',
            // 'thead', 'tr'": parse error, ignore the token.
            //
            // These are table structure tags reaching the "in body" rules, which
            // happens when they appear with no table around them. Ignoring them is
            // what keeps a stray `<col>` from being inserted as an ordinary
            // element: `<col>` is a void element, so the "any other start tag"
            // arm below would push it onto the stack of open elements and a
            // second one would then be inserted *inside* it.
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "frame"
                        | "head"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            // "Any other start tag": reconstruct the active formatting elements,
            // then insert an HTML element for the token. This element will be an
            // ordinary element.
            Token::StartTag(tag) => {
                self.reconstruct_active_formatting_elements();
                self.insert_element(tag, true);
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "body" => {
                if self.has_open_element("body") {
                    self.mode = InsertionMode::AfterBody;
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "html" => {
                if self.has_open_element("body") {
                    self.mode = InsertionMode::AfterBody;
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
            // "An end tag whose tag name is 'form'" (13.2.6.4.7).
            //
            // Outside template contents the form element pointer decides which
            // element is closed, and the pointer is cleared first either way — so
            // a `</form>` that closes nothing still ends the pointer's lifetime.
            // That clearing is the parser-side half of the same rule the DOM's
            // derived owner implements: after it, a later control is associated
            // with the nearest ancestor `form`, not with the form whose end tag
            // has been seen.
            //
            // Inside template contents the pointer is ignored, so the rules fall
            // back to the nearest form in scope.
            Token::EndTag(tag) if tag.name == "form" => {
                if !self.has_open_template() {
                    let node = self.form_element_pointer;
                    self.form_element_pointer = None;
                    let Some(node) = node.filter(|node| self.has_element_in_scope(*node)) else {
                        self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                        return Action::Consumed;
                    };
                    self.generate_implied_end_tags_except(None);
                    if self.current_node() != node {
                        self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    }
                    self.remove_from_open_elements(node);
                } else if !self.has_element_name_in_scope("form") {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                } else {
                    self.generate_implied_end_tags_except(None);
                    if self.current_tag() != Some("form") {
                        self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    }
                    self.pop_through("form");
                }
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "p" => {
                if !self.has_open_element("p") {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    self.insert_element(&empty_tag("p"), true);
                }
                self.pop_through("p");
                Action::Consumed
            }
            // "An end tag whose tag name is one of: 'address', 'article',
            // 'aside', 'blockquote', 'button', 'center', 'details', 'dialog',
            // 'dir', 'div', 'dl', 'fieldset', 'figcaption', 'figure', 'footer',
            // 'header', 'hgroup', 'listing', 'main', 'menu', 'nav', 'ol', 'pre',
            // 'search', 'section', 'select', 'summary', 'ul'": if there is no
            // element in scope with that name, ignore the token. Otherwise
            // generate implied end tags, pop until an element with that name has
            // been popped, and clear the list of active formatting elements up to
            // the last marker. The clearing is what stops formatting from leaking
            // out of a cell, a caption, or an object.
            Token::EndTag(tag) if Self::is_block_end(&tag.name) => {
                if !self.has_element_name_in_scope(&tag.name) {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    return Action::Consumed;
                }
                self.close_through_implied_end_tags(&tag.name);
                self.clear_active_formatting_up_to_last_marker();
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "li" => {
                self.pop_through_if_open("li");
                Action::Consumed
            }
            Token::EndTag(tag) if matches!(tag.name.as_str(), "dd" | "dt") => {
                self.pop_through_if_open(&tag.name);
                Action::Consumed
            }
            Token::EndTag(tag) if is_heading(&tag.name) => {
                if let Some(name) = self
                    .open_elements
                    .iter()
                    .rev()
                    .filter_map(|node| self.element_name(*node))
                    .find(|name| is_heading(name))
                    .map(str::to_owned)
                {
                    self.pop_through(&name);
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "br" => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.process_in_body(&Token::StartTag(empty_tag("br")))
            }
            // "An end tag whose tag name is one of: 'a', 'b', 'big', 'code',
            // 'em', 'font', 'i', 'nobr', 's', 'small', 'strike', 'strong',
            // 'tt', 'u'": run the adoption agency algorithm for the token.
            Token::EndTag(tag) if is_formatting_element_name(&tag.name) => {
                self.run_adoption_agency_algorithm(&tag.name);
                Action::Consumed
            }
            Token::EndTag(tag) => {
                self.pop_through_if_open(&tag.name);
                Action::Consumed
            }
            // "If the stack of template insertion modes is not empty, then
            // process the token using the rules for the 'in template' insertion
            // mode." (13.2.6.4.7)
            Token::Eof if !self.template_modes.is_empty() => self.process_in_template(token),
            Token::Eof => Action::Consumed,
        }
    }

    /// "An end tag whose tag name is one of: 'address', 'article', 'aside',
    /// 'blockquote', 'button', 'center', 'details', 'dialog', 'dir', 'div',
    /// 'dl', 'fieldset', 'figcaption', 'figure', 'footer', 'header', 'hgroup',
    /// 'listing', 'main', 'menu', 'nav', 'ol', 'pre', 'search', 'section',
    /// 'select', 'summary', 'ul'" (13.2.6.4.7), the end-tag half of the same list
    /// the block start-tag arm uses.
    fn is_block_end(name: &str) -> bool {
        is_block_start(name)
            || matches!(
                name,
                "button" | "center" | "hgroup" | "listing" | "pre" | "select"
            )
    }

    /// "If the stack of open elements has a [name] element in scope" (13.2.4.2),
    /// where the scope boundaries are the "has an element in scope" element
    /// types.
    ///
    /// The specific-scope algorithm has three outcomes, which is why this is a
    /// loop and not a search: "If node is target node, terminate in a match
    /// state. Otherwise, if node is one of the element types in list, terminate
    /// in a failure state." The order matters for an element that is itself a
    /// boundary, which is how a second `select` inside a `select` is found to be
    /// in scope.
    fn has_element_name_in_scope(&self, name: &str) -> bool {
        for node in self.open_elements.iter().rev() {
            if self.is_html_element(*node) && self.element_name(*node) == Some(name) {
                return true;
            }
            if self.is_scope_boundary(*node) {
                return false;
            }
        }
        false
    }

    fn has_select_in_scope(&self) -> bool {
        self.has_element_name_in_scope("select")
    }

    /// The last entry in the list of active formatting elements that is an
    /// element named `name` and is between the end of the list and the last
    /// marker, if any, or the start of the list otherwise (13.2.4.3). Returns its
    /// index in the list.
    fn find_formatting_element_since_marker(&self, name: &str) -> Option<usize> {
        let start = self.after_last_marker();
        (start..self.active_formatting.len()).rev().find(|index| {
            self.active_formatting[*index]
                .node()
                .is_some_and(|node| self.element_name(node) == Some(name))
        })
    }

    /// "Pop elements from the stack of open elements until an HTML element with
    /// the same tag name as the token has been popped from the stack" (13.2.6.4.7),
    /// preceded by "generate implied end tags".
    fn close_through_implied_end_tags(&mut self, name: &str) {
        self.generate_implied_end_tags_except(None);
        while let Some(node) = self.open_elements.pop() {
            if self.is_html_element(node) && self.element_name(node) == Some(name) {
                return;
            }
        }
    }

    /// Pop every occurrence of `node` from the stack of open elements, wherever
    /// it is. "Remove node from the stack of open elements" (13.2.6.4.7) is a
    /// random-access removal, not a pop: for the `</form>` rules the element being
    /// removed is the one the form element pointer named, which need not be the
    /// current node.
    fn remove_from_open_elements(&mut self, node: NodeId) {
        self.open_elements.retain(|open| *open != node);
    }

    /// "To generate implied end tags, except for [name]" (13.2.6.3): pop the
    /// current node for as long as it is one of the elements that get an implied
    /// end tag, stopping at `name` when one is given.
    ///
    /// This tree builder already closes most of these when the start tag that
    /// implies the end tag is seen, so this is often a no-op. It is not always
    /// one: the `option` and `optgroup` arms of the "in body" mode only pop an
    /// `option`, so an `li` left open by `<li>a<optgroup>` is popped here.
    fn generate_implied_end_tags_except(&mut self, except: Option<&str>) {
        while let Some(current) = self.open_elements.last().copied() {
            if !self.is_html_element(current) {
                return;
            }
            let Some(name) = self.element_name(current) else {
                return;
            };
            if !is_implied_end_tag_name(name) || Some(name) == except {
                return;
            }
            self.pop_current();
        }
    }

    fn process_in_table(&mut self, token: &Token) -> Action {
        match token {
            // "A character token, if the current node is table, tbody, template,
            // tfoot, thead, or tr element: Let the pending table character tokens
            // be an empty list of tokens. Set the original insertion mode to the
            // current insertion mode. Switch the insertion mode to 'in table
            // text' and reprocess the token."
            //
            // A CDATA section is character data (13.2.5.69), so it is subject to
            // the same rule.
            Token::Character(_) | Token::Cdata(_) if self.is_table_text_context() => {
                self.pending_table_characters.clear();
                self.original_mode = self.mode;
                self.mode = InsertionMode::InTableText;
                Action::Reprocess
            }
            Token::Comment(data) => {
                self.insert_comment(self.current_node(), data);
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            // "A start tag whose tag name is 'form'" (13.2.6.4.9): parse error;
            // if the form element pointer is not null and the parser is not
            // parsing template contents, ignore the token. Otherwise insert an
            // HTML element for the token, point the form element pointer at it if
            // the parser is not parsing template contents, and pop that form
            // element off the stack of open elements.
            //
            // The form stays in the tree — a `form` inside a `table` is inserted
            // into the table, not foster parented — but it leaves the stack, which
            // is what lets the following table content be inserted as a sibling of
            // the form rather than inside it.
            Token::StartTag(tag) if tag.name == "form" => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                if self.form_element_pointer.is_some() && !self.has_open_template() {
                    return Action::Consumed;
                }
                if let Some(form) = self.insert_element(tag, true) {
                    if !self.has_open_template() {
                        self.form_element_pointer = Some(form);
                    }
                    self.pop_current();
                }
                Action::Consumed
            }
            // "A start tag whose tag name is 'caption'": clear the stack back to a
            // table context, insert a marker at the end of the list of active
            // formatting elements, then insert an HTML element for the token and
            // switch the insertion mode to "in caption".
            Token::StartTag(tag) if tag.name == "caption" => {
                self.clear_stack_to_table_context();
                self.push_active_formatting_marker();
                self.insert_element(tag, true);
                self.mode = InsertionMode::InCaption;
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "colgroup" => {
                self.clear_stack_to_table_context();
                self.insert_element(tag, true);
                self.mode = InsertionMode::InColumnGroup;
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "col" => {
                self.clear_stack_to_table_context();
                self.insert_element(&empty_tag("colgroup"), true);
                self.mode = InsertionMode::InColumnGroup;
                Action::Reprocess
            }
            Token::StartTag(tag) if matches!(tag.name.as_str(), "tbody" | "tfoot" | "thead") => {
                self.clear_stack_to_table_context();
                self.insert_element(tag, true);
                self.mode = InsertionMode::InTableBody;
                Action::Consumed
            }
            Token::StartTag(tag) if matches!(tag.name.as_str(), "tr" | "td" | "th") => {
                self.clear_stack_to_table_context();
                self.insert_element(&empty_tag("tbody"), true);
                self.mode = InsertionMode::InTableBody;
                Action::Reprocess
            }
            Token::EndTag(tag) if tag.name == "table" => {
                if self.has_open_element("table") {
                    self.pop_through("table");
                    self.reset_insertion_mode();
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
                Action::Consumed
            }
            Token::StartTag(tag)
                if matches!(tag.name.as_str(), "style" | "script" | "template") =>
            {
                self.process_in_head(token)
            }
            Token::EndTag(tag) if tag.name == "template" => self.process_in_head(token),
            Token::Eof => self.process_in_body(token),
            _ => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.foster_parenting = true;
                let result = self.process_in_body(token);
                self.foster_parenting = false;
                result
            }
        }
    }

    /// The "in table text" insertion mode (13.2.6.4.10).
    ///
    /// Character tokens inside a table are collected rather than inserted one at
    /// a time, so that a run that turns out to contain anything other than
    /// whitespace can be reprocessed as a group through the "in table" mode's
    /// "anything else" entry, which is what foster parents it.
    fn process_in_table_text(&mut self, token: &Token) -> Action {
        match token {
            // "A character token that is U+0000 NULL: Parse error. Ignore the
            // token." The data state has already replaced U+0000 with U+FFFD, so
            // this is only reachable for a CDATA section, which cannot be
            // tokenized in a table.
            Token::Character(data) if data == "\0" => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            // "Any other character token: Append the character token to the
            // pending table character tokens list."
            Token::Character(data) | Token::Cdata(data) => {
                self.pending_table_characters.push_str(data);
                Action::Consumed
            }
            // "Anything else: If any of the tokens in the pending table character
            // tokens list are character tokens that are not ASCII whitespace,
            // then this is a parse error: reprocess the character tokens in the
            // pending table character tokens list using the rules given in the
            // 'anything else' entry in the 'in table' insertion mode. Otherwise,
            // insert the characters given by the pending table character tokens
            // list. Switch the insertion mode to the original insertion mode and
            // reprocess the token."
            _ => {
                let pending = std::mem::take(&mut self.pending_table_characters);
                if !pending.is_empty() {
                    if !is_all_html_whitespace(&pending) {
                        self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                        // "reprocess the character tokens in the pending table
                        // character tokens list using the rules given in the
                        // 'anything else' entry in the 'in table' insertion
                        // mode", and that entry is: "Parse error. Enable foster
                        // parenting, process the token using the rules for the
                        // 'in body' insertion mode, and then disable foster
                        // parenting." Enabling foster parenting is part of the
                        // step, not something the caller has already done.
                        //
                        // The pending characters are reprocessed, and then the
                        // *current* token is reprocessed too, which is what
                        // finally closes the table.
                        self.foster_parenting = true;
                        let _ = self.process_in_body(&Token::Character(pending));
                        self.foster_parenting = false;
                    } else {
                        self.insert_text(&pending);
                    }
                }
                self.mode = self.original_mode;
                Action::Reprocess
            }
        }
    }

    /// "If foster parenting is enabled and target is a table, tbody, tfoot, thead,
    /// or tr element" (13.2.6.1), the condition under which a node is foster
    /// parented rather than inserted at the target.
    fn foster_parenting_applies_to(&self, target: NodeId) -> bool {
        self.foster_parenting && self.is_table_context_element(target)
    }

    /// Whether the node is one of the table elements the foster-parenting test
    /// names.
    fn is_table_context_element(&self, node: NodeId) -> bool {
        self.is_html_element(node)
            && matches!(
                self.element_name(node),
                Some("table" | "tbody" | "tfoot" | "thead" | "tr")
            )
    }

    /// The "in table" character-token arm's condition: the current node is a
    /// table, tbody, template, tfoot, thead, or tr element (13.2.6.4.9). `template`
    /// is in this list but not in the foster-parenting test above, which is why
    /// the two are separate.
    fn is_table_text_context(&self) -> bool {
        let current = self.current_node();
        self.is_html_element(current)
            && matches!(
                self.element_name(current),
                Some("table" | "tbody" | "template" | "tfoot" | "thead" | "tr")
            )
    }

    fn process_in_caption(&mut self, token: &Token) -> Action {
        match token {
            // "An end tag whose tag name is 'caption'": if the stack of open
            // elements does not have a caption element in table scope, this is a
            // parse error; ignore the token. Otherwise generate implied end tags,
            // pop until a caption element has been popped, clear the list of
            // active formatting elements up to the last marker, and switch the
            // insertion mode to "in table".
            Token::EndTag(tag) if tag.name == "caption" => {
                if self.has_open_element("caption") {
                    self.generate_implied_end_tags_except(None);
                    self.pop_through("caption");
                    self.clear_active_formatting_up_to_last_marker();
                    self.mode = InsertionMode::InTable;
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
                Action::Consumed
            }
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                if self.has_open_element("caption") {
                    self.pop_through("caption");
                    self.mode = InsertionMode::InTable;
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
            Token::EndTag(tag) if tag.name == "table" => {
                if self.has_open_element("caption") {
                    self.pop_through("caption");
                    self.mode = InsertionMode::InTable;
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
            Token::EndTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "body"
                        | "col"
                        | "colgroup"
                        | "html"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            _ => self.process_in_body(token),
        }
    }

    fn process_in_column_group(&mut self, token: &Token) -> Action {
        match token {
            Token::Character(data) if is_all_html_whitespace(data) => {
                self.insert_text(data);
                Action::Consumed
            }
            Token::Comment(data) => {
                self.insert_comment(self.current_node(), data);
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "html" => self.process_in_body(token),
            Token::StartTag(tag) if tag.name == "template" => self.process_in_head(token),
            Token::EndTag(tag) if tag.name == "template" => self.process_in_head(token),
            Token::StartTag(tag) if tag.name == "col" => {
                self.insert_element(tag, false);
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "colgroup" => {
                if self.current_tag() == Some("colgroup") {
                    self.pop_current();
                    self.mode = InsertionMode::InTable;
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "col" => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::Eof => self.process_in_body(token),
            _ => {
                if self.current_tag() == Some("colgroup") {
                    self.pop_current();
                    self.mode = InsertionMode::InTable;
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
        }
    }

    fn process_in_table_body(&mut self, token: &Token) -> Action {
        match token {
            Token::StartTag(tag) if tag.name == "tr" => {
                self.clear_stack_to_table_body_context();
                self.insert_element(tag, true);
                self.mode = InsertionMode::InRow;
                Action::Consumed
            }
            Token::StartTag(tag) if matches!(tag.name.as_str(), "td" | "th") => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.clear_stack_to_table_body_context();
                self.insert_element(&empty_tag("tr"), true);
                self.mode = InsertionMode::InRow;
                Action::Reprocess
            }
            Token::EndTag(tag) if matches!(tag.name.as_str(), "tbody" | "tfoot" | "thead") => {
                if self.has_open_element(&tag.name) {
                    self.clear_stack_to_table_body_context();
                    self.pop_current();
                    self.mode = InsertionMode::InTable;
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
                Action::Consumed
            }
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead"
                ) =>
            {
                if self.has_table_body_in_scope() {
                    self.clear_stack_to_table_body_context();
                    self.pop_current();
                    self.mode = InsertionMode::InTable;
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
            Token::EndTag(tag) if tag.name == "table" => {
                if self.has_table_body_in_scope() {
                    self.clear_stack_to_table_body_context();
                    self.pop_current();
                    self.mode = InsertionMode::InTable;
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
            _ => self.process_in_table(token),
        }
    }

    fn process_in_row(&mut self, token: &Token) -> Action {
        match token {
            // "A start tag whose tag name is one of: 'th', 'td'": clear the stack
            // back to a table row context, insert an HTML element for the token,
            // then switch the insertion mode to "in cell", and insert a marker at
            // the end of the list of active formatting elements.
            Token::StartTag(tag) if matches!(tag.name.as_str(), "td" | "th") => {
                self.clear_stack_to_table_row_context();
                self.insert_element(tag, true);
                self.mode = InsertionMode::InCell;
                self.push_active_formatting_marker();
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "tr" => {
                if self.has_open_element("tr") {
                    self.clear_stack_to_table_row_context();
                    self.pop_current();
                    self.mode = InsertionMode::InTableBody;
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "tr" => {
                if self.has_open_element("tr") {
                    self.clear_stack_to_table_row_context();
                    self.pop_current();
                    self.mode = InsertionMode::InTableBody;
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
            Token::EndTag(tag)
                if matches!(tag.name.as_str(), "table" | "tbody" | "tfoot" | "thead") =>
            {
                if self.has_open_element("tr") {
                    self.clear_stack_to_table_row_context();
                    self.pop_current();
                    self.mode = InsertionMode::InTableBody;
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
            _ => self.process_in_table(token),
        }
    }

    fn process_in_cell(&mut self, token: &Token) -> Action {
        match token {
            // "An end tag whose tag name is one of: 'td', 'th'": if the stack of
            // open elements does not have an element in table scope that is an
            // HTML element with the same tag name, this is a parse error; ignore
            // the token. Otherwise generate implied end tags, pop until it has
            // been popped, clear the list of active formatting elements up to the
            // last marker, and switch the insertion mode to "in row".
            Token::EndTag(tag) if matches!(tag.name.as_str(), "td" | "th") => {
                if self.has_open_element(&tag.name) {
                    self.generate_implied_end_tags_except(None);
                    self.pop_through(&tag.name);
                    self.clear_active_formatting_up_to_last_marker();
                    self.mode = InsertionMode::InRow;
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
                Action::Consumed
            }
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                if self.has_cell_in_scope() {
                    self.close_current_cell();
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
            Token::EndTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "table" | "tbody" | "tfoot" | "thead" | "tr"
                ) =>
            {
                if self.has_cell_in_scope() {
                    self.close_current_cell();
                    Action::Reprocess
                } else {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    Action::Consumed
                }
            }
            _ => self.process_in_body(token),
        }
    }

    /// The "in template" insertion mode (13.2.6.4.16).
    ///
    /// A template's contents are parsed with this mode until a token switches
    /// the insertion mode, and the mode switched to becomes the current
    /// template insertion mode, so that leaving the template's contents for a
    /// `</template>` end tag can be picked up again.
    fn process_in_template(&mut self, token: &Token) -> Action {
        match token {
            // "A character token", "A comment token", and "A DOCTYPE token" are
            // processed using the rules for the "in body" insertion mode.
            Token::Character(_) | Token::Cdata(_) | Token::Comment(_) | Token::Doctype(_) => {
                self.process_in_body(token)
            }
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "base"
                        | "basefont"
                        | "bgsound"
                        | "link"
                        | "meta"
                        | "noframes"
                        | "script"
                        | "style"
                        | "template"
                        | "title"
                ) =>
            {
                self.process_in_head(token)
            }
            Token::EndTag(tag) if tag.name == "template" => self.process_in_head(token),
            Token::StartTag(tag) => {
                let mode = match tag.name.as_str() {
                    "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => InsertionMode::InTable,
                    "col" => InsertionMode::InColumnGroup,
                    "tr" => InsertionMode::InTableBody,
                    "td" | "th" => InsertionMode::InRow,
                    _ => InsertionMode::InBody,
                };
                // "Pop the current template insertion mode off the stack of
                // template insertion modes. Push <mode> onto the stack of
                // template insertion modes so that it is the new current
                // template insertion mode. Switch the insertion mode to <mode>,
                // and reprocess the token."
                self.replace_current_template_mode(mode);
                self.mode = mode;
                Action::Reprocess
            }
            Token::EndTag(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::Eof => {
                if !self.has_open_template() {
                    // "If there is no template element on the stack of open
                    // elements, then stop parsing. (fragment case)"
                    return Action::Consumed;
                }
                // "This is a parse error. Pop elements from the stack of open
                // elements until a template element has been popped from the
                // stack. Clear the list of active formatting elements up to the
                // last marker. Pop the current template insertion mode off the
                // stack of template insertion modes. Reset the insertion mode
                // appropriately. Reprocess the token."  An end-of-file token
                // ends the parse either way, so the reprocess only has to leave
                // the insertion mode consistent.
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.pop_through_template();
                self.clear_active_formatting_up_to_last_marker();
                self.pop_current_template_mode();
                self.reset_insertion_mode();
                self.process_in_body(token)
            }
        }
    }

    /// A `template` start tag, in the "in head" insertion mode (13.2.6.4.4):
    /// insert a marker at the end of the list of active formatting elements,
    /// switch to "in template", push it onto the stack of template insertion
    /// modes, and insert the element.
    ///
    /// "Set the frameset-ok flag to 'not ok'" is not run: this tree builder does
    /// not track that flag because no frameset insertion mode is implemented.
    ///
    /// The insertion mode changes before the element is inserted, so the element
    /// lands where the document is being built while its contents will be parsed
    /// by the new mode.
    fn enter_template(&mut self, tag: &TagToken) {
        // "Insert a marker at the end of the list of active formatting
        // elements." The marker is what stops formatting from leaking into or out
        // of a template's contents.
        self.push_active_formatting_marker();
        self.mode = InsertionMode::InTemplate;
        self.template_modes.push(InsertionMode::InTemplate);
        self.insert_element(tag, true);
    }

    /// An end tag whose tag name is "template", in the "in head" insertion mode
    /// (13.2.6.4.4).
    fn close_template(&mut self) -> Action {
        if !self.has_open_template() {
            // "If there is no template element on the stack of open elements,
            // then this is a parse error; ignore the token."
            self.parse_error(HtmlParseErrorCode::UnexpectedToken);
            return Action::Consumed;
        }
        // "Generate all implied end tags thoroughly. If the current node is not a
        // template element, then this is a parse error."  Implied end tags are
        // only generated for formatting elements and `dd`/`dt`/`li`/`optgroup`/
        // `option`/`p`/`rb`/`rp`/`rt`/`rtc`, and this tree builder pops those
        // eagerly on the matching start tags, so the current node is the
        // template whenever a template is open.
        if self.current_tag() != Some("template") {
            self.parse_error(HtmlParseErrorCode::UnexpectedToken);
        }
        // The last template on the stack is the one that is about to be popped,
        // and its insertion target is always null because a plain template has no
        // declarative shadow root.
        self.pop_through_template();
        // "Clear the list of active formatting elements up to the last marker."
        self.clear_active_formatting_up_to_last_marker();
        self.pop_current_template_mode();
        self.reset_insertion_mode();
        Action::Consumed
    }

    /// "Pop the current template insertion mode off the stack of template
    /// insertion modes. Push `mode` onto the stack of template insertion modes
    /// so that it is the new current template insertion mode."
    fn replace_current_template_mode(&mut self, mode: InsertionMode) {
        self.pop_current_template_mode();
        self.template_modes.push(mode);
    }

    fn pop_current_template_mode(&mut self) {
        self.template_modes.pop();
    }

    /// The current template insertion mode: the one most recently pushed, and
    /// "in template" for a template that has not switched modes yet.
    fn current_template_mode(&self) -> InsertionMode {
        self.template_modes
            .last()
            .copied()
            .unwrap_or(InsertionMode::InTemplate)
    }

    /// "When the steps below require the UA to push onto the list of active
    /// formatting elements an element element" (13.2.4.3).
    ///
    /// The Noah's Ark clause: "If there are already three elements in the list
    /// of active formatting elements after the last marker, if any, or anywhere
    /// in the list if there are no markers, that have the same tag name,
    /// namespace, and attributes as element, then remove the earliest such
    /// element from the list of active formatting elements."
    fn push_active_formatting_element(&mut self, node: NodeId, token: &TagToken) {
        let start = self.after_last_marker();
        let matches: Vec<usize> = (start..self.active_formatting.len())
            .filter(|index| {
                self.active_formatting[*index]
                    .node()
                    .is_some_and(|other| self.same_formatting_element(other, node))
            })
            .collect();
        if matches.len() >= 3 {
            // The earliest such element, which is the lowest list index.
            self.active_formatting.remove(matches[0]);
        }
        self.active_formatting.push(ActiveFormattingEntry::Element {
            node,
            token: token.clone(),
        });
    }

    /// "If there are already three elements ... after the last marker, if any, or
    /// anywhere in the list if there are no markers": the index one past the
    /// last marker, or the start of the list.
    fn after_last_marker(&self) -> usize {
        self.active_formatting
            .iter()
            .rposition(|entry| matches!(entry, ActiveFormattingEntry::Marker))
            .map_or(0, |index| index + 1)
    }

    /// Two entries match for the Noah's Ark clause when they have the same tag
    /// name, namespace, and attributes. "For these purposes, the attributes
    /// must be compared as they were when the elements were created by the
    /// parser; two elements have the same attributes if all their parsed
    /// attributes can be paired such that the two attributes in each pair have
    /// identical names, namespaces, and values (the order of the attributes
    /// does not matter)."
    ///
    /// A parser-created element's attributes are never mutated afterwards, so
    /// the element's current attributes are the parsed ones. Order does not
    /// matter, which is why this pairs rather than comparing sequences.
    fn same_formatting_element(&self, left: NodeId, right: NodeId) -> bool {
        let (Some(left_namespace), Some(right_namespace)) =
            (self.element_namespace(left), self.element_namespace(right))
        else {
            return false;
        };
        if left_namespace != right_namespace || self.element_name(left) != self.element_name(right)
        {
            return false;
        }
        let Some(NodeKind::Element(left_data)) = self.dom.node(left).map(render_dom::Node::kind)
        else {
            return false;
        };
        let Some(NodeKind::Element(right_data)) = self.dom.node(right).map(render_dom::Node::kind)
        else {
            return false;
        };
        if left_data.attributes.len() != right_data.attributes.len() {
            return false;
        }
        // Each attribute of the left element must pair with exactly one
        // attribute of the right element. An attribute is identified by its
        // namespace and local name, not its prefix.
        left_data.attributes.iter().all(|wanted| {
            right_data.attributes.iter().any(|candidate| {
                candidate.namespace == wanted.namespace
                    && candidate.local_name == wanted.local_name
                    && candidate.value == wanted.value
            })
        })
    }

    /// "Insert a marker at the end of the list of active formatting elements"
    /// (13.2.4.3).
    fn push_active_formatting_marker(&mut self) {
        self.active_formatting.push(ActiveFormattingEntry::Marker);
    }

    /// "To clear the list of active formatting elements up to the last marker"
    /// (13.2.4.3): pop entries from the end until the last one popped is a
    /// marker, or until the list is empty.
    fn clear_active_formatting_up_to_last_marker(&mut self) {
        while let Some(entry) = self.active_formatting.pop() {
            if matches!(entry, ActiveFormattingEntry::Marker) {
                break;
            }
        }
    }

    /// Whether the node is one of the entries in the list of active formatting
    /// elements, which is what the reconstruction step tests.
    fn is_in_active_formatting(&self, node: NodeId) -> bool {
        self.active_formatting
            .iter()
            .any(|entry| entry.node() == Some(node))
    }

    /// "When the steps below require the UA to reconstruct the active formatting
    /// elements" (13.2.4.3).
    ///
    /// Re-opens the formatting elements that are still in the list but no longer
    /// on the stack of open elements, innermost last, so their contents carry
    /// forward.
    fn reconstruct_active_formatting_elements(&mut self) {
        if self.active_formatting.is_empty() {
            return;
        }
        // "If the last (most recently added) entry in the list of active
        // formatting elements is a marker, or if it is an element that is in the
        // stack of open elements, then there is nothing to reconstruct."
        if matches!(
            self.active_formatting.last(),
            Some(ActiveFormattingEntry::Marker)
        ) || self
            .active_formatting
            .last()
            .and_then(ActiveFormattingEntry::node)
            .is_some_and(|node| self.open_elements.contains(&node))
        {
            return;
        }
        // Rewind: "If there are no entries before entry in the list of active
        // formatting elements, then jump to the step labeled create. Let entry be
        // the entry one earlier than entry in the list. If entry is neither a
        // marker nor an element that is also in the stack of open elements, go to
        // the step labeled rewind."
        //
        // The walk stops at the last marker or still-open element, and the Advance
        // step then moves one later, so the re-opening starts just after that
        // entry. With no such entry at all the whole list is re-opened, which is
        // what makes three unclosed `b` elements re-open all three rather than
        // only the last.
        let mut start = 0;
        for index in (0..self.active_formatting.len()).rev() {
            let is_boundary = match &self.active_formatting[index] {
                ActiveFormattingEntry::Marker => true,
                ActiveFormattingEntry::Element { node, .. } => self.open_elements.contains(node),
            };
            if is_boundary {
                start = index + 1;
                break;
            }
        }
        // Advance and Create: re-open every element entry from `start`,
        // outermost first, so the elements nest in the order they were opened.
        for index in start..self.active_formatting.len() {
            let Some(ActiveFormattingEntry::Element { node, token }) =
                self.active_formatting.get(index).cloned()
            else {
                continue;
            };
            if self.open_elements.contains(&node) {
                continue;
            }
            // "Create: Insert an HTML element for the token for which the element
            // entry was created, to obtain new element. Replace the entry for
            // entry in the list with an entry for new element."
            if let Some(new_element) = self.insert_element(&token, true) {
                self.active_formatting[index] = ActiveFormattingEntry::Element {
                    node: new_element,
                    token,
                };
            }
        }
    }

    /// The adoption agency algorithm (13.2.6.4.7), run for the end tag of a
    /// formatting element and for the `a` and `nobr` start tags.
    ///
    /// This is what makes mis-nested formatting elements produce the trees the
    /// spec describes: elements move between parents, and entries whose element
    /// has been closed are replaced rather than reopened.
    fn run_adoption_agency_algorithm(&mut self, subject: &str) {
        // "If the current node is an HTML element whose tag name is subject, and
        // the current node is not in the list of active formatting elements, then
        // pop the current node off the stack of open elements and return."
        if self.current_tag() == Some(subject)
            && self.is_html_element(self.current_node())
            && !self.is_in_active_formatting(self.current_node())
        {
            self.pop_current();
            return;
        }
        let mut outer_loop_counter = 0;
        loop {
            if outer_loop_counter >= 8 {
                return;
            }
            outer_loop_counter += 1;
            // "Let formattingElement be the last element in the list of active
            // formatting elements that is between the end of the list and the
            // last marker in the list, if any, or the start of the list
            // otherwise, and has the tag name subject."
            let start = self.after_last_marker();
            let formatting_index = (start..self.active_formatting.len()).rev().find(|index| {
                self.active_formatting[*index].node().is_some_and(|node| {
                    self.element_name(node) == Some(subject) && self.is_html_element(node)
                })
            });
            let Some(formatting_index) = formatting_index else {
                // "If there is no such element, then act as described in the
                // 'any other end tag' entry above and return."
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.pop_through_if_open(subject);
                return;
            };
            let Some(formatting_element) = self.active_formatting[formatting_index].node() else {
                return;
            };
            let Some(stack_index) = self
                .open_elements
                .iter()
                .position(|n| *n == formatting_element)
            else {
                // "If formattingElement is not in the stack of open elements, then
                // this is a parse error; remove the element from the list, and
                // return."
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.active_formatting.remove(formatting_index);
                return;
            };
            // "If formattingElement is in the stack of open elements, but the
            // element is not in scope, then this is a parse error; return."
            if !self.has_element_in_scope(formatting_element) {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                return;
            }
            if stack_index + 1 != self.open_elements.len() {
                // "If formattingElement is not the current node, this is a parse
                // error. (But do not return.)"
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
            }
            // "Let furthestBlock be the topmost node in the stack of open
            // elements that is lower in the stack than formattingElement, and is
            // an element in the special category."
            let Some(furthest_block) = self.open_elements[stack_index + 1..]
                .iter()
                .copied()
                .find(|node| self.is_special_element(*node))
            else {
                // "If there is no furthestBlock, then the UA must first pop all
                // the nodes from the bottom of the stack of open elements, from
                // the current node up to and including formattingElement, then
                // remove formattingElement from the list of active formatting
                // elements, and finally return."
                let target = self
                    .open_elements
                    .iter()
                    .position(|n| *n == formatting_element)
                    .unwrap_or(0);
                self.open_elements.truncate(target);
                self.active_formatting.remove(formatting_index);
                return;
            };
            // "Let commonAncestor be the element immediately above
            // formattingElement in the stack of open elements."
            let common_ancestor = self.open_elements[stack_index - 1];
            // "Let a bookmark note the position of formattingElement in the list
            // of active formatting elements relative to the elements on either
            // side of it in the list." The list is only ever indexed by the
            // algorithm, so the bookmark is the index it notes, and the algorithm
            // moves it when a new element takes the slot.
            let mut bookmark = formatting_index;
            let mut node = furthest_block;
            let mut last_node = furthest_block;
            let mut inner_loop_counter = 0;
            loop {
                inner_loop_counter += 1;
                let Some(node_index) = self.open_elements.iter().position(|n| *n == node) else {
                    return;
                };
                // "Let node be the element immediately above node in the stack of
                // open elements, or if node is no longer in the stack of open
                // elements ... the element that was immediately above node in the
                // stack of open elements before node was removed."
                if node_index == 0 {
                    break;
                }
                let above = self.open_elements[node_index - 1];
                if above == formatting_element {
                    break;
                }
                let list_index = self
                    .active_formatting
                    .iter()
                    .position(|entry| entry.node() == Some(above));
                if inner_loop_counter > 3 {
                    // "If innerLoopCounter is greater than 3 and node is in the
                    // list of active formatting elements, then remove node from
                    // the list of active formatting elements."
                    if let Some(index) = list_index {
                        self.active_formatting.remove(index);
                    }
                }
                let Some(list_index) =
                    list_index.filter(|_| inner_loop_counter <= 3).or_else(|| {
                        // Having just been removed from the list, `above` is no longer
                        // in it, so the next step's condition holds.
                        self.active_formatting
                            .iter()
                            .position(|entry| entry.node() == Some(above))
                    })
                else {
                    // "If node is not in the list of active formatting elements,
                    // then remove node from the stack of open elements and
                    // continue."
                    self.open_elements.remove(node_index);
                    node = above;
                    continue;
                };
                // "Create an element for the token for which the element node was
                // created, in the HTML namespace, with commonAncestor as the
                // intended parent; replace the entry for node in the list of
                // active formatting elements with an entry for the new element,
                // replace the entry for node in the stack of open elements with
                // an entry for the new element, and let node be the new element."
                let Some(ActiveFormattingEntry::Element { token, .. }) =
                    self.active_formatting.get(list_index).cloned()
                else {
                    // A marker cannot be the node the algorithm is walking past.
                    self.open_elements.remove(node_index);
                    node = above;
                    continue;
                };
                let new_node = self
                    .dom
                    .create_element_ns(Namespace::Html, token.name.clone());
                self.apply_attributes(new_node, &token);
                self.active_formatting[list_index] = ActiveFormattingEntry::Element {
                    node: new_node,
                    token,
                };
                if let Some(entry) = self.open_elements.get_mut(node_index) {
                    *entry = new_node;
                }
                node = new_node;
                // "If lastNode is furthestBlock, then move the aforementioned
                // bookmark to be immediately after the new node in the list of
                // active formatting elements."
                if last_node == furthest_block
                    && let Some(new_index) = self
                        .active_formatting
                        .iter()
                        .position(|entry| entry.node() == Some(new_node))
                {
                    bookmark = new_index + 1;
                }
                // "Append lastNode to node."
                if self.dom.append_child(node, last_node).is_err() {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
                last_node = node;
            }
            // "Let (target, refNode) be the adjusted insertion location given
            // (commonAncestor, null). If lastNode's parent is non-null, then
            // remove lastNode. If all of the following are true: lastNode's
            // parent is null; lastNode is not a host-including inclusive
            // ancestor of target; target is not a Document node, or it does not
            // have an element child; and refNode is null or its parent is target,
            // then insert lastNode into target before refNode."
            //
            // `refNode` is null, so the conditions reduce to the cycle check,
            // and appending to `commonAncestor` is what detaches lastNode from
            // its old parent: `Dom::append_child` refuses an insertion that
            // would make a node its own inclusive ancestor, so the "not a
            // host-including inclusive ancestor of target" condition is enforced
            // by the DOM rather than re-checked here.
            if self.dom.append_child(common_ancestor, last_node).is_err() {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
            }
            // "Create an element for the token for which formattingElement was
            // created, in the HTML namespace, with furthestBlock as the intended
            // parent. Take all of the child nodes of furthestBlock and append
            // them to the element created in the last step. Append that new
            // element to furthestBlock."
            let token = match &self.active_formatting[formatting_index] {
                ActiveFormattingEntry::Element { token, .. } => token.clone(),
                ActiveFormattingEntry::Marker => return,
            };
            let new_formatting_element = self
                .dom
                .create_element_ns(Namespace::Html, token.name.clone());
            self.apply_attributes(new_formatting_element, &token);
            for child in self
                .dom
                .children(furthest_block)
                .unwrap_or_default()
                .to_vec()
            {
                if self
                    .dom
                    .append_child(new_formatting_element, child)
                    .is_err()
                {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                }
            }
            if self
                .dom
                .append_child(furthest_block, new_formatting_element)
                .is_err()
            {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
            }
            // "Remove formattingElement from the list of active formatting
            // elements, and insert the new element into the list of active
            // formatting elements at the position of the aforementioned bookmark."
            self.active_formatting.remove(formatting_index);
            let bookmark = bookmark.min(self.active_formatting.len());
            self.active_formatting.insert(
                bookmark,
                ActiveFormattingEntry::Element {
                    node: new_formatting_element,
                    token,
                },
            );
            // "Remove formattingElement from the stack of open elements, and
            // insert the new element into the stack of open elements immediately
            // below the position of furthestBlock in that stack."
            //
            // "The current node is the bottommost node in this stack of open
            // elements", so "below" means one position closer to the bottom,
            // which is one index *after* furthestBlock. Placing it on the other
            // side would leave the new element between furthestBlock and the old
            // formatting element's position, the next outer-loop pass would find
            // it again, and `<b>1<p>2</b>3</p>` would nest one `b` per pass.
            if let Some(index) = self
                .open_elements
                .iter()
                .position(|n| *n == formatting_element)
            {
                self.open_elements.remove(index);
            }
            if let Some(index) = self.open_elements.iter().position(|n| *n == furthest_block) {
                self.open_elements
                    .insert(index.saturating_add(1), new_formatting_element);
            }
        }
    }

    /// "If the stack of open elements has an element in scope that is an HTML
    /// element with the same tag name as the token" (13.2.6.4.7), and its
    /// namespace-aware form used by the adoption agency algorithm.
    fn has_element_in_scope(&self, node: NodeId) -> bool {
        let Some(name) = self.element_name(node) else {
            return false;
        };
        for candidate in self.open_elements.iter().rev() {
            if *candidate == node {
                return true;
            }
            if self.is_scope_boundary(*candidate) {
                return false;
            }
            if self.element_name(*candidate) == Some(name) && self.is_html_element(*candidate) {
                return true;
            }
        }
        false
    }

    /// The element types of the "has an element in scope" algorithm (13.2.4.2).
    ///
    /// `button` is a boundary of *button* scope only, and no algorithm in this
    /// tree builder needs button scope, so it is not a boundary here.
    fn is_scope_boundary(&self, node: NodeId) -> bool {
        let name = self.element_name(node);
        if self.is_html_element(node)
            && matches!(
                name,
                Some(
                    "applet"
                        | "caption"
                        | "html"
                        | "table"
                        | "td"
                        | "th"
                        | "marquee"
                        | "object"
                        | "select"
                        | "template"
                )
            )
        {
            return true;
        }
        // MathML text integration points and MathML annotation-xml, and the SVG
        // HTML integration points, are boundaries of every scope.
        if self.element_namespace(node) == Some(Namespace::MathMl)
            && matches!(
                name,
                Some("mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml")
            )
        {
            return true;
        }
        self.element_namespace(node) == Some(Namespace::Svg)
            && matches!(name, Some("foreignObject" | "desc" | "title"))
    }

    /// "An element is in the special category" (13.2.4.2). Only the membership
    /// test matters to the adoption agency algorithm, and it is the full list.
    fn is_special_element(&self, node: NodeId) -> bool {
        let Some(name) = self.element_name(node) else {
            return false;
        };
        if self.is_html_element(node) && is_special_html_element_name(name) {
            return true;
        }
        if self.element_namespace(node) == Some(Namespace::MathMl)
            && matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml")
        {
            return true;
        }
        self.element_namespace(node) == Some(Namespace::Svg)
            && matches!(name, "foreignObject" | "desc" | "title")
    }

    fn process_after_body(&mut self, token: &Token) -> Action {
        match token {
            Token::Character(data) if is_all_html_whitespace(data) => self.process_in_body(token),
            Token::Comment(data) => {
                if let Some(html) = self.open_elements.first().copied() {
                    self.insert_comment(html, data);
                }
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "html" => self.process_in_body(token),
            Token::EndTag(tag) if tag.name == "html" => {
                self.mode = InsertionMode::AfterAfterBody;
                Action::Consumed
            }
            Token::Eof => Action::Consumed,
            _ => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.mode = InsertionMode::InBody;
                Action::Reprocess
            }
        }
    }

    fn process_after_after_body(&mut self, token: &Token) -> Action {
        match token {
            Token::Comment(data) => {
                self.insert_comment(self.dom.document(), data);
                Action::Consumed
            }
            Token::Doctype(_) => self.process_in_body(token),
            Token::StartTag(tag) if tag.name == "html" => self.process_in_body(token),
            Token::Character(data) if is_all_html_whitespace(data) => self.process_in_body(token),
            Token::Eof => Action::Consumed,
            _ => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.mode = InsertionMode::InBody;
                Action::Reprocess
            }
        }
    }

    fn insert_html_root(&mut self, tag: &TagToken) {
        let element = self.dom.create_element("html");
        self.apply_attributes(element, tag);
        if self.dom.append_child(self.dom.document(), element).is_ok() {
            self.open_elements.push(element);
        } else {
            self.parse_error(HtmlParseErrorCode::UnexpectedToken);
        }
    }

    fn insert_doctype(&mut self, doctype: &DoctypeToken) {
        let node = self.dom.create_document_type(
            doctype.name.as_deref().unwrap_or_default(),
            doctype.public_id.as_deref().unwrap_or_default(),
            doctype.system_id.as_deref().unwrap_or_default(),
        );
        if self.dom.append_child(self.dom.document(), node).is_err() {
            self.parse_error(HtmlParseErrorCode::UnexpectedToken);
        }
    }

    fn insert_element(&mut self, tag: &TagToken, push: bool) -> Option<NodeId> {
        // "When a start tag token is emitted with its self-closing flag set, if
        // the flag is not acknowledged when it is processed by the tree
        // construction stage, that is a
        // non-void-html-element-start-tag-with-trailing-solidus parse error"
        // (13.2.2). No HTML insertion mode acknowledges the flag, so every start
        // tag for a non-void HTML element that arrives here is one. The check
        // lives here rather than in the arms so that it does not depend on which
        // arm a tag name happens to reach. Foreign content acknowledges the flag
        // and goes through `insert_foreign_element` instead, so it is not
        // reported there.
        if tag.self_closing && !is_void_element(&tag.name) {
            self.parse_error(HtmlParseErrorCode::NonVoidHtmlElementStartTagWithTrailingSolidus);
        }
        let element = self
            .dom
            .create_element_ns(Namespace::Html, tag.name.clone());
        self.apply_attributes(element, tag);
        let element = self.attach_element(element, push)?;
        self.associate_from_form_element_pointer(element);
        Some(element)
    }

    /// The form-owner half of "create an element for the token" (13.2.6.1):
    /// "If element is a form-associated element and not a form-associated custom
    /// element, the form element pointer is not null, the parser is not parsing
    /// template contents, the parser's fragment context element is null, element
    /// is either not listed or doesn't have a form attribute, and the intended
    /// parent is in the same tree as the element pointed to by the form element
    /// pointer, then associate element with the form element pointed to by the
    /// form element pointer and set element's parser inserted flag."
    ///
    /// "An HTML parser is parsing template contents if there is a template
    /// element on the stack of open elements" (13.2.4.4), which is
    /// [`Self::has_open_template`]. The fragment context element is always null
    /// here: this tree builder only parses whole documents, so that half of the
    /// condition holds unconditionally.
    ///
    /// The association is recorded rather than derived, because the form the
    /// pointer names need not be an ancestor of the element — that is the whole
    /// point of the pointer. `Dom::form_owner` ignores it again as soon as the
    /// element stops being the child it was created for, which is what the
    /// spec's "reset the form owner" does when the ancestor chain changes.
    fn associate_from_form_element_pointer(&mut self, element: NodeId) {
        let Some(form) = self.form_element_pointer else {
            return;
        };
        if !self.dom.is_form_associated(element) {
            return;
        }
        if self.has_open_template() {
            // "It is ignored inside template elements."
            return;
        }
        if self.dom.is_listed_element(element)
            && self.dom.attribute(element, "form").ok().flatten().is_some()
        {
            // "element is either not listed or doesn't have a form attribute": a
            // listed element that names a form itself keeps that owner, and
            // `Dom::form_owner` resolves it from the attribute.
            return;
        }
        let Some(parent) = self.dom.parent(element) else {
            return;
        };
        if !self.dom.is_in_same_tree(parent, form) {
            return;
        }
        self.dom
            .set_parser_inserted_form_owner(element, form, parent);
    }

    /// Insert an already-created element at "the appropriate place for inserting
    /// a node" (13.2.6.1).
    ///
    /// "If foster parenting is enabled and target is a table, tbody, tfoot, thead,
    /// or tr element, then [foster parent]." The test is on the *target*, so
    /// while the "in table" mode is delegating to "in body" with foster parenting
    /// enabled, a node whose target is something else — a `b` that was
    /// foster-parented out of the table and is still the current node, say — is
    /// inserted at that target rather than before the table.
    fn attach_element(&mut self, element: NodeId, push: bool) -> Option<NodeId> {
        let target = self.current_node();
        let result = if self.foster_parenting_applies_to(target) {
            let (parent, reference) = self.foster_location();
            self.dom.insert_before(parent, element, reference)
        } else {
            let parent = self.insertion_parent(target);
            self.dom.append_child(parent, element)
        };
        if result.is_err() {
            self.parse_error(HtmlParseErrorCode::UnexpectedToken);
            return None;
        }
        if push {
            self.open_elements.push(element);
        }
        Some(element)
    }

    /// "Insert a foreign element for the token, with the adjusted current
    /// node's namespace" (13.2.6.1), as the "in body" rules for the `math` and
    /// `svg` start tags also spell it (13.2.6.4.7).
    ///
    /// The element is created in `namespace` and its attributes are the token's
    /// attributes after "adjust MathML attributes", "adjust SVG attributes", and
    /// "adjust foreign attributes" have run, so the case-adjusted names the DOM
    /// deliberately does not fold (`clipPath`, `viewBox`) and the namespaced
    /// `xlink:href` survive into the tree.
    fn insert_foreign_element(
        &mut self,
        name: &str,
        attributes: &[AttributeToken],
        namespace: &Namespace,
    ) -> Option<NodeId> {
        let element = self.dom.create_element_ns(namespace.clone(), name);
        for attribute in adjust_attributes(attributes, namespace) {
            if self
                .dom
                .set_attribute_ns(
                    element,
                    attribute.namespace,
                    attribute.prefix,
                    attribute.local_name,
                    attribute.value,
                )
                .is_err()
            {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
            }
        }
        self.attach_element(element, true)
    }

    /// The "in body" rules for the `math` and `svg` start tags: the attribute
    /// adjustments followed by the insertion and the self-closing
    /// acknowledgement.
    ///
    /// "Reconstruct the active formatting elements, if any" is not run: this
    /// tree builder has no list of active formatting elements, so there is
    /// nothing to reconstruct (a pre-existing limitation, not a deviation
    /// introduced here).
    fn insert_foreign_start_tag(&mut self, tag: &TagToken, namespace: &Namespace) {
        self.insert_foreign_element(&tag.name, &tag.attributes, namespace);
        if tag.self_closing {
            // "If the token has its self-closing flag set, pop the current node
            // off the stack of open elements and acknowledge the token's
            // self-closing flag."
            self.pop_current();
        }
    }

    fn apply_attributes(&mut self, element: NodeId, tag: &TagToken) {
        for attribute in &tag.attributes {
            if self
                .dom
                .set_attribute(element, &attribute.name, &attribute.value)
                .is_err()
            {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
            }
        }
    }

    fn merge_attributes(&mut self, element: NodeId, tag: &TagToken) {
        for attribute in &tag.attributes {
            if self
                .dom
                .attribute(element, &attribute.name)
                .is_ok_and(|value| value.is_none())
            {
                let _ = self
                    .dom
                    .set_attribute(element, &attribute.name, &attribute.value);
            }
        }
    }

    fn insert_text(&mut self, data: &str) {
        if data.is_empty() {
            return;
        }
        let target = self.current_node();
        if !self.foster_parenting_applies_to(target) {
            let parent = self.insertion_parent(target);
            if self.dom.append_text(parent, data).is_err() {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
            }
            return;
        }
        let (parent, reference) = self.foster_location();
        if let Some(reference) = reference
            && let Some(previous) = self.dom.previous_sibling(reference)
            && let Some(NodeKind::Text(existing)) =
                self.dom.node(previous).map(render_dom::Node::kind)
        {
            let mut combined = existing.clone();
            combined.push_str(data);
            let _ = self.dom.set_character_data(previous, combined);
            return;
        }
        let text = self.dom.create_text(data);
        if self.dom.insert_before(parent, text, reference).is_err() {
            self.parse_error(HtmlParseErrorCode::UnexpectedToken);
        }
    }

    fn insert_comment(&mut self, parent: NodeId, data: &str) {
        let comment = self.dom.create_comment(data);
        if self
            .dom
            .append_child(self.insertion_parent(parent), comment)
            .is_err()
        {
            self.parse_error(HtmlParseErrorCode::UnexpectedToken);
        }
    }

    fn enter_text_element(&mut self, tag: &TagToken, model: ContentModel) {
        if self.insert_element(tag, true).is_some() {
            self.tokenizer.switch_to(model, Some(&tag.name));
            self.original_mode = self.mode;
            self.mode = InsertionMode::Text;
        }
    }

    fn current_node(&self) -> NodeId {
        self.open_elements
            .last()
            .copied()
            .unwrap_or_else(|| self.dom.document())
    }

    fn current_tag(&self) -> Option<&str> {
        self.element_name(self.current_node())
    }

    fn element_name(&self, node: NodeId) -> Option<&str> {
        match self.dom.node(node)?.kind() {
            NodeKind::Element(data) => Some(&data.local_name),
            _ => None,
        }
    }

    /// The element's tag name converted to ASCII lowercase, as the rules for
    /// parsing tokens in foreign content compare end tags.
    fn element_name_ascii_lowercase(&self, node: NodeId) -> Option<String> {
        self.element_name(node).map(str::to_ascii_lowercase)
    }

    fn element_matches(&self, node: NodeId, namespace: &Namespace, local_name: &str) -> bool {
        self.element_namespace(node)
            .as_ref()
            .is_some_and(|found| found == namespace)
            && self.element_name(node) == Some(local_name)
    }

    /// The element's namespace, or `None` when the node is not an element.
    fn element_namespace(&self, node: NodeId) -> Option<Namespace> {
        match self.dom.node(node)?.kind() {
            NodeKind::Element(data) => Some(data.namespace.clone()),
            _ => None,
        }
    }

    fn is_html_element(&self, node: NodeId) -> bool {
        self.element_namespace(node)
            .is_some_and(|namespace| namespace == Namespace::Html)
    }

    fn is_template_element(&self, node: NodeId) -> bool {
        self.element_matches(node, &Namespace::Html, "template")
    }

    /// "To compute the adjusted insertion location": "If targetParent is not a
    /// template element, then return (targetParent, referenceChild). If
    /// targetParent's insertion target is null, then return (targetParent's
    /// template contents, null)." (13.2.6.1)
    ///
    /// A template element's insertion target is only non-null for a declarative
    /// shadow root, which this tree builder never creates, so a template's
    /// contents always receive the node and the template element itself never
    /// gets a child.
    fn insertion_parent(&self, parent: NodeId) -> NodeId {
        if self.is_template_element(parent) {
            self.dom.template_contents(parent).unwrap_or(parent)
        } else {
            parent
        }
    }

    /// "A node is a MathML text integration point if it is one of the following
    /// elements: A MathML mi element, A MathML mo element, A MathML mn element, A
    /// MathML ms element, A MathML mtext element" (13.2.6).
    fn is_mathml_text_integration_point(&self, node: NodeId) -> bool {
        self.element_namespace(node)
            .is_some_and(|namespace| namespace == Namespace::MathMl)
            && matches!(
                self.element_name(node),
                Some("mi" | "mo" | "mn" | "ms" | "mtext")
            )
    }

    fn is_mathml_annotation_xml(&self, node: NodeId) -> bool {
        self.element_matches(node, &Namespace::MathMl, "annotation-xml")
    }

    /// "A node is an HTML integration point if it is one of the following
    /// elements: A MathML annotation-xml element whose start tag token had an
    /// attribute with the name 'encoding' whose value was an ASCII
    /// case-insensitive match for 'text/html', ... 'application/xhtml+xml', An
    /// SVG foreignObject element, An SVG desc element, An SVG title element"
    /// (13.2.6).
    fn is_html_integration_point(&self, node: NodeId) -> bool {
        if self.element_matches(node, &Namespace::Svg, "foreignObject")
            || self.element_matches(node, &Namespace::Svg, "desc")
            || self.element_matches(node, &Namespace::Svg, "title")
        {
            return true;
        }
        if !self.is_mathml_annotation_xml(node) {
            return false;
        }
        // The "encoding" attribute is copied verbatim from the start tag token
        // onto the element, so reading it back is the spec's test of the
        // token. The tokenizer lower-cases attribute names and this
        // namespace never re-folds them, so the null-namespace name matches
        // exactly the attribute the token carried.
        self.dom.attribute(node, "encoding").is_ok_and(|value| {
            value.is_some_and(|value| {
                value.eq_ignore_ascii_case("text/html")
                    || value.eq_ignore_ascii_case("application/xhtml+xml")
            })
        })
    }

    /// The adjusted current node's namespace, which is also the namespace every
    /// foreign element inserted for a token gets ("Insert a foreign element for
    /// the token, with the adjusted current node's namespace").
    fn current_namespace(&self) -> Namespace {
        self.open_elements
            .last()
            .and_then(|node| self.element_namespace(*node))
            .unwrap_or(Namespace::Html)
    }

    /// "The rules for parsing tokens in foreign content" (13.2.6.5).
    #[allow(clippy::too_many_lines)]
    fn process_in_foreign_content(&mut self, token: &Token) -> Action {
        match token {
            Token::Character(data) | Token::Cdata(data) => {
                self.insert_foreign_character(data);
                Action::Consumed
            }
            Token::Comment(data) => {
                self.insert_comment(self.current_node(), data);
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if is_foreign_breakout_start_tag(tag) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.pop_to_integration_point();
                // "Reprocess the token according to the rules given in the
                // section corresponding to the current insertion mode in HTML
                // content."
                self.process_in_html_content(token)
            }
            Token::EndTag(tag) if matches!(tag.name.as_str(), "br" | "p") => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                self.pop_to_integration_point();
                self.process_in_html_content(token)
            }
            Token::StartTag(tag) => {
                // "Any other start tag".
                let namespace = self.current_namespace();
                // "If the adjusted current node is an element in the SVG
                // namespace, and the token's tag name is one of the ones in the
                // first column of the following table, change the tag name to
                // the name given in the corresponding cell in the second
                // column."  Foreign names are case-sensitive and the DOM does
                // not fold them, so the adjustment has to happen here.
                let name = match namespace {
                    Namespace::Svg => adjust_svg_tag_name(&tag.name),
                    _ => tag.name.clone(),
                };
                self.insert_foreign_element(&name, &tag.attributes, &namespace);
                if tag.self_closing {
                    // "Pop the current node off the stack of open elements and
                    // acknowledge the token's self-closing flag."  This is what
                    // makes `<path ... />` close itself. The spec's one exception
                    // is a self-closing `script`, whose flag is acknowledged and
                    // then handled as a `script` end tag; both paths pop the
                    // element, and the rest of that branch is the SVG scripting
                    // this engine does not host.
                    self.pop_current();
                }
                Action::Consumed
            }
            Token::EndTag(tag) if tag.name == "script" && self.is_svg_script() => {
                // "Pop the current node off the stack of open elements."  The
                // rest of that branch runs the SVG scripting rules, which this
                // engine does not host: an inline SVG script element is not
                // executed. Popping the element is the part the tree shape
                // depends on, and is what a browser without SVG scripting does.
                self.pop_current();
                Action::Consumed
            }
            Token::EndTag(_) => self.process_in_foreign_end_tag(token),
            // The dispatcher sends an end-of-file token to the current
            // insertion mode, so this arm is unreachable.
            Token::Eof => Action::Consumed,
        }
    }

    /// "A character token that is U+0000 NULL: Parse error. Insert a U+FFFD
    /// REPLACEMENT CHARACTER character." followed by "Any other character
    /// token: Insert the token's character." (13.2.6.5).
    ///
    /// The second step also sets the frameset-ok flag to "not ok", which this
    /// tree builder does not track because no frameset insertion mode is
    /// implemented.
    ///
    /// The CDATA section state passes U+0000 through untouched (13.2.5.69), so
    /// the replacement genuinely belongs here rather than in the tokenizer.
    fn insert_foreign_character(&mut self, data: &str) {
        if data.contains('\0') {
            self.parse_error(HtmlParseErrorCode::UnexpectedNullCharacter);
            let replaced = data.replace('\0', "\u{fffd}");
            self.insert_text(&replaced);
            return;
        }
        self.insert_text(data);
    }

    fn is_svg_script(&self) -> bool {
        self.element_matches(self.current_node(), &Namespace::Svg, "script")
    }

    /// "While the current node is not a MathML text integration point, an HTML
    /// integration point, or an element in the HTML namespace, pop elements from
    /// the stack of open elements" (13.2.6.5), for the token kinds that break
    /// out of foreign content.
    fn pop_to_integration_point(&mut self) {
        while let Some(current) = self.open_elements.last().copied()
            && !self.is_html_element(current)
            && !self.is_mathml_text_integration_point(current)
            && !self.is_html_integration_point(current)
        {
            self.open_elements.pop();
        }
    }

    /// "Any other end tag" in foreign content (13.2.6.5).
    fn process_in_foreign_end_tag(&mut self, token: &Token) -> Action {
        let Token::EndTag(tag) = token else {
            return Action::Consumed;
        };
        let name = tag.name.to_ascii_lowercase();
        if self
            .element_name_ascii_lowercase(self.current_node())
            .as_deref()
            != Some(name.as_str())
        {
            // "If node's tag name, converted to ASCII lowercase, is not the same
            // as the tag name of the token, then this is a parse error."
            self.parse_error(HtmlParseErrorCode::UnexpectedToken);
        }
        // The topmost element in the stack of open elements is its first entry,
        // the root `html` element: the spec's loop returns when it reaches it,
        // so this walk starts one entry above it. Every other step of the loop
        // follows below.
        for index in (1..self.open_elements.len()).rev() {
            let node = self.open_elements[index];
            if self.element_name_ascii_lowercase(node).as_deref() == Some(name.as_str()) {
                // "Pop elements from the stack of open elements until node has
                // been popped from the stack."
                self.open_elements.truncate(index);
                return Action::Consumed;
            }
            if self.is_html_element(node) {
                // "Otherwise, process the token according to the rules given in
                // the section corresponding to the current insertion mode in
                // HTML content."  The current node is untouched by the walk, so
                // the insertion mode's own rules do the matching.
                return self.process_in_html_content(token);
            }
        }
        Action::Consumed
    }

    fn has_open_element(&self, name: &str) -> bool {
        self.open_elements
            .iter()
            .any(|node| self.element_name(*node) == Some(name))
    }

    /// Whether a `template` element is on the stack of open elements, which
    /// several rules test. The namespace is part of the test: only an HTML
    /// `template` element has template contents, so only one of them can make
    /// the parser "parsing template contents" (13.2.4.2).
    fn has_open_template(&self) -> bool {
        self.open_elements
            .iter()
            .any(|node| self.is_template_element(*node))
    }

    /// Pop elements from the stack of open elements until an HTML `template`
    /// element has been popped. Unlike [`Self::pop_through`] this compares the
    /// namespace, so a foreign element that happens to be named `template`
    /// cannot end a template's contents early.
    fn pop_through_template(&mut self) {
        while let Some(node) = self.open_elements.pop() {
            if self.is_template_element(node) {
                return;
            }
        }
    }

    fn pop_current(&mut self) {
        self.open_elements.pop();
    }

    fn remove_temporary_head(&mut self) {
        let Some(head) = self.temporary_head.take() else {
            return;
        };
        if let Some(index) = self.open_elements.iter().rposition(|node| *node == head) {
            self.open_elements.remove(index);
        }
    }

    fn pop_through(&mut self, name: &str) {
        while let Some(node) = self.open_elements.pop() {
            if self.element_name(node) == Some(name) {
                return;
            }
        }
    }

    fn pop_through_if_open(&mut self, name: &str) {
        if self.has_open_element(name) {
            self.pop_through(name);
        } else {
            self.parse_error(HtmlParseErrorCode::UnexpectedToken);
        }
    }

    fn close_p_if_open(&mut self) {
        if self.has_open_element("p") {
            self.pop_through("p");
        }
    }

    fn close_matching_list_item(&mut self, name: &str) {
        // A list item in an outer list must not be implicitly closed while a
        // nested list is still open.  The old backwards search skipped over
        // that nested `ul`/`ol`, so `<li><ul><li>a</li><li>b</li>...` moved
        // the second item (and everything after it) outside the outer list.
        // Stop at the nearest list container, matching the HTML "list item
        // scope" rule.
        for index in (0..self.open_elements.len()).rev() {
            let Some(tag) = self.element_name(self.open_elements[index]) else {
                continue;
            };
            if tag == name {
                self.open_elements.truncate(index);
                return;
            }
            if matches!(tag, "ul" | "ol" | "menu") {
                return;
            }
        }
    }

    fn close_definition_item(&mut self) {
        if let Some(index) = self.open_elements.iter().rposition(|node| {
            self.element_name(*node)
                .is_some_and(|name| matches!(name, "dd" | "dt"))
        }) {
            self.open_elements.truncate(index);
        }
    }

    /// "To compute the adjusted insertion location" while foster parenting is
    /// enabled (13.2.6.1).
    ///
    /// "Let last template be the last template element in the stack of open
    /// elements, if there is one, and otherwise null. Let last table be the last
    /// table element in the stack of open elements, if there is one, and
    /// otherwise null. If last template is null and last table is null, then
    /// return the parent of target and null. If last template is non-null, and
    /// either last table is null, or there is no last table, or last template
    /// comes after last table in the stack of open elements, then switch the
    /// insertion mode to 'in template' and let the adjusted insertion location
    /// be inside last template's template contents, after its last child."
    fn foster_location(&self) -> (NodeId, Option<NodeId>) {
        let last_template = self
            .open_elements
            .iter()
            .rposition(|node| self.is_template_element(*node));
        let last_table = self
            .open_elements
            .iter()
            .rposition(|node| self.element_name(*node) == Some("table"));
        if let Some(template_index) = last_template
            && last_table.is_none_or(|table_index| template_index > table_index)
        {
            // The adjusted insertion location is inside the template's contents,
            // after its last child, which is an append with no reference child.
            return (
                self.insertion_parent(self.open_elements[template_index]),
                None,
            );
        }
        if let Some(table) = last_table.map(|index| self.open_elements[index]) {
            if let Some(parent) = self.dom.parent(table) {
                return (parent, Some(table));
            }
            if let Some(index) = self.open_elements.iter().position(|node| *node == table)
                && let Some(previous) = index
                    .checked_sub(1)
                    .and_then(|previous| self.open_elements.get(previous))
            {
                return (*previous, None);
            }
        }
        (self.current_node(), None)
    }

    fn clear_stack_to_table_context(&mut self) {
        while self
            .current_tag()
            .is_some_and(|name| !matches!(name, "table" | "template" | "html"))
        {
            self.pop_current();
        }
    }

    fn clear_stack_to_table_body_context(&mut self) {
        while self
            .current_tag()
            .is_some_and(|name| !matches!(name, "tbody" | "tfoot" | "thead" | "template" | "html"))
        {
            self.pop_current();
        }
    }

    fn clear_stack_to_table_row_context(&mut self) {
        while self
            .current_tag()
            .is_some_and(|name| !matches!(name, "tr" | "template" | "html"))
        {
            self.pop_current();
        }
    }

    fn has_table_body_in_scope(&self) -> bool {
        self.open_elements.iter().rev().any(|node| {
            self.element_name(*node)
                .is_some_and(|name| matches!(name, "tbody" | "tfoot" | "thead"))
        })
    }

    fn has_cell_in_scope(&self) -> bool {
        self.open_elements.iter().rev().any(|node| {
            self.element_name(*node)
                .is_some_and(|name| matches!(name, "td" | "th"))
        })
    }

    fn close_current_cell(&mut self) {
        if let Some(name) = self
            .open_elements
            .iter()
            .rev()
            .filter_map(|node| self.element_name(*node))
            .find(|name| matches!(*name, "td" | "th"))
            .map(str::to_owned)
        {
            // "Where the steps above say to close the cell": generate implied end
            // tags, pop until a td or th element has been popped, and clear the
            // list of active formatting elements up to the last marker. The
            // marker is what stops the cell's formatting from reaching the rest
            // of the row.
            self.generate_implied_end_tags_except(None);
            self.pop_through(&name);
            self.clear_active_formatting_up_to_last_marker();
            self.mode = InsertionMode::InRow;
        }
    }

    /// "Reset the insertion mode appropriately" (13.2.6.6), in the shape this
    /// tree builder has always used: one downward walk of the stack of open
    /// elements to the first element that implies a mode.
    ///
    /// A `template` element contributes "the current template insertion mode"
    /// rather than a mode of its own, so the mode a template switched away from
    /// is restored when the template is popped. `InTemplate` is the marker for
    /// that case: no other element in the walk maps to it.
    fn reset_insertion_mode(&mut self) {
        let decided = self.open_elements.iter().rev().find_map(|node| {
            if self.is_template_element(*node) {
                return Some(InsertionMode::InTemplate);
            }
            match self.element_name(*node)? {
                "td" | "th" => Some(InsertionMode::InCell),
                "tr" => Some(InsertionMode::InRow),
                "tbody" | "thead" | "tfoot" => Some(InsertionMode::InTableBody),
                "table" => Some(InsertionMode::InTable),
                "caption" => Some(InsertionMode::InCaption),
                "colgroup" => Some(InsertionMode::InColumnGroup),
                "head" => Some(InsertionMode::InHead),
                "body" => Some(InsertionMode::InBody),
                "html" => Some(InsertionMode::AfterHead),
                _ => None,
            }
        });
        self.mode = match decided {
            Some(InsertionMode::InTemplate) => self.current_template_mode(),
            Some(mode) => mode,
            None => InsertionMode::InBody,
        };
    }

    fn parse_error(&mut self, code: HtmlParseErrorCode) {
        self.tree_errors.push(HtmlParseError {
            offset: self.tokenizer.offset(),
            code,
        });
    }
}

/// The XLink namespace, as named by the "adjust foreign attributes" table
/// (13.2.6.1).
const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";
/// The XML namespace, as named by the "adjust foreign attributes" table.
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
/// The XMLNS namespace, as named by the "adjust foreign attributes" table.
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// The start tags that break out of foreign content (13.2.6.5): "A start tag
/// whose tag name is one of:" the HTML tag names that can never appear in SVG or
/// MathML content.
const FOREIGN_BREAKOUT_START_TAGS: &[&str] = &[
    "b",
    "big",
    "blockquote",
    "body",
    "br",
    "center",
    "code",
    "dd",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "i",
    "img",
    "li",
    "listing",
    "menu",
    "meta",
    "nobr",
    "ol",
    "p",
    "pre",
    "ruby",
    "s",
    "small",
    "span",
    "strong",
    "strike",
    "sub",
    "sup",
    "table",
    "tt",
    "u",
    "ul",
    "var",
];

/// "A start tag whose tag name is 'font', if the token has any attributes named
/// 'color', 'face', or 'size'" also breaks out of foreign content (13.2.6.5).
const FOREIGN_BREAKOUT_FONT_ATTRIBUTES: &[&str] = &["color", "face", "size"];

/// "A start tag whose tag name is one of: ... (This fixes the case of SVG
/// elements that are not all lowercase.)" (13.2.6.5).
const SVG_TAG_NAME_ADJUSTMENTS: &[(&str, &str)] = &[
    ("altglyph", "altGlyph"),
    ("altglyphdef", "altGlyphDef"),
    ("altglyphitem", "altGlyphItem"),
    ("animatecolor", "animateColor"),
    ("animatemotion", "animateMotion"),
    ("animatetransform", "animateTransform"),
    ("clippath", "clipPath"),
    ("feblend", "feBlend"),
    ("fecolormatrix", "feColorMatrix"),
    ("fecomponenttransfer", "feComponentTransfer"),
    ("fecomposite", "feComposite"),
    ("feconvolvematrix", "feConvolveMatrix"),
    ("fediffuselighting", "feDiffuseLighting"),
    ("fedisplacementmap", "feDisplacementMap"),
    ("fedistantlight", "feDistantLight"),
    ("fedropshadow", "feDropShadow"),
    ("feflood", "feFlood"),
    ("fefunca", "feFuncA"),
    ("fefuncb", "feFuncB"),
    ("fefuncg", "feFuncG"),
    ("fefuncr", "feFuncR"),
    ("fegaussianblur", "feGaussianBlur"),
    ("feimage", "feImage"),
    ("femerge", "feMerge"),
    ("femergenode", "feMergeNode"),
    ("femorphology", "feMorphology"),
    ("feoffset", "feOffset"),
    ("fepointlight", "fePointLight"),
    ("fespecularlighting", "feSpecularLighting"),
    ("fespotlight", "feSpotLight"),
    ("fetile", "feTile"),
    ("feturbulence", "feTurbulence"),
    ("foreignobject", "foreignObject"),
    ("glyphref", "glyphRef"),
    ("lineargradient", "linearGradient"),
    ("radialgradient", "radialGradient"),
    ("textpath", "textPath"),
];

/// "Adjust SVG attributes for the token" (13.2.6.1).
const SVG_ATTRIBUTE_ADJUSTMENTS: &[(&str, &str)] = &[
    ("attributename", "attributeName"),
    ("attributetype", "attributeType"),
    ("basefrequency", "baseFrequency"),
    ("baseprofile", "baseProfile"),
    ("calcmode", "calcMode"),
    ("clippathunits", "clipPathUnits"),
    ("diffuseconstant", "diffuseConstant"),
    ("edgemode", "edgeMode"),
    ("filterunits", "filterUnits"),
    ("glyphref", "glyphRef"),
    ("gradienttransform", "gradientTransform"),
    ("gradientunits", "gradientUnits"),
    ("kernelmatrix", "kernelMatrix"),
    ("kernelunitlength", "kernelUnitLength"),
    ("keypoints", "keyPoints"),
    ("keysplines", "keySplines"),
    ("keytimes", "keyTimes"),
    ("lengthadjust", "lengthAdjust"),
    ("limitingconeangle", "limitingConeAngle"),
    ("markerheight", "markerHeight"),
    ("markerunits", "markerUnits"),
    ("markerwidth", "markerWidth"),
    ("maskcontentunits", "maskContentUnits"),
    ("maskunits", "maskUnits"),
    ("numoctaves", "numOctaves"),
    ("pathlength", "pathLength"),
    ("patterncontentunits", "patternContentUnits"),
    ("patterntransform", "patternTransform"),
    ("patternunits", "patternUnits"),
    ("pointsatx", "pointsAtX"),
    ("pointsaty", "pointsAtY"),
    ("pointsatz", "pointsAtZ"),
    ("preservealpha", "preserveAlpha"),
    ("preserveaspectratio", "preserveAspectRatio"),
    ("primitiveunits", "primitiveUnits"),
    ("refx", "refX"),
    ("refy", "refY"),
    ("repeatcount", "repeatCount"),
    ("repeatdur", "repeatDur"),
    ("requiredextensions", "requiredExtensions"),
    ("requiredfeatures", "requiredFeatures"),
    ("specularconstant", "specularConstant"),
    ("specularexponent", "specularExponent"),
    ("spreadmethod", "spreadMethod"),
    ("startoffset", "startOffset"),
    ("stddeviation", "stdDeviation"),
    ("stitchtiles", "stitchTiles"),
    ("surfacescale", "surfaceScale"),
    ("systemlanguage", "systemLanguage"),
    ("tablevalues", "tableValues"),
    ("targetx", "targetX"),
    ("targety", "targetY"),
    ("textlength", "textLength"),
    ("viewbox", "viewBox"),
    ("viewtarget", "viewTarget"),
    ("xchannelselector", "xChannelSelector"),
    ("ychannelselector", "yChannelSelector"),
    ("zoomandpan", "zoomAndPan"),
];

/// "If any of the attributes on the token match the strings given in the first
/// column of the following table, let the attribute be a namespaced attribute,
/// with the prefix being the string given in the corresponding cell in the
/// second column, the local name being the string given in the corresponding
/// cell in the third column, and the namespace being the namespace given in the
/// corresponding cell in the fourth column." (13.2.6.1)
const FOREIGN_ATTRIBUTE_ADJUSTMENTS: &[(&str, &str, &str, &str)] = &[
    ("xlink:actuate", "xlink", "actuate", XLINK_NAMESPACE),
    ("xlink:arcrole", "xlink", "arcrole", XLINK_NAMESPACE),
    ("xlink:href", "xlink", "href", XLINK_NAMESPACE),
    ("xlink:role", "xlink", "role", XLINK_NAMESPACE),
    ("xlink:show", "xlink", "show", XLINK_NAMESPACE),
    ("xlink:title", "xlink", "title", XLINK_NAMESPACE),
    ("xlink:type", "xlink", "type", XLINK_NAMESPACE),
    ("xml:lang", "xml", "lang", XML_NAMESPACE),
    ("xml:space", "xml", "space", XML_NAMESPACE),
    ("xmlns", "", "xmlns", XMLNS_NAMESPACE),
    ("xmlns:xlink", "xmlns", "xlink", XMLNS_NAMESPACE),
];

/// One attribute of a token after the "adjust MathML attributes", "adjust SVG
/// attributes", and "adjust foreign attributes" steps have run (13.2.6.1).
///
/// Those steps rewrite the token's attributes, and "Creating an element for the
/// token" then appends them verbatim, so the namespace, prefix, and local name
/// have to be materialised here rather than kept in the tokenizer's flat
/// `AttributeToken`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdjustedAttribute {
    pub(crate) namespace: Option<&'static str>,
    pub(crate) prefix: Option<&'static str>,
    pub(crate) local_name: String,
    pub(crate) value: String,
}

/// Run the three attribute adjustment steps for a token in the order the rules
/// for parsing tokens in foreign content list them: "adjust MathML attributes",
/// then "adjust SVG attributes", then "adjust foreign attributes" (13.2.6.5).
///
/// Each step is gated on the adjusted current node's namespace, and the
/// foreign element is inserted with that same namespace, so the new element's
/// namespace is what selects them. The "in body" rules for the `math` and `svg`
/// start tags run the same steps unconditionally, which is what
/// [`Namespace::MathMl`] and [`Namespace::Svg`] as the argument express.
fn adjust_attributes(
    attributes: &[AttributeToken],
    namespace: &Namespace,
) -> Vec<AdjustedAttribute> {
    attributes
        .iter()
        .map(|attribute| {
            let name = match namespace {
                Namespace::MathMl => adjust_mathml_attribute_name(&attribute.name),
                Namespace::Svg => adjust_svg_attribute_name(&attribute.name),
                _ => attribute.name.clone(),
            };
            adjust_foreign_attribute(&name, &attribute.value)
        })
        .collect()
}

/// "To adjust MathML attributes for a token, then, if the token has an
/// attribute named definitionurl, change its name to definitionURL (note the
/// case difference)." (13.2.6.1)
fn adjust_mathml_attribute_name(name: &str) -> String {
    if name == "definitionurl" {
        "definitionURL".to_owned()
    } else {
        name.to_owned()
    }
}

/// "If the adjusted current node is an element in the SVG namespace, and the
/// token's tag name is one of the ones in the first column of the following
/// table, change the tag name to the name given in the corresponding cell in the
/// second column." (13.2.6.5)
fn adjust_svg_tag_name(name: &str) -> String {
    adjusted_name(name, SVG_TAG_NAME_ADJUSTMENTS)
}

/// "When the steps below require the user agent to adjust SVG attributes for a
/// token, then, for each attribute on the token whose attribute name is one of
/// the ones in the first column of the following table, change the attribute's
/// name to the name given in the corresponding cell in the second column."
/// (13.2.6.1)
fn adjust_svg_attribute_name(name: &str) -> String {
    adjusted_name(name, SVG_ATTRIBUTE_ADJUSTMENTS)
}

fn adjusted_name(name: &str, table: &[(&str, &str)]) -> String {
    table
        .iter()
        .find(|(from, _)| *from == name)
        .map_or_else(|| name.to_owned(), |(_, to)| (*to).to_owned())
}

/// "When the steps below require the user agent to adjust foreign attributes for
/// a token, then, if any of the attributes on the token match the strings given
/// in the first column of the following table, let the attribute be a namespaced
/// attribute ..." (13.2.6.1).  An attribute the table does not match keeps the
/// null namespace and no prefix.
fn adjust_foreign_attribute(name: &str, value: &str) -> AdjustedAttribute {
    if let Some((_, prefix, local_name, namespace)) = FOREIGN_ATTRIBUTE_ADJUSTMENTS
        .iter()
        .find(|(from, ..)| *from == name)
    {
        return AdjustedAttribute {
            namespace: Some(namespace),
            // The table spells the absent prefix of `xmlns` as "(none)".
            prefix: (!prefix.is_empty()).then_some(*prefix),
            local_name: (*local_name).to_owned(),
            value: value.to_owned(),
        };
    }
    AdjustedAttribute {
        namespace: None,
        prefix: None,
        local_name: name.to_owned(),
        value: value.to_owned(),
    }
}

/// "A start tag whose tag name is one of: 'b', 'big', ... 'var'" or "A start
/// tag whose tag name is 'font', if the token has any attributes named 'color',
/// 'face', or 'size'" (13.2.6.5).  A start tag on that list makes the parser
/// pop out of foreign content and reprocess the token in the current insertion
/// mode.
fn is_foreign_breakout_start_tag(tag: &TagToken) -> bool {
    if FOREIGN_BREAKOUT_START_TAGS.contains(&tag.name.as_str()) {
        return true;
    }
    tag.name == "font"
        && tag
            .attributes
            .iter()
            .any(|attribute| FOREIGN_BREAKOUT_FONT_ATTRIBUTES.contains(&attribute.name.as_str()))
}

fn empty_tag(name: &str) -> TagToken {
    TagToken {
        name: name.to_owned(),
        attributes: Vec::new(),
        self_closing: false,
    }
}

fn is_all_html_whitespace(data: &str) -> bool {
    data.chars()
        .all(|character| matches!(character, '\t' | '\n' | '\u{000c}' | '\r' | ' '))
}

fn is_heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

fn is_block_start(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "center"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "header"
            | "hgroup"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "search"
            | "section"
            | "summary"
            | "ul"
    )
}

fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// The HTML elements in the special category (13.2.4.2), as far as the
/// adoption agency algorithm's "furthest block" search needs.
fn is_special_html_element_name(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "applet"
            | "area"
            | "article"
            | "aside"
            | "base"
            | "basefont"
            | "bgsound"
            | "blockquote"
            | "body"
            | "br"
            | "button"
            | "caption"
            | "center"
            | "col"
            | "colgroup"
            | "dd"
            | "details"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "embed"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "frame"
            | "frameset"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "header"
            | "hgroup"
            | "hr"
            | "html"
            | "iframe"
            | "img"
            | "input"
            | "keygen"
            | "li"
            | "link"
            | "listing"
            | "main"
            | "marquee"
            | "menu"
            | "meta"
            | "nav"
            | "noembed"
            | "noframes"
            | "noscript"
            | "object"
            | "ol"
            | "p"
            | "param"
            | "plaintext"
            | "pre"
            | "script"
            | "search"
            | "section"
            | "select"
            | "source"
            | "style"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "template"
            | "textarea"
            | "tfoot"
            | "th"
            | "thead"
            | "title"
            | "tr"
            | "track"
            | "ul"
            | "wbr"
            | "xmp"
    )
}

/// The elements that get an implied end tag (13.2.6.3).
fn is_implied_end_tag_name(name: &str) -> bool {
    matches!(
        name,
        "dd" | "dt" | "li" | "optgroup" | "option" | "p" | "rb" | "rp" | "rt" | "rtc"
    )
}

/// The elements in the formatting category (13.2.4.2), which are the elements
/// that end up in the list of active formatting elements. Note that `a` and
/// `nobr` are in this list but are handled by their own "in body" arms.
fn is_formatting_element_name(name: &str) -> bool {
    matches!(
        name,
        "a" | "b"
            | "big"
            | "code"
            | "em"
            | "font"
            | "i"
            | "nobr"
            | "s"
            | "small"
            | "strike"
            | "strong"
            | "tt"
            | "u"
    )
}

fn doctype_quirks_mode(doctype: &DoctypeToken) -> QuirksMode {
    if doctype.force_quirks
        || !doctype
            .name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("html"))
    {
        return QuirksMode::Quirks;
    }
    let public_id = doctype
        .public_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if public_id.starts_with("-//w3c//dtd html 4.01 frameset//")
        || public_id.starts_with("-//w3c//dtd html 4.01 transitional//")
    {
        return if doctype.system_id.is_some() {
            QuirksMode::LimitedQuirks
        } else {
            QuirksMode::Quirks
        };
    }
    if public_id.starts_with("-//w3c//dtd xhtml 1.0 frameset//")
        || public_id.starts_with("-//w3c//dtd xhtml 1.0 transitional//")
    {
        return QuirksMode::LimitedQuirks;
    }
    if public_id.starts_with("-//w3o//dtd w3 html strict 3.0//")
        || public_id.starts_with("-/w3c/dtd html 4.0 transitional/en")
        || public_id.starts_with("html")
    {
        return QuirksMode::Quirks;
    }
    QuirksMode::NoQuirks
}

#[cfg(test)]
mod tests {
    use render_dom::{Dom, Namespace, NodeId, NodeKind};

    use super::super::tokenizer::HtmlParseErrorCode;
    use super::{
        FOREIGN_ATTRIBUTE_ADJUSTMENTS, FOREIGN_BREAKOUT_FONT_ATTRIBUTES,
        FOREIGN_BREAKOUT_START_TAGS, QuirksMode, SVG_ATTRIBUTE_ADJUSTMENTS,
        SVG_TAG_NAME_ADJUSTMENTS, parse_document,
    };

    fn find_element(dom: &Dom, root: NodeId, name: &str) -> Option<NodeId> {
        for child in dom.children(root).unwrap_or_default() {
            if let NodeKind::Element(data) = dom.node(*child)?.kind()
                && data.local_name == name
            {
                return Some(*child);
            }
            if let Some(found) = find_element(dom, *child, name) {
                return Some(found);
            }
        }
        None
    }

    /// The element with the given `id` at or below `root`.
    fn find_element_by_id(dom: &Dom, root: NodeId, id: &str) -> Option<NodeId> {
        for child in dom.children(root).unwrap_or_default() {
            if matches!(dom.node(*child)?.kind(), NodeKind::Element(_))
                && dom.attribute(*child, "id").ok().flatten() == Some(id)
            {
                return Some(*child);
            }
            if let Some(found) = find_element_by_id(dom, *child, id) {
                return Some(found);
            }
        }
        None
    }

    fn element_children(dom: &Dom, node: NodeId) -> Vec<String> {
        dom.children(node)
            .unwrap_or_default()
            .iter()
            .filter_map(|child| match dom.node(*child)?.kind() {
                NodeKind::Element(data) => Some(data.local_name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every element with the given local name below `root`, in tree order.
    fn find_all_elements(dom: &Dom, root: NodeId, name: &str) -> Vec<NodeId> {
        let mut found = Vec::new();
        for child in dom.children(root).unwrap_or_default() {
            if let Some(NodeKind::Element(data)) = dom.node(*child).map(render_dom::Node::kind)
                && data.local_name == name
            {
                found.push(*child);
            }
            found.extend(find_all_elements(dom, *child, name));
        }
        found
    }

    fn text_content(dom: &Dom, node: NodeId) -> String {
        let mut output = String::new();
        for child in dom.children(node).unwrap_or_default() {
            match dom.node(*child).map(render_dom::Node::kind) {
                Some(NodeKind::Text(data)) => output.push_str(data),
                Some(_) => output.push_str(&text_content(dom, *child)),
                None => {}
            }
        }
        output
    }

    #[test]
    fn creates_the_standard_implicit_document_structure() {
        let output = parse_document("<p>Hello</p>");
        let html = find_element(&output.dom, output.dom.document(), "html").unwrap();
        assert_eq!(element_children(&output.dom, html), vec!["head", "body"]);
        let body = find_element(&output.dom, html, "body").unwrap();
        let paragraph = find_element(&output.dom, body, "p").unwrap();
        assert_eq!(text_content(&output.dom, paragraph), "Hello");
        assert_eq!(output.quirks_mode, QuirksMode::Quirks);
    }

    #[test]
    fn preserves_doctype_and_separates_head_from_body() {
        let output = parse_document(
            "<!doctype html><html><head><title>T</title></head><body><main>P</main></body></html>",
        );
        assert_eq!(output.quirks_mode, QuirksMode::NoQuirks);
        assert!(matches!(
            output
                .dom
                .node(output.dom.children(output.dom.document()).unwrap()[0])
                .unwrap()
                .kind(),
            NodeKind::DocumentType(_)
        ));
        let html = find_element(&output.dom, output.dom.document(), "html").unwrap();
        assert_eq!(element_children(&output.dom, html), vec!["head", "body"]);
        let title = find_element(&output.dom, html, "title").unwrap();
        assert_eq!(text_content(&output.dom, title), "T");
    }

    #[test]
    fn applies_optional_p_and_li_end_tags() {
        let output = parse_document("<!doctype html><p>one<div>two</div><ul><li>a<li>b</ul>");
        let body = find_element(&output.dom, output.dom.document(), "body").unwrap();
        assert_eq!(element_children(&output.dom, body), vec!["p", "div", "ul"]);
        let list = find_element(&output.dom, body, "ul").unwrap();
        assert_eq!(element_children(&output.dom, list), vec!["li", "li"]);
    }

    #[test]
    fn keeps_outer_list_items_open_while_nested_list_is_active() {
        let output = parse_document(
            "<!doctype html><ul><li id=outer><ul><li id=inner-a></li><li id=inner-b></li></ul></li><li id=next></li></ul>",
        );
        let list = find_element(&output.dom, output.dom.document(), "ul").unwrap();
        assert_eq!(element_children(&output.dom, list), vec!["li", "li"]);
        let outer = find_element(&output.dom, list, "li").unwrap();
        assert_eq!(element_children(&output.dom, outer), vec!["ul"]);
        let nested = find_element(&output.dom, outer, "ul").unwrap();
        assert_eq!(element_children(&output.dom, nested), vec!["li", "li"]);
    }

    #[test]
    fn parses_rcdata_and_raw_text_without_creating_markup_children() {
        let output = parse_document(
            "<!doctype html><textarea>\nA&amp;<b></textarea><script>if(a<b){x='<i>'}</script>",
        );
        let textarea = find_element(&output.dom, output.dom.document(), "textarea").unwrap();
        assert_eq!(text_content(&output.dom, textarea), "A&<b>");
        assert!(find_element(&output.dom, textarea, "b").is_none());
        let script = find_element(&output.dom, output.dom.document(), "script").unwrap();
        assert_eq!(text_content(&output.dom, script), "if(a<b){x='<i>'}");
        assert!(find_element(&output.dom, script, "i").is_none());
    }

    #[test]
    fn inserts_an_implicit_tbody_and_recovers_bare_cells() {
        let output = parse_document("<!doctype html><table><tr><td>A<td>B</table>");
        let table = find_element(&output.dom, output.dom.document(), "table").unwrap();
        assert_eq!(element_children(&output.dom, table), vec!["tbody"]);
        let tbody = find_element(&output.dom, table, "tbody").unwrap();
        let row = find_element(&output.dom, tbody, "tr").unwrap();
        assert_eq!(element_children(&output.dom, row), vec!["td", "td"]);
    }

    #[test]
    fn foster_parents_non_table_content_before_the_table() {
        let output =
            parse_document("<!doctype html><div><table>outside<tr><td>inside</table></div>");
        let div = find_element(&output.dom, output.dom.document(), "div").unwrap();
        let children = output.dom.children(div).unwrap();
        assert!(
            matches!(output.dom.node(children[0]).unwrap().kind(), NodeKind::Text(data) if data == "outside")
        );
        assert!(
            matches!(output.dom.node(children[1]).unwrap().kind(), NodeKind::Element(data) if data.local_name == "table")
        );
    }

    #[test]
    fn merges_repeated_html_and_body_attributes_without_overwriting() {
        let output = parse_document(
            "<!doctype html><html lang=en><head></head><body id=first><body id=second class=page>",
        );
        let html = find_element(&output.dom, output.dom.document(), "html").unwrap();
        let body = find_element(&output.dom, html, "body").unwrap();
        assert_eq!(output.dom.attribute(html, "lang").unwrap(), Some("en"));
        assert_eq!(output.dom.attribute(body, "id").unwrap(), Some("first"));
        assert_eq!(output.dom.attribute(body, "class").unwrap(), Some("page"));
    }

    #[test]
    fn head_only_text_elements_after_head_are_still_attached_to_head() {
        let output = parse_document(
            "<!doctype html><html><head></head><title>late</title><body>content</body>",
        );
        let html = find_element(&output.dom, output.dom.document(), "html").unwrap();
        let head = find_element(&output.dom, html, "head").unwrap();
        let title = find_element(&output.dom, head, "title").unwrap();
        assert_eq!(text_content(&output.dom, title), "late");
        assert_eq!(element_children(&output.dom, html), vec!["head", "body"]);
    }

    #[test]
    fn parses_colgroups_and_captions_without_reprocessing_loops() {
        let output = parse_document(
            "<!doctype html><table><col><caption><b>Title</b></caption><tr><td>A</table>",
        );
        let table = find_element(&output.dom, output.dom.document(), "table").unwrap();
        assert_eq!(
            element_children(&output.dom, table),
            vec!["colgroup", "caption", "tbody"]
        );
        let colgroup = find_element(&output.dom, table, "colgroup").unwrap();
        assert_eq!(element_children(&output.dom, colgroup), vec!["col"]);
        let caption = find_element(&output.dom, table, "caption").unwrap();
        assert_eq!(text_content(&output.dom, caption), "Title");
    }

    /// A compact, namespace-aware dump of a subtree, one line per node:
    /// `local-name@namespace`, `#text "..."`, `#comment "..."`. Foreign content
    /// is only distinguishable from HTML content by its namespace, so every
    /// shape assertion below spells it out.
    fn outline(dom: &Dom, node: NodeId) -> String {
        let mut lines = Vec::new();
        collect_outline(dom, node, 0, &mut lines);
        lines.join("\n")
    }

    /// The outline of a whole document, which is what the inertness of template
    /// contents has to be visible in: a template's contents are in none of it.
    fn document_outline(input: &str) -> String {
        let output = parse_document(input);
        outline(&output.dom, output.dom.document())
    }

    /// The outline of a template's template contents, reachable only by asking
    /// the element for them.
    fn contents_outline(input: &str) -> String {
        let output = parse_document(input);
        let template = find_element(&output.dom, output.dom.document(), "template")
            .expect("a template element");
        let contents = output
            .dom
            .template_contents(template)
            .expect("template contents");
        outline(&output.dom, contents)
    }

    fn collect_outline(dom: &Dom, node: NodeId, depth: usize, lines: &mut Vec<String>) {
        for child in dom.children(node).unwrap_or_default() {
            let indent = "  ".repeat(depth);
            match dom.node(*child).map(render_dom::Node::kind) {
                Some(NodeKind::Element(data)) => {
                    lines.push(format!(
                        "{indent}{}@{}",
                        data.local_name,
                        namespace_label(&data.namespace)
                    ));
                    collect_outline(dom, *child, depth + 1, lines);
                }
                Some(NodeKind::Text(data)) => lines.push(format!("{indent}#text {data:?}")),
                Some(NodeKind::Comment(data)) => {
                    lines.push(format!("{indent}#comment {data:?}"));
                }
                _ => {}
            }
        }
    }

    fn namespace_label(namespace: &Namespace) -> String {
        match namespace {
            Namespace::Html => "html".to_owned(),
            Namespace::Svg => "svg".to_owned(),
            Namespace::MathMl => "mathml".to_owned(),
            Namespace::Other(other) => format!("other({other})"),
        }
    }

    fn namespace_of(dom: &Dom, node: NodeId) -> Namespace {
        match dom.node(node).map(render_dom::Node::kind) {
            Some(NodeKind::Element(data)) => data.namespace.clone(),
            _ => panic!("node is not an element"),
        }
    }

    /// Parse a document and return the outline of its `body`.
    fn body_outline(input: &str) -> String {
        let output = parse_document(input);
        let body = find_element(&output.dom, output.dom.document(), "body").expect("a body");
        outline(&output.dom, body)
    }

    fn error_codes(input: &str) -> Vec<HtmlParseErrorCode> {
        error_codes_of(&parse_document(input))
    }

    /// The parse errors of a parse that has already happened, which is what the
    /// scripting-flag tests need: `error_codes` always parses with scripting
    /// enabled.
    fn error_codes_of(output: &super::ParseOutput) -> Vec<HtmlParseErrorCode> {
        output.errors.iter().map(|error| error.code).collect()
    }

    /// `(namespace, prefix, local name, value)` for every attribute of an
    /// element, in tree order.
    fn attributes(
        dom: &Dom,
        node: NodeId,
    ) -> Vec<(Option<String>, Option<String>, String, String)> {
        match dom.node(node).map(render_dom::Node::kind) {
            Some(NodeKind::Element(data)) => data
                .attributes
                .iter()
                .map(|attribute| {
                    (
                        attribute.namespace.clone(),
                        attribute.prefix.clone(),
                        attribute.local_name.clone(),
                        attribute.value.clone(),
                    )
                })
                .collect(),
            _ => panic!("node is not an element"),
        }
    }

    #[test]
    fn an_inline_svg_gets_a_namespaced_subtree() {
        let output = parse_document(
            "<!doctype html><svg width=16 height=16 viewbox='0 0 16 16'><g><path d='M0 0'/><circle cx=1 cy=1 r=2/></g><text>hi</text></svg>",
        );
        let body = find_element(&output.dom, output.dom.document(), "body").unwrap();
        assert_eq!(
            outline(&output.dom, body),
            "\
svg@svg
  g@svg
    path@svg
    circle@svg
  text@svg
    #text \"hi\""
        );
        let svg = find_element(&output.dom, body, "svg").unwrap();
        assert_eq!(namespace_of(&output.dom, svg), Namespace::Svg);
        // "Adjust SVG attributes" applies to the `svg` start tag too.
        assert_eq!(
            output.dom.attribute_ns(svg, None, "viewBox").unwrap(),
            Some("0 0 16 16")
        );
        assert_eq!(output.dom.attribute_ns(svg, None, "viewbox").unwrap(), None);
        assert_eq!(output.dom.attribute(svg, "width").unwrap(), Some("16"));
    }

    #[test]
    fn inline_mathml_gets_a_mathml_subtree() {
        let output =
            parse_document("<!doctype html><math><mrow><mi>x</mi><mo>+</mo></mrow></math>");
        let body = find_element(&output.dom, output.dom.document(), "body").unwrap();
        assert_eq!(
            outline(&output.dom, body),
            "\
math@mathml
  mrow@mathml
    mi@mathml
      #text \"x\"
    mo@mathml
      #text \"+\""
        );
    }

    #[test]
    fn every_breakout_start_tag_leaves_foreign_content() {
        for name in FOREIGN_BREAKOUT_START_TAGS {
            let input = format!("<!doctype html><svg><{name}>x</{name}></svg>");
            let output = parse_document(&input);
            let document = output.dom.document();
            let element = find_element(&output.dom, document, name)
                .unwrap_or_else(|| panic!("no element named {name} in {input}"));
            assert_eq!(
                namespace_of(&output.dom, element),
                Namespace::Html,
                "{name} must break out into HTML content"
            );
            // The `svg` element itself stays an SVG element of the body with
            // nothing inside it.
            let body = find_element(&output.dom, document, "body").unwrap();
            let svg = find_element(&output.dom, body, "svg").unwrap();
            assert_eq!(namespace_of(&output.dom, svg), Namespace::Svg, "{input}");
            assert!(element_children(&output.dom, svg).is_empty(), "{input}");
        }
    }

    #[test]
    fn a_tag_outside_the_breakout_list_stays_in_foreign_content() {
        assert_eq!(
            body_outline("<!doctype html><svg><a><text>x</text></a><use href='#i'/></svg>"),
            "\
svg@svg
  a@svg
    text@svg
      #text \"x\"
  use@svg"
        );
    }

    #[test]
    fn font_breaks_out_only_when_it_carries_a_presentational_attribute() {
        for attribute in FOREIGN_BREAKOUT_FONT_ATTRIBUTES {
            let input = format!("<!doctype html><svg><font {attribute}=1>x</font></svg>");
            let output = parse_document(&input);
            let font = find_element(&output.dom, output.dom.document(), "font").unwrap();
            assert_eq!(namespace_of(&output.dom, font), Namespace::Html, "{input}");
        }
        assert_eq!(
            body_outline("<!doctype html><svg><font>x</font></svg>"),
            "\
svg@svg
  font@svg
    #text \"x\""
        );
    }

    #[test]
    fn a_self_closing_start_tag_is_acknowledged_in_foreign_content() {
        // The self-closing flag pops the element immediately, so the following
        // siblings land on the parent.
        assert_eq!(
            body_outline("<!doctype html><svg><g><path/><circle/></g><rect/></svg>"),
            "\
svg@svg
  g@svg
    path@svg
    circle@svg
  rect@svg"
        );
        // A self-closing `svg` in HTML content pops too.
        assert_eq!(
            body_outline("<!doctype html><svg/><p>after"),
            "\
svg@svg
p@html
  #text \"after\""
        );
    }

    #[test]
    fn a_self_closing_start_tag_is_not_acknowledged_in_html_content() {
        // The foreign-content rule must not leak into HTML content: `div/` is
        // not self-closing there and its element stays open.
        assert_eq!(
            body_outline("<!doctype html><div/><span/>"),
            "\
div@html
  span@html"
        );
        assert!(
            error_codes("<!doctype html><em/>")
                .contains(&HtmlParseErrorCode::NonVoidHtmlElementStartTagWithTrailingSolidus)
        );
    }

    #[test]
    fn an_end_tag_in_foreign_content_pops_to_the_matching_element() {
        assert_eq!(
            body_outline("<!doctype html><svg><g><rect></g></svg>"),
            "\
svg@svg
  g@svg
    rect@svg"
        );
        assert_eq!(
            body_outline("<!doctype html><svg><g><g></g></g></svg>"),
            "\
svg@svg
  g@svg
    g@svg"
        );
        // A tag name that matches nothing on the way out to the `body` element
        // is reprocessed as HTML content and then has no element to close, so
        // the foreign subtree stays open.
        assert_eq!(
            body_outline("<!doctype html><svg><g></use></g></svg>"),
            "\
svg@svg
  g@svg"
        );
    }

    #[test]
    fn an_end_br_tag_in_foreign_content_acts_like_a_start_tag() {
        // "`</br>` is special-cased to act as `<br>`". The breakout pops the
        // whole foreign stack, so the `br` the "in body" rules insert lands in
        // the body; the `g` element is only popped off the stack of open
        // elements, so it stays where it was in the tree.
        assert_eq!(
            body_outline("<!doctype html><svg><g></br></svg>"),
            "\
svg@svg
  g@svg
br@html"
        );
        assert_eq!(
            body_outline("<!doctype html><svg><g></p></svg>"),
            "\
svg@svg
  g@svg
p@html"
        );
    }

    #[test]
    fn a_breakout_end_tag_at_an_integration_point_terminates() {
        // The breakout end tag pops to the integration point and hands the token
        // to the HTML insertion mode. The integration point is still the current
        // node, so the token must not be routed back to the foreign-content
        // rules, which would pop nothing and loop.
        for (input, expected) in [
            (
                "<!doctype html><math><mi></br></mi></math>",
                "\
math@mathml
  mi@mathml
    br@html",
            ),
            (
                "<!doctype html><svg><desc></br></desc></svg>",
                "\
svg@svg
  desc@svg
    br@html",
            ),
        ] {
            assert_eq!(body_outline(input), expected, "{input}");
        }
    }

    #[test]
    fn an_svg_script_end_tag_pops_the_element() {
        assert_eq!(
            body_outline("<!doctype html><svg><script></script><rect/></svg>"),
            "\
svg@svg
  script@svg
  rect@svg"
        );
    }

    #[test]
    fn html_integration_points_return_to_html_content() {
        assert_eq!(
            body_outline(
                "<!doctype html><svg><foreignObject><div>x</div></foreignObject><rect/></svg>"
            ),
            "\
svg@svg
  foreignObject@svg
    div@html
      #text \"x\"
  rect@svg"
        );
        assert_eq!(
            body_outline("<!doctype html><svg><desc><div>x</div></desc></svg>"),
            "\
svg@svg
  desc@svg
    div@html
      #text \"x\""
        );
        // An HTML integration point only diverts start tags and character
        // tokens, so `</desc>` is handled by the foreign-content rules and pops
        // the HTML element that is still open above it.
        assert_eq!(
            body_outline("<!doctype html><svg><desc><div>x</div></desc><rect/></svg>"),
            "\
svg@svg
  desc@svg
    div@html
      #text \"x\"
  rect@svg"
        );
        assert_eq!(
            body_outline("<!doctype html><svg><title><div>x</div></title></svg>"),
            "\
svg@svg
  title@svg
    div@html
      #text \"x\""
        );
        // The `encoding` value is an ASCII case-insensitive match.
        for encoding in ["text/html", "TEXT/HTML", "application/xhtml+xml"] {
            let input = format!(
                "<!doctype html><math><annotation-xml encoding='{encoding}'><div>x</div></annotation-xml></math>"
            );
            assert_eq!(
                body_outline(&input),
                "\
math@mathml
  annotation-xml@mathml
    div@html
      #text \"x\"",
                "{encoding}"
            );
        }
    }

    #[test]
    fn a_mathml_annotation_xml_without_a_matching_encoding_is_not_an_integration_point() {
        for encoding in ["text/plain", "", "text/htmlx"] {
            let input = format!(
                "<!doctype html><math><annotation-xml encoding='{encoding}'><foo/></annotation-xml></math>"
            );
            assert_eq!(
                body_outline(&input),
                "\
math@mathml
  annotation-xml@mathml
    foo@mathml",
                "{encoding}"
            );
        }
    }

    #[test]
    fn mathml_text_integration_points_return_to_html_content() {
        assert_eq!(
            body_outline("<!doctype html><math><mi><b>x</b></mi></math>"),
            "\
math@mathml
  mi@mathml
    b@html
      #text \"x\""
        );
        // `mglyph` and `malignmark` are the two exceptions: they stay MathML.
        assert_eq!(
            body_outline("<!doctype html><math><mi><mglyph/><malignmark/></mi></math>"),
            "\
math@mathml
  mi@mathml
    mglyph@mathml
    malignmark@mathml"
        );
    }

    #[test]
    fn every_mathml_text_integration_point_name_is_recognised() {
        for name in ["mi", "mo", "mn", "ms", "mtext"] {
            let input = format!("<!doctype html><math><{name}><span>x</span></{name}></math>");
            assert_eq!(
                body_outline(&input),
                format!("math@mathml\n  {name}@mathml\n    span@html\n      #text \"x\""),
                "{name}"
            );
        }
    }

    #[test]
    fn a_mathml_annotation_xml_routes_an_svg_start_tag_to_the_html_insertion_mode() {
        // "The adjusted current node is a MathML annotation-xml element and the
        // token is a start tag whose tag name is 'svg'" (13.2.6). The `svg`
        // element is therefore created by the "in body" rules in the SVG
        // namespace instead of by the foreign-content rules, which would have
        // put it in the MathML namespace.
        let input = "<!doctype html><math><annotation-xml encoding='text/plain'><svg><circle/></svg></annotation-xml></math>";
        assert_eq!(
            body_outline(input),
            "\
math@mathml
  annotation-xml@mathml
    svg@svg
      circle@svg"
        );
    }

    #[test]
    fn a_mathml_text_integration_point_routes_a_start_tag_to_the_html_insertion_mode() {
        // The integration point sends the token to the HTML insertion mode. The
        // "in body" rules then insert the `div` at the current node and do not
        // know about the MathML element, so it becomes an HTML child of `mtext`
        // until the end tag closes the pair.
        assert_eq!(
            body_outline("<!doctype html><math><mtext><div>x</div></mtext><mglyph/></math>"),
            "\
math@mathml
  mtext@mathml
    div@html
      #text \"x\"
  mglyph@mathml"
        );
    }

    #[test]
    fn a_cdata_section_in_foreign_content_becomes_character_data() {
        let output = parse_document("<!doctype html><svg><text><![CDATA[<b>&amp;]]></text></svg>");
        let text = find_element(&output.dom, output.dom.document(), "text").unwrap();
        // A CDATA section is character data: no elements, no character
        // references, and no comment nodes.
        assert_eq!(text_content(&output.dom, text), "<b>&amp;");
        assert!(element_children(&output.dom, text).is_empty());
        assert!(
            output
                .errors
                .iter()
                .all(|error| error.code != HtmlParseErrorCode::CdataInHtmlContent)
        );
    }

    #[test]
    fn a_cdata_section_in_a_mathml_text_integration_point_becomes_character_data() {
        let output = parse_document("<!doctype html><math><mtext><![CDATA[x]]></mtext></math>");
        let mtext = find_element(&output.dom, output.dom.document(), "mtext").unwrap();
        assert_eq!(namespace_of(&output.dom, mtext), Namespace::MathMl);
        assert_eq!(text_content(&output.dom, mtext), "x");
    }

    #[test]
    fn a_cdata_section_replaces_null_and_reports_the_end_of_file() {
        let output = parse_document("<!doctype html><svg><text><![CDATA[a\0b]]></text></svg>");
        let text = find_element(&output.dom, output.dom.document(), "text").unwrap();
        assert_eq!(text_content(&output.dom, text), "a\u{fffd}b");
        assert!(
            output
                .errors
                .iter()
                .any(|error| error.code == HtmlParseErrorCode::UnexpectedNullCharacter)
        );
        assert!(
            error_codes("<!doctype html><svg><text><![CDATA[abc")
                .contains(&HtmlParseErrorCode::EofInCdata)
        );
    }

    #[test]
    fn a_cdata_section_in_html_content_stays_a_comment() {
        assert_eq!(
            body_outline("<!doctype html><p><![CDATA[x]]>y</p>"),
            "\
p@html
  #comment \"[CDATA[\"
  #comment \"x]]\"
  #text \"y\""
        );
        assert!(
            error_codes("<!doctype html><p><![CDATA[x]]></p>")
                .contains(&HtmlParseErrorCode::CdataInHtmlContent)
        );
    }

    #[test]
    fn every_svg_tag_name_adjustment_is_applied() {
        let markup = SVG_TAG_NAME_ADJUSTMENTS
            .iter()
            .map(|(from, _)| format!("<{from}/>"))
            .collect::<String>();
        let output = parse_document(&format!("<!doctype html><svg>{markup}</svg>"));
        let svg = find_element(&output.dom, output.dom.document(), "svg").unwrap();
        // Every adjusted name is a child of the `svg` element, so the outline of
        // the body is the adjusted tag names in table order.
        let expected: Vec<String> = SVG_TAG_NAME_ADJUSTMENTS
            .iter()
            .map(|(_, to)| format!("{to}@svg"))
            .collect();
        let actual: Vec<String> = element_children(&output.dom, svg)
            .into_iter()
            .zip(
                element_children(&output.dom, svg)
                    .iter()
                    .map(|name| format!("{name}@svg")),
            )
            .map(|(_, label)| label)
            .collect();
        assert_eq!(actual, expected);
        for child in output.dom.children(svg).unwrap() {
            assert_eq!(namespace_of(&output.dom, *child), Namespace::Svg);
        }
        assert_eq!(SVG_TAG_NAME_ADJUSTMENTS.len(), 37);
    }

    #[test]
    fn a_tag_name_the_svg_table_does_not_list_keeps_the_tokenizer_spelling() {
        // The tokenizer lower-cases tag names, so a name the table does not list
        // reaches the DOM lower-cased: the DOM must not invent case either way.
        assert_eq!(
            body_outline("<!doctype html><svg><myshape/><lineargradient/></svg>"),
            "\
svg@svg
  myshape@svg
  linearGradient@svg"
        );
        // A listed name is matched after lower-casing, whatever the source
        // spelling was.
        assert_eq!(
            body_outline("<!doctype html><svg><linearGradient/></svg>"),
            "\
svg@svg
  linearGradient@svg"
        );
    }

    #[test]
    fn every_svg_attribute_adjustment_is_applied() {
        let markup = SVG_ATTRIBUTE_ADJUSTMENTS
            .iter()
            .map(|(from, _)| format!(" {from}='v'"))
            .collect::<String>();
        let output = parse_document(&format!("<!doctype html><svg><g{markup}/></svg>"));
        let svg = find_element(&output.dom, output.dom.document(), "svg").unwrap();
        let group = find_element(&output.dom, svg, "g").unwrap();
        let names: Vec<String> = attributes(&output.dom, group)
            .into_iter()
            .map(|(_, _, local_name, _)| local_name)
            .collect();
        assert_eq!(
            names,
            SVG_ATTRIBUTE_ADJUSTMENTS
                .iter()
                .map(|(_, to)| (*to).to_owned())
                .collect::<Vec<_>>()
        );
        // Every one of them is a null-namespace attribute of the SVG element.
        assert!(
            attributes(&output.dom, group)
                .iter()
                .all(|(namespace, prefix, _, _)| namespace.is_none() && prefix.is_none())
        );
        assert_eq!(SVG_ATTRIBUTE_ADJUSTMENTS.len(), 58);
    }

    #[test]
    fn every_foreign_attribute_adjustment_is_applied() {
        let markup = FOREIGN_ATTRIBUTE_ADJUSTMENTS
            .iter()
            .map(|(from, ..)| format!(" {from}='v'"))
            .collect::<String>();
        let output = parse_document(&format!("<!doctype html><svg><use{markup}/></svg>"));
        let use_element = find_element(&output.dom, output.dom.document(), "use").unwrap();
        assert_eq!(
            attributes(&output.dom, use_element),
            FOREIGN_ATTRIBUTE_ADJUSTMENTS
                .iter()
                .map(|(_, prefix, local, namespace)| {
                    (
                        Some((*namespace).to_owned()),
                        (!prefix.is_empty()).then(|| (*prefix).to_owned()),
                        (*local).to_owned(),
                        "v".to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        );
        // A namespaced attribute is not reachable through the null-namespace
        // accessors, which is what makes it namespaced rather than a colon in a
        // name.
        assert_eq!(output.dom.attribute(use_element, "href").unwrap(), None);
        assert_eq!(
            output.dom.attribute(use_element, "xlink:href").unwrap(),
            None
        );
        assert_eq!(
            output
                .dom
                .attribute_ns(use_element, Some("http://www.w3.org/1999/xlink"), "href")
                .unwrap(),
            Some("v")
        );
        assert_eq!(FOREIGN_ATTRIBUTE_ADJUSTMENTS.len(), 11);
    }

    #[test]
    fn an_attribute_outside_the_foreign_table_keeps_the_null_namespace() {
        let output = parse_document("<!doctype html><svg><use d='M0 0' foo:bar='v'/></svg>");
        let use_element = find_element(&output.dom, output.dom.document(), "use").unwrap();
        assert_eq!(
            attributes(&output.dom, use_element),
            vec![
                (None, None, "d".to_owned(), "M0 0".to_owned()),
                (None, None, "foo:bar".to_owned(), "v".to_owned())
            ]
        );
    }

    #[test]
    fn mathml_attributes_are_adjusted_including_at_a_text_integration_point() {
        // "Adjust MathML attributes" runs for the `math` start tag and again
        // for every foreign start tag whose adjusted current node is in the
        // MathML namespace, which includes `mglyph` at a text integration point.
        for (input, name) in [
            ("<!doctype html><math definitionurl='u'></math>", "math"),
            (
                "<!doctype html><math><mi><mglyph definitionurl='u'/></mi></math>",
                "mglyph",
            ),
        ] {
            let output = parse_document(input);
            let element = find_element(&output.dom, output.dom.document(), name).unwrap();
            assert_eq!(
                output
                    .dom
                    .attribute_ns(element, None, "definitionURL")
                    .unwrap(),
                Some("u"),
                "{input}"
            );
            assert_eq!(
                output.dom.attribute(element, "definitionurl").unwrap(),
                None
            );
        }
    }

    #[test]
    fn an_svg_start_tag_in_a_table_is_still_foster_parented() {
        // Foster parenting around tables is delicate and already implemented;
        // foreign content must not disturb it. The `svg` start tag in the "in
        // table" mode is not table content, so it is foster parented, and its
        // own children land inside it because foster parenting applies to the
        // token being processed only.
        let output = parse_document(
            "<!doctype html><div><table><svg><circle/></svg><tr><td>A</table></div>",
        );
        let div = find_element(&output.dom, output.dom.document(), "div").unwrap();
        assert_eq!(
            outline(&output.dom, div),
            "\
svg@svg
  circle@svg
table@html
  tbody@html
    tr@html
      td@html
        #text \"A\""
        );
    }

    #[test]
    fn a_svg_start_tag_in_the_head_ends_up_in_the_body() {
        // The "in head" insertion mode has no foreign-content rule, so the tag
        // is reprocessed in "in body" after the head is closed.
        assert_eq!(
            body_outline("<!doctype html><html><head><svg><rect/></svg></head></html>"),
            "\
svg@svg
  rect@svg"
        );
    }

    #[test]
    fn template_contents_are_not_children_of_the_template() {
        let output =
            parse_document("<!doctype html><body><template><tr><td>x</td></tr></template>");
        let document = output.dom.document();
        let template = find_element(&output.dom, document, "template").unwrap();
        let contents = output.dom.template_contents(template).unwrap();

        // The element has no children at all: the rows are in the fragment.
        assert!(output.dom.children(template).unwrap().is_empty());
        assert_eq!(
            element_children(&output.dom, contents),
            vec!["tr"],
            "the rows belong to the template contents"
        );
        let row = find_element(&output.dom, contents, "tr").unwrap();
        assert_eq!(element_children(&output.dom, row), vec!["td"]);

        // Nothing inside the fragment is connected to the document, so a
        // traversal from the document root cannot reach any of it.
        assert!(!output.dom.is_connected(contents));
        assert!(!output.dom.is_connected(row));
        assert!(
            !output
                .dom
                .is_connected(find_element(&output.dom, contents, "td").unwrap())
        );
        assert!(output.dom.is_connected(template));
    }

    #[test]
    fn a_document_traversal_cannot_reach_template_contents() {
        // A depth-first walk of the document, which is what a child traversal,
        // `getElementById`, or a selector match over the document all reduce to.
        fn visible_names(dom: &Dom, node: NodeId) -> Vec<String> {
            let mut names = Vec::new();
            for child in dom.children(node).unwrap_or_default() {
                match dom.node(*child).map(render_dom::Node::kind) {
                    Some(NodeKind::Element(data)) => {
                        names.push(data.local_name.clone());
                        names.extend(visible_names(dom, *child));
                    }
                    Some(NodeKind::Text(data)) => names.push(format!("#text {data}")),
                    Some(NodeKind::Comment(data)) => names.push(format!("#comment {data}")),
                    _ => {}
                }
            }
            names
        }

        let output = parse_document(
            "<!doctype html><body><p>outside</p><template><p id=inside><b>deep</b></p></template><p>after</p></body>",
        );
        assert_eq!(
            visible_names(&output.dom, output.dom.document()),
            vec![
                "html",
                "head",
                "body",
                "p",
                "#text outside",
                "template",
                "p",
                "#text after",
            ]
        );
        // The `p` inside the template is a different element from the ones the
        // traversal can see, and it is only reachable through the contents.
        let template = find_element(&output.dom, output.dom.document(), "template").unwrap();
        let contents = output.dom.template_contents(template).unwrap();
        let inside = find_element(&output.dom, contents, "p").unwrap();
        assert_eq!(output.dom.attribute(inside, "id").unwrap(), Some("inside"));
        assert!(visible_names(&output.dom, contents).contains(&"b".to_owned()));
    }

    #[test]
    fn template_contents_round_trip_through_serialization() {
        // `template.innerHTML` is the serialization of the template contents,
        // and `outerHTML` wraps it, so the contents are not lost by either.
        let output = parse_document(
            "<!doctype html><body><template><div>a</div><!--c--><script>x<y</script></template>",
        );
        let template = find_element(&output.dom, output.dom.document(), "template").unwrap();
        assert_eq!(
            super::super::serialize_html_fragment(&output.dom, template),
            "<div>a</div><!--c--><script>x<y</script>"
        );
        assert_eq!(
            super::super::serialize_html_node(&output.dom, template),
            "<template><div>a</div><!--c--><script>x<y</script></template>"
        );
    }

    #[test]
    fn a_template_keeps_foreign_content_namespaced_and_serializable() {
        // The insertion-location redirect must not lose the foreign-content work:
        // the namespaced subtree lands in the fragment with its namespace and
        // its case-adjusted and namespaced attributes intact, and serializing it
        // gives the qualified names back.
        let output = parse_document(
            "<!doctype html><body><template><svg viewbox='0 0 1 1'><clippath><path d='M0 0'/></clippath><use xlink:href='#i'/></svg></template>",
        );
        let template = find_element(&output.dom, output.dom.document(), "template").unwrap();
        let contents = output.dom.template_contents(template).unwrap();
        assert_eq!(
            outline(&output.dom, contents),
            "\
svg@svg
  clipPath@svg
    path@svg
  use@svg"
        );
        let svg = find_element(&output.dom, contents, "svg").unwrap();
        assert_eq!(namespace_of(&output.dom, svg), Namespace::Svg);
        assert_eq!(
            output.dom.attribute_ns(svg, None, "viewBox").unwrap(),
            Some("0 0 1 1")
        );
        let use_element = find_element(&output.dom, contents, "use").unwrap();
        assert_eq!(
            output
                .dom
                .attribute_ns(use_element, Some("http://www.w3.org/1999/xlink"), "href")
                .unwrap(),
            Some("#i")
        );
        assert_eq!(
            super::super::serialize_html_fragment(&output.dom, template),
            "<svg viewBox=\"0 0 1 1\"><clipPath><path d=\"M0 0\"></path></clipPath><use xlink:href=\"#i\"></use></svg>"
        );
    }

    #[test]
    fn a_cdata_section_in_a_template_becomes_character_data_in_the_contents() {
        let output = parse_document(
            "<!doctype html><body><template><svg><text><![CDATA[a<b]]></text></svg></template>",
        );
        let template = find_element(&output.dom, output.dom.document(), "template").unwrap();
        let contents = output.dom.template_contents(template).unwrap();
        let text = find_element(&output.dom, contents, "text").unwrap();
        assert_eq!(namespace_of(&output.dom, text), Namespace::Svg);
        assert_eq!(text_content(&output.dom, text), "a<b");
    }

    #[test]
    fn the_in_template_mode_delegates_by_tag_name() {
        // Each delegation in 13.2.6.4.16 switches the insertion mode and makes
        // it the current template insertion mode. `caption` and friends delegate
        // to the "in table" mode, so the markup after them follows the ordinary
        // table rules: the `tr` gets the implied `tbody` that a `tr` outside a
        // row always gets.
        assert_eq!(
            contents_outline(
                "<!doctype html><body><template><caption>c</caption><tr><td>A</td></tr></template>"
            ),
            "\
caption@html
  #text \"c\"
tbody@html
  tr@html
    td@html
      #text \"A\""
        );
        // `td` and `th` delegate to the "in row" mode, which does not imply a
        // row of its own.
        for cell in ["td", "th"] {
            assert_eq!(
                contents_outline(&format!(
                    "<!doctype html><body><template><{cell}>A</{cell}></template>"
                )),
                format!("{cell}@html\n  #text \"A\""),
                "{cell}"
            );
        }
        // `tr` delegates to the "in table body" mode, and `col` to the "in column
        // group" mode, neither of which implies its container.
        assert_eq!(
            contents_outline("<!doctype html><body><template><tr><td>A</td></tr></template>"),
            "\
tr@html
  td@html
    #text \"A\""
        );
        assert_eq!(
            contents_outline("<!doctype html><body><template><col></template>"),
            "col@html"
        );
        // A start tag the "in template" mode has no rule for delegates to the
        // "in body" mode rather than being ignored.
        assert_eq!(
            contents_outline("<!doctype html><body><template><div>a</div></template>"),
            "div@html\n  #text \"a\""
        );
    }

    #[test]
    fn the_in_template_mode_ignores_an_end_tag_it_has_no_rule_for() {
        // "Any other end tag: Parse error. Ignore the token." The contents of a
        // template are still an ordinary open element stack, so an end tag with
        // nothing to close is dropped rather than unwinding the template.
        let output =
            parse_document("<!doctype html><body><template><div>a</div></span><p>b</p></template>");
        let template = find_element(&output.dom, output.dom.document(), "template").unwrap();
        let contents = output.dom.template_contents(template).unwrap();
        assert_eq!(
            outline(&output.dom, contents),
            "\
div@html
  #text \"a\"
p@html
  #text \"b\""
        );
        assert!(
            output
                .errors
                .iter()
                .any(|error| error.code == HtmlParseErrorCode::UnexpectedToken)
        );
    }

    /// The same depth-first walk over `dom.children` that `getElementById` and
    /// `render_css::selector::select_all` both perform. Those live in other
    /// crates, so this reproduces their traversal exactly rather than calling
    /// them: the point is that inertness is structural, so *any* walk that
    /// starts at the document and follows children cannot reach the contents.
    fn ids_and_tags_visible_from_the_document(dom: &Dom) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            if matches!(
                dom.node(node).map(render_dom::Node::kind),
                Some(NodeKind::Element(_))
            ) && let Ok(Some(id)) = dom.attribute(node, "id")
            {
                found.push(id.to_owned());
            }
            if let Some(children) = dom.children(node) {
                pending.extend(children.iter().rev());
            }
        }
        found
    }

    #[test]
    fn a_document_query_cannot_reach_an_id_inside_a_template() {
        let output = parse_document(
            "<!doctype html><body><p id=outside>out</p><template><p id=hidden>in</p></template></body>",
        );
        // The id inside the template is not among the ids a document-wide walk
        // finds, and the one outside is, so the walk is really running.
        assert_eq!(
            ids_and_tags_visible_from_the_document(&output.dom),
            vec!["outside"]
        );
        // The element is still there and still has its id; it is reachable only
        // by asking the template for its contents.
        let template = find_element(&output.dom, output.dom.document(), "template").unwrap();
        let contents = output.dom.template_contents(template).unwrap();
        let hidden = find_element(&output.dom, contents, "p").unwrap();
        assert_eq!(output.dom.attribute(hidden, "id").unwrap(), Some("hidden"));
        assert_eq!(
            ids_and_tags_visible_from_the_document(&output.dom)
                .iter()
                .filter(|id| *id == "hidden")
                .count(),
            0
        );
    }

    #[test]
    fn a_nested_template_pushes_a_second_template_insertion_mode() {
        let output = parse_document(
            "<!doctype html><body><template id=outer><div>a<template id=inner>b</template>c</div></template>",
        );
        let outer = find_element(&output.dom, output.dom.document(), "template").unwrap();
        let outer_contents = output.dom.template_contents(outer).unwrap();
        assert_eq!(output.dom.attribute(outer, "id").unwrap(), Some("outer"));
        // The inner template is a leaf in this outline even though it has text:
        // its own text is in its own fragment, not in its parent's. That is the
        // whole point of the mechanism, and it is what stops the second
        // `</template>` from closing the first.
        assert_eq!(
            outline(&output.dom, outer_contents),
            "\
div@html
  #text \"a\"
  template@html
  #text \"c\""
        );
        let inner = find_element(&output.dom, outer_contents, "template").unwrap();
        assert_eq!(output.dom.attribute(inner, "id").unwrap(), Some("inner"));
        assert!(output.dom.children(inner).unwrap().is_empty());
        // The inner template is a second, separate fragment holding only its own
        // text; the surrounding text stayed in the outer fragment.
        let inner_contents = output.dom.template_contents(inner).unwrap();
        assert!(inner_contents != outer_contents);
        assert_eq!(outline(&output.dom, inner_contents), "#text \"b\"");
    }

    #[test]
    fn closing_a_template_restores_the_insertion_mode_around_it() {
        // A template that switched to a table mode must give that mode back
        // before the following markup is parsed, or the markup after the
        // template would be parsed as table content.
        assert_eq!(
            document_outline(
                "<!doctype html><body><template><tr><td>in</td></tr></template><p>after</p>"
            ),
            "\
html@html
  head@html
  body@html
    template@html
    p@html
      #text \"after\""
        );
        assert_eq!(
            contents_outline(
                "<!doctype html><body><template><table><tr><td>in</td></tr></table></template><p>after</p>"
            ),
            "\
table@html
  tbody@html
    tr@html
      td@html
        #text \"in\""
        );
    }

    #[test]
    fn a_template_in_a_table_stays_in_the_table_but_keeps_its_contents_inert() {
        // The "in table" rules send a `template` start tag to the "in head"
        // rules without enabling foster parenting, so unlike other content the
        // "in table" mode cannot place, the element is inserted into the table.
        // Its contents are still a separate fragment, so nothing the table rules
        // implied for them leaks into the table.
        let output = parse_document(
            "<!doctype html><div><table><template><td>x</td></template><tr><td>A</td></tr></table></div>",
        );
        let div = find_element(&output.dom, output.dom.document(), "div").unwrap();
        assert_eq!(
            outline(&output.dom, div),
            "\
table@html
  template@html
  tbody@html
    tr@html
      td@html
        #text \"A\""
        );
        let template = find_element(&output.dom, div, "template").unwrap();
        assert!(output.dom.children(template).unwrap().is_empty());
        let contents = output.dom.template_contents(template).unwrap();
        assert_eq!(outline(&output.dom, contents), "td@html\n  #text \"x\"");
        // The `tr` after the template is ordinary table content and still gets
        // its implied `tbody`, so the template did not disturb the table.
        let table = find_element(&output.dom, div, "table").unwrap();
        assert_eq!(
            element_children(&output.dom, table),
            vec!["template", "tbody"]
        );
    }

    #[test]
    fn a_template_as_the_first_element_lands_in_the_head() {
        // "Before head" has no rule for `template`, so it inserts a head element
        // and reprocesses the token in the "in head" mode, which creates the
        // template there. This is what a browser does too.
        assert_eq!(
            document_outline("<!doctype html><template>x</template>"),
            "\
html@html
  head@html
    template@html
  body@html"
        );
    }

    #[test]
    fn an_end_tag_without_an_open_template_is_a_parse_error() {
        let output = parse_document("<!doctype html><body><div></template></div>");
        assert!(
            output
                .errors
                .iter()
                .any(|error| error.code == HtmlParseErrorCode::UnexpectedToken)
        );
        // The token is ignored, so the `div` is still open when the paragraph
        // that follows lands inside it.
        assert_eq!(
            body_outline("<!doctype html><body><div></template><p>after</div>"),
            "\
div@html
  p@html
    #text \"after\""
        );
    }

    #[test]
    fn a_start_tag_ignored_inside_a_template_is_ignored() {
        // The "in body" rules ignore a second `body` or a repeated `html` while
        // a template element is on the stack, so a stray `<body>` inside a
        // template cannot re-open the document body.
        assert_eq!(
            contents_outline("<!doctype html><body><template><body>inner</body></template>"),
            "#text \"inner\""
        );
        let output =
            parse_document("<!doctype html><body><template><html id=x>inner</html></template>");
        let template = find_element(&output.dom, output.dom.document(), "template").unwrap();
        let html = output.dom.children(output.dom.document()).unwrap()[1];
        assert!(matches!(
            output.dom.node(html).map(render_dom::Node::kind),
            Some(NodeKind::Element(data)) if data.local_name == "html"
        ));
        assert!(output.dom.children(template).unwrap().is_empty());
    }

    #[test]
    fn an_unclosed_template_is_ended_by_the_end_of_file() {
        // The "in template" rules for an end-of-file token pop to the template
        // and reset the insertion mode, and the parse ends either way.
        let output = parse_document("<!doctype html><body><template><div>a");
        let template = find_element(&output.dom, output.dom.document(), "template").unwrap();
        let contents = output.dom.template_contents(template).unwrap();
        assert_eq!(outline(&output.dom, contents), "div@html\n  #text \"a\"");
        assert!(
            output
                .errors
                .iter()
                .any(|error| error.code == HtmlParseErrorCode::UnexpectedToken)
        );
    }

    /// The body outline of a document, for the formatting-element shapes below.
    fn afe_outline(input: &str) -> String {
        body_outline(input)
    }

    /// The number of same-named elements in the chain rooted at `node`, counting
    /// `node` itself, and that chain's names for a failure message. `node` must be
    /// a `local_name` element. This is how the effects of the list of active
    /// formatting elements become visible in the tree: the number of elements the
    /// *list* still holds is the number that get re-opened.
    fn formatting_chain_depth(dom: &Dom, node: NodeId, local_name: &str) -> (usize, String) {
        let mut depth = 1;
        let mut label = format!("{local_name} ");
        let mut current = node;
        while let Some(child) = dom
            .children(current)
            .unwrap_or_default()
            .iter()
            .find(|child| {
                matches!(
                    dom.node(**child).map(render_dom::Node::kind),
                    Some(NodeKind::Element(data)) if data.local_name == local_name
                )
            })
        {
            depth += 1;
            label.push_str(local_name);
            label.push(' ');
            current = *child;
        }
        (depth, label)
    }

    #[test]
    fn the_noahs_ark_clause_bounds_the_list_not_the_tree() {
        // Every start tag still creates an element, so four `b` start tags nest
        // four deep whatever the clause does. The clause bounds the *list*, which
        // is why the fourth element does not evict the first from the tree.
        assert_eq!(
            afe_outline("<!doctype html><body><b><b><b><b>x"),
            "\
b@html
  b@html
    b@html
      b@html
        #text \"x\""
        );
    }

    #[test]
    fn the_noahs_ark_clause_keeps_at_most_three_identical_formatting_entries() {
        // The clause is visible once the elements leave the stack of open
        // elements without leaving the list, which is what a table end tag does:
        // it pops the `b` elements and leaves the list alone, so the text after
        // the table re-opens only the three that survived.
        for (count, reopened) in [(3_usize, 3_usize), (4, 3), (5, 3), (8, 3)] {
            let markup = "<b>".repeat(count);
            let input = format!("<!doctype html><body><table>{markup}x</table>y");
            let output = parse_document(&input);
            let body = find_element(&output.dom, output.dom.document(), "body").unwrap();
            // The `b` elements that were foster parented before the table are
            // empty; the re-opened ones are the last child of the body.
            let last_b = output
                .dom
                .children(body)
                .unwrap()
                .iter()
                .rev()
                .find(|child| {
                    matches!(
                        output.dom.node(**child).map(render_dom::Node::kind),
                        Some(NodeKind::Element(data)) if data.local_name == "b"
                    )
                })
                .copied()
                .unwrap();
            let (depth, label) = formatting_chain_depth(&output.dom, last_b, "b");
            assert_eq!(
                depth, reopened,
                "{count} `b` start tags in {input}: {label}"
            );
        }
    }

    #[test]
    fn the_noahs_ark_clause_compares_attributes() {
        // The clause counts elements with the same tag name, namespace, *and*
        // attributes, so `i` elements with different ids are different families
        // and none of them is evicted, however many there are.
        for count in [3_usize, 6] {
            let markup = (1..=count)
                .map(|index| format!("<i id={index}>"))
                .collect::<String>();
            let input = format!("<!doctype html><body><table>{markup}x</table>y");
            let output = parse_document(&input);
            let body = find_element(&output.dom, output.dom.document(), "body").unwrap();
            let last_i = output
                .dom
                .children(body)
                .unwrap()
                .iter()
                .rev()
                .find(|child| {
                    matches!(
                        output.dom.node(**child).map(render_dom::Node::kind),
                        Some(NodeKind::Element(data)) if data.local_name == "i"
                    )
                })
                .copied()
                .unwrap();
            let (reopened, label) = formatting_chain_depth(&output.dom, last_i, "i");
            assert_eq!(
                reopened, count,
                "{count} distinct i elements in {input}: {label}"
            );
        }
        // Two elements with the *same* attributes are one family, so four of them
        // re-open only three deep.
        let output = parse_document(
            "<!doctype html><body><table><i id=1><i id=1><i id=1><i id=1>x</table>y",
        );
        let body = find_element(&output.dom, output.dom.document(), "body").unwrap();
        let last_i = output
            .dom
            .children(body)
            .unwrap()
            .iter()
            .rev()
            .find(|child| {
                matches!(
                    output.dom.node(**child).map(render_dom::Node::kind),
                    Some(NodeKind::Element(data)) if data.local_name == "i"
                )
            })
            .copied()
            .unwrap();
        let (reopened, _) = formatting_chain_depth(&output.dom, last_i, "i");
        assert_eq!(reopened, 3);
    }

    #[test]
    fn a_marker_stops_reconstruction_at_the_youngest_block() {
        // A `td` pushes a marker, so the `b` that was open before the table is not
        // re-opened inside the cell, and the `</td>` clears the list up to the marker
        // so it cannot leak into the rest of the row either.
        assert_eq!(
            afe_outline("<!doctype html><body><b>1<table><tr><td>2</td></tr></table>3"),
            "\
b@html
  #text \"1\"
  table@html
    tbody@html
      tr@html
        td@html
          #text \"2\"
  #text \"3\""
        );
    }

    #[test]
    fn a_marker_is_pushed_for_a_cell_a_caption_and_a_template() {
        // Each of these pushes a marker, so formatting from outside does not
        // reappear inside.
        assert_eq!(
            afe_outline("<!doctype html><body><b>1<table><caption>2</caption><tr><td>3</table>"),
            "\
b@html
  #text \"1\"
  table@html
    caption@html
      #text \"2\"
    tbody@html
      tr@html
        td@html
          #text \"3\""
        );
        // A template is a marker too, and the marker is removed by `</template>`,
        // so the `b` is still active afterwards.
        assert_eq!(
            afe_outline("<!doctype html><body><b>1<template>2</template>3"),
            "\
b@html
  #text \"1\"
  template@html
  #text \"3\""
        );
    }

    #[test]
    fn reconstruction_reopens_formatting_elements_that_left_the_stack() {
        // "This has the effect of reopening all the formatting elements that were
        // opened in the current body, cell, or caption (whichever is youngest)
        // that haven't been explicitly closed" (13.2.4.3). A `</table>` end tag
        // pops the `b` elements off the stack and leaves the list alone, so the
        // text after the table re-opens one. An *explicitly* closed `</b>` does
        // not: the adoption agency algorithm removes it from the list too.
        assert_eq!(
            afe_outline("<!doctype html><body><table><b>x</table>y"),
            "\
b@html
  #text \"x\"
table@html
b@html
  #text \"y\""
        );
        assert_eq!(
            afe_outline("<!doctype html><body><b>1</b><div>2</div>3"),
            "\
b@html
  #text \"1\"
div@html
  #text \"2\"
#text \"3\""
        );
    }

    #[test]
    fn svg_interacts_with_the_formatting_list() {
        // `<b><svg><b>` is the case the task calls out. The `svg` start tag
        // reconstructs the `b`, so the second `b` is a sibling of the `svg`
        // rather than a descendant: `b` is in the breakout list, so inside the
        // foreign subtree it breaks back out to the HTML content that is already
        // open. The `svg` is therefore left empty, and the second `b` is an HTML
        // element, not an SVG one.
        assert_eq!(
            afe_outline("<!doctype html><body><b>1<svg><b>2</b></svg>3</b>"),
            "\
b@html
  #text \"1\"
  svg@svg
  b@html
    #text \"2\"
  #text \"3\""
        );
        let output = parse_document("<!doctype html><body><b>1<svg><b>2</b></svg>3</b>");
        let svg = find_element(&output.dom, output.dom.document(), "svg").unwrap();
        assert_eq!(namespace_of(&output.dom, svg), Namespace::Svg);
        assert!(element_children(&output.dom, svg).is_empty());
        // There is one `b` in the HTML namespace around the foreign subtree and
        // one after it, and no `b` inside the SVG namespace at all.
        let bolds = find_all_elements(&output.dom, output.dom.document(), "b");
        assert_eq!(bolds.len(), 2);
        assert!(
            bolds
                .iter()
                .all(|node| namespace_of(&output.dom, *node) == Namespace::Html)
        );
    }

    #[test]
    fn the_adoption_agency_algorithm_splits_misnested_formatting_elements() {
        // 13.2.10.1's worked example, with the DOM the spec prints for it:
        // `html head body p #text: 1 b #text: 2 i #text: 3 i #text: 4 #text: 5`.
        assert_eq!(
            afe_outline("<!doctype html><body><p>1<b>2<i>3</b>4</i>5</p>"),
            "\
p@html
  #text \"1\"
  b@html
    #text \"2\"
    i@html
      #text \"3\"
  i@html
    #text \"4\"
  #text \"5\""
        );
    }

    #[test]
    fn the_adoption_agency_algorithm_moves_formatting_into_the_furthest_block() {
        // 13.2.10.2's worked example: `html head body b #text: 1 p b #text: 2
        // #text: 3`. The `b` inside the `p` is a *new* element; the original was
        // closed, and the list is left with the new one so the following text
        // stays in the paragraph.
        assert_eq!(
            afe_outline("<!doctype html><body><b>1<p>2</b>3</p>"),
            "\
b@html
  #text \"1\"
p@html
  b@html
    #text \"2\"
  #text \"3\""
        );
    }

    #[test]
    fn the_adoption_agency_algorithm_foster_parents_in_a_table() {
        // 13.2.10.3's worked example: the `b` before the table is foster
        // parented, the `td` marker keeps it out of the cell, and the `bbb` run
        // is reprocessed as a group through the "in table" "anything else" entry,
        // which re-opens a `b` and foster parents that too.
        assert_eq!(
            afe_outline("<!doctype html><body><table><b><tr><td>aaa</td></tr>bbb</table>ccc"),
            "\
b@html
b@html
  #text \"bbb\"
table@html
  tbody@html
    tr@html
      td@html
        #text \"aaa\"
b@html
  #text \"ccc\""
        );
    }

    #[test]
    fn a_second_anchor_does_not_nest_anchors() {
        // "A start tag whose tag name is 'a'": if the list contains an `a` since
        // the last marker, run the adoption agency algorithm for the token, so
        // the first `a` is closed rather than nested.
        assert_eq!(
            afe_outline("<!doctype html><body><a href=1>1<a href=2>2</a>3</a>"),
            "\
a@html
  #text \"1\"
a@html
  #text \"2\"
#text \"3\""
        );
    }

    #[test]
    fn a_second_nobr_is_split_by_the_adoption_agency_algorithm() {
        // The same rule for `nobr`, which has its own start-tag arm.
        assert_eq!(
            afe_outline("<!doctype html><body><nobr>1<nobr>2</nobr>3"),
            "\
nobr@html
  #text \"1\"
nobr@html
  #text \"2\"
#text \"3\""
        );
    }

    #[test]
    fn an_option_in_a_select_closes_the_previous_one() {
        // "Otherwise, if the current node is an option element, then pop the
        // current node off the stack of open elements" (13.2.6.4.7). The current
        // standard has no separate "in select" insertion mode: `select`,
        // `option`, and `optgroup` are handled by the "in body" rules, so stray
        // markup inside an option is inserted rather than ignored.
        assert_eq!(
            afe_outline("<!doctype html><body><select><option>a<option>b</select>"),
            "\
select@html
  option@html
    #text \"a\"
  option@html
    #text \"b\""
        );
        assert_eq!(
            afe_outline("<!doctype html><body><select><optgroup label=g><option>o</select>"),
            "\
select@html
  optgroup@html
    option@html
      #text \"o\""
        );
        // Stray markup inside an option is inserted, which is what the current
        // standard prescribes now that there is no "in select" mode to ignore it.
        assert_eq!(
            afe_outline("<!doctype html><body><select><option><div>d</div></select>"),
            "\
select@html
  option@html
    div@html
      #text \"d\""
        );
    }

    #[test]
    fn a_nested_select_start_tag_is_ignored() {
        // "Otherwise, if the stack of open elements has a select element in
        // scope: Parse error. Ignore the token" (13.2.6.4.7). A `select` that is
        // itself a boundary is still found to be in scope, because the
        // specific-scope algorithm tests the target before the boundary list.
        assert_eq!(
            afe_outline("<!doctype html><body><select><select><option>a</select>"),
            "\
select@html
  option@html
    #text \"a\""
        );
    }

    #[test]
    fn an_input_inside_a_select_closes_the_select() {
        // "If the stack of open elements has a select element in scope: Parse
        // error. Pop elements from the stack of open elements until a select
        // element has been popped from the stack" (13.2.6.4.7). The `input` is
        // then inserted at the node the closed `select` was in, so it is a
        // sibling of the `select` rather than a child of it.
        assert_eq!(
            afe_outline("<!doctype html><body><select><input value=v><option>a</select>"),
            "\
select@html
input@html
option@html
  #text \"a\""
        );
    }

    #[test]
    fn whitespace_only_text_in_a_table_stays_in_the_table() {
        // The "in table text" mode collects the run and, finding it to be all
        // whitespace, inserts it where the table is.
        assert_eq!(
            afe_outline("<!doctype html><body><table>  <tr><td>A</table>"),
            "\
table@html
  #text \"  \"
  tbody@html
    tr@html
      td@html
        #text \"A\""
        );
    }

    #[test]
    fn a_mixed_whitespace_and_text_run_in_a_table_is_foster_parented_as_a_group() {
        // The "in table text" mode exists so that a run which turns out not to be
        // all whitespace is reprocessed *as a group* through the "in table"
        // "anything else" entry. The whitespace that led the run therefore moves
        // out of the table with the rest of it instead of being left behind: the
        // whole run lands before the table, as one text node. The character
        // reference splits the run into three tokens, which is what makes the
        // mixed case reachable.
        let output = parse_document("<!doctype html><div><table> &amp; x <tr><td>A</table></div>");
        let div = find_element(&output.dom, output.dom.document(), "div").unwrap();
        assert_eq!(
            outline(&output.dom, div),
            "\
#text \" & x \"
table@html
  tbody@html
    tr@html
      td@html
        #text \"A\""
        );
        let table = find_element(&output.dom, div, "table").unwrap();
        assert!(
            output
                .dom
                .children(table)
                .unwrap()
                .iter()
                .all(|child| !matches!(
                    output.dom.node(*child).map(render_dom::Node::kind),
                    Some(NodeKind::Text(_))
                )),
            "no text may be left inside the table"
        );
    }

    #[test]
    fn non_whitespace_text_in_a_table_is_foster_parented_before_it() {
        let output =
            parse_document("<!doctype html><div><table>outside<tr><td>inside</table></div>");
        let div = find_element(&output.dom, output.dom.document(), "div").unwrap();
        assert_eq!(
            outline(&output.dom, div),
            "\
#text \"outside\"
table@html
  tbody@html
    tr@html
      td@html
        #text \"inside\""
        );
    }
    /// `<param>`, `<source>`, and `<track>` are the void elements whose
    /// "in body" rule is "Insert an HTML element for the token, then immediately
    /// pop the current node off the stack of open elements" (13.2.6.4.7). Two of
    /// them in a row are siblings, which is the shape `<video>`, `<audio>`,
    /// `<picture>`, and `<object>` all rely on: if the first one stayed on the
    /// stack, the second would be inserted *inside* it, and a walker that
    /// collects the children in document order would see only the first.
    #[test]
    fn consecutive_void_children_of_a_replaced_element_are_siblings() {
        for (container, child) in [
            ("video", "source"),
            ("audio", "source"),
            ("picture", "source"),
            ("object", "param"),
        ] {
            let input = format!(
                "<!doctype html><body><{container} id=c><{child} id=1><{child} id=2><{child} id=3></{container}>"
            );
            let output = parse_document(&input);
            let element = find_element(&output.dom, output.dom.document(), container).unwrap();
            let children = output.dom.children(element).unwrap().to_vec();
            let ids: Vec<Option<&str>> = children
                .iter()
                .map(|node| output.dom.attribute(*node, "id").unwrap())
                .collect();
            assert_eq!(ids, vec![Some("1"), Some("2"), Some("3")], "{input}");
        }
    }

    /// The same rule for `hr`, whose "in body" rule additionally closes a `p`
    /// element first. Two `hr` in a row are siblings, and an `hr` inside an open
    /// `p` closes it.
    #[test]
    fn consecutive_hr_elements_are_siblings_and_an_hr_closes_a_paragraph() {
        assert_eq!(
            afe_outline("<!doctype html><body><hr id=1><hr id=2><hr id=3>"),
            "\
hr@html
hr@html
hr@html"
        );
        assert_eq!(
            afe_outline("<!doctype html><body><p>text<hr id=1>after"),
            "\
p@html
  #text \"text\"
hr@html
#text \"after\""
        );
    }

    /// The void elements whose "in body" rule reconstructs the active formatting
    /// elements first. These are checked alongside the rules above so that the two
    /// families of void-element rules — the ones that reconstruct and the ones
    /// that do not — cannot drift apart again.
    #[test]
    fn the_void_element_families_both_pop_the_element_they_insert() {
        // "area, br, embed, img, keygen, wbr" and "input" reconstruct first, so
        // a void element is still a sibling of the one before it and does not
        // swallow an open formatting element.
        assert_eq!(
            afe_outline("<!doctype html><body><img id=1><img id=2><br id=3><wbr id=4>"),
            "\
img@html
img@html
br@html
wbr@html"
        );
        // A formatting element between two void elements: the void elements do
        // not close the `b`, and reconstruction does not add another one.
        assert_eq!(
            afe_outline("<!doctype html><body><b>1<img id=1>2<img id=2>3</b>"),
            "\
b@html
  #text \"1\"
  img@html
  #text \"2\"
  img@html
  #text \"3\""
        );
        // A `source` has no "in table" rule, so it reaches that mode's "anything
        // else" entry and is foster parented out of the table — and because the
        // target is the `tbody`, not a `b`, the `b` is left holding the table and
        // the `source` beside it rather than inside the table.
        assert_eq!(
            afe_outline(
                "<!doctype html><body><b>1<table><tr><td>2</td></tr><source id=1></table>3"
            ),
            "\
b@html
  #text \"1\"
  source@html
  table@html
    tbody@html
      tr@html
        td@html
          #text \"2\"
  #text \"3\""
        );
    }

    /// Every void element, two in a row, in body content. The point of the sweep
    /// is that no void element may stay on the stack of open elements, because a
    /// void element that does is a container for everything that follows it.
    #[test]
    fn every_void_element_pops_itself_so_runs_of_them_stay_flat() {
        for name in [
            "area", "base", "br", "col", "embed", "hr", "img", "input", "keygen", "link", "meta",
            "param", "source", "track", "wbr",
        ] {
            let input =
                format!("<!doctype html><body><div><{name} id=1><{name} id=2><{name} id=3></div>");
            let output = parse_document(&input);
            let div = find_element(&output.dom, output.dom.document(), "div").unwrap();
            let children = output.dom.children(div).unwrap().to_vec();
            // A `col` in body content is ignored outright, so it contributes no
            // children at all; every other void element contributes three flat
            // siblings.
            let expected = if name == "col" { 0 } else { 3 };
            assert_eq!(children.len(), expected, "{input}");
            for (offset, child) in children.iter().enumerate() {
                assert!(
                    matches!(
                        output.dom.node(*child).map(render_dom::Node::kind),
                        Some(NodeKind::Element(data)) if data.local_name == name
                    ),
                    "{input}: child {offset} is not a {name}"
                );
            }
        }
    }

    /// `<link>`, `<meta>` and `<base>` in body content are handled by the "in
    /// head" rules, which the "in body" rules delegate to, and `col` inside a
    /// `colgroup` by the "in column group" rules. Both paths must also keep the
    /// elements flat, and the "in body" rules must not have started a
    /// `colgroup`/`tbody` that the markup did not ask for.
    #[test]
    fn void_elements_reached_through_the_in_head_and_column_group_rules_stay_flat() {
        let output =
            parse_document("<!doctype html><head><meta id=1><meta id=2><link id=3><base id=4>");
        let head = find_element(&output.dom, output.dom.document(), "head").unwrap();
        assert_eq!(
            outline(&output.dom, head),
            "\
meta@html
meta@html
link@html
base@html"
        );
        assert_eq!(
            afe_outline(
                "<!doctype html><body><table><colgroup><col id=1><col id=2></colgroup><tr><td>x"
            ),
            "\
table@html
  colgroup@html
    col@html
    col@html
  tbody@html
    tr@html
      td@html
        #text \"x\""
        );
    }

    /// "A start tag whose tag name is one of: 'caption', 'col', 'colgroup',
    /// 'frame', 'head', 'tbody', 'td', 'tfoot', 'th', 'thead', 'tr'": parse
    /// error, ignore the token (13.2.6.4.7). This is what stops a stray void
    /// `col` from being inserted as an ordinary element, which the "any other
    /// start tag" arm would push onto the stack of open elements.
    #[test]
    fn table_structure_tags_in_body_content_are_ignored() {
        for name in [
            "caption", "col", "colgroup", "frame", "head", "tbody", "td", "tfoot", "th", "thead",
            "tr",
        ] {
            let input = format!("<!doctype html><body><div><{name} id=1><{name} id=2></div>");
            let output = parse_document(&input);
            let div = find_element(&output.dom, output.dom.document(), "div").unwrap();
            assert!(
                output.dom.children(div).unwrap().is_empty(),
                "{input} should insert nothing, found {:?}",
                output.dom.children(div).unwrap()
            );
            assert!(
                error_codes(&input).contains(&HtmlParseErrorCode::UnexpectedToken),
                "{input} should report a parse error"
            );
        }
    }
    /// With scripting enabled — rENDER's mode, the one `parse_document` uses —
    /// a `noscript` element's contents are raw text (13.2.6.4.4 and
    /// 13.2.6.4.7). The elements a page writes inside a `noscript` to serve
    /// non-scripted visitors are therefore inert: nothing is fetched, no
    /// stylesheet is applied, and the subtree stays where the author put it.
    #[test]
    fn with_scripting_enabled_noscript_contents_are_raw_text() {
        // The head case. Before this rule the whole `noscript` subtree was pushed
        // out of the head into the body, and the `style` inside it became a real
        // stylesheet element.
        assert_eq!(
            document_outline(
                "<!doctype html><html><head><noscript><style>p{color:red}</style></noscript></head><body><p>x"
            ),
            "\
html@html
  head@html
    noscript@html
      #text \"<style>p{color:red}</style>\"
  body@html
    p@html
      #text \"x\""
        );
        // The classic no-JS fallback stylesheet. As an element it is fetched and
        // its rules apply document-wide; `display: none` on the `noscript` does
        // not prevent that, because a stylesheet is not rendered. As text it is
        // neither fetched nor applied, which is what a scripting-enabled browser
        // does.
        let output = parse_document(
            "<!doctype html><body><noscript><link rel=stylesheet href=fallback.css></noscript>",
        );
        let links = find_all_elements(&output.dom, output.dom.document(), "link");
        assert!(
            links.is_empty(),
            "a stylesheet inside a noscript is inert text, not a link element to fetch"
        );
        assert_eq!(
            body_outline(
                "<!doctype html><body><noscript><link rel=stylesheet href=fallback.css></noscript>"
            ),
            "\
noscript@html
  #text \"<link rel=stylesheet href=fallback.css>\""
        );
        // A fallback image is not requested either.
        assert_eq!(
            body_outline("<!doctype html><body><noscript><img src=a.png></noscript>"),
            "\
noscript@html
  #text \"<img src=a.png>\""
        );
        // The raw text runs to the `</noscript>` end tag and no further, so a
        // following `title` in the head is still a head element.
        assert_eq!(
            document_outline(
                "<!doctype html><head><noscript><style>a{}</style><p>stray</noscript><title>t</title></head><body>x"
            ),
            "\
html@html
  head@html
    noscript@html
      #text \"<style>a{}</style><p>stray\"
    title@html
      #text \"t\"
  body@html
    #text \"x\""
        );
    }

    /// With scripting disabled, a `noscript` element's contents are markup, which
    /// is the fallback content the element exists to provide, and a `noscript` in
    /// the head is parsed by the "in head noscript" insertion mode.
    #[test]
    fn with_scripting_disabled_noscript_contents_are_markup() {
        fn parse_without_scripting(input: &str) -> String {
            let output = super::parse_document_with_scripting(input, false);
            outline(&output.dom, output.dom.document())
        }
        // In the body, a `noscript` has no special rule at all once scripting is
        // disabled, so its contents are ordinary body content.
        assert_eq!(
            parse_without_scripting(
                "<!doctype html><body><noscript><link rel=stylesheet href=fallback.css><img src=a.png></noscript>"
            ),
            "\
html@html
  head@html
  body@html
    noscript@html
      link@html
      img@html"
        );
        // In the head, the "in head noscript" mode delegates `style` to the
        // "in head" rules, so a fallback stylesheet written in the head is a real
        // stylesheet element and stays in the head.
        assert_eq!(
            parse_without_scripting(
                "<!doctype html><html><head><noscript><style>p{color:red}</style></noscript></head><body><p>x"
            ),
            "\
html@html
  head@html
    noscript@html
      style@html
        #text \"p{color:red}\"
  body@html
    p@html
      #text \"x\""
        );
    }

    /// The "in head noscript" insertion mode's "anything else" entry: an element
    /// that the mode does not list ends the `noscript`, and the token is
    /// reprocessed from "in head" onwards. The `</noscript>` end tag leaves the
    /// same way, without a parse error.
    #[test]
    fn in_head_noscript_ends_the_element_for_unlisted_content() {
        fn parse_without_scripting(input: &str) -> String {
            let output = super::parse_document_with_scripting(input, false);
            outline(&output.dom, output.dom.document())
        }
        // A `p` start tag is not one of the tokens the mode handles, so the
        // `noscript` is popped and the `p` is reprocessed: "in head" has no `p`
        // rule, so it closes the head, and "after head" has none either, so the
        // `p` is inserted in the body.
        assert_eq!(
            parse_without_scripting(
                "<!doctype html><head><noscript><style>a{}</style><p>stray</noscript><title>t</title></head><body>x"
            ),
            "\
html@html
  head@html
    noscript@html
      style@html
        #text \"a{}\"
  body@html
    p@html
      #text \"stray\"
      title@html
        #text \"t\"
      #text \"x\""
        );
        // A clean `</noscript>` end tag pops the element and returns to "in
        // head" without reporting a parse error, so what follows is still parsed
        // as head content.
        let output = super::parse_document_with_scripting(
            "<!doctype html><head><noscript><meta name=a></noscript><title>t</title></head><body>x",
            false,
        );
        assert_eq!(
            outline(&output.dom, output.dom.document()),
            "\
html@html
  head@html
    noscript@html
      meta@html
    title@html
      #text \"t\"
  body@html
    #text \"x\""
        );
        assert!(
            !output
                .errors
                .iter()
                .any(|error| error.code == HtmlParseErrorCode::UnexpectedToken),
            "a well-formed noscript in the head reports no parse error"
        );
        // A nested `noscript` or a `head` start tag inside one is a parse error
        // and is ignored, so it inserts nothing.
        let output = super::parse_document_with_scripting(
            "<!doctype html><head><noscript><noscript><head></noscript><title>t</title></head><body>x",
            false,
        );
        let head = find_element(&output.dom, output.dom.document(), "head").unwrap();
        assert_eq!(
            outline(&output.dom, head),
            "\
noscript@html
title@html
  #text \"t\""
        );
        assert!(
            error_codes_of(&output).contains(&HtmlParseErrorCode::UnexpectedToken),
            "a nested noscript or head start tag is a parse error"
        );
        // The end of file is not one of the tokens the mode lists, so it takes
        // the "anything else" path: the `noscript` is popped and the parse ends
        // in "in head", which closes the head and creates a body.
        assert_eq!(
            parse_without_scripting("<!doctype html><head><noscript><style>a{}</style>"),
            "\
html@html
  head@html
    noscript@html
      style@html
        #text \"a{}\"
  body@html"
        );
    }
    /// The form owner of a control, described by the `id` of its owning form, or
    /// `none`.
    fn form_owner_label(output: &super::ParseOutput, control: NodeId) -> String {
        match output.dom.form_owner(control) {
            Some(form) => format!(
                "form#{}",
                output
                    .dom
                    .attribute(form, "id")
                    .ok()
                    .flatten()
                    .unwrap_or("?")
            ),
            None => "none".to_owned(),
        }
    }

    /// The form element pointer and the DOM's form owner are two descriptions of
    /// one thing, so they are pinned against each other here rather than tested
    /// separately: the pair of cases below differ only in a `</form>` end tag,
    /// and the owner's answer changes with it.
    ///
    /// In both cases the form is inside a `table`, and the "in table" rules pop it
    /// off the stack of open elements, so the form is a *sibling* of the row
    /// rather than an ancestor of the cell. Nothing but the form element pointer
    /// can associate the control with it, which is what 13.2.4.4 means by
    /// associating controls with forms "in the face of dramatically bad markup".
    #[test]
    fn the_form_element_pointer_and_the_dom_owner_agree() {
        // The pointer is set by the `form` start tag, so a control created while
        // it is set is associated with that form even though the form is not its
        // ancestor.
        let output =
            parse_document("<!doctype html><body><table><form id=f><tr><td><input name=a></table>");
        let input = find_element(&output.dom, output.dom.document(), "input").unwrap();
        let form = find_element(&output.dom, output.dom.document(), "form").unwrap();
        // The form really is not an ancestor of the control, so the derived rules
        // alone would report no owner.
        assert_ne!(output.dom.parent(form), output.dom.parent(input));
        assert_eq!(form_owner_label(&output, input), "form#f");

        // Adding a `</form>` end tag clears the pointer — the end tag sets it to
        // null before anything else happens, and reports a parse error because the
        // form is not in scope any more. The control created afterwards is
        // therefore associated with nothing, which is the DOM's answer and not a
        // special case.
        let cleared = parse_document(
            "<!doctype html><body><table><form id=f></form><tr><td><input name=a></table>",
        );
        let input = find_element(&cleared.dom, cleared.dom.document(), "input").unwrap();
        assert_eq!(form_owner_label(&cleared, input), "none");
        assert!(
            error_codes(
                "<!doctype html><body><table><form id=f></form><tr><td><input name=a></table>"
            )
            .contains(&HtmlParseErrorCode::UnexpectedToken)
        );
    }

    /// A form start tag inside a table leaves the form in the tree but takes it off
    /// the stack of open elements, so the table content that follows is a sibling
    /// of the form rather than inside it (13.2.6.4.9).
    #[test]
    fn a_form_in_a_table_is_popped_from_the_stack_but_stays_in_the_tree() {
        let output = parse_document("<!doctype html><body><table><form id=f><tr><td>cell</table>");
        assert_eq!(
            body_outline("<!doctype html><body><table><form id=f><tr><td>cell</table>"),
            "\
table@html
  form@html
  tbody@html
    tr@html
      td@html
        #text \"cell\""
        );
        // Both are children of the table, which is what "popped off the stack"
        // leaves behind.
        let table = find_element(&output.dom, output.dom.document(), "table").unwrap();
        let form = find_element(&output.dom, table, "form").unwrap();
        assert_eq!(output.dom.parent(form), Some(table));
    }

    /// A second `form` start tag is a parse error and is ignored while the form
    /// element pointer is set, so the markup nests in the tree even though the
    /// tags are nested (13.2.6.4.7).
    #[test]
    fn a_second_form_start_tag_is_ignored_while_the_pointer_is_set() {
        let output = parse_document(
            "<!doctype html><body><form id=outer><div id=box><form id=inner><input name=a></form></div></form>",
        );
        // The inner `form` never becomes an element.
        let forms = find_all_elements(&output.dom, output.dom.document(), "form");
        assert_eq!(forms.len(), 1, "only the outer form is inserted");
        let form = forms[0];
        assert_eq!(
            output.dom.attribute(form, "id").ok().flatten(),
            Some("outer")
        );
        // The control's owner is that one form: the canonical shape from 4.10.2,
        // where a control inside a nested `form` tag belongs to the form the
        // parser actually opened.
        let input = find_element(&output.dom, output.dom.document(), "input").unwrap();
        assert_eq!(form_owner_label(&output, input), "form#outer");
        // And the walk stops there: no form above the outer one exists to be found.
        assert!(error_codes(
            "<!doctype html><body><form id=outer><div id=box><form id=inner><input name=a></form></div></form>"
        )
        .contains(&HtmlParseErrorCode::UnexpectedToken));
    }

    /// The form element pointer "is ignored inside template elements" (13.2.4.4),
    /// so a `form` in a template's contents neither sets the pointer for what
    /// follows it nor associates a control with a form from outside the template.
    #[test]
    fn the_form_element_pointer_is_ignored_inside_a_template() {
        let output = parse_document(
            "<!doctype html><body><template><form id=f><input name=a></form></template>",
        );
        let template = find_element(&output.dom, output.dom.document(), "template").unwrap();
        let contents = output.dom.template_contents(template).unwrap();
        let input = find_element(&output.dom, contents, "input").unwrap();
        let form = find_element(&output.dom, contents, "form").unwrap();
        // The pointer was not set, but the form is still the control's nearest
        // ancestor, so the owner is the same form either way. What is *not*
        // allowed is the pointer leaking out of the template and associating a
        // control that follows it in the document.
        assert_eq!(form_owner_label(&output, input), "form#f");
        assert!(output.dom.parent(form).is_some());

        // A control after the template, with no form of its own, is associated
        // with nothing: the template's `form` start tag did not set the pointer.
        let output =
            parse_document("<!doctype html><body><template><form id=f></template><input name=a>");
        let input = find_element(&output.dom, output.dom.document(), "input").unwrap();
        assert_eq!(form_owner_label(&output, input), "none");
    }

    /// The parser's association is discarded once the control is moved, so a
    /// wholesale replacement such as `innerHTML` re-resolves the owner instead of
    /// keeping the parser's choice. This is the worked example in 4.10.18.3: the
    /// parser associates the control with the inner nested form "c", and when the
    /// nodes are moved into the outer form's subtree the owner becomes "a".
    #[test]
    fn a_parser_association_does_not_survive_a_move() {
        let mut output = parse_document(
            "<!doctype html><body><form id=a><div id=b></div></form><table><form id=c><tr><td><input id=x name=i></table>",
        );
        let div = find_element(&output.dom, output.dom.document(), "div").unwrap();
        let control = find_element(&output.dom, output.dom.document(), "input").unwrap();
        // Before the move: the pointer's form, which is not an ancestor.
        assert_eq!(form_owner_label(&output, control), "form#c");
        // Move it, as the `innerHTML` algorithm does with the nodes of a temporary
        // document.
        output.dom.append_child(div, control).unwrap();
        assert_eq!(form_owner_label(&output, control), "form#a");
    }

    /// The form-associated elements of the standard, as the parser leaves them:
    /// each one inside a `form` has that form as its owner, and one outside a
    /// form with no `form` attribute has none.
    #[test]
    fn every_form_associated_element_takes_the_owning_form() {
        let output = parse_document(
            "<!doctype html><body><form id=f><button id=b></button><fieldset id=fs></fieldset><input id=i><object id=o></object><output id=ou></output><select id=s></select><textarea id=t></textarea><img id=im></form><button id=b2></button>",
        );
        for id in ["b", "fs", "i", "o", "ou", "s", "t", "im"] {
            let element = find_element_by_id(&output.dom, output.dom.document(), id).unwrap();
            assert!(output.dom.is_form_associated(element), "{id}");
            assert_eq!(form_owner_label(&output, element), "form#f", "{id}");
        }
        // `form` is not form-associated, and a control outside every form has no
        // owner.
        let form = find_element(&output.dom, output.dom.document(), "form").unwrap();
        assert!(!output.dom.is_form_associated(form));
        assert_eq!(output.dom.form_owner(form), None);
        let outside = find_element_by_id(&output.dom, output.dom.document(), "b2").unwrap();
        assert_eq!(form_owner_label(&output, outside), "none");
    }

    /// A `form` content attribute is honoured by the parser-built tree, including
    /// naming a form that is not an ancestor, and it is *not* silently ignored
    /// when it names nothing (4.10.18.3).
    #[test]
    fn a_form_content_attribute_decides_the_owner_in_a_parsed_tree() {
        for (markup, id, expected) in [
            (
                "<!doctype html><body><form id=f></form><div id=box><input id=i form=f name=a>",
                "i",
                "form#f",
            ),
            // Names an ID nothing has.
            (
                "<!doctype html><body><form id=f><input id=i form=typo name=a></form>",
                "i",
                "none",
            ),
            // Names an element that is not a form.
            (
                "<!doctype html><body><form id=f><div id=box><input id=i form=box name=a></div></form>",
                "i",
                "none",
            ),
        ] {
            let output = parse_document(markup);
            let control = find_element_by_id(&output.dom, output.dom.document(), id).unwrap();
            assert_eq!(form_owner_label(&output, control), expected, "{markup}");
        }
    }
}
