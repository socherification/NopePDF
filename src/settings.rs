//! Persistent settings and per-file history, stored as TOML in the user's
//! config directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 200;

#[derive(Serialize, Deserialize, Clone)]
pub struct RecentFile {
    pub path: PathBuf,
    pub page: usize,
    pub zoom: f32,
    #[serde(default)]
    pub fit: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub dark: bool,
    pub invert_pages: bool,
    pub sidebar: bool,
    pub recent: Vec<RecentFile>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { dark: false, invert_pages: false, sidebar: true, recent: Vec::new() }
    }
}

fn file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("NopePDF").join("settings.toml"))
}

impl Settings {
    pub fn load() -> Self {
        file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string(self) {
            let _ = std::fs::write(f, s);
        }
    }

    pub fn find(&self, path: &Path) -> Option<&RecentFile> {
        self.recent.iter().find(|r| r.path == path)
    }

    pub fn remember(&mut self, entry: RecentFile) {
        self.recent.retain(|r| r.path != entry.path);
        self.recent.insert(0, entry);
        self.recent.truncate(MAX_RECENT);
    }
}
