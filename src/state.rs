use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::window_finder::WindowInfo;

/// Persistent state for window ordering.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct WindowState {
    /// WM_CLASS strings in display order
    pub window_order: Vec<String>,
    /// Hash of the sorted window set (to detect changes)
    pub window_set_hash: String,
}

impl WindowState {
    /// Load state from file, or return default if not found.
    pub fn load() -> Self {
        let path = match Self::state_path() {
            Some(p) => p,
            None => return Self::default(),
        };

        if !path.exists() {
            return Self::default();
        }

        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save state to file.
    pub fn save(&self) {
        let path = match Self::state_path() {
            Some(p) => p,
            None => {
                log::warn!("Cannot determine config directory for state file");
                return;
            }
        };

        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                log::warn!("Cannot create config directory: {}", e);
                return;
            }
        }

        match serde_json::to_string_pretty(self) {
            Ok(content) => {
                if let Err(e) = fs::write(&path, content) {
                    log::warn!("Cannot save state: {}", e);
                }
            }
            Err(e) => log::warn!("Cannot serialize state: {}", e),
        }
    }

    /// Get a unique key for a window (WM_CLASS + WM_NAME).
    fn window_key(window: &WindowInfo) -> String {
        let class = window.wm_class.as_deref().unwrap_or("");
        let name = window.wm_name.as_deref().unwrap_or("");
        format!("{}|{}", class, name)
    }

    /// Compute a hash of the window set (sorted window keys).
    pub fn compute_hash(windows: &[WindowInfo]) -> String {
        let mut keys: Vec<String> = windows.iter().map(Self::window_key).collect();
        keys.sort();

        let mut hasher = DefaultHasher::new();
        keys.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// Sort windows according to saved order.
    /// Windows not in saved order go to the end, preserving their relative order.
    pub fn sort_windows(&self, windows: &mut [WindowInfo]) {
        windows.sort_by_key(|w| {
            let key = Self::window_key(w);
            self.window_order
                .iter()
                .position(|k| k == &key)
                .unwrap_or(usize::MAX)
        });
    }

    /// Update state from current windows.
    pub fn update_from_windows(&mut self, windows: &[WindowInfo]) {
        self.window_order = windows.iter().map(Self::window_key).collect();
        self.window_set_hash = Self::compute_hash(windows);
    }

    fn state_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("xpose").join("state.json"))
    }
}
