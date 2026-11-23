//! Java syntax highlighting into an egui `LayoutJob`.

use eframe::egui::{Color32, FontId, TextFormat, text::LayoutJob};

pub const KEYWORDS: &[&str] = &[
    "abstract", "assert", "break", "case", "catch", "class", "const", "continue", "default", "do",
    "else", "enum", "extends", "final", "finally", "for", "goto", "if", "implements", "import",
    "instanceof", "interface", "native", "new", "package", "private", "protected", "public",
    "return", "static", "strictfp", "super", "switch", "synchronized", "this", "throw", "throws",
    "transient", "try", "volatile", "while", "var", "record", "sealed", "permits", "non-sealed",
    "yield", "module", "exports", "requires", "opens", "uses", "provides", "transitive", "when",
];
pub const PRIMITIVES: &[&str] =
    &["boolean", "byte", "char", "short", "int", "long", "float", "double", "void"];
const LITERALS: &[&str] = &["true", "false", "null"];

struct Palette {
    plain: Color32,
    keyword: Color32,
    primitive: Color32,
    literal: Color32,
    string: Color32,
    number: Color32,
    comment: Color32,
    annotation: Color32,
    type_: Color32,
    method: Color32,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            plain: Color32::from_rgb(0xBC, 0xBE, 0xC4),
            keyword: Color32::from_rgb(0xCF, 0x8E, 0x6D),
            primitive: Color32::from_rgb(0xCF, 0x8E, 0x6D),
            literal: Color32::from_rgb(0xCF, 0x8E, 0x6D),
            string: Color32::from_rgb(0x6A, 0xAB, 0x73),
            number: Color32::from_rgb(0x2A, 0xAC, 0xB8),
            comment: Color32::from_rgb(0x7A, 0x7E, 0x85),
            annotation: Color32::from_rgb(0xB3, 0xAE, 0x60),
            type_: Color32::from_rgb(0x8F, 0xB8, 0xE6),
            method: Color32::from_rgb(0x56, 0xA8, 0xF5),
        }
    } else {
        Palette {
            plain: Color32::from_rgb(0x08, 0x08, 0x08),
            keyword: Color32::from_rgb(0x00, 0x33, 0xB3),
            primitive: Color32::from_rgb(0x00, 0x33, 0xB3),
            literal: Color32::from_rgb(0x00, 0x33, 0xB3),
            string: Color32::from_rgb(0x06, 0x7D, 0x17),
            number: Color32::from_rgb(0x17, 0x50, 0xEB),
            comment: Color32::from_rgb(0x8C, 0x8C, 0x8C),
            annotation: Color32::from_rgb(0x9E, 0x88, 0x0D),
            type_: Color32::from_rgb(0x00, 0x62, 0x7A),
            method: Color32::from_rgb(0x00, 0x62, 0x7A),
        }
    }
}

#[inline]
pub fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}

#[inline]
pub fn is_ident(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

fn starts_token(b: u8) -> bool {
    is_ident(b) || matches!(b, b'"' | b'\'' | b'/' | b'@')
}

pub fn highlight(text: &str, dark: bool, font: FontId) -> LayoutJob {
    let p = palette(dark);
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let b = text.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        let c = b[i];
        let start = i;
        let color;
        if c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            color = p.comment;
        } else if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            i += 2;
            while i < n && !(b[i] == b'*' && i + 1 < n && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            color = p.comment;
        } else if c == b'"' {
            if text[i..].starts_with("\"\"\"") {
                i += 3;
                while i < n && !text[i..].starts_with("\"\"\"") {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i = (i + 3).min(n);
            } else {
                i += 1;
                while i < n && b[i] != b'"' && b[i] != b'\n' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i = (i + 1).min(n);
            }
            color = p.string;
        } else if c == b'\'' {
            i += 1;
            while i < n && b[i] != b'\'' && b[i] != b'\n' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            color = p.string;
        } else if c == b'@' && i + 1 < n && is_ident_start(b[i + 1]) {
            i += 1;
            while i < n && (is_ident(b[i]) || b[i] == b'.') {
                i += 1;
            }
            color = p.annotation;
        } else if c.is_ascii_digit() {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
            color = p.number;
        } else if is_ident_start(c) {
            while i < n && is_ident(b[i]) {
                i += 1;
            }
            let w = &text[start..i];
            let mut j = i;
            while j < n && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            color = if KEYWORDS.contains(&w) {
                p.keyword
            } else if PRIMITIVES.contains(&w) {
                p.primitive
            } else if LITERALS.contains(&w) {
                p.literal
            } else if j < n && b[j] == b'(' {
                p.method
            } else if w.as_bytes()[0].is_ascii_uppercase() {
                p.type_
            } else {
                p.plain
            };
        } else {
            i += 1;
            while i < n && !starts_token(b[i]) {
                i += 1;
            }
            color = p.plain;
        }
        // Escapes may have stepped into the middle of a multi-byte char.
        i = i.min(n);
        while i < n && !text.is_char_boundary(i) {
            i += 1;
        }
        job.append(&text[start..i], 0.0, TextFormat::simple(font.clone(), color));
    }
    job
}
