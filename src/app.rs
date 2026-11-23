mod ui;

use crate::complete::ProjectIndex;
use crate::editor::{self, CompletionState, EditorCtx, QuickMenu, SnippetSession, Tab};
use crate::filetree::{self, FileTree, TreeAction};
use crate::java::{self, Jdk, Problem};
use crate::lsp::{self, ActionStep, Diag, FileOp, Lsp, Status, TextEdit};
use crate::proc::{self, Proc};
use crate::settings::{self, Settings};
use eframe::egui::{self, Color32, Id, Key, KeyboardShortcut, Modifiers, RichText};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const OUTPUT_CAP: usize = 400_000;

#[derive(PartialEq, Clone, Copy)]
enum BottomTab {
    Output,
    Problems,
    Terminal,
}

#[derive(PartialEq, Clone, Copy)]
enum Phase {
    Idle,
    Building,
    Running,
}

enum Dialog {
    NewFile { dir: PathBuf, name: String },
    NewFolder { dir: PathBuf, name: String },
    Rename { path: PathBuf, name: String },
    Delete { path: PathBuf },
    CloseTab { idx: usize },
    Exit,
    Settings { jdk: String },
    About,
}

/// A completion request waiting for the language server.
struct PendingCompletion {
    id: u64,
    tab: Id,
    start: usize,
    manual: bool,
}

/// An accepted item whose imports are being fetched.
struct PendingResolve {
    tab: Id,
    before: String,
}

/// A Ctrl+. request waiting for the language server.
struct PendingQuick {
    id: u64,
    tab: Id,
    /// The menu only applies to exactly this text.
    text: String,
}

/// The range and problems to ask quick fixes for. With the cursor on a line that has
/// a problem elsewhere, the fixes for that problem are offered, as in VS Code.
fn quick_fix_params(text: &str, diags: &[Diag], a: usize, b: usize) -> ((lsp::Pos, lsp::Pos), Vec<Value>) {
    let start = lsp::pos_of_byte(text, editor::byte_index(text, a));
    let end = lsp::pos_of_byte(text, editor::byte_index(text, b));
    let visible = || diags.iter().filter(|d| d.severity <= 3);
    let hits: Vec<Value> = visible().filter(|d| d.start <= end && start <= d.end).map(|d| (*d.raw).clone()).collect();
    if !hits.is_empty() {
        return ((start, end), hits);
    }
    match visible().find(|d| d.start.0 <= start.0 && start.0 <= d.end.0) {
        Some(d) => ((d.start, d.end), vec![(*d.raw).clone()]),
        None => ((start, end), Vec::new()),
    }
}

/// Carry out a workspace edit from the language server, on open tabs and on disk.
/// Returns the file changes the server should be told about.
fn apply_file_ops(ctx: &egui::Context, tabs: &mut Vec<Tab>, lsp: Option<&Lsp>, ops: &[FileOp]) -> Result<Vec<(PathBuf, u8)>, String> {
    for op in ops {
        if let FileOp::Edit { path, version: Some(v), .. } = op {
            if lsp.and_then(|l| l.doc_version(path)).is_some_and(|cur| cur != *v) {
                return Err("The file changed; try the quick fix again.".into());
            }
        }
    }
    let tab_of = |tabs: &[Tab], path: &Path| {
        let key = lsp::path_key(path);
        tabs.iter().position(|t| t.path.as_deref().is_some_and(|p| lsp::path_key(p) == key))
    };
    let mut notes = Vec::new();
    for op in ops {
        match op {
            FileOp::Edit { path, edits, .. } => {
                if let Some(i) = tab_of(tabs, path) {
                    let tab = &mut tabs[i];
                    let changes = editor::apply_text_edits(&mut tab.text, edits);
                    editor::remap_cursor(ctx, tab.id, &changes);
                    tab.dirty = tab.text != tab.saved;
                } else {
                    let disk = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
                    let crlf = disk.contains("\r\n");
                    let mut text = disk.replace("\r\n", "\n");
                    editor::apply_text_edits(&mut text, edits);
                    let out = if crlf { text.replace('\n', "\r\n") } else { text };
                    std::fs::write(path, out).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
                    notes.push((path.clone(), 2));
                }
            }
            FileOp::Create { path, overwrite, ignore_if_exists } => {
                if path.exists() && !overwrite {
                    if *ignore_if_exists {
                        continue;
                    }
                    return Err(format!("{} already exists.", path.display()));
                }
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::write(path, "").map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
                notes.push((path.clone(), 1));
            }
            FileOp::Rename { from, to, overwrite, ignore_if_exists } => {
                if to.exists() && !overwrite {
                    if *ignore_if_exists {
                        continue;
                    }
                    return Err(format!("{} already exists.", to.display()));
                }
                if let Some(dir) = to.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::rename(from, to).map_err(|e| format!("Cannot rename {}: {e}", from.display()))?;
                if let Some(i) = tab_of(tabs, from) {
                    tabs[i].path = Some(to.clone());
                }
                notes.push((from.clone(), 3));
                notes.push((to.clone(), 1));
            }
            FileOp::Delete { path, ignore_if_missing } => {
                if !path.exists() {
                    if *ignore_if_missing {
                        continue;
                    }
                    return Err(format!("{} does not exist.", path.display()));
                }
                trash::delete(path).map_err(|e| format!("Cannot delete {}: {e}", path.display()))?;
                if let Some(i) = tab_of(tabs, path) {
                    tabs.remove(i);
                }
                notes.push((path.clone(), 3));
            }
        }
    }
    Ok(notes)
}

pub struct IdeApp {
    egui_ctx: egui::Context,
    lsp: Option<Lsp>,
    session: Option<SnippetSession>,
    quick_menu: Option<QuickMenu>,
    pending_quick: Option<PendingQuick>,
    /// Set by the editor this frame; sent once the server has the latest text.
    want_quick: Option<(Id, PathBuf, (lsp::Pos, lsp::Pos), Vec<Value>)>,
    /// Live problems per file, keyed by `lsp::path_key`.
    diags: HashMap<String, (PathBuf, Vec<Diag>)>,
    pending_completion: Option<PendingCompletion>,
    pending_resolves: HashMap<u64, PendingResolve>,
    /// Set by the editor this frame; sent once the server has the latest text.
    want_completion: Option<(Id, PathBuf, editor::CompletionRequest, lsp::Pos)>,
    settings: Settings,
    jdk: Jdk,
    root: Option<PathBuf>,
    tree: FileTree,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,

    phase: Phase,
    run_proc: Option<Proc>,
    pending_run: Option<Command>,
    output: String,
    stdin_line: String,
    problems: Vec<Problem>,

    term: Option<Proc>,
    term_buf: String,
    term_input: String,
    term_history: Vec<String>,
    term_hist_pos: Option<usize>,
    focus_term_input: bool,
    focus_stdin: bool,

    bottom_tab: BottomTab,
    dialog: Option<Dialog>,
    completion: Option<CompletionState>,
    index: ProjectIndex,
    status: String,
    line_col: Option<(usize, usize)>,
    allow_close: bool,
    applied_dark: Option<bool>,
    pending_term_dir: Option<PathBuf>,
    focus_editor: Option<Id>,
}

impl IdeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings) -> Self {
        let jdk = Jdk::detect(settings.jdk_path.as_deref());
        let mut app = IdeApp {
            egui_ctx: cc.egui_ctx.clone(),
            lsp: None,
            session: None,
            quick_menu: None,
            pending_quick: None,
            want_quick: None,
            diags: HashMap::new(),
            pending_completion: None,
            pending_resolves: HashMap::new(),
            want_completion: None,
            jdk,
            root: None,
            tree: FileTree::default(),
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            phase: Phase::Idle,
            run_proc: None,
            pending_run: None,
            output: String::new(),
            stdin_line: String::new(),
            problems: Vec::new(),
            term: None,
            term_buf: String::new(),
            term_input: String::new(),
            term_history: Vec::new(),
            term_hist_pos: None,
            focus_term_input: false,
            focus_stdin: false,
            bottom_tab: BottomTab::Output,
            dialog: None,
            completion: None,
            index: ProjectIndex::default(),
            status: String::new(),
            line_col: None,
            allow_close: false,
            applied_dark: None,
            pending_term_dir: None,
            focus_editor: None,
            settings,
        };
        app.apply_theme(&cc.egui_ctx);
        app.output = app.welcome_text();
        // Reopen the last folder, or the bundled sample workspace on first run.
        let workspace = settings::exe_dir().join("workspace").join("HelloWorld");
        let start = app.settings.last_folder.clone().filter(|f| f.is_dir()).or_else(|| workspace.is_dir().then_some(workspace));
        if let Some(f) = start {
            app.open_folder(f);
        }
        app
    }

    fn welcome_text(&self) -> String {
        let mut s = format!("Java IDE ready.\nJDK: {} ({})\n", self.jdk.version, self.jdk.origin);
        if let Some(h) = &self.jdk.home {
            s.push_str(&format!("JDK home: {}\n", h.display()));
        }
        if self.jdk.major == 0 {
            s.push_str("\n⚠ No Java installation was found. Put a JDK 25 into a folder named `jdk` next to JavaIDE.exe,\n  or pick one in Settings.\n");
        } else if self.jdk.major < java::REQUIRED_MAJOR {
            s.push_str(&format!("\n⚠ This JDK is older than Java {}. Newer language features may not compile.\n", java::REQUIRED_MAJOR));
        } else if !self.jdk.has_javac {
            s.push_str("\n⚠ This is a JRE without javac — compiling won't work. Use a full JDK.\n");
        }
        s.push_str("\nF5 run · Ctrl+B build · Ctrl+Space complete · Ctrl+/ comment · Ctrl+` terminal\n");
        s
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let dark = self.settings.dark_mode;
        if self.applied_dark == Some(dark) {
            return;
        }
        self.applied_dark = Some(dark);
        let (theme, mut v) = if dark {
            (egui::Theme::Dark, egui::Visuals::dark())
        } else {
            (egui::Theme::Light, egui::Visuals::light())
        };
        if dark {
            v.panel_fill = Color32::from_rgb(0x2B, 0x2D, 0x30);
            v.window_fill = Color32::from_rgb(0x2B, 0x2D, 0x30);
            v.extreme_bg_color = Color32::from_rgb(0x1E, 0x1F, 0x22);
            v.faint_bg_color = Color32::from_rgb(0x32, 0x34, 0x38);
            v.selection.bg_fill = Color32::from_rgb(0x21, 0x4A, 0x83);
        } else {
            v.extreme_bg_color = Color32::from_rgb(0xFF, 0xFF, 0xFF);
            v.panel_fill = Color32::from_rgb(0xF2, 0xF3, 0xF5);
        }
        ctx.set_theme(theme);
        ctx.set_visuals_of(theme, v);
    }

    fn new_id(&mut self) -> Id {
        self.next_id += 1;
        Id::new(("javaide-tab", self.next_id))
    }

    // ------------------------------------------------------------ files

    fn open_folder(&mut self, dir: PathBuf) {
        self.tree.reset();
        self.index.rebuild(&dir);
        self.settings.last_folder = Some(dir.clone());
        self.settings.save();
        if let Some(t) = &mut self.term {
            t.send(&cd_command(&dir));
        }
        self.status = format!("Opened {}", dir.display());
        self.root = Some(dir);
        let same = match (&self.lsp, &self.root) {
            (Some(l), Some(r)) => {
                !l.single_file && lsp::path_key(&l.root) == lsp::path_key(r) && !matches!(l.status(), Status::Failed(_))
            }
            _ => false,
        };
        if !same {
            self.start_lsp();
        }
    }

    // ------------------------------------------------------------ language server

    fn stop_lsp(&mut self) {
        if let Some(mut l) = self.lsp.take() {
            l.stop(false);
        }
        self.diags.clear();
        self.pending_completion = None;
        self.pending_resolves.clear();
        self.want_completion = None;
        self.completion = None;
        self.session = None;
        self.quick_menu = None;
        self.pending_quick = None;
        self.want_quick = None;
    }

    /// Serve the open folder or, without one, the folder of the open Java file.
    fn start_lsp(&mut self) {
        self.stop_lsp();
        if let Some(root) = self.root.clone() {
            self.start_lsp_at(&root, false);
            return;
        }
        let file = self.tabs.iter().filter_map(|t| t.path.clone()).find(|p| p.extension().is_some_and(|e| e == "java"));
        if let Some(dir) = file.as_deref().and_then(Path::parent) {
            self.start_lsp_at(dir, true);
        }
    }

    fn start_lsp_at(&mut self, root: &Path, single_file: bool) {
        self.stop_lsp();
        if self.jdk.major < lsp::MIN_JAVA {
            return;
        }
        let Some(server) = lsp::find_server() else { return };
        let ctx = self.egui_ctx.clone();
        match Lsp::start(&server, &self.jdk, root, single_file, move || ctx.request_repaint()) {
            Ok(l) => self.lsp = Some(l),
            Err(e) => self.status = e,
        }
    }

    /// With no folder open, a Java file brings the language server up for its folder.
    fn ensure_single_file_lsp(&mut self, path: &Path) {
        if self.root.is_some() || !path.extension().is_some_and(|e| e == "java") {
            return;
        }
        let Some(dir) = path.parent() else { return };
        let covered = self.lsp.as_ref().is_some_and(|l| {
            lsp::path_key(dir) == lsp::path_key(&l.root) && !matches!(l.status(), Status::Failed(_))
        });
        if !covered {
            self.start_lsp_at(dir, true);
        }
    }

    /// Apply a server edit to tabs and disk; false if it could not be applied.
    fn finish_ops(&mut self, ops: &[FileOp]) -> bool {
        match apply_file_ops(&self.egui_ctx, &mut self.tabs, self.lsp.as_ref(), ops) {
            Ok(notes) => {
                self.active = self.active.min(self.tabs.len().saturating_sub(1));
                self.completion = None;
                self.quick_menu = None;
                self.session = None;
                if !notes.is_empty() {
                    self.notify_files(&notes);
                }
                self.tree.refresh();
                if let Some(r) = self.root.clone() {
                    self.index.rebuild(&r);
                }
                self.egui_ctx.request_repaint();
                true
            }
            Err(e) => {
                self.status = e;
                false
            }
        }
    }

    fn run_action(&mut self, action: &Value, resolved: bool) {
        let Some(l) = &mut self.lsp else { return };
        match l.run_action(action, resolved) {
            ActionStep::Apply { ops, then } => {
                if self.finish_ops(&ops) {
                    if let Some(command) = then {
                        if let Some(l) = &mut self.lsp {
                            if let ActionStep::Apply { ops, .. } = l.run_command(&command) {
                                self.finish_ops(&ops);
                            }
                        }
                    }
                }
            }
            ActionStep::Waiting => {}
            ActionStep::Unsupported => self.status = "This action is not supported here.".into(),
        }
    }

    fn notify_files(&mut self, changes: &[(PathBuf, u8)]) {
        if let Some(l) = &mut self.lsp {
            l.files_changed(changes);
        }
    }

    fn poll_lsp(&mut self) {
        let Some(l) = &mut self.lsp else { return };
        let events = l.poll();
        for event in events {
            match event {
                lsp::Event::Diagnostics { path, items } => {
                    let key = lsp::path_key(&path);
                    if items.is_empty() {
                        self.diags.remove(&key);
                    } else {
                        self.diags.insert(key, (path, items));
                    }
                }
                lsp::Event::Completion { id, items } => {
                    if self.pending_completion.as_ref().is_some_and(|p| p.id == id) {
                        let p = self.pending_completion.take().unwrap();
                        Self::show_completion(&self.egui_ctx, &self.tabs, self.active, &mut self.completion, p, items);
                    }
                }
                lsp::Event::Resolved { id, edits } => {
                    if let Some(p) = self.pending_resolves.remove(&id) {
                        Self::apply_imports(
                            &self.egui_ctx,
                            &mut self.tabs,
                            &mut self.completion,
                            &mut self.session,
                            &mut self.status,
                            p,
                            &edits,
                        );
                    }
                }
                lsp::Event::CodeActions { id, items } => {
                    if self.pending_quick.as_ref().is_some_and(|p| p.id == id) {
                        let p = self.pending_quick.take().unwrap();
                        let current = self.tabs.get(self.active).filter(|t| t.id == p.tab && t.text == p.text);
                        if current.is_some() {
                            if items.is_empty() {
                                self.status = "No quick fixes here.".into();
                            } else {
                                self.quick_menu = Some(QuickMenu { tab_id: p.tab, items, selected: 0, pos: egui::Pos2::ZERO });
                            }
                        }
                    }
                }
                lsp::Event::ActionResolved { action } => self.run_action(&action, true),
                lsp::Event::ApplyEdit { request, ops } => {
                    let applied = self.finish_ops(&ops);
                    if let Some(l) = &mut self.lsp {
                        l.reply_apply_edit(&request, applied);
                    }
                }
                lsp::Event::CommandFailed { message } => self.status = message,
            }
        }
    }

    fn show_completion(
        ctx: &egui::Context,
        tabs: &[Tab],
        active: usize,
        completion: &mut Option<CompletionState>,
        p: PendingCompletion,
        items: Vec<crate::complete::Item>,
    ) {
        let Some(tab) = tabs.get(active).filter(|t| t.id == p.tab) else { return };
        let Some(cur) = editor::cursor_of(ctx, tab.id) else { return };
        let bi = editor::byte_index(&tab.text, cur);
        let sb = editor::word_start(&tab.text, bi);
        if editor::char_index(&tab.text, sb) != p.start {
            return; // the cursor has left the word this list was for
        }
        if items.is_empty() {
            if completion.as_ref().is_some_and(|c| c.tab_id == tab.id) {
                *completion = None;
            }
            return;
        }
        let pos = completion.as_ref().map_or(egui::Pos2::ZERO, |c| c.pos);
        let mut state = CompletionState {
            tab_id: tab.id,
            start: p.start,
            items: Vec::new(),
            all: items,
            selected: 0,
            pos,
            manual: p.manual,
            scroll: 0.0,
            jump: true,
        };
        state.refilter(&tab.text[sb..bi]);
        *completion = if state.items.is_empty() { None } else { Some(state) };
        ctx.request_repaint();
    }

    fn apply_imports(
        ctx: &egui::Context,
        tabs: &mut [Tab],
        completion: &mut Option<CompletionState>,
        session: &mut Option<SnippetSession>,
        status: &mut String,
        p: PendingResolve,
        edits: &[TextEdit],
    ) {
        if edits.is_empty() {
            return;
        }
        let Some(tab) = tabs.iter_mut().find(|t| t.id == p.tab) else { return };
        match editor::apply_additional(&mut tab.text, &p.before, edits) {
            Some((boundary, delta)) => {
                editor::shift_cursor(ctx, tab.id, boundary, delta);
                if let Some(s) = session.as_mut().filter(|s| s.tab_id == tab.id) {
                    s.shift_from(boundary, delta);
                }
                tab.dirty = tab.text != tab.saved;
                if completion.as_ref().is_some_and(|c| c.tab_id == tab.id) {
                    *completion = None;
                }
                ctx.request_repaint();
            }
            None => *status = "Could not add the import: the file changed above it.".into(),
        }
    }

    /// Send text changes, then any completion request that depends on them.
    fn sync_lsp(&mut self) {
        let Some(l) = &mut self.lsp else { return };
        let docs = self
            .tabs
            .iter()
            .filter_map(|t| t.path.as_deref().filter(|p| p.extension().is_some_and(|e| e == "java")).map(|p| (p, t.text.as_str())));
        l.sync(docs);
        if let Some((tab, path, req, pos)) = self.want_completion.take() {
            if let Some(old) = self.pending_completion.take() {
                l.cancel(old.id);
            }
            if let Some(id) = l.request_completion(&path, pos, req.trigger) {
                self.pending_completion = Some(PendingCompletion { id, tab, start: req.start, manual: req.manual });
            }
        }
        if let Some((tab, path, range, diagnostics)) = self.want_quick.take() {
            let text = self.tabs.iter().find(|t| t.id == tab).map(|t| t.text.clone());
            if let (Some(text), Some(id)) = (text, l.request_code_actions(&path, range, diagnostics)) {
                self.pending_quick = Some(PendingQuick { id, tab, text });
            }
        }
    }

    /// Live problems plus build errors the language server hasn't reported.
    fn problem_list(&self) -> Vec<Problem> {
        let mut list: Vec<Problem> = Vec::new();
        for (path, items) in self.diags.values() {
            for d in items.iter().filter(|d| d.severity <= 3) {
                list.push(Problem { file: path.clone(), line: d.start.0 + 1, is_error: d.severity == 1, message: d.message.clone() });
            }
        }
        for p in &self.problems {
            let key = lsp::path_key(&p.file);
            if !list.iter().any(|l| l.line == p.line && l.is_error == p.is_error && lsp::path_key(&l.file) == key) {
                list.push(p.clone());
            }
        }
        list.sort_by(|a, b| {
            (lsp::path_key(&a.file), a.line, !a.is_error).cmp(&(lsp::path_key(&b.file), b.line, !b.is_error))
        });
        list
    }

    fn pick_folder(&mut self) {
        let mut d = rfd::FileDialog::new();
        if let Some(r) = &self.root {
            d = d.set_directory(r);
        }
        if let Some(dir) = d.pick_folder() {
            self.open_folder(dir);
        }
    }

    fn pick_file(&mut self) {
        let mut d = rfd::FileDialog::new().add_filter("Java", &["java"]).add_filter("All files", &["*"]);
        if let Some(r) = &self.root {
            d = d.set_directory(r);
        }
        if let Some(f) = d.pick_file() {
            self.open_file(&f);
        }
    }

    fn open_file(&mut self, path: &Path) {
        let key = lsp::path_key(path);
        if let Some(i) = self.tabs.iter().position(|t| t.path.as_deref().is_some_and(|p| lsp::path_key(p) == key)) {
            self.active = i;
            return;
        }
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() > 8 * 1024 * 1024 => self.status = "File is too large to open (> 8 MB)".into(),
            Ok(bytes) if bytes.iter().take(8000).any(|&b| b == 0) => {
                self.status = format!("{} looks like a binary file", path.display())
            }
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let id = self.new_id();
                self.tabs.push(Tab::new(Some(path.to_path_buf()), text, id));
                self.active = self.tabs.len() - 1;
                self.tree.selected = Some(path.to_path_buf());
                self.focus_editor = Some(id);
                self.ensure_single_file_lsp(path);
            }
            Err(e) => self.status = format!("Cannot open {}: {e}", path.display()),
        }
    }

    fn new_untitled(&mut self) {
        let id = self.new_id();
        self.tabs.push(Tab::new(None, "void main() {\n    IO.println(\"Hello, Java 25!\");\n}\n".into(), id));
        self.active = self.tabs.len() - 1;
    }

    fn save_tab(&mut self, idx: usize) -> bool {
        let Some(tab) = self.tabs.get_mut(idx) else { return false };
        if tab.path.is_none() {
            let mut d = rfd::FileDialog::new().add_filter("Java", &["java"]).set_file_name("Main.java");
            if let Some(r) = &self.root {
                d = d.set_directory(r);
            }
            match d.save_file() {
                Some(p) => tab.path = Some(p),
                None => return false,
            }
        }
        let path = tab.path.clone().unwrap();
        match std::fs::write(&path, tab.contents_for_disk()) {
            Ok(()) => {
                tab.mark_saved();
                let text = tab.text.clone();
                if path.extension().is_some_and(|e| e == "java") {
                    self.index.update_file(&path, &text);
                    if let Some(l) = &mut self.lsp {
                        l.did_save(&path);
                    }
                }
                self.tree.refresh();
                self.status = format!("Saved {}", path.display());
                self.ensure_single_file_lsp(&path);
                true
            }
            Err(e) => {
                self.status = format!("Save failed: {e}");
                false
            }
        }
    }

    fn save_all(&mut self) -> bool {
        let mut ok = true;
        for i in 0..self.tabs.len() {
            if self.tabs[i].dirty || self.tabs[i].path.is_none() {
                ok &= self.save_tab(i);
            }
        }
        ok
    }

    fn close_tab(&mut self, idx: usize, force: bool) {
        if idx >= self.tabs.len() {
            return;
        }
        if self.tabs[idx].dirty && !force {
            self.dialog = Some(Dialog::CloseTab { idx });
            return;
        }
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if idx < self.active {
            self.active -= 1;
        }
        self.completion = None;
        self.session = None;
        self.quick_menu = None;
        let java_left = self.tabs.iter().any(|t| t.path.as_ref().is_some_and(|p| p.extension().is_some_and(|e| e == "java")));
        if self.root.is_none() && !java_left {
            self.stop_lsp();
        }
    }

    fn handle_tree_action(&mut self, a: TreeAction) {
        match a {
            TreeAction::Open(p) => self.open_file(&p),
            TreeAction::NewFile(dir) => self.dialog = Some(Dialog::NewFile { dir, name: "Main.java".into() }),
            TreeAction::NewFolder(dir) => self.dialog = Some(Dialog::NewFolder { dir, name: String::new() }),
            TreeAction::Rename(path) => {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.dialog = Some(Dialog::Rename { path, name });
            }
            TreeAction::Delete(path) => self.dialog = Some(Dialog::Delete { path }),
            TreeAction::Reveal(p) => filetree::reveal(&p),
            TreeAction::OpenTerminalHere(dir) => {
                self.settings.show_bottom = true;
                self.bottom_tab = BottomTab::Terminal;
                self.ensure_terminal_at(&dir);
            }
        }
    }

    fn create_file(&mut self, dir: &Path, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err("Enter a file name (no slashes).".into());
        }
        let path = dir.join(name);
        if path.exists() {
            return Err(format!("{name} already exists."));
        }
        let content = if let Some(class) = name.strip_suffix(".java") {
            let src_root = self.root.as_deref().map(java::source_root).unwrap_or_else(|| dir.to_path_buf());
            let pkg = java::package_for_dir(&src_root, dir);
            java::template(class, pkg.as_deref())
        } else {
            String::new()
        };
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
        self.notify_files(&[(path.clone(), 1)]);
        self.tree.refresh();
        self.tree.expand(dir);
        self.open_file(&path);
        if path.extension().is_some_and(|e| e == "java") {
            if let Ok(src) = std::fs::read_to_string(&path) {
                self.index.update_file(&path, &src);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ build & run

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn start_proc(&mut self, cmd: Command, ctx: &egui::Context) -> bool {
        match Proc::spawn(cmd, ctx) {
            Ok(p) => {
                self.run_proc = Some(p);
                true
            }
            Err(e) => {
                self.output.push_str(&format!(
                    "\n✖ Could not start {}: {e}\n  Is a JDK installed? Put one in a `jdk` folder next to JavaIDE.exe or set it in Settings.\n",
                    if self.phase == Phase::Building { "javac" } else { "java" }
                ));
                self.phase = Phase::Idle;
                self.pending_run = None;
                false
            }
        }
    }

    fn build(&mut self, ctx: &egui::Context, then_run: bool) {
        if self.phase != Phase::Idle {
            self.stop();
        }
        if !self.save_all() {
            self.status = "Build cancelled: not all files were saved".into();
            return;
        }
        self.settings.show_bottom = true;
        self.bottom_tab = BottomTab::Output;
        self.problems.clear();
        self.output.clear();

        let active_path = self.active_tab().and_then(|t| t.path.clone());
        let in_project = match (&self.root, &active_path) {
            (Some(r), Some(p)) => p.starts_with(r),
            (Some(_), None) => true,
            _ => false,
        };

        // A file outside any opened folder: use the source launcher.
        if !in_project {
            let Some(file) = active_path else {
                self.output.push_str("Open a folder or a .java file first.\n");
                return;
            };
            if !then_run {
                self.output.push_str("Build needs an open project folder. Use Run (F5) to launch a single file.\n");
                return;
            }
            self.output.push_str(&format!("▶ java {}\n\n", file.display()));
            self.phase = Phase::Running;
                self.focus_stdin = true;
            let cmd = java::source_launch_command(&self.jdk, &file, &self.settings.run_args);
            self.start_proc(cmd, ctx);
            return;
        }

        let root = self.root.clone().unwrap();
        let main = if then_run {
            match self.find_main_class(&root) {
                Some(m) => Some(m),
                None => {
                    self.output.push_str("✖ No class with a main method found in the project.\n");
                    return;
                }
            }
        } else {
            None
        };
        match java::build_command(&self.jdk, &root) {
            Ok((cmd, n)) => {
                self.output.push_str(&format!("🔨 Compiling {n} file(s) with {}…\n", self.jdk.version));
                self.pending_run = main.map(|m| {
                    self.output.push_str(&format!("   main class: {m}\n"));
                    java::run_command(&self.jdk, &root, &m, &self.settings.run_args)
                });
                self.phase = Phase::Building;
                self.start_proc(cmd, ctx);
            }
            Err(e) => self.output.push_str(&format!("✖ {e}\n")),
        }
    }

    fn find_main_class(&self, root: &Path) -> Option<String> {
        if let Some(t) = self.active_tab() {
            if let Some(p) = &t.path {
                if p.extension().is_some_and(|e| e == "java") && java::has_main(&t.text) {
                    return Some(java::main_class(p, &t.text));
                }
            }
        }
        let mut files = Vec::new();
        java::collect_sources(&java::source_root(root), &mut files);
        files.sort_by_key(|f| (f.file_stem().is_none_or(|s| s != "Main"), f.components().count()));
        files.into_iter().find_map(|f| {
            let src = std::fs::read_to_string(&f).ok()?;
            java::has_main(&src).then(|| java::main_class(&f, &src))
        })
    }

    fn stop(&mut self) {
        if let Some(p) = &mut self.run_proc {
            p.kill();
            self.output.push_str("\n■ Stopped.\n");
        }
        self.run_proc = None;
        self.pending_run = None;
        self.phase = Phase::Idle;
    }

    fn poll_processes(&mut self, ctx: &egui::Context) {
        if let Some(p) = &mut self.run_proc {
            let s = p.drain();
            if !s.is_empty() {
                proc::append_capped(&mut self.output, &proc::clean(&s), OUTPUT_CAP);
            }
            if let Some(code) = p.poll_exit() {
                let code_s = code.map_or("killed".to_string(), |c| c.to_string());
                match self.phase {
                    Phase::Building => {
                        let base = self.root.clone().unwrap_or_default();
                        self.problems = java::parse_problems(&self.output, &base);
                        if code == Some(0) {
                            self.output.push_str("✔ Build succeeded.\n");
                            if let Some(cmd) = self.pending_run.take() {
                                self.output.push_str("\n");
                                self.phase = Phase::Running;
                self.focus_stdin = true;
                                self.run_proc = None;
                                self.start_proc(cmd, ctx);
                                return;
                            }
                        } else {
                            let errs = self.problems.iter().filter(|p| p.is_error).count();
                            self.output.push_str(&format!("✖ Build failed ({errs} error(s)).\n"));
                            if !self.problems.is_empty() {
                                self.bottom_tab = BottomTab::Problems;
                            }
                        }
                        self.pending_run = None;
                    }
                    Phase::Running => {
                        self.output.push_str(&format!("\n[Process finished with exit code {code_s}]\n"));
                    }
                    Phase::Idle => {}
                }
                self.phase = Phase::Idle;
                self.run_proc = None;
            }
        }
        if let Some(t) = &mut self.term {
            let s = t.drain();
            if !s.is_empty() {
                proc::append_capped(&mut self.term_buf, &proc::clean(&s), OUTPUT_CAP);
            }
            if !t.is_running() {
                self.term_buf.push_str("\n[shell exited — press Restart]\n");
                self.term = None;
            }
        }
    }

    // ------------------------------------------------------------ terminal

    fn ensure_terminal(&mut self, ctx: &egui::Context) {
        if self.term.is_none() {
            let dir = self.root.clone().unwrap_or_else(settings::exe_dir);
            self.start_terminal(&dir, ctx);
        }
    }

    fn ensure_terminal_at(&mut self, dir: &Path) {
        match &mut self.term {
            Some(t) => t.send(&cd_command(dir)),
            None => self.pending_term_dir = Some(dir.to_path_buf()),
        }
    }

    fn start_terminal(&mut self, dir: &Path, ctx: &egui::Context) {
        let mut cmd = shell_command();
        cmd.current_dir(dir);
        self.jdk.apply_env(&mut cmd);
        match Proc::spawn(cmd, ctx) {
            Ok(p) => {
                self.term = Some(p);
                self.focus_term_input = true;
            }
            Err(e) => self.term_buf.push_str(&format!("Could not start shell: {e}\n")),
        }
    }

    fn term_submit(&mut self) {
        let line = std::mem::take(&mut self.term_input);
        let trimmed = line.trim();
        if !trimmed.is_empty() && self.term_history.last().map(String::as_str) != Some(trimmed) {
            self.term_history.push(trimmed.to_string());
        }
        self.term_hist_pos = None;
        if matches!(trimmed, "cls" | "clear") {
            self.term_buf.clear();
            // still send so the shell prints a fresh prompt
        }
        if let Some(t) = &mut self.term {
            // the shell runs with echo off, so show the command after the prompt ourselves
            self.term_buf.push_str(&format!("{line}\n"));
            t.send(&format!("{line}\n"));
        }
        self.focus_term_input = true;
    }
}

fn shell_command() -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        // UTF-8 code page so Java output and file names render correctly.
        c.args(["/Q", "/K", "chcp 65001 >nul & echo Terminal ready. JDK tools are on PATH. & prompt $P$G$S"]);
        c
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut c = Command::new(shell);
        c.arg("-i");
        c.env("TERM", "dumb");
        c
    }
}

fn cd_command(dir: &Path) -> String {
    if cfg!(windows) {
        format!("cd /d \"{}\"\n", dir.display())
    } else {
        format!("cd '{}'\n", dir.display().to_string().replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::epaint::{ColorMode, Shape};
    use eframe::egui::text::{CCursor, CCursorRange};
    use eframe::egui::text_edit::TextEditState;
    use std::time::Instant;

    const ERROR_RED: Color32 = Color32::from_rgb(0xF1, 0x4C, 0x4C);

    /// Drives the real editor widget and the real language server, without a window.
    struct Harness {
        ctx: egui::Context,
        lsp: Lsp,
        tabs: Vec<Tab>,
        file: PathBuf,
        completion: Option<CompletionState>,
        index: ProjectIndex,
        diags: Vec<Diag>,
        pending: Option<PendingCompletion>,
        resolves: HashMap<u64, PendingResolve>,
        session: Option<SnippetSession>,
        quick_menu: Option<QuickMenu>,
        pending_quick: Option<PendingQuick>,
        status: String,
        time: f64,
    }

    impl Harness {
        fn run_action(&mut self, action: &Value, resolved: bool) {
            match self.lsp.run_action(action, resolved) {
                ActionStep::Apply { ops, then } => {
                    self.apply(&ops);
                    if let Some(c) = then {
                        if let ActionStep::Apply { ops, .. } = self.lsp.run_command(&c) {
                            self.apply(&ops);
                        }
                    }
                }
                ActionStep::Waiting => {}
                ActionStep::Unsupported => panic!("unsupported action: {action}"),
            }
        }

        fn apply(&mut self, ops: &[FileOp]) -> bool {
            let notes = apply_file_ops(&self.ctx, &mut self.tabs, Some(&self.lsp), ops).expect("edit failed");
            if !notes.is_empty() {
                self.lsp.files_changed(&notes);
            }
            self.completion = None;
            self.quick_menu = None;
            self.session = None;
            true
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            for event in self.lsp.poll() {
                match event {
                    lsp::Event::Diagnostics { path, items } => {
                        if lsp::path_key(&path) == lsp::path_key(&self.file) {
                            self.diags = items;
                        }
                    }
                    lsp::Event::Completion { id, items } => {
                        if self.pending.as_ref().is_some_and(|p| p.id == id) {
                            let p = self.pending.take().unwrap();
                            IdeApp::show_completion(&self.ctx, &self.tabs, 0, &mut self.completion, p, items);
                        }
                    }
                    lsp::Event::Resolved { id, edits } => {
                        if let Some(p) = self.resolves.remove(&id) {
                            IdeApp::apply_imports(
                                &self.ctx,
                                &mut self.tabs,
                                &mut self.completion,
                                &mut self.session,
                                &mut self.status,
                                p,
                                &edits,
                            );
                        }
                    }
                    lsp::Event::CodeActions { id, items } => {
                        if self.pending_quick.as_ref().is_some_and(|p| p.id == id) {
                            let p = self.pending_quick.take().unwrap();
                            if self.tabs[0].text == p.text && !items.is_empty() {
                                self.quick_menu = Some(QuickMenu { tab_id: p.tab, items, selected: 0, pos: egui::Pos2::ZERO });
                            }
                        }
                    }
                    lsp::Event::ActionResolved { action } => self.run_action(&action, true),
                    lsp::Event::ApplyEdit { request, ops } => {
                        let applied = self.apply(&ops);
                        self.lsp.reply_apply_edit(&request, applied);
                    }
                    lsp::Event::CommandFailed { message } => panic!("command failed: {message}"),
                }
            }
            self.time += 0.02;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))),
                time: Some(self.time),
                focused: true,
                events,
                ..Default::default()
            };
            let ready = self.lsp.is_ready();
            let (tabs, completion, index, diags) = (&mut self.tabs, &mut self.completion, &self.index, &self.diags);
            let (session, quick_menu) = (&mut self.session, &mut self.quick_menu);
            let mut result = None;
            let ctx = self.ctx.clone();
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    result = Some(editor::editor_ui(
                        ui,
                        &mut tabs[0],
                        EditorCtx {
                            dark: true,
                            font_size: 14.0,
                            index,
                            completion: &mut *completion,
                            autocomplete: true,
                            recent: &[],
                            lsp_ready: ready,
                            diags,
                            session: &mut *session,
                            quick_menu: &mut *quick_menu,
                        },
                    ));
                });
            });
            out.textures_delta.clear(); // no GPU here
            let res = result.unwrap();
            if res.cancel_request {
                if let Some(p) = self.pending.take() {
                    self.lsp.cancel(p.id);
                }
            }
            if let Some(a) = res.accepted {
                let id = self.lsp.resolve(&a.raw);
                self.resolves.insert(id, PendingResolve { tab: self.tabs[0].id, before: a.before });
            }
            self.lsp.sync(std::iter::once((self.file.as_path(), self.tabs[0].text.as_str())));
            if let Some(req) = res.request {
                let pos = lsp::pos_of_byte(&self.tabs[0].text, req.byte);
                if let Some(old) = self.pending.take() {
                    self.lsp.cancel(old.id);
                }
                if let Some(id) = self.lsp.request_completion(&self.file, pos, req.trigger) {
                    self.pending = Some(PendingCompletion { id, tab: self.tabs[0].id, start: req.start, manual: req.manual });
                }
            }
            if let Some((a, b)) = res.quick_request {
                let (range, found) = quick_fix_params(&self.tabs[0].text, &self.diags, a, b);
                let id = self.lsp.request_code_actions(&self.file, range, found).expect("server not ready");
                self.pending_quick = Some(PendingQuick { id, tab: self.tabs[0].id, text: self.tabs[0].text.clone() });
            }
            if let Some(action) = res.quick_choice {
                self.run_action(&action, false);
            }
            out
        }

        fn ctrl_period(&mut self) {
            self.frame(vec![egui::Event::Key {
                key: Key::Period,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::COMMAND,
            }]);
            self.frame(Vec::new());
        }

        fn wait(&mut self, secs: u64, mut done: impl FnMut(&Harness, &egui::FullOutput) -> bool) -> bool {
            let end = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < end {
                let out = self.frame(Vec::new());
                if done(self, &out) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            false
        }

        fn place_cursor(&mut self, byte: usize) {
            let id = self.tabs[0].id;
            let ci = editor::char_index(&self.tabs[0].text, byte);
            self.ctx.memory_mut(|m| m.request_focus(id));
            self.frame(Vec::new());
            let mut st = TextEditState::load(&self.ctx, id).unwrap_or_default();
            st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(ci))));
            st.store(&self.ctx, id);
            self.frame(Vec::new());
        }

        fn type_text(&mut self, s: &str) {
            for c in s.chars() {
                self.frame(vec![egui::Event::Text(c.to_string())]);
                self.frame(Vec::new());
            }
        }

        fn key(&mut self, key: Key) {
            self.frame(vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }]);
            self.frame(Vec::new());
        }

        /// Move the popup's selection to the first item matching, then accept it.
        fn accept(&mut self, wanted: impl Fn(&crate::complete::Item) -> bool) {
            let target = self.completion.as_ref().and_then(|c| c.items.iter().position(&wanted)).expect("item not in popup");
            for _ in 0..target {
                self.key(Key::ArrowDown);
            }
            let c = self.completion.as_ref().expect("popup closed");
            assert!(wanted(&c.items[c.selected]), "wrong item selected: {}", c.items[c.selected].label);
            self.key(Key::Enter);
        }
    }

    fn painted_error(out: &egui::FullOutput) -> (bool, bool) {
        let (mut underline, mut marker) = (false, false);
        for s in &out.shapes {
            match &s.shape {
                Shape::Path(p) if p.stroke.color == ColorMode::Solid(ERROR_RED) => underline = true,
                Shape::Circle(c) if c.fill == ERROR_RED => marker = true,
                _ => {}
            }
        }
        (underline, marker)
    }

    /// `JAVAIDE_JDTLS=<server folder> cargo test -- --ignored real_server_through`
    #[test]
    #[ignore]
    fn real_server_through_the_editor() {
        let server = lsp::find_server().expect("set JAVAIDE_JDTLS to the server folder");
        let jdk = Jdk::detect(None);
        let root = std::env::temp_dir().join(format!("javaide-editor-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src").join("Main.java");
        let start = "public class Main {\n    public static void main(String[] args) {\n        \n    }\n}\n";
        std::fs::write(&file, start).unwrap();

        let ctx = egui::Context::default();
        let lsp = Lsp::start(&server, &jdk, &root, false, || {}).unwrap();
        let id = Id::new("test-tab");
        let mut h = Harness {
            ctx,
            lsp,
            tabs: vec![Tab::new(Some(file.clone()), start.to_string(), id)],
            file: file.clone(),
            completion: None,
            index: ProjectIndex::default(),
            diags: Vec::new(),
            pending: None,
            resolves: HashMap::new(),
            session: None,
            quick_menu: None,
            pending_quick: None,
            status: String::new(),
            time: 0.0,
        };
        assert!(h.wait(150, |h, _| h.lsp.is_ready()), "server not ready: {:?}", h.lsp.status());
        h.wait(4, |_, _| false); // let the first project build settle

        // 1. Typing a class name offers it, and accepting it adds the import.
        let line = start.find("        \n").unwrap() + 8;
        h.place_cursor(line);
        h.type_text("ArrayLi");
        let is_array_list = |i: &crate::complete::Item| i.label.starts_with("ArrayList") && i.detail.contains("java.util");
        assert!(
            h.wait(45, |h, _| h.completion.as_ref().is_some_and(|c| c.items.iter().any(is_array_list))),
            "ArrayList was not suggested"
        );
        h.accept(is_array_list);
        assert!(
            h.wait(30, |h, _| h.tabs[0].text.contains("import java.util.ArrayList;")),
            "no import added:\n{}",
            h.tabs[0].text
        );
        let text = h.tabs[0].text.clone();
        let cur = editor::cursor_of(&h.ctx, id).unwrap();
        let before_cursor = &text[..editor::byte_index(&text, cur)];
        assert!(before_cursor.trim_end_matches('<').ends_with("ArrayList"), "cursor moved off the word:\n{before_cursor}");
        assert_eq!(text.matches("ArrayList").count(), 2, "{text}");

        // 2. The unfinished statement is reported live, and drawn in the editor.
        let mut painted = (false, false);
        assert!(
            h.wait(45, |h, out| {
                painted = painted_error(out);
                h.diags.iter().any(|d| d.severity == 1) && painted == (true, true)
            }),
            "no live error drawn: {painted:?} {:?}",
            h.diags
        );

        // 3. Fixing the code clears the error without saving.
        let fixed = "import java.util.ArrayList;\n\npublic class Main {\n    public static void main(String[] args) {\n        ArrayList<String> xs = new ArrayList<>();\n        \n    }\n}\n";
        h.tabs[0].text = fixed.to_string();
        h.completion = None;
        assert!(h.wait(45, |h, _| !h.diags.iter().any(|d| d.severity == 1)), "error not cleared: {:?}", h.diags);

        // 4. Members are offered after a dot.
        let blank = fixed.rfind("        \n").unwrap() + 8;
        h.place_cursor(blank);
        h.type_text("System.out.pri");
        let is_println = |i: &crate::complete::Item| i.label.starts_with("println");
        assert!(
            h.wait(45, |h, _| h.completion.as_ref().is_some_and(|c| c.items.iter().any(is_println))),
            "println was not suggested"
        );
        h.accept(is_println);
        assert!(h.tabs[0].text.contains("System.out.println("), "{}", h.tabs[0].text);

        // 5. Tab walks through a method's parameters.
        let with_string = "import java.util.ArrayList;\n\npublic class Main {\n    public static void main(String[] args) {\n        ArrayList<String> xs = new ArrayList<>();\n        String word = \"hello\";\n        \n    }\n}\n";
        h.session = None;
        h.completion = None;
        h.tabs[0].text = with_string.to_string();
        assert!(h.wait(45, |h, _| !h.diags.iter().any(|d| d.severity == 1)), "error not cleared: {:?}", h.diags);
        h.place_cursor(with_string.rfind("        \n").unwrap() + 8);
        h.type_text("word.subs");
        let two_args = |i: &crate::complete::Item| i.label.starts_with("substring(int") && i.label.contains(',');
        assert!(
            h.wait(45, |h, _| h.completion.as_ref().is_some_and(|c| c.items.iter().any(two_args))),
            "substring(begin, end) was not suggested"
        );
        h.accept(two_args);
        h.frame(Vec::new());
        assert!(h.session.is_some(), "no parameter fields: {}", h.tabs[0].text);
        h.type_text("1");
        h.key(Key::Tab);
        h.type_text("3");
        h.key(Key::Tab);
        assert!(h.tabs[0].text.contains("word.substring(1, 3)"), "{}", h.tabs[0].text);
        assert!(h.session.is_none(), "the last Tab should end the fields");
        h.type_text(";");
        assert!(h.tabs[0].text.contains("word.substring(1, 3);"), "{}", h.tabs[0].text);

        // 6. Ctrl+. on an unknown type offers the import and applies it.
        let needs_import = "public class Main {\n    public static void main(String[] args) {\n        List<String> names = null;\n    }\n}\n";
        h.session = None;
        h.completion = None;
        h.tabs[0].text = needs_import.to_string();
        assert!(
            h.wait(45, |h, _| h.diags.iter().any(|d| d.severity == 1 && d.message.contains("List"))),
            "no error for the missing import: {:?}",
            h.diags
        );
        h.place_cursor(needs_import.find("List<").unwrap() + 2);
        h.ctrl_period();
        let is_import = |a: &lsp::CodeAction| a.title.contains("Import") && a.title.contains("java.util");
        assert!(
            h.wait(30, |h, _| h.quick_menu.as_ref().is_some_and(|m| m.items.iter().any(is_import))),
            "no import quick fix: {:?}",
            h.quick_menu.as_ref().map(|m| m.items.iter().map(|a| a.title.clone()).collect::<Vec<_>>())
        );
        let target = h.quick_menu.as_ref().unwrap().items.iter().position(is_import).unwrap();
        for _ in 0..target {
            h.key(Key::ArrowDown);
        }
        h.key(Key::Enter);
        assert!(
            h.wait(30, |h, _| h.tabs[0].text.contains("import java.util.List;")),
            "the quick fix did not add the import:\n{}",
            h.tabs[0].text
        );
        assert!(h.wait(45, |h, _| !h.diags.iter().any(|d| d.severity == 1)), "error not cleared: {:?}", h.diags);

        h.lsp.stop(true);
        let _ = std::fs::remove_dir_all(&root);
    }
}
