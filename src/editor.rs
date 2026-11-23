//! The code editor: line-number gutter, syntax highlighting, smart indent,
//! bracket pairing, line comments and the completion popup.

use crate::complete::{self, Item, Kind, ProjectIndex};
use crate::highlight;
use eframe::egui::{
    self, Align, Color32, FontId, Id, Key, Modifiers, Pos2, RichText, Sense, Vec2,
    text::{CCursor, CCursorRange, LayoutJob},
    text_edit::TextEditState,
};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

pub struct Tab {
    pub path: Option<PathBuf>,
    pub text: String,
    pub saved: String,
    pub dirty: bool,
    pub crlf: bool,
    pub id: Id,
    pub goto_line: Option<usize>,
    hl_cache: Option<(u64, LayoutJob)>,
}

impl Tab {
    pub fn new(path: Option<PathBuf>, text: String, id: Id) -> Tab {
        let crlf = text.contains("\r\n");
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        Tab { path, saved: text.clone(), text, dirty: false, crlf, id, goto_line: None, hl_cache: None }
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
    pub items: Vec<Item>,
    pub selected: usize,
    pub pos: Pos2,
    pub manual: bool,
}

pub struct EditorCtx<'a> {
    pub dark: bool,
    pub font_size: f32,
    pub index: &'a ProjectIndex,
    pub completion: &'a mut Option<CompletionState>,
    pub autocomplete: bool,
    /// Recently accepted completions, most recent first.
    pub recent: &'a [String],
}

pub struct EditorResult {
    pub line_col: Option<(usize, usize)>,
    pub changed: bool,
    /// Label of the completion accepted this frame, if any.
    pub used: Option<String>,
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
fn apply_completion(text: &mut String, start: usize, cursor: usize, item: &Item) -> usize {
    let bs = byte_of(text, start);
    let bc = byte_of(text, cursor);
    let ws = leading_ws(&text[line_start(text, bs)..]).to_string();
    let mut ins = item.insert.replace('\n', &format!("\n{ws}"));
    // Don't double up parentheses when they're already there.
    if item.kind == Kind::Method && text[bc..].starts_with('(') {
        ins = item.label.clone();
    }
    let (ins, off) = match ins.find("$0") {
        Some(p) => (ins.replacen("$0", "", 1), ins[..p].chars().count()),
        None => {
            let n = ins.chars().count();
            (ins, n)
        }
    };
    text.replace_range(bs..bc, &ins);
    start + off
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
    let popup_open = ec.completion.as_ref().is_some_and(|c| c.tab_id == id);
    let mut changed = false;

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
    }
    let mut acts: Vec<Act> = Vec::new();
    if has_focus {
        ctx.input_mut(|i| {
            if popup_open {
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
            } else if i.consume_key(Modifiers::SHIFT, Key::Tab) {
                acts.push(Act::Dedent);
            } else if i.consume_key(Modifiers::NONE, Key::Tab) {
                acts.push(Act::Indent);
            }
            if i.consume_key(Modifiers::COMMAND, Key::Space) {
                acts.push(Act::Trigger);
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
                if let Some(c) = ec.completion.as_mut() {
                    let n = c.items.len();
                    c.selected = if *a == Act::Down { (c.selected + 1) % n } else { (c.selected + n - 1) % n };
                }
            }
            Act::Accept => accept_item = ec.completion.as_ref().map(|c| c.selected),
            Act::Close => *ec.completion = None,
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

    let scroll = egui::ScrollArea::both().id_salt(id.with("scroll")).auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let avail_h = ui.available_height();
            let (gutter_rect, _) = ui.allocate_exact_size(Vec2::new(gutter_w, avail_h), Sense::hover());
            let rows = ((avail_h / row_h) as usize).max(1);
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
                let nc = apply_completion(&mut tab.text, c.start, cur.1, item);
                set_cursor(&ctx, id, nc, nc);
                used = Some(item.label.clone());
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
            match complete::complete(&tab.text, bi, ec.index, manual, ec.recent) {
                Some(res) => {
                    let start = char_of(&tab.text, res.start);
                    let keep_sel = ec.completion.as_ref().filter(|c| c.start == start).map_or(0, |c| c.selected);
                    *ec.completion = Some(CompletionState {
                        tab_id: id,
                        start,
                        selected: keep_sel.min(res.items.len() - 1),
                        items: res.items,
                        pos: popup_anchor.unwrap_or_default(),
                        manual,
                    });
                }
                None => *ec.completion = None,
            }
        }
    }

    // ---- popup
    let mut clicked: Option<usize> = None;
    let mut popup_hovered = false;
    if let Some(c) = ec.completion.as_mut().filter(|c| c.tab_id == id) {
        if let Some(p) = popup_anchor {
            c.pos = p;
        }
        let resp = egui::Area::new(Id::new("javaide-completion"))
            .order(egui::Order::Foreground)
            .fixed_pos(c.pos)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(320.0);
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for (i, item) in c.items.iter().enumerate() {
                            let selected = i == c.selected;
                            let r = ui
                                .horizontal(|ui| {
                                    let badge_col = match item.kind {
                                        Kind::Method => Color32::from_rgb(0xB0, 0x7C, 0xE8),
                                        Kind::Field | Kind::Variable => Color32::from_rgb(0x56, 0xA8, 0xF5),
                                        Kind::Class => Color32::from_rgb(0xE7, 0x9F, 0x3C),
                                        Kind::Snippet => Color32::from_rgb(0x6A, 0xAB, 0x73),
                                        Kind::Keyword => Color32::from_rgb(0xCF, 0x8E, 0x6D),
                                    };
                                    ui.label(RichText::new(item.kind.badge()).monospace().color(badge_col).strong());
                                    let r = ui.add(
                                        egui::Button::selectable(selected, RichText::new(&item.label).monospace())
                                            .frame_when_inactive(false),
                                    );
                                    ui.label(RichText::new(&item.detail).small().weak());
                                    r
                                })
                                .inner;
                            if selected && (acts.contains(&Act::Up) || acts.contains(&Act::Down)) {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
                });
            });
        popup_hovered = resp.response.contains_pointer();
    }
    if let Some(i) = clicked {
        if let Some(c) = ec.completion.take() {
            let cur = get_cursor(&ctx, id).map_or(c.start, |r| r.1);
            let nc = apply_completion(&mut tab.text, c.start, cur, &c.items[i]);
            set_cursor(&ctx, id, nc, nc);
            used = Some(c.items[i].label.clone());
            ctx.memory_mut(|m| m.request_focus(id));
            changed = true;
        }
    } else if !editor_has_focus && !popup_hovered && ec.completion.as_ref().is_some_and(|c| c.tab_id == id) {
        *ec.completion = None;
    }

    if changed {
        tab.dirty = tab.text != tab.saved;
    }
    EditorResult { line_col, changed, used }
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
    fn completion_snippet_keeps_indent() {
        let mut t = "    sout".to_string();
        let item = Item { label: "psvm".into(), insert: "void main() {\n    $0\n}".into(), detail: String::new(), kind: Kind::Snippet };
        let c = apply_completion(&mut t, 4, 8, &item);
        assert_eq!(t, "    void main() {\n        \n    }");
        assert_eq!(c, "    void main() {\n        ".chars().count());
    }
}
