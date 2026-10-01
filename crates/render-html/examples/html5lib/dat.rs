//! Parser for the html5lib tree-construction `.dat` format.
//!
//! The format is deliberately small: a file is a sequence of tests separated by
//! blank lines, and each test is a sequence of sections introduced by a line
//! that is exactly one of a fixed set of keywords. The section bodies are raw
//! lines; nothing needs unescaping, so no serialisation library is involved.
//!
//! See `tree-construction/README.md` in the fetched suite for the authoritative
//! description of the format.

use std::fmt;
use std::fs;
use std::path::Path;

/// A context element, as named by a `#document-fragment` section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextElement {
    pub namespace: &'static str,
    pub local_name: String,
}

impl fmt::Display for ContextElement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.namespace, self.local_name)
    }
}

/// What a test says about the parser's scripting flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scripting {
    /// `#script-on`: run only with scripting enabled.
    On,
    /// `#script-off`: run only with scripting disabled.
    Off,
    /// Neither directive: run in both modes.
    Both,
}

impl Scripting {
    /// The modes this directive asks for, as `scripting_enabled` flags.
    #[must_use]
    pub fn modes(self) -> Vec<bool> {
        match self {
            Self::On => vec![true],
            Self::Off => vec![false],
            Self::Both => vec![true, false],
        }
    }
}
/// One test from a `.dat` file.
#[derive(Clone, Debug)]
pub struct DatTest {
    /// Source file name, without the directory.
    pub file: String,
    /// Zero-based index of the test within the file.
    pub index: usize,
    /// The `#data` body, with the final newline removed.
    pub data: String,
    /// The number of parse errors the suite expects in the `#errors` section.
    pub expected_error_count: usize,
    /// The number in the optional `#new-errors` section.
    ///
    /// **This is a renaming of `#errors`, not a list of extra errors**, and the
    /// two must not simply be added together. The suite carries the legacy
    /// diagnostic vocabulary in `#errors` and the current standard's names in
    /// `#new-errors`, and the same condition appears in both: 20 cases list one
    /// identical name in both sections, and of the 295 cases carrying a
    /// `#new-errors` section, 291 have a `#new-errors` at least as short as
    /// their `#errors`. So a parser that reports each condition once under its
    /// current name is correct, and a summed count asks it to report the same
    /// diagnostic twice under two spellings.
    ///
    /// The four cases where `#new-errors` is *longer* than `#errors` are
    /// genuine additions: NUL characters in `plain-text-unsafe.dat`, where
    /// `unexpected-null-character` is a condition the legacy list did not
    /// express at all.
    pub expected_new_error_count: usize,
    /// The `#errors` lines themselves, in order.
    ///
    /// Only the **count** is a conformance requirement -- the format says "it
    /// doesn't matter what those lines are" -- but a count with no names cannot
    /// be acted on, and the names are what turn "under-reports by one" into a
    /// rule to look at. They are kept for reporting and never compared.
    pub expected_error_names: Vec<String>,
    /// The `#new-errors` lines, for the same reason.
    pub expected_new_error_names: Vec<String>,
    pub fragment_context: Option<ContextElement>,
    pub scripting: Scripting,
    /// The `#document` tree dump, one entry per line, `| ` prefix included.
    pub expected_tree: Vec<String>,
    /// The HTML fragment serialisation that follows the tree when the test has
    /// a `#document-fragment` section.
    pub expected_serialization: Option<String>,
}

impl DatTest {
    /// `file.dat#index`, a stable identifier for a test across runs.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}#{}", self.file, self.index)
    }
}

/// A suite file that could not be read.
#[derive(Debug)]
pub struct SuiteReadError {
    pub path: String,
    pub message: String,
}

impl fmt::Display for SuiteReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Data,
    Errors,
    NewErrors,
    DocumentFragment,
    Document,
}

struct PartialTest {
    data: String,
    errors: usize,
    new_errors: usize,
    error_names: Vec<String>,
    new_error_names: Vec<String>,
    fragment_context: Option<ContextElement>,
    scripting: Scripting,
    document: Document,
    serialization: Option<String>,
}

impl PartialTest {
    fn new() -> Self {
        Self {
            data: String::new(),
            errors: 0,
            new_errors: 0,
            error_names: Vec::new(),
            new_error_names: Vec::new(),
            fragment_context: None,
            scripting: Scripting::Both,
            document: Document::new(),
            serialization: None,
        }
    }
}

/// Parse one `.dat` file into its tests.
pub fn parse_file(path: &Path) -> Result<Vec<DatTest>, SuiteReadError> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let text = fs::read_to_string(path).map_err(|error| SuiteReadError {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let mut tests = parse_text(&file_name, &text);
    for (index, test) in tests.iter_mut().enumerate() {
        test.index = index;
    }
    Ok(tests)
}

/// Parse the text of a `.dat` file.
///
/// Section keywords are matched only when they are the whole line, which is
/// what the format requires, so a `#data` body containing text that happens to
/// start with `#` cannot be mistaken for a section header. A test ends when the
/// next `#data` begins, which is why a blank line inside a section body is not
/// treated as a separator.
pub fn parse_text(file_name: &str, text: &str) -> Vec<DatTest> {
    let mut tests = Vec::new();
    let mut current: Option<PartialTest> = None;
    let mut section = Section::None;
    let mut index = 0usize;

    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match line {
            "#data" => {
                if let Some(finished) = current.take() {
                    tests.push(finish(file_name, index, finished));
                    index += 1;
                }
                current = Some(PartialTest::new());
                section = Section::Data;
                continue;
            }
            "#errors" => {
                section = Section::Errors;
                continue;
            }
            "#new-errors" => {
                section = Section::NewErrors;
                continue;
            }
            "#document-fragment" => {
                section = Section::DocumentFragment;
                continue;
            }
            "#script-on" => {
                if let Some(test) = current.as_mut() {
                    test.scripting = Scripting::On;
                }
                continue;
            }
            "#script-off" => {
                if let Some(test) = current.as_mut() {
                    test.scripting = Scripting::Off;
                }
                continue;
            }
            "#document" => {
                section = Section::Document;
                continue;
            }
            _ => {}
        }

        let Some(test) = current.as_mut() else {
            continue;
        };
        match section {
            Section::None => {}
            Section::Data => {
                test.data.push_str(line);
                test.data.push('\n');
            }
            Section::Errors => {
                if !line.is_empty() {
                    test.errors += 1;
                    test.error_names.push(line.to_owned());
                }
            }
            Section::NewErrors => {
                if !line.is_empty() {
                    test.new_errors += 1;
                    test.new_error_names.push(line.to_owned());
                }
            }
            Section::DocumentFragment => {
                if test.fragment_context.is_none() && !line.is_empty() {
                    test.fragment_context = Some(parse_context_element(line));
                }
            }
            Section::Document => {
                // A line that continues an unfinished node is part of that node
                // even though it does not start with `|`, so the pending flag has
                // to be consulted before the serialisation test.
                let is_node_line = test.document.is_pending() || line.trim_start().starts_with('|');
                if is_node_line {
                    test.document.push(line);
                } else if !line.is_empty() && test.fragment_context.is_some() {
                    // With a context element, the serialisation of the fragment
                    // follows the tree.
                    test.serialization = Some(line.to_owned());
                }
            }
        }
    }
    if let Some(finished) = current.take() {
        tests.push(finish(file_name, index, finished));
    }
    tests
}

/// A node whose value may span more than one physical line.
///
/// The format says "Newlines aren't escaped", so a text node holding a newline
/// is written as an opening quote, the value with its newline in it, and a
/// closing quote. Reading the tree one physical line at a time therefore
/// produces a truncated value and a spurious "missing node", which is a harness
/// artefact wearing the costume of an engine defect. The terminator for each
/// kind of node is the first occurrence of its closing sequence that is
/// immediately followed by the end of a line, which is the only place a node can
/// end unambiguously.
struct Document {
    pending: Option<String>,
    nodes: Vec<String>,
}

impl Document {
    fn new() -> Self {
        Self {
            pending: None,
            nodes: Vec::new(),
        }
    }

    fn push(&mut self, line: &str) {
        if let Some(pending) = self.pending.as_mut() {
            pending.push('\n');
            pending.push_str(line);
            if node_ends_here(pending) {
                self.nodes
                    .push(self.pending.take().expect("pending was just borrowed"));
            }
            return;
        }
        let node = line.to_owned();
        if !node_ends_here(&node) {
            self.pending = Some(node);
            return;
        }
        self.nodes.push(node);
    }

    fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    fn finish(self) -> Vec<String> {
        let mut nodes = self.nodes;
        if let Some(pending) = self.pending {
            // A tree that ends mid-node is malformed; keep what was read rather
            // than dropping it, so the comparison still reports a difference.
            nodes.push(pending);
        }
        nodes
    }
}
/// Whether a node is complete at the end of the text read for it so far.
///
/// The opening delimiter has already been consumed by the caller, which is what
/// keeps the empty value `""` from being read as an unterminated node and stops
/// a value that contains its own closing character from ending early.
fn node_ends_here(node: &str) -> bool {
    let Some(payload) = node.trim_start().strip_prefix("| ") else {
        return true;
    };
    let payload = payload.trim_start();
    if payload == "content" {
        return true;
    }
    if payload.starts_with("<!-- ") {
        return payload.ends_with("-->");
    }
    if payload.starts_with('"') {
        // `"` + value + `"`: the value must be non-empty, so a lone opening
        // quote is a node whose value continues on the next line.
        return payload.len() > 1 && payload.ends_with('"');
    }
    if payload.starts_with('<') {
        // An element, a DOCTYPE, or a processing instruction: each is written
        // as a single line that ends at its `>`.
        //
        // This test has to come before the attribute case below, because a
        // DOCTYPE's public and system identifiers are quoted and a DOCTYPE line
        // can therefore contain `"` without being an attribute.
        return payload.ends_with('>');
    }
    // `name="value"`.
    payload.ends_with('"')
}

fn finish(file_name: &str, index: usize, mut test: PartialTest) -> DatTest {
    // The format says the final newline of the `#data` body is not part of the
    // input.
    if test.data.ends_with('\n') {
        test.data.pop();
    }
    DatTest {
        file: file_name.to_owned(),
        index,
        data: test.data,
        expected_error_count: test.errors,
        expected_new_error_count: test.new_errors,
        expected_error_names: test.error_names,
        expected_new_error_names: test.new_error_names,
        fragment_context: test.fragment_context,
        scripting: test.scripting,
        expected_tree: test.document.finish(),
        expected_serialization: test.serialization,
    }
}

fn parse_context_element(line: &str) -> ContextElement {
    if let Some(rest) = line.strip_prefix("svg ") {
        return ContextElement {
            namespace: "svg",
            local_name: rest.to_owned(),
        };
    }
    if let Some(rest) = line.strip_prefix("math ") {
        return ContextElement {
            namespace: "math",
            local_name: rest.to_owned(),
        };
    }
    ContextElement {
        namespace: "html",
        local_name: line.to_owned(),
    }
}
