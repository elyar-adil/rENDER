use crate::dom::{Dom, Namespace, NodeId, NodeKind};

use super::tokenizer::{
    ContentModel, DoctypeToken, HtmlParseError, HtmlParseErrorCode, TagToken, Token, Tokenizer,
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
    AfterHead,
    InBody,
    Text,
    InTable,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    AfterBody,
    AfterAfterBody,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Consumed,
    Reprocess,
}

/// Parse an HTML document into the Rust DOM.
#[must_use]
pub fn parse_document(input: &str) -> ParseOutput {
    TreeBuilder::new(input).parse()
}

struct TreeBuilder<'a> {
    tokenizer: Tokenizer<'a>,
    dom: Dom,
    open_elements: Vec<NodeId>,
    mode: InsertionMode,
    original_mode: InsertionMode,
    head_element: Option<NodeId>,
    body_element: Option<NodeId>,
    tree_errors: Vec<HtmlParseError>,
    quirks_mode: QuirksMode,
    foster_parenting: bool,
    ignore_next_line_feed: bool,
    temporary_head: Option<NodeId>,
}

impl<'a> TreeBuilder<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            tokenizer: Tokenizer::new(input),
            dom: Dom::new(),
            open_elements: Vec::new(),
            mode: InsertionMode::Initial,
            original_mode: InsertionMode::Initial,
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

    fn process(&mut self, token: &Token) -> Action {
        match self.mode {
            InsertionMode::Initial => self.process_initial(token),
            InsertionMode::BeforeHtml => self.process_before_html(token),
            InsertionMode::BeforeHead => self.process_before_head(token),
            InsertionMode::InHead => self.process_in_head(token),
            InsertionMode::AfterHead => self.process_after_head(token),
            InsertionMode::InBody => self.process_in_body(token),
            InsertionMode::Text => self.process_text(token),
            InsertionMode::InTable => self.process_in_table(token),
            InsertionMode::InCaption => self.process_in_caption(token),
            InsertionMode::InColumnGroup => self.process_in_column_group(token),
            InsertionMode::InTableBody => self.process_in_table_body(token),
            InsertionMode::InRow => self.process_in_row(token),
            InsertionMode::InCell => self.process_in_cell(token),
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
            Token::StartTag(tag) if matches!(tag.name.as_str(), "style" | "noframes") => {
                self.enter_text_element(tag, ContentModel::RawText);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "script" => {
                self.enter_text_element(tag, ContentModel::ScriptData);
                Action::Consumed
            }
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
            Token::Comment(data) => {
                self.insert_comment(self.current_node(), data);
                Action::Consumed
            }
            Token::Doctype(_) => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "html" => {
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
                        | "title"
                ) =>
            {
                self.process_in_head(token)
            }
            Token::StartTag(tag) if tag.name == "body" => {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                if let Some(body) = self.body_element {
                    self.merge_attributes(body, tag);
                }
                Action::Consumed
            }
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
            Token::StartTag(tag) if matches!(tag.name.as_str(), "pre" | "listing") => {
                self.close_p_if_open();
                self.insert_element(tag, true);
                self.ignore_next_line_feed = true;
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
            Token::StartTag(tag) if matches!(tag.name.as_str(), "xmp" | "iframe" | "noembed") => {
                self.close_p_if_open();
                self.enter_text_element(tag, ContentModel::RawText);
                Action::Consumed
            }
            Token::StartTag(tag) if tag.name == "script" => {
                self.enter_text_element(tag, ContentModel::ScriptData);
                Action::Consumed
            }
            Token::StartTag(tag) if is_void_element(&tag.name) => {
                if tag.name == "hr" {
                    self.close_p_if_open();
                }
                self.insert_element(tag, false);
                Action::Consumed
            }
            Token::StartTag(tag) => {
                self.insert_element(tag, true);
                if tag.self_closing {
                    self.parse_error(
                        HtmlParseErrorCode::NonVoidHtmlElementStartTagWithTrailingSolidus,
                    );
                }
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
            Token::EndTag(tag) if tag.name == "p" => {
                if !self.has_open_element("p") {
                    self.parse_error(HtmlParseErrorCode::UnexpectedToken);
                    self.insert_element(&empty_tag("p"), true);
                }
                self.pop_through("p");
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
            Token::EndTag(tag) => {
                self.pop_through_if_open(&tag.name);
                Action::Consumed
            }
            Token::Eof => Action::Consumed,
        }
    }

    fn process_in_table(&mut self, token: &Token) -> Action {
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
            Token::StartTag(tag) if tag.name == "caption" => {
                self.clear_stack_to_table_context();
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

    fn process_in_caption(&mut self, token: &Token) -> Action {
        match token {
            Token::EndTag(tag) if tag.name == "caption" => {
                if self.has_open_element("caption") {
                    self.pop_through("caption");
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
            Token::StartTag(tag) if matches!(tag.name.as_str(), "td" | "th") => {
                self.clear_stack_to_table_row_context();
                self.insert_element(tag, true);
                self.mode = InsertionMode::InCell;
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
            Token::EndTag(tag) if matches!(tag.name.as_str(), "td" | "th") => {
                if self.has_open_element(&tag.name) {
                    self.pop_through(&tag.name);
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
        let element = self
            .dom
            .create_element_ns(Namespace::Html, tag.name.clone());
        self.apply_attributes(element, tag);
        let result = if self.foster_parenting {
            let (parent, reference) = self.foster_location();
            self.dom.insert_before(parent, element, reference)
        } else {
            self.dom.append_child(self.current_node(), element)
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
        if !self.foster_parenting {
            if self.dom.append_text(self.current_node(), data).is_err() {
                self.parse_error(HtmlParseErrorCode::UnexpectedToken);
            }
            return;
        }
        let (parent, reference) = self.foster_location();
        if let Some(reference) = reference
            && let Some(previous) = self.dom.previous_sibling(reference)
            && let Some(NodeKind::Text(existing)) =
                self.dom.node(previous).map(crate::dom::Node::kind)
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
        if self.dom.append_child(parent, comment).is_err() {
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

    fn has_open_element(&self, name: &str) -> bool {
        self.open_elements
            .iter()
            .any(|node| self.element_name(*node) == Some(name))
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
        if let Some(index) = self
            .open_elements
            .iter()
            .rposition(|node| self.element_name(*node) == Some(name))
        {
            self.open_elements.truncate(index);
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

    fn foster_location(&self) -> (NodeId, Option<NodeId>) {
        if let Some(table) = self
            .open_elements
            .iter()
            .rev()
            .copied()
            .find(|node| self.element_name(*node) == Some("table"))
        {
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
            self.pop_through(&name);
            self.mode = InsertionMode::InRow;
        }
    }

    fn reset_insertion_mode(&mut self) {
        self.mode = self
            .open_elements
            .iter()
            .rev()
            .filter_map(|node| self.element_name(*node))
            .find_map(|name| match name {
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
            })
            .unwrap_or(InsertionMode::InBody);
    }

    fn parse_error(&mut self, code: HtmlParseErrorCode) {
        self.tree_errors.push(HtmlParseError {
            offset: self.tokenizer.offset(),
            code,
        });
    }
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
    use crate::dom::{Dom, NodeId, NodeKind};

    use super::{QuirksMode, parse_document};

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

    fn text_content(dom: &Dom, node: NodeId) -> String {
        let mut output = String::new();
        for child in dom.children(node).unwrap_or_default() {
            match dom.node(*child).map(crate::dom::Node::kind) {
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
}
