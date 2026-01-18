//! xdeskie integration - parse state file for window-to-desktop mappings.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use x11rb::protocol::xproto::Window;

/// xdeskie state loaded from /tmp/xdeskie/state.json
#[derive(Debug, Deserialize)]
pub struct XdeskieState {
    pub current: u32,
    pub desktops: u32,
    /// Window ID (as string) -> desktop assignment (0=sticky, 1+=specific desktop)
    pub windows: HashMap<String, u32>,
    /// Per-desktop stacking order: desktop number (as string) -> window IDs in order (bottom to top)
    #[serde(default)]
    pub stacking: HashMap<String, Vec<String>>,
}

impl XdeskieState {
    const STATE_PATH: &'static str = "/tmp/xdeskie/state.json";

    /// Load xdeskie state from the state file.
    /// Returns None if the file doesn't exist or can't be parsed.
    pub fn load() -> Option<Self> {
        let path = Path::new(Self::STATE_PATH);
        if !path.exists() {
            log::debug!("xdeskie state file not found at {}", Self::STATE_PATH);
            return None;
        }

        match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<XdeskieState>(&content) {
                Ok(state) => {
                    log::debug!("Loaded xdeskie state: {} desktops, {} windows",
                              state.desktops, state.windows.len());
                    Some(state)
                }
                Err(e) => {
                    log::warn!("Failed to parse xdeskie state: {}", e);
                    None
                }
            },
            Err(e) => {
                log::warn!("Failed to read xdeskie state: {}", e);
                None
            }
        }
    }

    /// Get desktop assignment for a window.
    /// Returns 0 for sticky windows, 1+ for specific desktops, None if not found.
    pub fn get_desktop(&self, window_id: Window) -> Option<u32> {
        self.windows.get(&window_id.to_string()).copied()
    }

    /// Get all windows assigned to a specific desktop, including sticky (0) windows.
    /// Desktop numbers are 1-indexed (matches xdeskie's internal representation).
    /// Returns window IDs in stacking order if available.
    pub fn windows_on_desktop(&self, desktop: u32) -> Vec<Window> {
        let mut result = Vec::new();

        // Try to use stacking order if available
        let desktop_key = desktop.to_string();
        if let Some(stacking) = self.stacking.get(&desktop_key) {
            // Use stacking order for this desktop
            for id_str in stacking {
                if let Ok(id) = id_str.parse::<Window>() {
                    // Check if this window is actually on this desktop or is sticky
                    if let Some(&assigned) = self.windows.get(id_str) {
                        if assigned == 0 || assigned == desktop {
                            result.push(id);
                        }
                    }
                }
            }
        }

        // Also add any windows not in stacking order
        for (id_str, &assigned) in &self.windows {
            if assigned == 0 || assigned == desktop {
                if let Ok(id) = id_str.parse::<Window>() {
                    if !result.contains(&id) {
                        result.push(id);
                    }
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_state() {
        let json = r#"{
            "current": 1,
            "desktops": 4,
            "windows": {
                "12345": 1,
                "67890": 0
            },
            "stacking": {
                "1": ["67890", "12345"]
            }
        }"#;

        let state: XdeskieState = serde_json::from_str(json).unwrap();
        assert_eq!(state.current, 1);
        assert_eq!(state.desktops, 4);
        assert_eq!(state.windows.len(), 2);
        assert_eq!(state.get_desktop(12345), Some(1));
        assert_eq!(state.get_desktop(67890), Some(0)); // sticky

        let windows = state.windows_on_desktop(1);
        assert!(windows.contains(&12345));
        assert!(windows.contains(&67890)); // sticky windows included
    }
}
