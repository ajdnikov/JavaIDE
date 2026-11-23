//! Portable settings: stored next to the executable, never in the user profile.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub dark_mode: bool,
    pub font_size: f32,
    pub last_folder: Option<PathBuf>,
    /// Optional JDK override. Empty = auto-detect (bundled `jdk/` first).
    pub jdk_path: Option<PathBuf>,
    pub show_sidebar: bool,
    pub show_bottom: bool,
    pub autocomplete: bool,
    pub run_args: String,
    /// Completion items accepted recently, most recent first (ranked first in the popup).
    pub recent: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark_mode: true,
            font_size: 14.0,
            last_folder: None,
            jdk_path: None,
            show_sidebar: true,
            show_bottom: true,
            autocomplete: true,
            run_args: String::new(),
            recent: Vec::new(),
        }
    }
}

/// Directory containing the running executable.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn settings_path() -> PathBuf {
    exe_dir().join("javaide-settings.json")
}

/// Paths inside the IDE folder are stored relative to it, so the whole folder
/// can move between machines / drive letters (USB sticks).
fn to_portable(p: &Path) -> PathBuf {
    p.strip_prefix(exe_dir()).map(Path::to_path_buf).unwrap_or_else(|_| p.to_path_buf())
}

fn from_portable(p: &Path) -> PathBuf {
    if p.is_relative() { exe_dir().join(p) } else { p.to_path_buf() }
}

impl Settings {
    pub const RECENT_MAX: usize = 400;

    /// Remember that a completion was just used: it moves to the front of the list.
    pub fn note_used(&mut self, label: &str) {
        self.recent.retain(|r| r != label);
        self.recent.insert(0, label.to_string());
        self.recent.truncate(Self::RECENT_MAX);
    }

    pub fn load() -> Self {
        let mut s: Settings = std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        s.last_folder = s.last_folder.map(|p| from_portable(&p));
        s.jdk_path = s.jdk_path.map(|p| from_portable(&p));
        s.font_size = s.font_size.clamp(8.0, 40.0);
        s.recent.truncate(Settings::RECENT_MAX);
        s
    }

    pub fn save(&self) {
        let mut s = self.clone();
        s.last_folder = s.last_folder.map(|p| to_portable(&p));
        s.jdk_path = s.jdk_path.map(|p| to_portable(&p));
        if let Ok(t) = serde_json::to_string_pretty(&s) {
            let _ = std::fs::write(settings_path(), t);
        }
    }
}
