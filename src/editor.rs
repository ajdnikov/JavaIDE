//! The code editor: line-number gutter, syntax highlighting, smart indent,
//! bracket pairing, line comments and the completion popup.

use crate::complete::{self, Item, Kind, ProjectIndex};
use crate::highlight;
use crate::lsp::{self, CodeAction, Diag, TextEdit};
use eframe::egui::{
    self, Align, Color32, FontId, Id, Key, Modifiers, Pos2, RichText, Sense, Stroke, Vec2,
    text::{CCursor, CCursorRange, LayoutJob},
    text_edit::TextEditState,
};
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

pub struct Tab {
    pub path: Option<PathBuf>,
    pub text: String,
    pub saved: String,
    pub dirty: bool,
    pub crlf: bool,
    pub id: Id,
    pub goto_line: Option<usize>,
    hl_cache: Option<(u64, LayoutJob)>,
    /// The `{...}` block at the cursor, for (text hash, selection).
    brace_cache: Option<(u64, usize, usize, Option<BraceBlock>)>,
}

/// A `{...}` pair and the part of each line it covers, all in chars.
#[derive(Clone, Debug, PartialEq)]
struct BraceBlock {
    open: usize,
    close: usize,
    first_line: usize,
    lines: Vec<(usize, usize)>,
}

const BRACE_BLOCK_FILL: Color32 = Color32::from_rgba_premultiplied(9, 16, 22, 26);
const BRACE_PAIR_FILL: Color32 = Color32::from_rgba_premultiplied(30, 55, 75, 90);

/// The brace the selection touches: the one left of the cursor, else the one right
/// of it, or a single selected brace. Offsets are bytes of the blanked text.
fn touching_brace(clean: &[u8], a: usize, b: usize) -> Option<usize> {
    let is_brace = |i: usize| matches!(clean.get(i), Some(b'{' | b'}'));
    if a != b {
        return (b == a + 1 && is_brace(a)).then_some(a);
    }
    if a > 0 && is_brace(a - 1) {
        Some(a - 1)
    } else if is_brace(a) {
        Some(a)
    } else {
        None
    }
}

/// The (open, close) byte offsets of the pair containing the brace at `at`.
fn matching_brace(clean: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    if clean[at] == b'{' {
        for (i, &c) in clean.iter().enumerate().skip(at) {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((at, i));
                    }
                }
                _ => {}
            }
        }
    } else {
        for i in (0..=at).rev() {
            match clean[i] {
                b'}' => depth += 1,
                b'{' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((i, at));
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// The block whose brace the char selection `a..b` touches. Braces in strings and
/// comments don't count.
fn brace_block(text: &str, a: usize, b: usize) -> Option<BraceBlock> {
    if !text.contains('{') {
        return None;
    }
    let blanked = complete::blank_comments_and_strings(text);
    let clean = blanked.as_bytes();
    let at = touching_brace(clean, byte_of(text, a), byte_of(text, b))?;
    let (open, close) = matching_brace(clean, at)?;
    let (open_c, close_c) = (char_of(text, open), char_of(text, close));
    let mut lines = Vec::new();
    let (mut start, mut ci) = (open_c, open_c);
    for ch in text[open..=close].chars() {
        if ch == '\n' {
            lines.push((start, ci));
            start = ci + 1;
        }
        ci += 1;
    }
    lines.push((start, close_c + 1));
    Some(BraceBlock { open: open_c, close: close_c, first_line: text[..open].matches('\n').count(), lines })
}

impl Tab {
    pub fn new(path: Option<PathBuf>, text: String, id: Id) -> Tab {
        let crlf = text.contains("\r\n");
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        Tab { path, saved: text.clone(), text, dirty: false, crlf, id, goto_line: None, hl_cache: None, brace_cache: None }
    }

    /// The block at this selection, recomputed only when the text or selection changes.
    fn brace_block_at(&mut self, a: usize, b: usize) -> Option<&BraceBlock> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.text.hash(&mut h);
        let key = h.finish();
        if !self.brace_cache.as_ref().is_some_and(|c| (c.0, c.1, c.2) == (key, a, b)) {
            self.brace_cache = Some((key, a, b, brace_block(&self.text, a, b)));
        }
        self.brace_cache.as_ref().and_then(|c| c.3.as_ref())
    }

    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".into())
    }

    pub fn is_java(&self) -> bool {
        self.path.as_ref().is_none_or(|p| p.extension().is_some_and(|e| e == "java"))
    }

    pub fn contents_for_disk(&self) -> String {
        if self.crlf { self.text.replace('\n', "\r\n") } else { self.text.clone() }
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.text.clone();
        self.dirty = false;
    }
}

pub struct CompletionState {
    pub tab_id: Id,
    /// Char index where the completed word starts.
    pub start: usize,
    /// Items matching what has been typed so far.
    pub items: Vec<Item>,
    /// The language server's full list, refiltered locally while typing.
    pub all: Vec<Item>,
    pub selected: usize,
    pub pos: Pos2,
    pub manual: bool,
    pub scroll: f32,
    /// Apply `scroll` to the list on the next frame.
    pub jump: bool,
}

impl CompletionState {
    /// Narrow the server's list to `prefix`, keeping the server's ranking within a match level.
    pub fn refilter(&mut self, prefix: &str) {
        let lower = prefix.to_lowercase();
        let mut hits: Vec<(u8, usize)> = self
            .all
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                let name = it.lsp.as_ref().map_or(it.label.as_str(), |d| d.filter.as_str());
                complete::match_level(name, prefix, &lower).map(|level| (level, i))
            })
            .collect();
        hits.sort_unstable();
        self.items = hits.into_iter().map(|(_, i)| self.all[i].clone()).collect();
        self.selected = 0;
        self.scroll = 0.0;
        self.jump = true;
    }
}

/// The Ctrl+. menu.
pub struct QuickMenu {
    pub tab_id: Id,
    pub items: Vec<CodeAction>,
    pub selected: usize,
    pub pos: Pos2,
}

/// Char ranges of each tab stop, in visiting order. The first range of a stop is
/// the one being edited; the others mirror it. The last stop ends the session.
type StopGroups = Vec<Vec<(usize, usize)>>;

/// An inserted snippet whose fields can be visited with Tab.
pub struct SnippetSession {
    pub tab_id: Id,
    groups: StopGroups,
    current: usize,
}

impl SnippetSession {
    fn new(tab_id: Id, groups: StopGroups, text: &mut String) -> Option<SnippetSession> {
        if groups.len() < 2 {
            return None;
        }
        let mut s = SnippetSession { tab_id, groups, current: 0 };
        for g in 0..s.groups.len() {
            s.mirror(g, text);
        }
        Some(s)
    }

    fn range(&self) -> (usize, usize) {
        self.groups[self.current][0]
    }

    fn is_finished(&self) -> bool {
        self.current + 1 >= self.groups.len()
    }

    fn advance(&mut self, forward: bool) -> (usize, usize) {
        if forward {
            self.current = (self.current + 1).min(self.groups.len() - 1);
        } else {
            self.current = self.current.saturating_sub(1);
        }
        self.range()
    }

    /// Whether `c` is still inside the inserted snippet.
    fn contains(&self, c: usize) -> bool {
        let all = self.groups.iter().flatten();
        let lo = all.clone().map(|r| r.0).min().unwrap_or(0);
        let hi = all.map(|r| r.1).max().unwrap_or(0);
        (lo..=hi).contains(&c)
    }

    /// Adjust every range for a replacement of `old` chars at `at` by `new` chars.
    /// `owner` is the range being typed into: it always grows or shrinks.
    fn shift(&mut self, at: usize, old: usize, new: usize, owner: (usize, usize)) {
        let delta = new as isize - old as isize;
        let moved = |v: usize| (v as isize + delta).max(0) as usize;
        let edit_end = at + old;
        for (g, group) in self.groups.iter_mut().enumerate() {
            for (i, r) in group.iter_mut().enumerate() {
                if (g, i) == owner {
                    r.1 = moved(r.1);
                } else if r.0 >= edit_end {
                    *r = (moved(r.0), moved(r.1));
                } else if r.1 <= at {
                    // entirely before the edit
                } else if r.0 <= at && edit_end <= r.1 {
                    r.1 = moved(r.1); // an outer field containing the edit
                } else {
                    *r = (r.0.min(at), r.1.max(at + new).min(moved(r.1).max(at + new)));
                }
            }
        }
    }

    /// Copy a stop's text to its mirrors.
    fn mirror(&mut self, g: usize, text: &mut String) {
        for i in 1..self.groups[g].len() {
            let (ps, pe) = self.groups[g][0];
            let value: String = text.chars().skip(ps).take(pe - ps).collect();
            let (s, e) = self.groups[g][i];
            let (bs, be) = (byte_of(text, s), byte_of(text, e));
            if text[bs..be] != value {
                text.replace_range(bs..be, &value);
                self.shift(s, e - s, value.chars().count(), (g, i));
            }
        }
    }

    /// Follow an edit. Returns how far the current field moved, or `None` if the
    /// edit was outside it and the session should end.
    fn on_edit(&mut self, before: &str, text: &mut String) -> Option<isize> {
        let (ps, pe) = self.range();
        let (at, old, new) = text_diff(before, text, pe);
        if at < ps || at + old > pe {
            return None;
        }
        self.shift(at, old, new, (self.current, 0));
        self.mirror(self.current, text);
        Some(self.range().0 as isize - ps as isize)
    }

    /// Visit a snippet inserted inside the current field before the remaining stops.
    fn splice(&mut self, inner: StopGroups) {
        let at = self.current + 1;
        self.groups.splice(at..at, inner);
        self.current = at;
    }

    /// Text was inserted above the snippet (an import).
    pub fn shift_from(&mut self, boundary: usize, delta: isize) {
        let moved = |v: usize| if v >= boundary { (v as isize + delta).max(0) as usize } else { v };
        for r in self.groups.iter_mut().flatten() {
            *r = (moved(r.0), moved(r.1));
        }
    }
}

/// The single replacement turning `before` into `after`, as (at, old, new) in chars.
/// The start is capped at `limit`, so repeated letters resolve into the edited field.
fn text_diff(before: &str, after: &str, limit: usize) -> (usize, usize, usize) {
    let (b, a): (Vec<char>, Vec<char>) = (before.chars().collect(), after.chars().collect());
    let mut p = 0;
    while p < limit && p < b.len() && p < a.len() && b[p] == a[p] {
        p += 1;
    }
    let mut s = 0;
    while s < b.len() - p && s < a.len() - p && b[b.len() - 1 - s] == a[a.len() - 1 - s] {
        s += 1;
    }
    (p, b.len() - p - s, a.len() - p - s)
}

/// Tab stops in visiting order: 1, 2, ... and finally 0 (or the end of the text).
fn build_groups(stops: &[(u32, usize, usize)], base: usize, len: usize) -> StopGroups {
    let mut numbers: Vec<u32> = stops.iter().map(|s| s.0).filter(|n| *n > 0).collect();
    numbers.sort_unstable();
    numbers.dedup();
    let mut groups: StopGroups = Vec::with_capacity(numbers.len() + 1);
    for n in numbers {
        let mut g: Vec<(usize, usize)> = stops.iter().filter(|s| s.0 == n).map(|s| (base + s.1, base + s.2)).collect();
        g.sort_unstable();
        if let Some(p) = g.iter().position(|r| r.1 > r.0) {
            let primary = g.remove(p);
            g.insert(0, primary);
        }
        groups.push(g);
    }
    let end = stops.iter().find(|s| s.0 == 0).map_or(base + len, |s| base + s.1);
    groups.push(vec![(end, end)]);
    groups
}

pub struct EditorCtx<'a> {
    pub dark: bool,
    pub font_size: f32,
    pub index: &'a ProjectIndex,
    pub completion: &'a mut Option<CompletionState>,
    pub autocomplete: bool,
    /// Recently accepted completions, most recent first.
    pub recent: &'a [String],
    /// The Java language server can answer completion requests.
    pub lsp_ready: bool,
    /// Live problems for this file.
    pub diags: &'a [Diag],
    pub session: &'a mut Option<SnippetSession>,
    pub quick_menu: &'a mut Option<QuickMenu>,
}

/// Ask the language server for completions at this byte offset.
pub struct CompletionRequest {
    pub byte: usize,
    pub start: usize,
    pub trigger: Option<char>,
    pub manual: bool,
}

/// A language-server item was inserted; its imports still have to be fetched.
pub struct Accepted {
    pub raw: Arc<Value>,
    /// The text before the item was inserted (the coordinates of its extra edits).
    pub before: String,
}

pub struct EditorResult {
    pub line_col: Option<(usize, usize)>,
    pub changed: bool,
    /// Label of the completion accepted this frame, if any.
    pub used: Option<String>,
    pub request: Option<CompletionRequest>,
    pub accepted: Option<Accepted>,
    /// Any completion request still in flight is no longer wanted.
    pub cancel_request: bool,
    /// Ctrl+. was pressed with this char selection.
    pub quick_request: Option<(usize, usize)>,
    /// The code action picked from the Ctrl+. menu.
    pub quick_choice: Option<Arc<Value>>,
}

// ------------------------------------------------------------ text helpers

fn byte_of(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map_or(s.len(), |(b, _)| b)
}

fn char_of(s: &str, bi: usize) -> usize {
    s[..bi.min(s.len())].chars().count()
}

fn line_start(s: &str, bi: usize) -> usize {
    s[..bi].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(s: &str, bi: usize) -> usize {
    s[bi..].find('\n').map_or(s.len(), |i| bi + i)
}

fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

fn set_cursor(ctx: &egui::Context, id: Id, a: usize, b: usize) {
    let mut st = TextEditState::load(ctx, id).unwrap_or_default();
    let range = if a == b {
        CCursorRange::one(CCursor::new(a))
    } else {
        CCursorRange::two(CCursor::new(a), CCursor::new(b))
    };
    st.cursor.set_char_range(Some(range));
    st.store(ctx, id);
}

fn get_cursor(ctx: &egui::Context, id: Id) -> Option<(usize, usize)> {
    let r = TextEditState::load(ctx, id)?.cursor.char_range()?;
    let (a, b) = (r.primary.index.0, r.secondary.index.0);
    Some((a.min(b), a.max(b)))
}

/// Apply `f` to every line touched by the char range; returns the new range.
fn transform_lines(text: &mut String, (a, b): (usize, usize), f: impl Fn(&str) -> String) -> (usize, usize) {
    let ba = byte_of(text, a);
    let mut bb = byte_of(text, b);
    // a selection ending at column 0 doesn't include that line
    if b > a && bb > 0 && text.as_bytes()[bb - 1] == b'\n' {
        bb -= 1;
    }
    let s = line_start(text, ba);
    let e = line_end(text, bb);
    let old = text[s..e].to_string();
    let new: Vec<String> = old.split('\n').map(&f).collect();
    let first_delta = new[0].chars().count() as isize - old.split('\n').next().unwrap().chars().count() as isize;
    let new = new.join("\n");
    let total_delta = new.chars().count() as isize - old.chars().count() as isize;
    text.replace_range(s..e, &new);
    let line_start_ci = char_of(text, s) as isize;
    let na = (a as isize + first_delta).max(line_start_ci) as usize;
    let nb = (b as isize + total_delta).max(na as isize) as usize;
    (na, nb)
}

fn indent(text: &mut String, (a, b): (usize, usize)) -> (usize, usize) {
    let ba = byte_of(text, a);
    let bb = byte_of(text, b);
    if a == b || !text[ba..bb].contains('\n') {
        let col = text[line_start(text, ba)..ba].chars().count();
        let n = 4 - col % 4;
        text.replace_range(ba..bb, &" ".repeat(n));
        return (a + n, a + n);
    }
    transform_lines(text, (a, b), |l| if l.trim().is_empty() { l.to_string() } else { format!("    {l}") })
}

fn dedent(text: &mut String, range: (usize, usize)) -> (usize, usize) {
    transform_lines(text, range, |l| {
        if let Some(r) = l.strip_prefix('\t') {
            return r.to_string();
        }
        let n = l.len() - l.trim_start_matches(' ').len();
        l[n.min(4)..].to_string()
    })
}

fn toggle_comment(text: &mut String, range: (usize, usize)) -> (usize, usize) {
    let ba = byte_of(text, range.0);
    let bb = byte_of(text, range.1);
    let block = &text[line_start(text, ba)..line_end(text, bb)];
    let all_commented = block
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.trim_start().starts_with("//"));
    let min_indent = block
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .map(|l| leading_ws(l).len())
        .min()
        .unwrap_or(0);
    transform_lines(text, range, |l| {
        if l.trim().is_empty() {
            l.to_string()
        } else if all_commented {
            let ws = leading_ws(l);
            let rest = &l[ws.len() + 2..];
            format!("{ws}{}", rest.strip_prefix(' ').unwrap_or(rest))
        } else {
            format!("{}// {}", &l[..min_indent], &l[min_indent..])
        }
    })
}

/// Replace the word `[start, cursor)` with a completion item; returns the new cursor.
fn apply_completion(text: &mut String, start: usize, cursor: usize, item: &Item) -> (usize, Option<StopGroups>) {
    let bs = byte_of(text, start);
    let bc = byte_of(text, cursor);
    let ws = leading_ws(&text[line_start(text, bs)..]).to_string();
    let mut ins = item.insert.replace('\n', &format!("\n{ws}"));
    // Don't double up parentheses when they're already there.
    if item.kind == Kind::Method && text[bc..].starts_with('(') {
        ins = ins.find('(').map_or_else(|| item.label.clone(), |p| ins[..p].to_string());
    }
    if item.lsp.is_some() {
        let (plain, stops) = lsp::strip_stops(&ins);
        text.replace_range(bs..bc, &plain);
        let groups = build_groups(&stops, start, plain.chars().count());
        return (groups[0][0].0, Some(groups));
    }
    let (ins, off) = match ins.find("$0") {
        Some(p) => (ins.replacen("$0", "", 1), ins[..p].chars().count()),
        None => {
            let n = ins.chars().count();
            (ins, n)
        }
    };
    text.replace_range(bs..bc, &ins);
    (start + off, None)
}

/// Insert a completion; a language-server item replaces from its own edit start.
fn accept(text: &mut String, start: usize, cursor: usize, item: &Item) -> (usize, Option<Accepted>, Option<StopGroups>) {
    let Some(data) = &item.lsp else {
        let (nc, groups) = apply_completion(text, start, cursor, item);
        return (nc, None, groups);
    };
    let before = text.clone();
    let from = data.start.map_or(start, |p| char_of(text, lsp::byte_of_pos(text, p)).min(cursor));
    let (nc, groups) = apply_completion(text, from, cursor, item);
    (nc, Some(Accepted { raw: data.raw.clone(), before }), groups)
}

/// Apply edits expressed against the current text. Returns them as (at, old, new)
/// in chars, in the original coordinates, ascending.
pub fn apply_text_edits(text: &mut String, edits: &[TextEdit]) -> Vec<(usize, usize, usize)> {
    let mut ranges: Vec<(usize, usize, &str)> =
        edits.iter().rev().map(|e| (lsp::byte_of_pos(text, e.start), lsp::byte_of_pos(text, e.end), e.text.as_str())).collect();
    ranges.sort_by(|a, b| b.0.cmp(&a.0));
    let mut changes = Vec::with_capacity(ranges.len());
    for (s, e, new) in ranges {
        changes.push((char_of(text, s), text[s..e].chars().count(), new.chars().count()));
        text.replace_range(s..e, new);
    }
    changes.reverse();
    changes
}

/// Where a char offset ends up after `changes`.
fn remap(c: usize, changes: &[(usize, usize, usize)]) -> usize {
    let mut delta = 0isize;
    for &(at, old, new) in changes {
        if c >= at + old {
            delta += new as isize - old as isize;
        } else if c > at {
            return (at as isize + delta + new as isize) as usize;
        } else {
            break;
        }
    }
    (c as isize + delta).max(0) as usize
}

/// Keep the cursor on the same text after `apply_text_edits`.
pub fn remap_cursor(ctx: &egui::Context, id: Id, changes: &[(usize, usize, usize)]) {
    if let Some((a, b)) = get_cursor(ctx, id) {
        set_cursor(ctx, id, remap(a, changes), remap(b, changes));
    }
}

/// Apply an item's extra edits (its imports). They are expressed against `before`,
/// so they only apply while the text up to them is unchanged.
/// Returns the char offset below which the edits landed and the length change.
pub fn apply_additional(text: &mut String, before: &str, edits: &[TextEdit]) -> Option<(usize, isize)> {
    let mut ranges: Vec<(usize, usize, &str)> =
        edits.iter().rev().map(|e| (lsp::byte_of_pos(before, e.start), lsp::byte_of_pos(before, e.end), e.text.as_str())).collect();
    let max_end = ranges.iter().map(|r| r.1).max()?;
    if text.get(..max_end) != before.get(..max_end) {
        return None;
    }
    let boundary = text[..max_end].chars().count();
    ranges.sort_by(|a, b| b.0.cmp(&a.0));
    let mut delta = 0isize;
    for (s, e, new) in ranges {
        delta += new.chars().count() as isize - text[s..e].chars().count() as isize;
        text.replace_range(s..e, new);
    }
    Some((boundary, delta))
}

/// Keep the cursor on the same text after edits were applied above it.
pub fn shift_cursor(ctx: &egui::Context, id: Id, boundary: usize, delta: isize) {
    if let Some((a, b)) = get_cursor(ctx, id) {
        let shift = |c: usize| if c >= boundary { (c as isize + delta).max(0) as usize } else { c };
        set_cursor(ctx, id, shift(a), shift(b));
    }
}

/// The char-based cursor of an editor, if it has one.
pub fn cursor_of(ctx: &egui::Context, id: Id) -> Option<usize> {
    get_cursor(ctx, id).map(|r| r.1)
}

/// Byte offset where the identifier ending at `bi` starts.
pub fn word_start(text: &str, bi: usize) -> usize {
    let b = text.as_bytes();
    let mut s = bi;
    while s > 0 && highlight::is_ident(b[s - 1]) {
        s -= 1;
    }
    while !text.is_char_boundary(s) {
        s += 1;
    }
    s
}

pub fn char_index(text: &str, bi: usize) -> usize {
    char_of(text, bi)
}

pub fn byte_index(text: &str, ci: usize) -> usize {
    byte_of(text, ci)
}

fn severity_color(severity: u8) -> Color32 {
    match severity {
        1 => Color32::from_rgb(0xF1, 0x4C, 0x4C),
        2 => Color32::from_rgb(0xE5, 0xA5, 0x3C),
        _ => Color32::from_rgb(0x56, 0xA8, 0xF5),
    }
}

/// Char count of the first `units` UTF-16 units of `s`.
fn chars_in_units(s: &str, units: usize) -> usize {
    let mut seen = 0;
    for (n, ch) in s.chars().enumerate() {
        if seen >= units {
            return n;
        }
        seen += ch.len_utf16();
    }
    s.chars().count()
}

fn squiggle(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    let mut points = Vec::with_capacity(((x1 - x0) / 2.0) as usize + 2);
    let (mut x, mut up) = (x0, false);
    while x < x1 {
        points.push(Pos2::new(x, if up { y - 1.5 } else { y }));
        x += 2.0;
        up = !up;
    }
    points.push(Pos2::new(x1, if up { y - 1.5 } else { y }));
    painter.line(points, Stroke::new(1.0, color));
}

fn braces_unbalanced(text: &str) -> bool {
    let clean = complete::blank_comments_and_strings(text);
    let open = clean.bytes().filter(|&b| b == b'{').count();
    let close = clean.bytes().filter(|&b| b == b'}').count();
    open > close
}

/// Smart edits after a single typed character. Returns the new cursor if changed.
fn after_char_typed(text: &mut String, cursor: usize, c: char) -> Option<usize> {
    let bc = byte_of(text, cursor);
    let next = text[bc..].chars().next();
    let prev2 = text[..bc].chars().rev().nth(1);
    let closer_ok = next.is_none_or(|n| n.is_whitespace() || ")]};,".contains(n));
    match c {
        '\n' => {
            let prev_line_start = line_start(text, bc - 1);
            let prev_line = &text[prev_line_start..bc - 1];
            let ws = leading_ws(prev_line).to_string();
            let trimmed = prev_line.trim_end();
            let opens = trimmed.ends_with('{') || trimmed.ends_with('(') || trimmed.ends_with('[');
            let mut ins = ws.clone();
            if opens {
                ins.push_str("    ");
            }
            let new_cursor = cursor + ins.chars().count();
            let rest_of_line = &text[bc..line_end(text, bc)];
            if trimmed.ends_with('{') {
                if next == Some('}') {
                    ins.push('\n');
                    ins.push_str(&ws);
                } else if rest_of_line.trim().is_empty() && braces_unbalanced(text) {
                    ins.push('\n');
                    ins.push_str(&ws);
                    ins.push('}');
                }
            } else if (trimmed.ends_with('(') && next == Some(')')) || (trimmed.ends_with('[') && next == Some(']')) {
                ins.push('\n');
                ins.push_str(&ws);
            }
            // drop whitespace that was right after the cursor
            let trailing = rest_of_line.len() - rest_of_line.trim_start_matches([' ', '\t']).len();
            text.replace_range(bc..bc + trailing, &ins);
            Some(new_cursor)
        }
        '}' => {
            let ls = line_start(text, bc - 1);
            let before = &text[ls..bc - 1];
            if !before.is_empty() && before.trim().is_empty() {
                let n = before.len().min(4);
                text.replace_range(bc - 1 - n..bc - 1, "");
                return Some(cursor - n);
            }
            if next == Some('}') {
                text.remove(bc);
            }
            None
        }
        ')' | ']' => {
            if next == Some(c) {
                text.remove(bc);
            }
            None
        }
        '{' | '(' | '[' if closer_ok && !complete::in_string_or_comment(&text[..bc - c.len_utf8()]) => {
            text.insert(bc, match c {
                '{' => '}',
                '(' => ')',
                _ => ']',
            });
            None
        }
        '"' | '\'' => {
            if next == Some(c) {
                text.remove(bc);
            } else if closer_ok && !prev2.is_some_and(|p| p.is_alphanumeric() || p == '\\' || p == c) {
                text.insert(bc, c);
            }
            None
        }
        _ => None,
    }
}

// --------------------------------------------------------------------- UI

pub fn editor_ui(ui: &mut egui::Ui, tab: &mut Tab, ec: EditorCtx) -> EditorResult {
    let id = tab.id;
    let ctx = ui.ctx().clone();
    let font = FontId::monospace(ec.font_size);
    let has_focus = ctx.memory(|m| m.has_focus(id));
    let popup_open = ec.completion.as_ref().is_some_and(|c| c.tab_id == id && !c.items.is_empty());
    let mut changed = false;
    let mut cancel_request = false;
    let mut accepted: Option<Accepted> = None;
    let mut request: Option<CompletionRequest> = None;
    let mut quick_request: Option<(usize, usize)> = None;
    let mut quick_choice: Option<Arc<Value>> = None;
    let mut inserted_stops: Option<StopGroups> = None;
    if ec.session.as_ref().is_some_and(|s| s.tab_id != id) {
        *ec.session = None;
    }
    if ec.quick_menu.as_ref().is_some_and(|m| m.tab_id != id) {
        *ec.quick_menu = None;
    }
    let quick_open = ec.quick_menu.is_some();
    let session_open = ec.session.is_some();
    let text_at_start = session_open.then(|| tab.text.clone());

    // ---- keys we handle ourselves (must run before the TextEdit sees them)
    #[derive(PartialEq)]
    enum Act {
        Up,
        Down,
        Accept,
        Close,
        Indent,
        Dedent,
        Trigger,
        Comment,
        QuickFix,
        QuickUp,
        QuickDown,
        QuickAccept,
        QuickClose,
        NextStop,
        PrevStop,
        EndSnippet,
    }
    let mut acts: Vec<Act> = Vec::new();
    if has_focus {
        ctx.input_mut(|i| {
            if quick_open {
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    acts.push(Act::QuickDown);
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    acts.push(Act::QuickUp);
                }
                if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                    acts.push(Act::QuickAccept);
                }
                if i.consume_key(Modifiers::NONE, Key::Escape) {
                    acts.push(Act::QuickClose);
                }
            } else if popup_open {
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    acts.push(Act::Down);
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    acts.push(Act::Up);
                }
                if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                    acts.push(Act::Accept);
                }
                if i.consume_key(Modifiers::NONE, Key::Escape) {
                    acts.push(Act::Close);
                }
            } else if session_open {
                if i.consume_key(Modifiers::SHIFT, Key::Tab) {
                    acts.push(Act::PrevStop);
                } else if i.consume_key(Modifiers::NONE, Key::Tab) {
                    acts.push(Act::NextStop);
                } else if i.consume_key(Modifiers::NONE, Key::Escape) {
                    acts.push(Act::EndSnippet);
                }
            } else if i.consume_key(Modifiers::SHIFT, Key::Tab) {
                acts.push(Act::Dedent);
            } else if i.consume_key(Modifiers::NONE, Key::Tab) {
                acts.push(Act::Indent);
            }
            if i.consume_key(Modifiers::COMMAND, Key::Space) {
                acts.push(Act::Trigger);
            }
            if i.consume_key(Modifiers::COMMAND, Key::Period) {
                acts.push(Act::QuickFix);
            }
            if i.consume_key(Modifiers::COMMAND, Key::Slash) {
                acts.push(Act::Comment);
            }
        });
    }

    let mut used: Option<String> = None;
    let mut accept_item: Option<usize> = None;
    for a in &acts {
        match a {
            Act::Up | Act::Down => {
                if let Some(c) = ec.completion.as_mut().filter(|c| !c.items.is_empty()) {
                    let n = c.items.len();
                    c.selected = if *a == Act::Down { (c.selected + 1) % n } else { (c.selected + n - 1) % n };
                }
            }
            Act::Accept => accept_item = ec.completion.as_ref().map(|c| c.selected),
            Act::Close => {
                *ec.completion = None;
                cancel_request = true;
            }
            Act::Indent | Act::Dedent | Act::Comment => {
                if let Some(r) = get_cursor(&ctx, id) {
                    let (na, nb) = match a {
                        Act::Indent => indent(&mut tab.text, r),
                        Act::Dedent => dedent(&mut tab.text, r),
                        _ => toggle_comment(&mut tab.text, r),
                    };
                    set_cursor(&ctx, id, na, nb);
                    changed = true;
                }
            }
            Act::Trigger => {}
            Act::QuickFix => {
                *ec.completion = None;
                cancel_request = true;
                quick_request = get_cursor(&ctx, id);
            }
            Act::QuickUp | Act::QuickDown => {
                if let Some(m) = ec.quick_menu.as_mut().filter(|m| !m.items.is_empty()) {
                    let n = m.items.len();
                    m.selected = if *a == Act::QuickDown { (m.selected + 1) % n } else { (m.selected + n - 1) % n };
                }
            }
            Act::QuickAccept => {
                if let Some(m) = ec.quick_menu.take() {
                    quick_choice = m.items.get(m.selected).map(|x| x.raw.clone());
                }
            }
            Act::QuickClose => *ec.quick_menu = None,
            Act::NextStop | Act::PrevStop => {
                if let Some(s) = ec.session.as_mut() {
                    let (x, y) = s.advance(*a == Act::NextStop);
                    set_cursor(&ctx, id, x, y);
                    if s.is_finished() {
                        *ec.session = None;
                    }
                }
            }
            Act::EndSnippet => *ec.session = None,
        }
    }

    // ---- the text area
    let prev_len = tab.text.chars().count();
    let prev_cursor = get_cursor(&ctx, id).map(|r| r.1);

    let gutter_digits = (tab.text.matches('\n').count() + 1).to_string().len().max(3);
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let row_h = ui.fonts_mut(|f| f.row_height(&font));
    let gutter_w = char_w * gutter_digits as f32 + 18.0;
    let dark = ec.dark;
    let editor_bg = ui.visuals().extreme_bg_color;

    let mut cache = tab.hl_cache.take();
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
        let s = buf.as_str();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        dark.hash(&mut h);
        font.size.to_bits().hash(&mut h);
        let key = h.finish();
        let job = match &cache {
            Some((k, job)) if *k == key => job.clone(),
            _ => {
                let job = highlight::highlight(s, dark, font.clone());
                cache = Some((key, job.clone()));
                job
            }
        };
        ui.fonts_mut(|f| f.layout_job(job))
    };

    let mut popup_anchor: Option<Pos2> = None;
    let mut line_col = None;
    let mut cursor_now: Option<usize> = None;
    let mut text_changed = false;
    let mut editor_has_focus = has_focus;
    let mut line_severity: HashMap<usize, u8> = HashMap::new();
    for d in ec.diags.iter().filter(|d| d.severity <= 3) {
        let e = line_severity.entry(d.start.0).or_insert(d.severity);
        *e = (*e).min(d.severity);
    }

    let scroll = egui::ScrollArea::both().id_salt(id.with("scroll")).auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let avail_h = ui.available_height();
            let (gutter_rect, _) = ui.allocate_exact_size(Vec2::new(gutter_w, avail_h), Sense::hover());
            let rows = ((avail_h / row_h) as usize).max(1);
            // Reserved before the text, so the block highlight is drawn behind it.
            let brace_shape = ui.painter().add(egui::Shape::Noop);
            let out = egui::TextEdit::multiline(&mut tab.text)
                .id(id)
                .font(font.clone())
                .code_editor()
                .frame(egui::Frame::NONE)
                .margin(egui::Margin { left: 6, right: 6, top: 2, bottom: 2 })
                .desired_width(f32::INFINITY)
                .desired_rows(rows)
                .lock_focus(true)
                .layouter(&mut layouter)
                .show(ui);

            text_changed = out.response.response.changed();
            editor_has_focus = out.response.response.has_focus();
            let galley = &out.galley;
            let origin = out.galley_pos;

            // gutter: background + line numbers for visible rows
            let painter = ui.painter();
            let clip = ui.clip_rect();

            // the whole `{...}` block when the cursor is next to one of its braces
            let selection = out.cursor_range.map(|r| {
                let (x, y) = (r.primary.index.0, r.secondary.index.0);
                (x.min(y), x.max(y))
            });
            if let Some(block) = selection.and_then(|(x, y)| tab.brace_block_at(x, y)) {
                let at = |c: usize| galley.pos_from_cursor(CCursor::new(c)).translate(origin.to_vec2());
                let top = ((clip.top() - origin.y) / row_h).floor().max(0.0) as usize;
                let bottom = ((clip.bottom() - origin.y) / row_h).ceil().max(0.0) as usize;
                let from = top.max(block.first_line);
                let to = bottom.min(block.first_line + block.lines.len() - 1);
                let mut shapes = Vec::new();
                for line in from..=to {
                    let (s, e) = block.lines[line - block.first_line];
                    let (a, b) = (at(s), at(e));
                    let right = b.left().max(a.left() + char_w * 0.5);
                    let area = egui::Rect::from_min_max(a.left_top(), Pos2::new(right, a.bottom()));
                    shapes.push(egui::Shape::rect_filled(area, 2.0, BRACE_BLOCK_FILL));
                }
                for c in [block.open, block.close] {
                    let (a, b) = (at(c), at(c + 1));
                    let area = egui::Rect::from_min_max(a.left_top(), Pos2::new(b.left(), a.bottom()));
                    shapes.push(egui::Shape::rect_filled(area, 2.0, BRACE_PAIR_FILL));
                }
                painter.set(brace_shape, egui::Shape::Vec(shapes));
            }

            let full = egui::Rect::from_min_max(
                Pos2::new(gutter_rect.left(), clip.top().min(gutter_rect.top())),
                Pos2::new(gutter_rect.right(), clip.bottom().max(origin.y + galley.size().y + 4.0)),
            );
            painter.rect_filled(full, 0.0, editor_bg.gamma_multiply(0.92));
            let cursor_line = out.cursor_range.map(|r| {
                tab.text[..byte_of(&tab.text, r.primary.index.0)].matches('\n').count()
            });
            let mut line = 0usize;
            let mut new_line = true;
            let num_color = ui.visuals().weak_text_color();
            let cur_color = ui.visuals().strong_text_color();
            for row in &galley.rows {
                let y = origin.y + row.pos.y;
                if new_line && y + row_h >= clip.top() && y <= clip.bottom() {
                    if let Some(&sev) = line_severity.get(&line) {
                        painter.circle_filled(Pos2::new(gutter_rect.left() + 6.0, y + row_h / 2.0), 3.0, severity_color(sev));
                    }
                    painter.text(
                        Pos2::new(gutter_rect.right() - 10.0, y),
                        egui::Align2::RIGHT_TOP,
                        (line + 1).to_string(),
                        font.clone(),
                        if Some(line) == cursor_line { cur_color } else { num_color },
                    );
                }
                if row.ends_with_newline {
                    line += 1;
                    new_line = true;
                } else {
                    new_line = false;
                }
            }

            // live problems: wavy underlines, with the message on hover
            if !line_severity.is_empty() {
                let mut starts: Vec<(usize, usize)> = vec![(0, 0)];
                let mut chars = 0;
                for (b, ch) in tab.text.char_indices() {
                    chars += 1;
                    if ch == '\n' {
                        starts.push((b + 1, chars));
                    }
                }
                let first = ((clip.top() - origin.y) / row_h).floor().max(0.0) as usize;
                let last = ((clip.bottom() - origin.y) / row_h).ceil().max(0.0) as usize;
                let pointer = ctx.pointer_hover_pos().filter(|p| clip.contains(*p));
                let mut hover: Vec<&str> = Vec::new();
                for d in ec.diags.iter().filter(|d| d.severity <= 3) {
                    let to_line = d.end.0.min(last).min(starts.len() - 1);
                    for l in d.start.0.max(first)..=to_line {
                        let (lb, lc) = starts[l];
                        let le = starts.get(l + 1).map_or(tab.text.len(), |s| s.0 - 1);
                        let line_text = &tab.text[lb..le];
                        let total = line_text.chars().count();
                        let indent = line_text.chars().take_while(|c| c.is_whitespace()).count();
                        let from = if l == d.start.0 { chars_in_units(line_text, d.start.1) } else { indent };
                        let to = if l == d.end.0 { chars_in_units(line_text, d.end.1) } else { total };
                        let (from, to) = if to > from {
                            (from, to)
                        } else if from < total {
                            (from, from + 1)
                        } else {
                            (total.saturating_sub(1), total)
                        };
                        let a = galley.pos_from_cursor(CCursor::new(lc + from)).translate(origin.to_vec2());
                        let b = galley.pos_from_cursor(CCursor::new(lc + to)).translate(origin.to_vec2());
                        let x1 = b.left().max(a.left() + char_w);
                        squiggle(painter, a.left(), x1, a.bottom() - 0.5, severity_color(d.severity));
                        let area = egui::Rect::from_min_max(a.left_top(), Pos2::new(x1, a.bottom()));
                        if pointer.is_some_and(|p| area.contains(p)) && !hover.contains(&d.message.as_str()) {
                            hover.push(&d.message);
                        }
                    }
                }
                if !hover.is_empty() {
                    let tip = format!("{}\n\nQuick fix: Ctrl+.", hover.join("\n"));
                    let _ = out.response.response.clone().on_hover_text_at_pointer(tip);
                }
            }

            // the snippet field being filled in
            if let Some(s) = ec.session.as_ref() {
                for &(from, to) in &s.groups[s.current] {
                    let a = galley.pos_from_cursor(CCursor::new(from)).translate(origin.to_vec2());
                    let b = galley.pos_from_cursor(CCursor::new(to)).translate(origin.to_vec2());
                    if (a.top() - b.top()).abs() < 1.0 {
                        let area = egui::Rect::from_min_max(a.left_top(), Pos2::new(b.left().max(a.left() + 2.0), a.bottom()));
                        painter.rect_filled(area, 2.0, Color32::from_rgba_unmultiplied(0x56, 0x9C, 0xD6, 45));
                    }
                }
            }

            if let Some(r) = out.cursor_range {
                let ci = r.primary.index.0;
                cursor_now = Some(ci);
                let rect = galley.pos_from_cursor(r.primary).translate(origin.to_vec2());
                popup_anchor = Some(rect.left_bottom() + Vec2::new(0.0, 2.0));
                let bi = byte_of(&tab.text, ci);
                let ls = line_start(&tab.text, bi);
                line_col = Some((tab.text[..bi].matches('\n').count() + 1, tab.text[ls..bi].chars().count() + 1));
            }

            // jump to a line (from the Problems list)
            if let Some(target) = tab.goto_line.take() {
                let mut bi = 0;
                for _ in 1..target {
                    match tab.text[bi..].find('\n') {
                        Some(i) => bi += i + 1,
                        None => break,
                    }
                }
                let ci = char_of(&tab.text, bi);
                let rect = galley.pos_from_cursor(CCursor::new(ci)).translate(origin.to_vec2());
                ui.scroll_to_rect(rect.expand2(Vec2::new(0.0, row_h * 3.0)), Some(Align::Center));
                set_cursor(&ctx, id, ci, ci);
                ctx.memory_mut(|m| m.request_focus(id));
            }
        });
    });
    let _ = scroll;
    tab.hl_cache = cache;

    // ---- smart typing
    if text_changed {
        changed = true;
        let new_len = tab.text.chars().count();
        if let (Some(pc), Some(cc)) = (prev_cursor, cursor_now) {
            if new_len == prev_len + 1 && cc == pc + 1 && cc > 0 {
                let typed = tab.text[byte_of(&tab.text, cc - 1)..].chars().next().unwrap_or('\0');
                if let Some(nc) = after_char_typed(&mut tab.text, cc, typed) {
                    set_cursor(&ctx, id, nc, nc);
                    cursor_now = Some(nc);
                }
            }
        }
    }

    // ---- completion
    if let Some(sel) = accept_item {
        if let (Some(c), Some(cur)) = (ec.completion.take(), get_cursor(&ctx, id)) {
            if let Some(item) = c.items.get(sel) {
                let (nc, acc, stops) = accept(&mut tab.text, c.start, cur.1, item);
                set_cursor(&ctx, id, nc, nc);
                if acc.is_none() {
                    used = Some(item.label.clone());
                }
                accepted = acc;
                inserted_stops = stops;
                cancel_request = true;
                changed = true;
                cursor_now = None;
            }
        }
    }

    let trigger = acts.contains(&Act::Trigger);
    let still_open = ec.completion.as_ref().is_some_and(|c| c.tab_id == id);
    if let Some(cur) = cursor_now {
        let wants = trigger || (still_open && (text_changed || prev_cursor != Some(cur))) || (ec.autocomplete && text_changed);
        if wants && tab.is_java() {
            let manual = trigger || ec.completion.as_ref().is_some_and(|c| c.manual);
            let bi = byte_of(&tab.text, cur);
            if ec.lsp_ready && tab.path.is_some() {
                let sb = word_start(&tab.text, bi);
                let prefix = &tab.text[sb..bi];
                let in_word = !prefix.is_empty() && !prefix.as_bytes()[0].is_ascii_digit();
                let prev = tab.text[..sb].chars().next_back();
                let after_trigger = prefix.is_empty() && matches!(prev, Some('.' | '@' | '#'));
                let quiet = complete::in_string_or_comment(&tab.text[..bi]);
                if manual || (!quiet && (in_word || after_trigger)) {
                    // Keep showing the current list, narrowed, until the new one arrives.
                    let start = char_of(&tab.text, sb);
                    match ec.completion.as_mut().filter(|c| c.tab_id == id && c.start == start) {
                        Some(c) => {
                            c.refilter(prefix);
                            c.manual = manual;
                        }
                        None => *ec.completion = None,
                    }
                    if text_changed || trigger {
                        let typed = if after_trigger && !trigger { prev } else { None };
                        request = Some(CompletionRequest { byte: bi, start, trigger: typed, manual });
                    }
                } else {
                    *ec.completion = None;
                    cancel_request = true;
                }
            } else {
                match complete::complete(&tab.text, bi, ec.index, manual, ec.recent) {
                    Some(res) => {
                        let start = char_of(&tab.text, res.start);
                        let keep_sel = ec.completion.as_ref().filter(|c| c.start == start).map_or(0, |c| c.selected);
                        *ec.completion = Some(CompletionState {
                            tab_id: id,
                            start,
                            selected: keep_sel.min(res.items.len() - 1),
                            items: res.items,
                            all: Vec::new(),
                            pos: popup_anchor.unwrap_or_default(),
                            manual,
                            scroll: 0.0,
                            jump: true,
                        });
                    }
                    None => *ec.completion = None,
                }
            }
        }
    }

    // ---- popup
    let mut clicked: Option<usize> = None;
    let mut popup_hovered = false;
    if let Some(c) = ec.completion.as_mut().filter(|c| c.tab_id == id && !c.items.is_empty()) {
        if let Some(p) = popup_anchor {
            c.pos = p;
        }
        let row_height = (row_h + 6.0).max(ui.spacing().interact_size.y);
        let list_height = 320.0;
        if acts.contains(&Act::Up) || acts.contains(&Act::Down) {
            let top = c.selected as f32 * row_height;
            if top < c.scroll {
                c.scroll = top;
            } else if top + row_height > c.scroll + list_height {
                c.scroll = top + row_height - list_height;
            }
            c.jump = true;
        }
        let resp = egui::Area::new(Id::new("javaide-completion"))
            .order(egui::Order::Foreground)
            .fixed_pos(c.pos)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(460.0);
                    ui.set_max_width(760.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut area = egui::ScrollArea::vertical()
                        .id_salt("javaide-completion-list")
                        .max_height(list_height)
                        .auto_shrink([false, true]);
                    if c.jump {
                        area = area.vertical_scroll_offset(c.scroll);
                        c.jump = false;
                    }
                    let selected_now = c.selected;
                    let items = &c.items;
                    let out = area.show_rows(ui, row_height, items.len(), |ui, range| {
                        for i in range {
                            let item = &items[i];
                            let row = Vec2::new(ui.available_width(), row_height);
                            let r = ui
                                .allocate_ui_with_layout(row, egui::Layout::left_to_right(Align::Center), |ui| {
                                    ui.set_min_size(row);
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    let badge_col = match item.kind {
                                        Kind::Method => Color32::from_rgb(0xB0, 0x7C, 0xE8),
                                        Kind::Field | Kind::Variable => Color32::from_rgb(0x56, 0xA8, 0xF5),
                                        Kind::Class => Color32::from_rgb(0xE7, 0x9F, 0x3C),
                                        Kind::Snippet => Color32::from_rgb(0x6A, 0xAB, 0x73),
                                        Kind::Keyword => Color32::from_rgb(0xCF, 0x8E, 0x6D),
                                    };
                                    ui.label(RichText::new(item.kind.badge()).monospace().color(badge_col).strong());
                                    let r = ui.add(
                                        egui::Button::selectable(i == selected_now, RichText::new(&item.label).monospace())
                                            .frame_when_inactive(false),
                                    );
                                    ui.add(egui::Label::new(RichText::new(&item.detail).small().weak()).truncate());
                                    r
                                })
                                .inner;
                            if r.clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
                    c.scroll = out.state.offset.y;
                });
            });
        popup_hovered = resp.response.contains_pointer();
    }
    if let Some(i) = clicked {
        if let Some(c) = ec.completion.take() {
            let cur = get_cursor(&ctx, id).map_or(c.start, |r| r.1);
            let (nc, acc, stops) = accept(&mut tab.text, c.start, cur, &c.items[i]);
            set_cursor(&ctx, id, nc, nc);
            if acc.is_none() {
                used = Some(c.items[i].label.clone());
            }
            accepted = acc;
            inserted_stops = stops;
            cancel_request = true;
            request = None;
            ctx.memory_mut(|m| m.request_focus(id));
            changed = true;
        }
    } else if !editor_has_focus && !popup_hovered && ec.completion.as_ref().is_some_and(|c| c.tab_id == id) {
        *ec.completion = None;
        cancel_request = true;
    }

    // ---- snippet fields: follow edits, mirror them, and start new sessions
    let mut field_moved = 0isize;
    if let Some(before) = &text_at_start {
        if tab.text != *before {
            match ec.session.as_mut().and_then(|s| s.on_edit(before, &mut tab.text)) {
                Some(moved) => {
                    field_moved = moved;
                    if moved != 0 {
                        if let Some((x, y)) = get_cursor(&ctx, id) {
                            let shifted = |v: usize| (v as isize + moved).max(0) as usize;
                            set_cursor(&ctx, id, shifted(x), shifted(y));
                        }
                    }
                }
                None => *ec.session = None,
            }
        } else if let (Some(s), Some((x, y))) = (ec.session.as_ref(), get_cursor(&ctx, id)) {
            if !s.contains(x) || !s.contains(y) {
                *ec.session = None;
            }
        }
    }
    if let Some(mut groups) = inserted_stops {
        for r in groups.iter_mut().flatten() {
            *r = ((r.0 as isize + field_moved) as usize, (r.1 as isize + field_moved) as usize);
        }
        if groups.len() > 1 {
            match ec.session.as_mut() {
                Some(s) => s.splice(groups),
                None => *ec.session = SnippetSession::new(id, groups, &mut tab.text),
            }
            if let Some(s) = ec.session.as_ref() {
                let (x, y) = s.range();
                set_cursor(&ctx, id, x, y);
            }
        }
    }

    // ---- the Ctrl+. menu
    if text_changed || changed {
        *ec.quick_menu = None;
    }
    let mut quick_clicked: Option<usize> = None;
    let mut quick_hovered = false;
    if let Some(m) = ec.quick_menu.as_mut().filter(|m| !m.items.is_empty()) {
        if let Some(p) = popup_anchor {
            m.pos = p;
        }
        let moved = acts.contains(&Act::QuickUp) || acts.contains(&Act::QuickDown);
        let resp = egui::Area::new(Id::new("javaide-quick-fix"))
            .order(egui::Order::Foreground)
            .fixed_pos(m.pos)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(320.0);
                    ui.set_max_width(680.0);
                    egui::ScrollArea::vertical().id_salt("javaide-quick-fix-list").max_height(320.0).show(ui, |ui| {
                        for (i, item) in m.items.iter().enumerate() {
                            let r = ui.add(egui::Button::selectable(i == m.selected, &item.title).frame_when_inactive(false));
                            if moved && i == m.selected {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                quick_clicked = Some(i);
                            }
                        }
                    });
                });
            });
        quick_hovered = resp.response.contains_pointer();
    }
    if let Some(i) = quick_clicked {
        if let Some(m) = ec.quick_menu.take() {
            quick_choice = m.items.get(i).map(|x| x.raw.clone());
            ctx.memory_mut(|mem| mem.request_focus(id));
        }
    } else if !editor_has_focus && !quick_hovered {
        *ec.quick_menu = None;
    }

    if changed {
        tab.dirty = tab.text != tab.saved;
    }
    EditorResult { line_col, changed, used, request, accepted, cancel_request, quick_request, quick_choice }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate typing `c` at the `|` marker; returns text with `|` at the new cursor.
    fn typed(src: &str, c: char) -> String {
        let cur = src.find('|').unwrap();
        let mut t = src.replacen('|', "", 1);
        t.insert(cur, c);
        let ci = char_of(&t, cur) + 1;
        let nc = after_char_typed(&mut t, ci, c).unwrap_or(ci);
        let b = byte_of(&t, nc);
        t.insert(b, '|');
        t
    }

    #[test]
    fn enter_after_brace_indents_and_closes() {
        assert_eq!(typed("class A {|", '\n'), "class A {\n    |\n}");
        assert_eq!(typed("    void f() {|}", '\n'), "    void f() {\n        |\n    }");
        assert_eq!(typed("        int x = 1;|", '\n'), "        int x = 1;\n        |");
    }

    #[test]
    fn pairs_and_overtype() {
        assert_eq!(typed("foo|", '('), "foo(|)");
        assert_eq!(typed("foo(|)", ')'), "foo()|");
        assert_eq!(typed("x = |;", '"'), "x = \"|\";");
        assert_eq!(typed("x = \"ab|\";", '"'), "x = \"ab\"|;");
        assert_eq!(typed("        |", '}'), "    }|");
    }

    #[test]
    fn braces_close_themselves() {
        assert_eq!(typed("class A |", '{'), "class A {|}");
        assert_eq!(typed("int[] a = |;", '{'), "int[] a = {|};");
        assert_eq!(typed("class A {|}", '\n'), "class A {\n    |\n}");
        // typing the closing brace over the automatic one
        assert_eq!(typed("class A {|}", '}'), "class A {}|");
        // not inside strings or comments
        assert_eq!(typed("String s = \"a|", '{'), "String s = \"a{|");
        assert_eq!(typed("// note |", '{'), "// note {|");
        // not in front of an identifier
        assert_eq!(typed("|foo", '{'), "{|foo");
    }

    #[test]
    fn indent_comment_ops() {
        let mut t = "a\nb".to_string();
        let r = indent(&mut t, (0, 3));
        assert_eq!(t, "    a\n    b");
        let _ = dedent(&mut t, r);
        assert_eq!(t, "a\nb");
        let mut t = "    int x;".to_string();
        toggle_comment(&mut t, (5, 5));
        assert_eq!(t, "    // int x;");
        toggle_comment(&mut t, (5, 5));
        assert_eq!(t, "    int x;");
    }

    #[test]
    fn a_cursor_next_to_a_brace_finds_its_block() {
        let t = "class A {\n    void f() {\n        int x = 1;\n    }\n}\n";
        let ch = |needle: &str| char_of(t, t.find(needle).unwrap());
        let (outer_open, inner_open) = (ch("{"), ch("{\n        int"));
        let inner_close = ch("}\n}");
        let outer_close = inner_close + 2;
        let pair = |c: usize| brace_block(t, c, c).map(|b| (b.open, b.close));

        for c in [outer_open, outer_open + 1, outer_close, outer_close + 1] {
            assert_eq!(pair(c), Some((outer_open, outer_close)), "cursor at {c}");
        }
        for c in [inner_open, inner_open + 1, inner_close, inner_close + 1] {
            assert_eq!(pair(c), Some((inner_open, inner_close)), "cursor at {c}");
        }
        assert_eq!(pair(ch("int")), None);
        // a selected brace counts, any other selection doesn't
        assert_eq!(brace_block(t, inner_open, inner_open + 1).map(|b| b.close), Some(inner_close));
        assert_eq!(brace_block(t, inner_open, inner_open + 2), None);

        let b = brace_block(t, inner_open, inner_open).unwrap();
        assert_eq!(b.first_line, 1);
        assert_eq!(b.lines.len(), 3);
        assert_eq!(b.lines[0], (inner_open, inner_open + 1));
        assert_eq!(b.lines[2].1, inner_close + 1);
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_count() {
        let t = "void f() {\n    String s = \"}\"; // }\n    char c = '{'; /* { */\n}\n";
        let open = char_of(t, t.find('{').unwrap());
        let close = char_of(t, t.rfind('}').unwrap());
        assert_eq!(brace_block(t, open, open).map(|b| b.close), Some(close));
        assert_eq!(brace_block(t, close + 1, close + 1).map(|b| b.open), Some(open));
        let in_string = char_of(t, t.find("\"}\"").unwrap()) + 1;
        assert_eq!(brace_block(t, in_string, in_string), None);
    }

    #[test]
    fn unmatched_and_non_ascii_braces() {
        assert_eq!(brace_block("class A {", 9, 9), None);
        assert_eq!(brace_block("}", 0, 0), None);
        let t = "// čšž😀\nclass Č { String š = \"ž\"; }";
        let open = t[..t.find('{').unwrap()].chars().count();
        let close = t[..t.find('}').unwrap()].chars().count();
        assert_eq!(brace_block(t, open, open).map(|b| (b.open, b.close)), Some((open, close)));
    }

    /// Renders the editor without a window and returns its top-level shapes.
    fn render_with_cursor(text: &str, cursor: usize) -> Vec<egui::Shape> {
        let ctx = egui::Context::default();
        let id = Id::new("brace-test");
        let mut tab = Tab::new(None, text.to_string(), id);
        let index = ProjectIndex::default();
        let (mut completion, mut session, mut quick_menu) = (None, None, None);
        let mut shapes = Vec::new();
        for frame in 0..4 {
            if frame == 1 {
                ctx.memory_mut(|m| m.request_focus(id));
                set_cursor(&ctx, id, cursor, cursor);
            }
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
                time: Some(frame as f64 * 0.02),
                focused: true,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    editor_ui(
                        ui,
                        &mut tab,
                        EditorCtx {
                            dark: true,
                            font_size: 14.0,
                            index: &index,
                            completion: &mut completion,
                            autocomplete: false,
                            recent: &[],
                            lsp_ready: false,
                            diags: &[],
                            session: &mut session,
                            quick_menu: &mut quick_menu,
                        },
                    );
                });
            });
            out.textures_delta.clear();
            shapes = out.shapes.into_iter().map(|c| c.shape).collect();
        }
        shapes
    }

    fn fills(shape: &egui::Shape, color: Color32) -> usize {
        match shape {
            egui::Shape::Vec(v) => v.iter().map(|s| fills(s, color)).sum(),
            egui::Shape::Rect(r) if r.fill == color => 1,
            _ => 0,
        }
    }

    #[test]
    fn the_block_is_painted_behind_the_text() {
        let t = "class A {\n    int x;\n}\n";
        let shapes = render_with_cursor(t, t.find('{').unwrap());
        let block: usize = shapes.iter().map(|s| fills(s, BRACE_BLOCK_FILL)).sum();
        let pair: usize = shapes.iter().map(|s| fills(s, BRACE_PAIR_FILL)).sum();
        assert_eq!((block, pair), (3, 2), "one area per line, plus both braces");
        let highlight = shapes.iter().position(|s| fills(s, BRACE_PAIR_FILL) > 0).unwrap();
        let text = shapes.iter().position(|s| matches!(s, egui::Shape::Text(_))).unwrap();
        assert!(highlight < text, "the highlight must not cover the text");

        let away = render_with_cursor(t, t.find("int").unwrap());
        assert_eq!(away.iter().map(|s| fills(s, BRACE_PAIR_FILL)).sum::<usize>(), 0);
    }

    fn session_for(snippet: &str, text: &mut String) -> SnippetSession {
        let (plain, stops) = lsp::strip_stops(&lsp::expand_snippet(snippet));
        let base = text.chars().count();
        text.push_str(&plain);
        let groups = build_groups(&stops, base, plain.chars().count());
        SnippetSession::new(Id::new("t"), groups, text).unwrap()
    }

    /// Replace the current field with `value`, as typing over its selection does.
    fn fill(s: &mut SnippetSession, text: &mut String, value: &str) {
        let before = text.clone();
        let (a, b) = s.range();
        let (x, y) = (byte_of(text, a), byte_of(text, b));
        text.replace_range(x..y, value);
        assert!(s.on_edit(&before, text).is_some(), "edit left the field");
    }

    #[test]
    fn tab_visits_fields_and_ends_at_the_final_stop() {
        let mut t = "s.".to_string();
        let mut s = session_for("substring(${1:begin}, ${2:end})$0", &mut t);
        assert_eq!(t, "s.substring(begin, end)");
        fill(&mut s, &mut t, "1");
        s.advance(true);
        fill(&mut s, &mut t, "22");
        assert_eq!(t, "s.substring(1, 22)");
        assert!(!s.is_finished());
        let end = s.advance(true);
        assert!(s.is_finished());
        assert_eq!(end.0, t.chars().count());
    }

    #[test]
    fn mirrored_fields_follow_typing() {
        let mut t = String::new();
        let mut s = session_for("for (int ${1:i} = 0; ${1:i} < ${2:n}; ${1:i}++) {\n    $0\n}", &mut t);
        fill(&mut s, &mut t, "index");
        assert_eq!(t, "for (int index = 0; index < n; index++) {\n    \n}");
        s.advance(true);
        fill(&mut s, &mut t, "size");
        assert_eq!(t, "for (int index = 0; index < size; index++) {\n    \n}");
        let end = s.advance(true);
        assert_eq!(&t[byte_of(&t, end.0)..], "\n}");
    }

    #[test]
    fn typing_a_letter_equal_to_the_next_one_stays_in_the_field() {
        let mut t = String::new();
        let mut s = session_for("${1:x}a$0", &mut t);
        let before = t.clone();
        t.insert(1, 'a');
        assert!(s.on_edit(&before, &mut t).is_some());
        assert_eq!(s.range(), (0, 2));
    }

    #[test]
    fn an_edit_outside_the_field_ends_the_session() {
        let mut t = "abc ".to_string();
        let mut s = session_for("f(${1:x})$0", &mut t);
        let before = t.clone();
        t.insert(0, 'z');
        assert!(s.on_edit(&before, &mut t).is_none());
    }

    #[test]
    fn nested_snippet_is_visited_before_the_outer_fields() {
        let mut t = String::new();
        let mut s = session_for("a(${1:x}, ${2:y})$0", &mut t);
        let before = t.clone();
        let (from, to) = s.range();
        t.replace_range(byte_of(&t, from)..byte_of(&t, to), "g(p)");
        s.on_edit(&before, &mut t).unwrap();
        s.splice(vec![vec![(from + 2, from + 3)], vec![(from + 4, from + 4)]]);
        assert_eq!(s.range(), (from + 2, from + 3));
        s.advance(true);
        let outer = s.advance(true);
        assert_eq!(&t[byte_of(&t, outer.0)..byte_of(&t, outer.1)], "y");
    }

    #[test]
    fn text_edits_apply_and_remap_offsets() {
        let mut t = "abc\ndef\n".to_string();
        let edits = [
            TextEdit { start: (0, 0), end: (0, 0), text: "XX".into() },
            TextEdit { start: (1, 0), end: (1, 2), text: "Q".into() },
        ];
        let changes = apply_text_edits(&mut t, &edits);
        assert_eq!(t, "XXabc\nQf\n");
        assert_eq!(remap(1, &changes), 3); // the "b"
        assert_eq!(remap(6, &changes), 7); // the "f"
        assert_eq!(remap(5, &changes), 7); // inside the replaced text: after it
    }

    #[test]
    fn import_edits_land_above_the_completed_word() {
        let before = "class A {\n    Arr\n}\n";
        let mut text = "class A {\n    ArrayList\n}\n".to_string();
        let edits = [TextEdit { start: (0, 0), end: (0, 0), text: "import java.util.ArrayList;\n\n".into() }];
        let (boundary, delta) = apply_additional(&mut text, before, &edits).unwrap();
        assert_eq!(text, "import java.util.ArrayList;\n\nclass A {\n    ArrayList\n}\n");
        assert_eq!((boundary, delta), (0, 29));
    }

    #[test]
    fn import_edits_are_dropped_if_the_text_above_changed() {
        let before = "package a;\n\nclass A {}\n";
        let mut text = "package b;\n\nclass A {}\n".to_string();
        let edits = [TextEdit { start: (1, 0), end: (1, 0), text: "import x.Y;\n".into() }];
        assert!(apply_additional(&mut text, before, &edits).is_none());
        assert_eq!(text, "package b;\n\nclass A {}\n");
    }

    #[test]
    fn several_edits_at_one_spot_keep_their_order() {
        let before = "class A {}\n";
        let mut text = before.to_string();
        let edits = [
            TextEdit { start: (0, 0), end: (0, 0), text: "import a.B;\n".into() },
            TextEdit { start: (0, 0), end: (0, 0), text: "import c.D;\n\n".into() },
        ];
        apply_additional(&mut text, before, &edits).unwrap();
        assert_eq!(text, "import a.B;\nimport c.D;\n\nclass A {}\n");
    }

    #[test]
    fn completion_snippet_keeps_indent() {
        let mut t = "    sout".to_string();
        let item = Item { label: "psvm".into(), insert: "void main() {\n    $0\n}".into(), detail: String::new(), kind: Kind::Snippet, lsp: None };
        let (c, _) = apply_completion(&mut t, 4, 8, &item);
        assert_eq!(t, "    void main() {\n        \n    }");
        assert_eq!(c, "    void main() {\n        ".chars().count());
    }
}
