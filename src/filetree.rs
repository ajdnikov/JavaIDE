//! Lazily loaded file-system tree for the sidebar.

use eframe::egui::{self, RichText};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub enum TreeAction {
    Open(PathBuf),
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf),
    Delete(PathBuf),
    Reveal(PathBuf),
    OpenTerminalHere(PathBuf),
}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

#[derive(Default)]
pub struct FileTree {
    expanded: HashSet<PathBuf>,
    cache: HashMap<PathBuf, (Instant, Vec<Entry>)>,
    pub selected: Option<PathBuf>,
}

impl FileTree {
    pub fn reset(&mut self) {
        self.expanded.clear();
        self.cache.clear();
        self.selected = None;
    }

    pub fn refresh(&mut self) {
        self.cache.clear();
    }

    pub fn expand(&mut self, p: &Path) {
        self.expanded.insert(p.to_path_buf());
    }

    /// The directory new files should go into: the selected dir, or the selected file's dir.
    pub fn target_dir(&self, root: &Path) -> PathBuf {
        match &self.selected {
            Some(p) if p.is_dir() => p.clone(),
            Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_else(|| root.to_path_buf()),
            None => root.to_path_buf(),
        }
    }

    fn list(&mut self, dir: &Path) -> Vec<Entry> {
        if let Some((t, v)) = self.cache.get(dir) {
            if t.elapsed() < Duration::from_secs(2) {
                return v.clone();
            }
        }
        let mut v: Vec<Entry> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| Entry {
                        path: e.path(),
                        name: e.file_name().to_string_lossy().to_string(),
                        is_dir: e.file_type().map(|t| t.is_dir()).unwrap_or(false),
                    })
                    .filter(|e| e.name != ".git")
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        self.cache.insert(dir.to_path_buf(), (Instant::now(), v.clone()));
        v
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, root: &Path, open_file: Option<&Path>, actions: &mut Vec<TreeAction>) {
        let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.display().to_string());
        let r = ui.add(egui::Label::new(RichText::new(format!("🗁 {name}")).strong()).sense(egui::Sense::click()));
        r.clone().on_hover_text(root.display().to_string());
        if r.clicked() {
            self.selected = Some(root.to_path_buf());
        }
        r.context_menu(|ui| dir_menu(ui, root, actions));
        self.expanded.insert(root.to_path_buf());
        self.dir_ui(ui, root, 1, open_file, actions);
    }

    fn dir_ui(&mut self, ui: &mut egui::Ui, dir: &Path, depth: usize, open_file: Option<&Path>, actions: &mut Vec<TreeAction>) {
        for e in self.list(dir) {
            let open = self.expanded.contains(&e.path);
            let label = if e.is_dir {
                format!("{} {}", if open { "⏷ 🗁" } else { "⏵ 🗀" }, e.name)
            } else {
                let icon = match e.path.extension().and_then(|x| x.to_str()) {
                    Some("java") => "☕",
                    Some("jar") => "📦",
                    Some("class") => "⚙",
                    Some("md" | "txt") => "📝",
                    _ => "🗋",
                };
                format!("    {icon} {}", e.name)
            };
            let selected = self.selected.as_deref() == Some(e.path.as_path()) || open_file == Some(e.path.as_path());
            let r = ui
                .horizontal(|ui| {
                    ui.add_space(depth as f32 * 12.0);
                    ui.add(egui::Button::selectable(selected, label).frame_when_inactive(false))
                })
                .inner;
            if r.clicked() {
                self.selected = Some(e.path.clone());
                if e.is_dir {
                    if open {
                        self.expanded.remove(&e.path);
                    } else {
                        self.expanded.insert(e.path.clone());
                    }
                } else {
                    actions.push(TreeAction::Open(e.path.clone()));
                }
            }
            r.context_menu(|ui| {
                if e.is_dir {
                    dir_menu(ui, &e.path, actions);
                } else {
                    if ui.button("Open").clicked() {
                        actions.push(TreeAction::Open(e.path.clone()));
                        ui.close();
                    }
                    if let Some(parent) = e.path.parent() {
                        if ui.button("New File…").clicked() {
                            actions.push(TreeAction::NewFile(parent.to_path_buf()));
                            ui.close();
                        }
                    }
                }
                ui.separator();
                if ui.button("Rename…").clicked() {
                    actions.push(TreeAction::Rename(e.path.clone()));
                    ui.close();
                }
                if ui.button("Delete…").clicked() {
                    actions.push(TreeAction::Delete(e.path.clone()));
                    ui.close();
                }
                if ui.button("Reveal in File Explorer").clicked() {
                    actions.push(TreeAction::Reveal(e.path.clone()));
                    ui.close();
                }
            });
            if e.is_dir && open {
                self.dir_ui(ui, &e.path, depth + 1, open_file, actions);
            }
        }
    }
}

fn dir_menu(ui: &mut egui::Ui, dir: &Path, actions: &mut Vec<TreeAction>) {
    if ui.button("New File…").clicked() {
        actions.push(TreeAction::NewFile(dir.to_path_buf()));
        ui.close();
    }
    if ui.button("New Folder…").clicked() {
        actions.push(TreeAction::NewFolder(dir.to_path_buf()));
        ui.close();
    }
    if ui.button("Open Terminal Here").clicked() {
        actions.push(TreeAction::OpenTerminalHere(dir.to_path_buf()));
        ui.close();
    }
}

pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        let mut c = std::process::Command::new("explorer");
        c.arg(format!("/select,{}", path.display()));
        let _ = c.spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}
