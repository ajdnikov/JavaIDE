//! Panels, menus and dialogs of the IDE window.

use super::*;

impl eframe::App for IdeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);
        self.poll_processes(&ctx);
        if let Some(dir) = self.pending_term_dir.take() {
            self.term = None;
            self.start_terminal(&dir, &ctx);
        }
        self.shortcuts(&ctx);
        self.close_request(&ctx);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &ctx));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, &ctx));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.settings.show_sidebar {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(250.0)
                .size_range(140.0..=700.0)
                .show(ui, |ui| self.sidebar(ui));
        }
        if self.settings.show_bottom {
            egui::Panel::bottom("bottom")
                .resizable(true)
                .default_size(240.0)
                .size_range(90.0..=900.0)
                .show(ui, |ui| self.bottom_panel(ui, &ctx));
        }
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
        self.dialogs(&ctx);

        if self.phase != Phase::Idle || self.term.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn on_exit(&mut self) {
        self.settings.save();
        self.stop();
        self.term = None;
    }
}

impl IdeApp {
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let sc = KeyboardShortcut::new;
        let cmd = Modifiers::COMMAND;
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let fire = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        if fire(sc(cs, Key::S)) {
            self.save_all();
        } else if fire(sc(cmd, Key::S)) {
            self.save_tab(self.active);
        }
        if fire(sc(cmd, Key::O)) {
            self.pick_folder();
        }
        if fire(sc(cmd, Key::N)) {
            self.new_file_prompt();
        }
        if fire(sc(cmd, Key::W)) {
            self.close_tab(self.active, false);
        }
        if fire(sc(Modifiers::SHIFT, Key::F5)) {
            self.stop();
        } else if fire(sc(Modifiers::NONE, Key::F5)) {
            self.build(ctx, true);
        }
        if fire(sc(cmd, Key::B)) {
            self.build(ctx, false);
        }
        if fire(sc(cmd, Key::Plus)) || fire(sc(cmd, Key::Equals)) {
            self.settings.font_size = (self.settings.font_size + 1.0).min(40.0);
        }
        if fire(sc(cmd, Key::Minus)) {
            self.settings.font_size = (self.settings.font_size - 1.0).max(8.0);
        }
        if fire(sc(cmd, Key::Backtick)) {
            if self.settings.show_bottom && self.bottom_tab == BottomTab::Terminal {
                self.settings.show_bottom = false;
            } else {
                self.settings.show_bottom = true;
                self.bottom_tab = BottomTab::Terminal;
                self.focus_term_input = true;
            }
        }
        if fire(sc(cmd, Key::Tab)) && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    fn new_file_prompt(&mut self) {
        match &self.root {
            Some(r) => {
                let dir = self.tree.target_dir(r);
                self.dialog = Some(Dialog::NewFile { dir, name: "Main.java".into() });
            }
            None => self.new_untitled(),
        }
    }

    fn open_settings(&mut self) {
        let jdk = self.settings.jdk_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        self.dialog = Some(Dialog::Settings { jdk });
    }

    fn close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.tabs.iter().any(|t| t.dirty) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::Exit);
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("Open Folder…").shortcut_text("Ctrl+O")).clicked() {
                    self.pick_folder();
                }
                if ui.button("Open File…").clicked() {
                    self.pick_file();
                }
                ui.separator();
                if ui.add(egui::Button::new("New File…").shortcut_text("Ctrl+N")).clicked() {
                    self.new_file_prompt();
                }
                if ui.button("New Scratch File").clicked() {
                    self.new_untitled();
                }
                ui.separator();
                if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                    self.save_tab(self.active);
                }
                if ui.add(egui::Button::new("Save All").shortcut_text("Ctrl+Shift+S")).clicked() {
                    self.save_all();
                }
                if ui.add(egui::Button::new("Close Tab").shortcut_text("Ctrl+W")).clicked() {
                    self.close_tab(self.active, false);
                }
                if self.root.is_some() && ui.button("Close Folder").clicked() {
                    self.root = None;
                    self.tree.reset();
                    self.index = ProjectIndex::default();
                    self.settings.last_folder = None;
                    self.settings.save();
                }
                ui.separator();
                if ui.button("Settings…").clicked() {
                    self.open_settings();
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Run", |ui| {
                if ui.add(egui::Button::new("▶ Run").shortcut_text("F5")).clicked() {
                    self.build(ctx, true);
                }
                if ui.add(egui::Button::new("🔨 Build").shortcut_text("Ctrl+B")).clicked() {
                    self.build(ctx, false);
                }
                let stop = egui::Button::new("■ Stop").shortcut_text("Shift+F5");
                if ui.add_enabled(self.phase != Phase::Idle, stop).clicked() {
                    self.stop();
                }
                ui.separator();
                if ui.add_enabled(self.root.is_some(), egui::Button::new("Clean output folder")).clicked() {
                    if let Some(r) = &self.root {
                        let out = java::out_dir(r);
                        let _ = std::fs::remove_dir_all(&out);
                        self.tree.refresh();
                        self.status = format!("Removed {}", out.display());
                    }
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.settings.dark_mode, "Dark mode");
                ui.checkbox(&mut self.settings.show_sidebar, "File explorer");
                ui.checkbox(&mut self.settings.show_bottom, "Bottom panel");
                ui.checkbox(&mut self.settings.autocomplete, "Auto-complete while typing");
                ui.separator();
                if ui.add(egui::Button::new("Zoom in").shortcut_text("Ctrl+=")).clicked() {
                    self.settings.font_size = (self.settings.font_size + 1.0).min(40.0);
                }
                if ui.add(egui::Button::new("Zoom out").shortcut_text("Ctrl+-")).clicked() {
                    self.settings.font_size = (self.settings.font_size - 1.0).max(8.0);
                }
                if ui.add(egui::Button::new("Terminal").shortcut_text("Ctrl+`")).clicked() {
                    self.settings.show_bottom = true;
                    self.bottom_tab = BottomTab::Terminal;
                    self.focus_term_input = true;
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts & about").clicked() {
                    self.dialog = Some(Dialog::About);
                }
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let idle = self.phase == Phase::Idle;
            let run = egui::Button::new(RichText::new("▶ Run").color(Color32::from_rgb(0x5F, 0xB8, 0x65)));
            if ui.add_enabled(idle, run).on_hover_text("Build and run (F5)").clicked() {
                self.build(ctx, true);
            }
            if ui.add_enabled(idle, egui::Button::new("🔨 Build")).on_hover_text("Compile project (Ctrl+B)").clicked() {
                self.build(ctx, false);
            }
            let stop = egui::Button::new(RichText::new("■ Stop").color(Color32::from_rgb(0xE0, 0x6C, 0x6C)));
            if ui.add_enabled(!idle, stop).on_hover_text("Stop (Shift+F5)").clicked() {
                self.stop();
            }
            ui.separator();
            ui.label("Args:");
            ui.add(
                egui::TextEdit::singleline(&mut self.settings.run_args)
                    .desired_width(200.0)
                    .hint_text("program arguments"),
            );
            ui.separator();
            if ui.button("📂 Open Folder").clicked() {
                self.pick_folder();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.settings.dark_mode { "☀ Light" } else { "🌙 Dark" };
                if ui.button(label).on_hover_text("Toggle dark mode").clicked() {
                    self.settings.dark_mode = !self.settings.dark_mode;
                    self.settings.save();
                }
                if ui.button("⚙").on_hover_text("Settings").clicked() {
                    self.open_settings();
                }
                match self.phase {
                    Phase::Building => {
                        ui.spinner();
                        ui.label("Building…");
                    }
                    Phase::Running => {
                        ui.spinner();
                        ui.label("Running…");
                    }
                    Phase::Idle => {}
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let ok = self.jdk.major >= java::REQUIRED_MAJOR && self.jdk.has_javac;
            let jdk_col = if ok { ui.visuals().text_color() } else { Color32::from_rgb(0xE5, 0xA5, 0x3C) };
            let home = self.jdk.home.as_ref().map(|h| h.display().to_string()).unwrap_or_else(|| "from PATH".into());
            ui.label(RichText::new(format!("☕ Java {} · {}", self.jdk.major, self.jdk.origin)).color(jdk_col))
                .on_hover_text(format!("{}\n{home}", self.jdk.version));
            ui.separator();
            ui.label(&self.status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label("UTF-8");
                if let Some(t) = self.active_tab() {
                    ui.separator();
                    ui.label(if t.crlf { "CRLF" } else { "LF" });
                }
                if let Some((l, c)) = self.line_col {
                    ui.separator();
                    ui.label(format!("Ln {l}, Col {c}"));
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("EXPLORER");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                    self.tree.refresh();
                    if let Some(r) = &self.root {
                        self.index.rebuild(r);
                    }
                }
                if let Some(r) = self.root.clone() {
                    if ui.small_button("🗀").on_hover_text("New folder").clicked() {
                        let dir = self.tree.target_dir(&r);
                        self.dialog = Some(Dialog::NewFolder { dir, name: String::new() });
                    }
                    if ui.small_button("🗋").on_hover_text("New file").clicked() {
                        let dir = self.tree.target_dir(&r);
                        self.dialog = Some(Dialog::NewFile { dir, name: "Main.java".into() });
                    }
                }
            });
        });
        ui.separator();
        let Some(root) = self.root.clone() else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label("No folder opened");
                ui.add_space(8.0);
                if ui.button("📂 Open Folder…").clicked() {
                    self.pick_folder();
                }
                if ui.button("📄 Open File…").clicked() {
                    self.pick_file();
                }
            });
            return;
        };
        let mut actions = Vec::new();
        let open_file = self.active_tab().and_then(|t| t.path.clone());
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            self.tree.ui(ui, &root, open_file.as_deref(), &mut actions);
        });
        for a in actions {
            self.handle_tree_action(a);
        }
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        if self.tabs.is_empty() {
            self.line_col = None;
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Open a file from the explorer, or press Ctrl+N to create one.\n\nF5 runs the current program.")
                        .size(16.0)
                        .weak(),
                );
            });
            return;
        }
        let mut close: Option<usize> = None;
        egui::ScrollArea::horizontal().id_salt("tabstrip").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, t) in self.tabs.iter().enumerate() {
                    let title = if t.dirty { format!("{} *", t.title()) } else { t.title() };
                    let r = ui.add(egui::Button::selectable(i == self.active, title));
                    let r = match &t.path {
                        Some(p) => r.on_hover_text(p.display().to_string()),
                        None => r,
                    };
                    if r.clicked() {
                        self.active = i;
                    }
                    if r.middle_clicked() {
                        close = Some(i);
                    }
                    if ui.small_button("×").on_hover_text("Close (Ctrl+W)").clicked() {
                        close = Some(i);
                    }
                    ui.add_space(6.0);
                }
            });
        });
        if let Some(i) = close {
            self.close_tab(i, false);
            if self.tabs.is_empty() {
                return;
            }
        }
        ui.separator();

        if let Some(id) = self.focus_editor.take() {
            ui.ctx().memory_mut(|m| m.request_focus(id));
        }
        let active = self.active.min(self.tabs.len() - 1);
        let res = editor::editor_ui(
            ui,
            &mut self.tabs[active],
            EditorCtx {
                dark: self.settings.dark_mode,
                font_size: self.settings.font_size,
                index: &self.index,
                completion: &mut self.completion,
                autocomplete: self.settings.autocomplete,
                recent: &self.settings.recent,
            },
        );
        self.line_col = res.line_col;
        if let Some(label) = res.used {
            self.settings.note_used(&label);
        }
        if res.changed {
            let tab = &self.tabs[active];
            if let Some(p) = tab.path.clone().filter(|p| p.extension().is_some_and(|e| e == "java")) {
                let text = tab.text.clone();
                self.index.update_file(&p, &text);
            }
            ui.ctx().request_repaint();
        }
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.bottom_tab, BottomTab::Output, "Output");
            let n = self.problems.len();
            let label = if n > 0 { format!("Problems ({n})") } else { "Problems".into() };
            ui.selectable_value(&mut self.bottom_tab, BottomTab::Problems, label);
            if ui.selectable_value(&mut self.bottom_tab, BottomTab::Terminal, "Terminal").clicked() {
                self.focus_term_input = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✖").on_hover_text("Hide panel").clicked() {
                    self.settings.show_bottom = false;
                }
                match self.bottom_tab {
                    BottomTab::Output => {
                        if ui.small_button("Clear").clicked() {
                            self.output.clear();
                        }
                    }
                    BottomTab::Terminal => {
                        if ui.small_button("Clear").clicked() {
                            self.term_buf.clear();
                        }
                        if ui.small_button("Restart").on_hover_text("Kill and restart the shell").clicked() {
                            self.term = None;
                            self.term_buf.push_str("\n[restarting shell]\n");
                            let dir = self.root.clone().unwrap_or_else(settings::exe_dir);
                            self.start_terminal(&dir, ctx);
                        }
                    }
                    BottomTab::Problems => {}
                }
            });
        });
        ui.separator();
        let font = egui::FontId::monospace((self.settings.font_size - 1.0).max(9.0));
        match self.bottom_tab {
            BottomTab::Output => self.output_tab(ui, font),
            BottomTab::Problems => self.problems_tab(ui),
            BottomTab::Terminal => self.terminal_tab(ui, ctx, font),
        }
        // Claim any leftover height so the resizable panel never shrinks to its content.
        let rest = ui.available_size();
        if rest.y > 0.0 {
            ui.allocate_exact_size(rest, egui::Sense::hover());
        }
    }

    fn output_tab(&mut self, ui: &mut egui::Ui, font: egui::FontId) {
        let running = self.phase == Phase::Running;
        let input_h = if running { 30.0 } else { 0.0 };
        egui::ScrollArea::vertical()
            .id_salt("output")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height(ui.available_height() - input_h)
            .min_scrolled_height((ui.available_height() - input_h).max(20.0))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.output.as_str())
                        .font(font.clone())
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY),
                );
            });
        if running {
            ui.horizontal(|ui| {
                ui.label("stdin ›");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.stdin_line)
                        .id(Id::new("javaide-stdin"))
                        .font(font)
                        .desired_width(f32::INFINITY)
                        .hint_text("input for the program — press Enter to send"),
                );
                if self.focus_stdin {
                    r.request_focus();
                    self.focus_stdin = false;
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let line = std::mem::take(&mut self.stdin_line);
                    self.output.push_str(&format!("{line}\n"));
                    if let Some(p) = &mut self.run_proc {
                        p.send(&format!("{line}\n"));
                    }
                    r.request_focus();
                }
            });
        }
    }

    fn problems_tab(&mut self, ui: &mut egui::Ui) {
        if self.problems.is_empty() {
            ui.weak("No problems.");
        }
        let mut goto: Option<(PathBuf, usize)> = None;
        egui::ScrollArea::vertical().id_salt("problems").auto_shrink([false, false]).show(ui, |ui| {
            for p in &self.problems {
                let icon = if p.is_error {
                    RichText::new("✖").color(Color32::from_rgb(0xE0, 0x6C, 0x6C))
                } else {
                    RichText::new("⚠").color(Color32::from_rgb(0xE5, 0xA5, 0x3C))
                };
                ui.horizontal(|ui| {
                    ui.label(icon);
                    let name = p.file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let b = egui::Button::selectable(false, format!("{name}:{}  {}", p.line, p.message)).frame_when_inactive(false);
                    if ui.add(b).on_hover_text(p.file.display().to_string()).clicked() {
                        goto = Some((p.file.clone(), p.line));
                    }
                });
            }
        });
        if let Some((f, line)) = goto {
            self.open_file(&f);
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.goto_line = Some(line);
            }
        }
    }

    fn terminal_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, font: egui::FontId) {
        self.ensure_terminal(ctx);
        egui::ScrollArea::vertical()
            .id_salt("terminal")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 30.0)
            .min_scrolled_height((ui.available_height() - 30.0).max(20.0))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.term_buf.as_str())
                        .font(font.clone())
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY),
                );
            });
        ui.horizontal(|ui| {
            ui.label(RichText::new("›").monospace().strong());
            let id = Id::new("javaide-term-input");
            if ui.memory(|m| m.has_focus(id)) && !self.term_history.is_empty() {
                let (up, down) = ui.input_mut(|i| {
                    (i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::ArrowDown))
                });
                if up || down {
                    let n = self.term_history.len();
                    let pos = match (self.term_hist_pos, up) {
                        (None, true) => Some(n - 1),
                        (None, false) => None,
                        (Some(p), true) => Some(p.saturating_sub(1)),
                        (Some(p), false) if p + 1 < n => Some(p + 1),
                        (Some(_), false) => None,
                    };
                    self.term_hist_pos = pos;
                    self.term_input = pos.map(|p| self.term_history[p].clone()).unwrap_or_default();
                }
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.term_input)
                    .id(id)
                    .font(font)
                    .desired_width(f32::INFINITY)
                    .hint_text("type a command (java, javac, dir, git …) and press Enter"),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.term_submit();
            }
            if self.focus_term_input {
                r.request_focus();
                self.focus_term_input = false;
            }
        });
    }

    // ------------------------------------------------------------ dialogs

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let mut keep = true;
        let mut error: Option<String> = None;
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));

        egui::Modal::new(Id::new("javaide-dialog")).show(ctx, |ui| {
            ui.set_min_width(400.0);
            self.dialog_body(ui, &mut dialog);
            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                self.dialog_buttons(ui, ctx, &dialog, enter, &mut keep, &mut error);
                let info_only = matches!(dialog, Dialog::About);
                if !info_only && ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
            if let Some(e) = &error {
                ui.colored_label(Color32::from_rgb(0xE0, 0x6C, 0x6C), e);
            }
        });
        if esc && error.is_none() {
            keep = false;
        }
        if error.is_some() {
            keep = true;
        }
        if keep && self.dialog.is_none() {
            self.dialog = Some(dialog);
        }
        if let Some(e) = error {
            self.status = e;
        }
    }

    fn dialog_body(&mut self, ui: &mut egui::Ui, dialog: &mut Dialog) {
        match dialog {
            Dialog::NewFile { dir, name } | Dialog::NewFolder { dir, name } => {
                ui.heading("Create");
                ui.label(format!("in {}", dir.display()));
                ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)).request_focus();
            }
            Dialog::Rename { path, name } => {
                ui.heading("Rename");
                ui.label(path.display().to_string());
                ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)).request_focus();
            }
            Dialog::Delete { path } => {
                ui.heading("Delete");
                ui.label(format!("Move {} to the recycle bin?", path.display()));
            }
            Dialog::CloseTab { idx } => {
                ui.heading("Unsaved changes");
                let name = self.tabs.get(*idx).map(Tab::title).unwrap_or_default();
                ui.label(format!("Save changes to {name} before closing?"));
            }
            Dialog::Exit => {
                ui.heading("Unsaved changes");
                ui.label("Some files have unsaved changes. Save them before exiting?");
            }
            Dialog::Settings { jdk } => {
                ui.heading("Settings");
                ui.add_space(6.0);
                ui.label("JDK folder (empty = auto-detect; a `jdk` folder next to the exe is preferred):");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(jdk).desired_width(290.0).hint_text("auto"));
                    if ui.button("Browse…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            *jdk = p.display().to_string();
                        }
                    }
                });
                ui.label(RichText::new(format!("Current: {} — {}", self.jdk.version, self.jdk.origin)).small().weak());
                ui.add_space(6.0);
                ui.add(egui::Slider::new(&mut self.settings.font_size, 8.0..=40.0).text("Editor font size"));
                ui.checkbox(&mut self.settings.dark_mode, "Dark mode");
                ui.checkbox(&mut self.settings.autocomplete, "Auto-complete while typing (Ctrl+Space always works)");
            }
            Dialog::About => {
                ui.heading("Java IDE");
                ui.label("A small, portable Java IDE written in Rust.");
                ui.add_space(6.0);
                egui::Grid::new("keys").striped(true).show(ui, |ui| {
                    for (k, d) in [
                        ("F5", "Build & run"),
                        ("Shift+F5", "Stop"),
                        ("Ctrl+B", "Build"),
                        ("Ctrl+Space", "Code completion"),
                        ("Tab / Shift+Tab", "Indent / dedent"),
                        ("Ctrl+/", "Toggle line comment"),
                        ("Ctrl+S / Ctrl+Shift+S", "Save / save all"),
                        ("Ctrl+N / Ctrl+O", "New file / open folder"),
                        ("Ctrl+W / Ctrl+Tab", "Close / next tab"),
                        ("Ctrl+`", "Terminal"),
                        ("Ctrl+= / Ctrl+-", "Font size"),
                    ] {
                        ui.monospace(k);
                        ui.label(d);
                        ui.end_row();
                    }
                });
                ui.add_space(6.0);
                ui.label(format!("JDK: {}", self.jdk.version));
            }
        }
    }

    fn dialog_buttons(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        dialog: &Dialog,
        enter: bool,
        keep: &mut bool,
        error: &mut Option<String>,
    ) {
        match dialog {
            Dialog::NewFile { dir, name } => {
                if ui.button("Create").clicked() || enter {
                    match self.create_file(dir, name) {
                        Ok(()) => *keep = false,
                        Err(e) => *error = Some(e),
                    }
                }
            }
            Dialog::NewFolder { dir, name } => {
                if ui.button("Create").clicked() || enter {
                    let n = name.trim();
                    if n.is_empty() || n.contains(['/', '\\']) {
                        *error = Some("Enter a folder name (no slashes).".into());
                    } else {
                        let p = dir.join(n);
                        match std::fs::create_dir_all(&p) {
                            Ok(()) => {
                                self.tree.refresh();
                                self.tree.expand(dir);
                                self.tree.selected = Some(p);
                                *keep = false;
                            }
                            Err(e) => *error = Some(e.to_string()),
                        }
                    }
                }
            }
            Dialog::Rename { path, name } => {
                if ui.button("Rename").clicked() || enter {
                    let new = path.with_file_name(name.trim());
                    if name.trim().is_empty() || new.exists() {
                        *error = Some("That name is empty or already taken.".into());
                        return;
                    }
                    match std::fs::rename(path, &new) {
                        Ok(()) => {
                            for t in &mut self.tabs {
                                if let Some(tp) = &t.path {
                                    if let Ok(rest) = tp.strip_prefix(path) {
                                        t.path = Some(if rest.as_os_str().is_empty() { new.clone() } else { new.join(rest) });
                                    }
                                }
                            }
                            self.tree.refresh();
                            if let Some(r) = &self.root {
                                self.index.rebuild(r);
                            }
                            *keep = false;
                        }
                        Err(e) => *error = Some(e.to_string()),
                    }
                }
            }
            Dialog::Delete { path } => {
                if ui.button("Move to Recycle Bin").clicked() {
                    match trash::delete(path) {
                        Ok(()) => {
                            self.tabs.retain(|t| t.path.as_ref().is_none_or(|p| !p.starts_with(path)));
                            self.active = self.active.min(self.tabs.len().saturating_sub(1));
                            self.tree.refresh();
                            if let Some(r) = &self.root {
                                self.index.rebuild(r);
                            }
                            *keep = false;
                        }
                        Err(e) => *error = Some(format!("Could not delete: {e}")),
                    }
                }
            }
            Dialog::CloseTab { idx } => {
                let idx = *idx;
                if ui.button("Save").clicked() || enter {
                    if self.save_tab(idx) {
                        self.close_tab(idx, true);
                    }
                    *keep = false;
                }
                if ui.button("Don't Save").clicked() {
                    self.close_tab(idx, true);
                    *keep = false;
                }
            }
            Dialog::Exit => {
                if ui.button("Save All & Exit").clicked() || enter {
                    if self.save_all() {
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    *keep = false;
                }
                if ui.button("Exit Without Saving").clicked() {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    *keep = false;
                }
            }
            Dialog::Settings { jdk } => {
                if ui.button("Save").clicked() {
                    let p = jdk.trim();
                    self.settings.jdk_path = if p.is_empty() { None } else { Some(PathBuf::from(p)) };
                    self.jdk = Jdk::detect(self.settings.jdk_path.as_deref());
                    self.settings.save();
                    self.status = format!("Using {} ({})", self.jdk.version, self.jdk.origin);
                    self.term = None; // restarted lazily so PATH picks up the new JDK
                    *keep = false;
                }
            }
            Dialog::About => {
                if ui.button("OK").clicked() || enter {
                    *keep = false;
                }
            }
        }
    }
}
