mod ui;

use crate::complete::ProjectIndex;
use crate::editor::{self, CompletionState, EditorCtx, Tab};
use crate::filetree::{self, FileTree, TreeAction};
use crate::java::{self, Jdk, Problem};
use crate::proc::{self, Proc};
use crate::settings::{self, Settings};
use eframe::egui::{self, Color32, Id, Key, KeyboardShortcut, Modifiers, RichText};
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

pub struct IdeApp {
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
        if let Some(i) = self.tabs.iter().position(|t| t.path.as_deref() == Some(path)) {
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
                }
                self.tree.refresh();
                self.status = format!("Saved {}", path.display());
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
