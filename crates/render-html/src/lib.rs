//! WHATWG-oriented HTML tokenization and tree construction.

mod encoding;
mod serialization;
mod tokenizer;
mod tree_builder;

pub use encoding::{
    DecodedHtml, EncodingDeclarationSource, HtmlDecodeDiagnostic, HtmlDecodeDiagnosticCode,
    HtmlDecodeError, HtmlDecodeLimits, HtmlDecodeOptions, HtmlEncodingSource, decode_html_bytes,
};
pub use serialization::{
    serialize_html_fragment, serialize_html_fragment_with_scripting, serialize_html_node,
    serialize_html_node_with_scripting,
};
pub use tokenizer::{
    AttributeToken, ContentModel, DoctypeToken, HtmlParseError, HtmlParseErrorCode, TagToken,
    Token, Tokenizer,
};
pub use tree_builder::{ParseOutput, QuirksMode, parse_document, parse_document_with_scripting};
