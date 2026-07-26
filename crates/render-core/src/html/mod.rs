//! WHATWG-oriented HTML tokenization and tree construction.

mod encoding;
mod tokenizer;
mod tree_builder;

pub use encoding::{
    DecodedHtml, EncodingDeclarationSource, HtmlDecodeDiagnostic, HtmlDecodeDiagnosticCode,
    HtmlDecodeError, HtmlDecodeLimits, HtmlDecodeOptions, HtmlEncodingSource, decode_html_bytes,
};
pub use tokenizer::{
    AttributeToken, ContentModel, DoctypeToken, HtmlParseError, HtmlParseErrorCode, TagToken,
    Token, Tokenizer,
};
pub use tree_builder::{ParseOutput, QuirksMode, parse_document};
