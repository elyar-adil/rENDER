use render_core::html::parse_document;
use render_core::js::{JsErrorKind, JsRuntime};

fn verdict(source: &str) -> Option<bool> {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    match runtime.execute(&mut parsed.dom, source) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == JsErrorKind::Syntax => None,
        Err(error) => {
            eprintln!("    [verdict] {}", error.message());
            for (depth, frame) in runtime.debug_call_stack().iter().enumerate().rev() {
                eprintln!("      #{depth}: {frame}");
            }
            Some(false)
        }
    }
}

struct Scan {
    cuts: Vec<usize>,
    functions: Vec<(usize, usize)>,
}

#[allow(
    clippy::too_many_lines,
    reason = "single-pass scanner covering strings, comments, and regex literals"
)]
fn scan(body: &str) -> Scan {
    let characters: Vec<char> = body.chars().collect();
    let mut cuts = Vec::new();
    let mut functions = Vec::new();
    let mut depth = 0i64;
    let mut index = 0usize;
    let mut regex_allowed = true;
    while index < characters.len() {
        let character = characters[index];
        match character {
            '"' | '\'' => {
                index += 1;
                while index < characters.len() {
                    if characters[index] == '\\' {
                        index += 2;
                        continue;
                    }
                    if characters[index] == character {
                        break;
                    }
                    index += 1;
                }
                regex_allowed = false;
            }
            '/' if characters.get(index + 1) == Some(&'/') => {
                while index < characters.len() && characters[index] != '\n' {
                    index += 1;
                }
            }
            '/' if characters.get(index + 1) == Some(&'*') => {
                index += 2;
                while index + 1 < characters.len()
                    && !(characters[index] == '*' && characters[index + 1] == '/')
                {
                    index += 1;
                }
                index += 1;
            }
            '/' if regex_allowed && characters.get(index + 1) != Some(&'=') => {
                index += 1;
                let mut in_class = false;
                while index < characters.len() {
                    match characters[index] {
                        '\\' => index += 1,
                        '[' => in_class = true,
                        ']' => in_class = false,
                        '/' if !in_class => break,
                        '\n' => break,
                        _ => {}
                    }
                    index += 1;
                }
                while index + 1 < characters.len() && characters[index + 1].is_ascii_alphabetic() {
                    index += 1;
                }
                regex_allowed = false;
            }
            '(' | '[' | '{' => {
                depth += 1;
                regex_allowed = true;
            }
            ')' | ']' | '}' => {
                depth -= 1;
                regex_allowed = false;
            }
            'f' if depth == 0
                && characters.len() >= index + 9
                && characters[index..index + 9].iter().collect::<String>() == "function "
                && characters
                    .get(index.wrapping_sub(1))
                    .is_none_or(|previous| !previous.is_ascii_alphanumeric()) =>
            {
                // Top-level function declaration: record span through its
                // closing brace.
                if let Some(open_brace) = characters[index..].iter().position(|c| *c == '{') {
                    let brace_start = index + open_brace;
                    let mut local_depth = 0i64;
                    let mut cursor = brace_start;
                    while cursor < characters.len() {
                        match characters[cursor] {
                            '{' => local_depth += 1,
                            '}' => {
                                local_depth -= 1;
                                if local_depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        cursor += 1;
                    }
                    functions.push((index, cursor + 1));
                    index = cursor;
                }
                regex_allowed = false;
            }
            ';' if depth == 0 => cuts.push(index + 1),
            c if c.is_whitespace() => {}
            c => {
                regex_allowed =
                    !(c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '.' || c == ')');
            }
        }
        index += 1;
    }
    Scan { cuts, functions }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: bisect <file>");
    let source = std::fs::read_to_string(&path).expect("readable script");
    let head = "(function(window,undefined){".to_owned();
    let tail = "})(window);".to_owned();

    let Some(start) = source.find(&head) else {
        panic!("wrapper head not found");
    };
    let body_start = start + head.len();
    let Some(body_end) = source.rfind(&tail) else {
        panic!("wrapper tail not found");
    };
    let body: Vec<char> = source[body_start..body_end].chars().collect();

    let scan = scan(&body.iter().collect::<String>());
    println!(
        "cuts {} functions {}",
        scan.cuts.len(),
        scan.functions.len()
    );

    let slice_text = |range: std::ops::Range<usize>| -> String {
        range
            .start
            .checked_mul(0)
            .map_or_else(String::new, |_| String::new())
            + &body[range].iter().collect::<String>()
    };

    let build = |cut: usize, hoist_from: usize| -> String {
        let mut candidate = format!("{head}{}", slice_text(0..cut));
        for (function_start, function_end) in &scan.functions {
            if *function_start >= hoist_from {
                candidate.push_str(&slice_text(*function_start..*function_end));
                candidate.push(';');
            }
        }
        candidate.push_str(&tail);
        candidate
    };

    let mut last_ok = 0usize;
    for cut in &scan.cuts {
        let candidate = build(*cut, *cut);
        match verdict(&candidate) {
            Some(true) => last_ok = *cut,
            Some(false) => {
                println!("failure between ok-cut {last_ok} and failing cut {cut}");
                let from = cut.saturating_sub(220);
                let to = (*cut + 60).min(body.len());
                println!(
                    "FAIL ...{}>>><<<{}...",
                    slice_text(from..*cut),
                    slice_text(*cut..to)
                );
                return;
            }
            None => {}
        }
    }
    println!("no failing prefix; last ok {last_ok}");
}
