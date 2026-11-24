//! Editing commands on whole lines and selections. Offsets are chars.

use crate::complete::in_string_or_comment;
use crate::editor::{byte_index, char_index};
use crate::highlight::is_ident;

/// The byte range of the lines a selection touches, without the last newline.
/// A selection ending at column 0 doesn't include that line.
pub fn line_span(text: &str, a: usize, b: usize) -> (usize, usize) {
    let ba = byte_index(text, a);
    let mut bb = byte_index(text, b);
    if b > a && bb > ba && text.as_bytes()[bb - 1] == b'\n' {
        bb -= 1;
    }
    let s = text[..ba].rfind('\n').map_or(0, |i| i + 1);
    let e = text[bb..].find('\n').map_or(text.len(), |i| bb + i);
    (s, e)
}

pub fn duplicate_lines(text: &mut String, (a, b): (usize, usize)) -> (usize, usize) {
    let (s, e) = line_span(text, a, b);
    let block = text[s..e].to_string();
    let shift = block.chars().count() + 1;
    text.insert_str(e, &format!("\n{block}"));
    (a + shift, b + shift)
}

/// Remove the selected lines; returns the new cursor, in the same column.
pub fn delete_lines(text: &mut String, (a, b): (usize, usize)) -> usize {
    let (s, e) = line_span(text, a, b);
    let col = a - char_index(text, s);
    let (from, to) = if e < text.len() {
        (s, e + 1)
    } else if s > 0 {
        (s - 1, e)
    } else {
        (s, e)
    };
    text.replace_range(from..to, "");
    let ls = if from == s { s } else { text[..from].rfind('\n').map_or(0, |i| i + 1) };
    let le = text[ls..].find('\n').map_or(text.len(), |i| ls + i);
    char_index(text, ls) + col.min(text[ls..le].chars().count())
}

/// Swap the selected lines with the line above or below; `None` at the file's edge.
pub fn move_lines(text: &mut String, (a, b): (usize, usize), up: bool) -> Option<(usize, usize)> {
    let (s, e) = line_span(text, a, b);
    let block = text[s..e].to_string();
    if up {
        if s == 0 {
            return None;
        }
        let ps = text[..s - 1].rfind('\n').map_or(0, |i| i + 1);
        let prev = text[ps..s - 1].to_string();
        text.replace_range(ps..e, &format!("{block}\n{prev}"));
        let shift = prev.chars().count() + 1;
        Some((a - shift, b - shift))
    } else {
        if e >= text.len() {
            return None;
        }
        let ne = text[e + 1..].find('\n').map_or(text.len(), |i| e + 1 + i);
        let next = text[e + 1..ne].to_string();
        text.replace_range(s..ne, &format!("{next}\n{block}"));
        let shift = next.chars().count() + 1;
        Some((a + shift, b + shift))
    }
}

/// Home: the first non-blank of the line, or column 0 when already there.
pub fn smart_home(text: &str, cursor: usize) -> usize {
    let bi = byte_index(text, cursor);
    let ls = text[..bi].rfind('\n').map_or(0, |i| i + 1);
    let blanks = text[ls..].bytes().take_while(|b| matches!(b, b' ' | b'\t')).count();
    let first = ls + blanks;
    char_index(text, if bi == first { ls } else { first })
}

/// Backspace that removes more than one char: both halves of an empty pair, or
/// indentation back to the previous tab stop. Returns the char range to delete.
pub fn smart_backspace(text: &str, cursor: usize) -> Option<(usize, usize)> {
    if cursor == 0 {
        return None;
    }
    let bi = byte_index(text, cursor);
    let prev = text[..bi].chars().next_back()?;
    let next = text[bi..].chars().next();
    if closer(prev).is_some() && next == closer(prev) && !in_string_or_comment(&text[..bi - prev.len_utf8()]) {
        return Some((cursor - 1, cursor + 1));
    }
    let ls = text[..bi].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[ls..bi];
    if before.is_empty() || !before.bytes().all(|b| b == b' ') {
        return None;
    }
    let n = match before.len() % 4 {
        0 => 4,
        r => r,
    };
    (n > 1).then(|| (cursor - n, cursor))
}

pub fn closer(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        _ => None,
    }
}

/// Typing `open` over this selection surrounds it instead of replacing it.
pub fn can_wrap(text: &str, (a, b): (usize, usize), open: char) -> bool {
    closer(open).is_some() && a != b && (!matches!(open, '"' | '\'') || !text[byte_index(text, a)..byte_index(text, b)].contains('\n'))
}

/// Surround the selection with a pair; returns the selection inside it.
pub fn wrap_selection(text: &mut String, (a, b): (usize, usize), open: char) -> (usize, usize) {
    let close = closer(open).unwrap_or(open);
    let (ba, bb) = (byte_index(text, a), byte_index(text, b));
    text.insert(bb, close);
    text.insert(ba, open);
    (a + 1, b + 1)
}

/// The identifier at or just before the cursor.
pub fn word_at(text: &str, cursor: usize) -> Option<String> {
    let bi = byte_index(text, cursor);
    let b = text.as_bytes();
    let (mut s, mut e) = (bi, bi);
    while s > 0 && is_ident(b[s - 1]) {
        s -= 1;
    }
    while e < b.len() && is_ident(b[e]) {
        e += 1;
    }
    (s < e).then(|| text[s..e].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keeps_the_selection_on_the_copy() {
        let mut t = "a\nbc\nd".to_string();
        assert_eq!(duplicate_lines(&mut t, (3, 3)), (6, 6));
        assert_eq!(t, "a\nbc\nbc\nd");
        let mut t = "a\nb\nc".to_string();
        duplicate_lines(&mut t, (0, 2)); // ends at column 0 of "b"
        assert_eq!(t, "a\na\nb\nc");
    }

    #[test]
    fn delete_handles_first_middle_last_and_only_lines() {
        let mut t = "aaa\nb\nccc".to_string();
        assert_eq!(delete_lines(&mut t, (5, 5)), 5);
        assert_eq!(t, "aaa\nccc");
        assert_eq!(delete_lines(&mut t, (6, 6)), 2);
        assert_eq!(t, "aaa");
        assert_eq!(delete_lines(&mut t, (1, 1)), 0);
        assert_eq!(t, "");
    }

    #[test]
    fn move_lines_up_and_down() {
        let mut t = "a\nbb\nc".to_string();
        assert_eq!(move_lines(&mut t, (3, 3), true), Some((1, 1)));
        assert_eq!(t, "bb\na\nc");
        assert_eq!(move_lines(&mut t, (1, 1), true), None);
        assert_eq!(move_lines(&mut t, (1, 1), false), Some((3, 3)));
        assert_eq!(t, "a\nbb\nc");
        assert_eq!(move_lines(&mut t, (5, 5), false), None);
        let mut t = "1\n2\n3\n4".to_string();
        assert_eq!(move_lines(&mut t, (2, 5), false), Some((4, 7)));
        assert_eq!(t, "1\n4\n2\n3");
    }

    #[test]
    fn home_toggles_between_code_and_column_zero() {
        let t = "x\n    foo";
        assert_eq!(smart_home(t, 9), 6);
        assert_eq!(smart_home(t, 6), 2);
        assert_eq!(smart_home(t, 2), 6);
    }

    #[test]
    fn backspace_removes_pairs_and_indent() {
        assert_eq!(smart_backspace("f()", 2), Some((1, 3)));
        assert_eq!(smart_backspace("x = \"\";", 5), Some((4, 6)));
        // between two strings the quotes are not a pair
        assert_eq!(smart_backspace("\"a\"\"b\"", 3), None);
        assert_eq!(smart_backspace("        x", 8), Some((4, 8)));
        assert_eq!(smart_backspace("      x", 6), Some((4, 6)));
        assert_eq!(smart_backspace("     x", 5), None);
        assert_eq!(smart_backspace("  a b", 4), None);
    }

    #[test]
    fn typing_a_bracket_wraps_the_selection() {
        let mut t = "a + b".to_string();
        assert!(can_wrap(&t, (0, 5), '('));
        assert_eq!(wrap_selection(&mut t, (0, 5), '('), (1, 6));
        assert_eq!(t, "(a + b)");
        assert!(!can_wrap("a\nb", (0, 3), '"'));
        assert!(can_wrap("a\nb", (0, 3), '{'));
        assert!(!can_wrap("ab", (0, 2), 'x'));
    }

    #[test]
    fn word_under_the_cursor() {
        assert_eq!(word_at("int count = 1;", 6).as_deref(), Some("count"));
        assert_eq!(word_at("int count = 1;", 9).as_deref(), Some("count"));
        assert_eq!(word_at("a + b", 2), None);
    }
}
