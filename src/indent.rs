//! Indentation rules: where a new line starts, where a closing brace belongs,
//! and which line the formatter should tidy after a keystroke.

use crate::complete::{blank_comments_and_strings, in_string_or_comment};

pub const UNIT: &str = "    ";

fn line_start(s: &[u8], at: usize) -> usize {
    s[..at].iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1)
}

fn line_end(s: &[u8], at: usize) -> usize {
    s[at..].iter().position(|&c| c == b'\n').map_or(s.len(), |i| at + i)
}

fn indent_at(text: &str, ls: usize) -> &str {
    let rest = &text[ls..];
    let n = rest.bytes().take_while(|b| matches!(b, b' ' | b'\t')).count();
    &rest[..n]
}

/// The nearest line above the one starting at `ls` that holds code.
fn previous_code_line(clean: &[u8], ls: usize) -> Option<usize> {
    let mut at = ls;
    while at > 0 {
        let start = line_start(clean, at - 1);
        if !clean[start..at - 1].trim_ascii().is_empty() {
            return Some(start);
        }
        at = start;
    }
    None
}

/// The line ends in an operator, so the expression goes on below it.
fn continues(code: &[u8]) -> bool {
    if code.ends_with(b"++") || code.ends_with(b"--") {
        return false;
    }
    code.last().is_some_and(|c| b"=+-*/%?&|^".contains(c)) || code.ends_with(b"->")
}

/// Where the `(` or `[` opens that this line closes without having opened it.
fn unclosed_from_above(clean: &[u8], ls: usize, le: usize) -> Option<usize> {
    let (mut open, mut unmatched) = (0usize, 0usize);
    for &c in &clean[ls..le] {
        match c {
            b'(' | b'[' => open += 1,
            b')' | b']' if open > 0 => open -= 1,
            b')' | b']' => unmatched += 1,
            _ => {}
        }
    }
    if unmatched == 0 {
        return None;
    }
    for i in (0..ls).rev() {
        match clean[i] {
            b')' | b']' => unmatched += 1,
            b'(' | b'[' => {
                unmatched -= 1;
                if unmatched == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The first line of the statement that the line at `ls` belongs to.
fn statement_line(clean: &[u8], mut ls: usize) -> usize {
    for _ in 0..64 {
        let le = line_end(clean, ls);
        if let Some(open) = unclosed_from_above(clean, ls, le) {
            ls = line_start(clean, open);
            continue;
        }
        let Some(prev) = previous_code_line(clean, ls) else { break };
        let prev_code = clean[prev..line_end(clean, prev)].trim_ascii();
        if clean[ls..le].trim_ascii().starts_with(b".") || continues(prev_code) {
            ls = prev;
        } else {
            break;
        }
    }
    ls
}

fn starts_with_word(code: &[u8], word: &[u8]) -> bool {
    code.starts_with(word)
        && !code.get(word.len()).is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$')
}

/// `if (...)`, `for (...)`, `while (...)`, `else` or `do` with no body yet.
fn is_control_header(code: &[u8]) -> bool {
    if code == b"else" || code == b"do" {
        return true;
    }
    if ![&b"if"[..], b"for", b"while", b"else"].iter().any(|w| starts_with_word(code, w)) || code.last() != Some(&b')') {
        return false;
    }
    let Some(open) = code.iter().position(|&c| c == b'(') else { return false };
    let mut depth = 0usize;
    for (i, &c) in code.iter().enumerate().skip(open) {
        match c {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i == code.len() - 1;
                }
            }
            _ => {}
        }
    }
    false
}

fn is_case_label(code: &[u8]) -> bool {
    (starts_with_word(code, b"case") || starts_with_word(code, b"default")) && (code.ends_with(b":") || code.ends_with(b"->"))
}

/// The indent after a finished statement, which also ends any brace-less
/// `if`/`for`/`while` it was the body of.
fn after_statement<'a>(text: &'a str, clean: &[u8], ls: usize) -> &'a str {
    let mut line = statement_line(clean, ls);
    for _ in 0..16 {
        let Some(prev) = previous_code_line(clean, line) else { break };
        let head = statement_line(clean, prev);
        if !is_control_header(clean[head..line_end(clean, prev)].trim_ascii()) {
            break;
        }
        line = head;
    }
    indent_at(text, line)
}

/// The indent for the line after the `\n` at byte `nl`.
pub fn new_line_indent(text: &str, nl: usize) -> String {
    let blanked = blank_comments_and_strings(text);
    let clean = blanked.as_bytes();
    let ls = line_start(clean, nl);
    let own = indent_at(text, ls);
    let code = clean[ls..nl].trim_ascii();
    let Some(&last) = code.last() else { return own.to_string() };
    let head = statement_line(clean, ls);
    let stmt = indent_at(text, head);
    if last == b'{' {
        format!("{stmt}{UNIT}")
    } else if matches!(last, b'(' | b'[') || is_case_label(code) {
        format!("{own}{UNIT}")
    } else if is_control_header(clean[head..nl].trim_ascii()) {
        format!("{stmt}{UNIT}")
    } else if continues(code) {
        if head == ls { format!("{own}{UNIT}") } else { own.to_string() }
    } else if matches!(last, b';' | b'}') {
        after_statement(text, clean, ls).to_string()
    } else if unclosed_from_above(clean, ls, nl).is_some() {
        stmt.to_string()
    } else {
        own.to_string()
    }
}

/// The indent a `}` at byte `at` should have: that of the statement its `{` opens.
pub fn closing_indent(text: &str, at: usize) -> Option<String> {
    let blanked = blank_comments_and_strings(text);
    let clean = blanked.as_bytes();
    let mut depth = 0usize;
    for i in (0..at).rev() {
        match clean[i] {
            b'}' => depth += 1,
            b'{' if depth == 0 => return Some(indent_at(text, statement_line(clean, line_start(clean, i))).to_string()),
            b'{' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Inside a block comment, the prefix for the new line after the `\n` at `nl`,
/// and the closing line to add when the comment isn't closed yet.
pub fn comment_continuation(text: &str, nl: usize) -> Option<(String, Option<String>)> {
    let before = &text[..nl];
    let open = before.rfind("/*")?;
    if before[open..].contains("*/") || in_string_or_comment(&text[..open]) {
        return None;
    }
    let ls = before.rfind('\n').map_or(0, |i| i + 1);
    let ws = indent_at(text, ls);
    let line = before[ls..].trim_start();
    if line.starts_with("/*") {
        let after = &text[nl + 1..];
        let closed = match (after.find("*/"), after.find("/*")) {
            (Some(c), Some(o)) => c < o,
            (Some(_), None) => true,
            _ => false,
        };
        Some((format!("{ws} * "), (!closed).then(|| format!("{ws} */"))))
    } else if line.starts_with('*') {
        Some((format!("{ws}* "), None))
    } else {
        Some((ws.to_string(), None))
    }
}

/// The lines worth formatting after `typed` was entered, with the cursor at byte `at`.
pub fn format_line(text: &str, at: usize, typed: char) -> Option<(usize, usize)> {
    let blanked = blank_comments_and_strings(text);
    let clean = blanked.as_bytes();
    let number = |ls: usize| clean[..ls].iter().filter(|&&c| c == b'\n').count();
    if typed == '\n' {
        let ls = line_start(clean, at);
        if ls == 0 {
            return None;
        }
        let prev = line_start(clean, ls - 1);
        let code = clean[prev..ls - 1].trim_ascii();
        if code.is_empty() {
            return None;
        }
        let first = if code.ends_with(b";") { first_header_line(clean, prev) } else { prev };
        return Some((number(first), number(prev)));
    }
    if at == 0 || !matches!(typed, ';' | '}' | ':') || clean[at - 1] != typed as u8 {
        return None;
    }
    let ls = line_start(clean, at);
    let code = clean[ls..at].trim_ascii();
    let wanted = match typed {
        // Inside `for (...;` the statement isn't complete yet.
        ';' => code.iter().filter(|&&c| c == b'(').count() <= code.iter().filter(|&&c| c == b')').count(),
        ':' => is_case_label(code),
        _ => true,
    };
    if !wanted {
        return None;
    }
    let first = if typed == ';' { first_header_line(clean, ls) } else { ls };
    Some((number(first), number(ls)))
}

/// A brace-less `if (...)` can't be parsed until its statement exists, so it's
/// formatted together with it. Returns the topmost such header above `ls`.
fn first_header_line(clean: &[u8], ls: usize) -> usize {
    let mut first = ls;
    while let Some(prev) = previous_code_line(clean, first) {
        if !is_control_header(clean[prev..line_end(clean, prev)].trim_ascii()) {
            break;
        }
        first = prev;
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The indent after pressing Enter at the `|` marker.
    fn after_enter(src: &str) -> String {
        let at = src.find('|').unwrap();
        let text = src.replacen('|', "\n", 1);
        new_line_indent(&text, at)
    }

    #[test]
    fn blocks_and_brackets_indent() {
        assert_eq!(after_enter("class A {|"), "    ");
        assert_eq!(after_enter("    void f() { // note|"), "        ");
        assert_eq!(after_enter("    foo(|"), "        ");
        assert_eq!(after_enter("    int[] a = [|"), "        ");
    }

    #[test]
    fn a_body_lines_up_with_a_wrapped_header() {
        assert_eq!(after_enter("    void f(int a,\n            int b) {|"), "        ");
    }

    #[test]
    fn brace_less_control_flow() {
        assert_eq!(after_enter("    if (a && (b > c))|"), "        ");
        assert_eq!(after_enter("    else|"), "        ");
        assert_eq!(after_enter("    for (int i = 0; i < n; i++)|"), "        ");
        assert_eq!(after_enter("    if (a)\n        foo();|"), "    ");
        assert_eq!(after_enter("    if (a)\n        if (b)\n            foo();|"), "    ");
        // a complete one-liner is not a header
        assert_eq!(after_enter("    if (a) foo();|"), "    ");
        assert_eq!(after_enter("    do { x(); } while (a);|"), "    ");
    }

    #[test]
    fn switch_labels() {
        assert_eq!(after_enter("        case 1:|"), "            ");
        assert_eq!(after_enter("        default ->|"), "            ");
    }

    #[test]
    fn continuation_lines_return_to_the_statement() {
        assert_eq!(after_enter("    int x = 1 +|"), "        ");
        assert_eq!(after_enter("    int x = 1 +\n        2 +|"), "        ");
        assert_eq!(after_enter("    int x = 1 +\n        2;|"), "    ");
        assert_eq!(after_enter("    foo(a,\n        b);|"), "    ");
        assert_eq!(after_enter("    list.stream()\n        .map(f)\n        .toList();|"), "    ");
        assert_eq!(after_enter("    run(() -> {\n        x();\n    });|"), "    ");
        assert_eq!(after_enter("    i++;|"), "    ");
    }

    #[test]
    fn comments_and_strings_do_not_count() {
        assert_eq!(after_enter("    int x; // {|"), "    ");
        assert_eq!(after_enter("    String s = \"{\";|"), "    ");
        assert_eq!(after_enter("    // if (a)|"), "    ");
    }

    #[test]
    fn closing_braces_find_their_statement() {
        let t = "class A {\n    void f(int a,\n            int b) {\n        x();\n        }";
        assert_eq!(closing_indent(t, t.len() - 1).as_deref(), Some("    "));
        assert_eq!(closing_indent("  }", 2), None);
    }

    #[test]
    fn block_comments_continue() {
        let t = "    /**\n";
        assert_eq!(comment_continuation(t, 7), Some(("     * ".into(), Some("     */".into()))));
        let t = "    /**\n     * a\n     */";
        assert_eq!(comment_continuation(t, 7), Some(("     * ".into(), None)));
        assert_eq!(comment_continuation(t, 16), Some(("     * ".into(), None)));
        let t = "/* a */ int x;\n";
        assert_eq!(comment_continuation(t, 14), None);
        let t = "String s = \"/*\";\n";
        assert_eq!(comment_continuation(t, 16), None);
    }

    #[test]
    fn which_line_gets_formatted() {
        assert_eq!(format_line("a\nint x=1;", 10, ';'), Some((1, 1)));
        assert_eq!(format_line("if(a)\n  if(b)\n    c();", 22, ';'), Some((0, 2)));
        assert_eq!(format_line("if(a)\n  c();\n  ", 15, '\n'), Some((0, 1)));
        assert_eq!(format_line("for (int i=0;)", 13, ';'), None);
        assert_eq!(format_line("String s = \"a;", 14, ';'), None);
        assert_eq!(format_line("// a;", 5, ';'), None);
        assert_eq!(format_line("int x=1;\n    ", 13, '\n'), Some((0, 0)));
        assert_eq!(format_line("// note\n", 8, '\n'), None);
        assert_eq!(format_line("case 1:", 7, ':'), Some((0, 0)));
        assert_eq!(format_line("a ? b:", 6, ':'), None);
    }
}
