use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use x11rb::protocol::xproto::Window;

use crate::connection::XConnection;
use crate::error::Result;

const DEFAULT_DESKTOP_COUNT: u32 = 4;

/// Persistent state for virtual desktop management.
///
/// Tracks which desktop each window belongs to and synchronizes
/// with X11 root window properties for cross-instance communication.
///
/// All desktop numbers are 0-indexed (0, 1, 2, 3, ...).
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct DesktopState {
    /// Current desktop (0-indexed)
    pub current: u32,
    /// Total number of desktops
    pub desktops: u32,
    /// Window ID (as string) -> desktop number (0-indexed)
    pub windows: HashMap<String, u32>,
    /// Windows hidden by the application itself (not by desktop switch)
    #[serde(default)]
    pub app_hidden: HashSet<String>,
    /// Window stacking order per desktop (bottom to top)
    /// desktop number (0-indexed) -> ordered list of window IDs
    #[serde(default)]
    pub stacking: HashMap<u32, Vec<String>>,
}

impl DesktopState {
    /// Load state from file, or create default.
    pub fn load() -> Result<Self> {
        let path = Self::state_path()?;

        if !path.exists() {
            return Ok(Self::default_state());
        }

        let content = fs::read_to_string(&path)?;
        let state: DesktopState = serde_json::from_str(&content)?;
        Ok(state)
    }

    fn default_state() -> Self {
        DesktopState {
            current: 0,
            desktops: DEFAULT_DESKTOP_COUNT,
            windows: HashMap::new(),
            app_hidden: HashSet::new(),
            stacking: HashMap::new(),
        }
    }

    /// Save state to file.
    pub fn save(&self) -> Result<()> {
        let path = Self::state_path()?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        fs::write(&path, content)?;
        Ok(())
    }

    /// Sync state from X properties (for cross-instance communication).
    pub fn sync_from_x(&mut self, xconn: &XConnection) -> Result<()> {
        if let Some(current) = xconn.get_current_desktop()? {
            self.current = current;
        }
        if let Some(count) = xconn.get_num_desktops()? {
            self.desktops = count;
        }
        Ok(())
    }

    /// Write state to X properties.
    pub fn sync_to_x(&self, xconn: &XConnection) -> Result<()> {
        xconn.set_current_desktop(self.current)?;
        xconn.set_num_desktops(self.desktops)?;
        Ok(())
    }

    /// Get desktop for a window, assigning to current desktop if new.
    ///
    /// Returns the desktop number (0-indexed).
    pub fn get_window_desktop(&mut self, window_id: Window, current_desktop: u32) -> u32 {
        let key = window_id.to_string();

        if let Some(&desktop) = self.windows.get(&key) {
            return desktop;
        }

        // New window: assign to current desktop (0-indexed)
        self.windows.insert(key, current_desktop);
        current_desktop
    }

    /// Set desktop for a window.
    pub fn set_window_desktop(&mut self, window_id: Window, desktop: u32) {
        self.windows.insert(window_id.to_string(), desktop);
    }

    /// Check if window should be visible on the given desktop.
    ///
    /// All desktop numbers are 0-indexed.
    /// Returns false for app-hidden windows regardless of desktop.
    pub fn is_visible_on(&self, window_id: Window, desktop: u32) -> bool {
        let key = window_id.to_string();

        if self.app_hidden.contains(&key) {
            return false;
        }

        match self.windows.get(&key) {
            Some(&win_desktop) => win_desktop == desktop,
            None => true, // Unknown windows visible until assigned
        }
    }

    /// Mark window as hidden by the application itself.
    pub fn set_app_hidden(&mut self, window_id: Window, hidden: bool) {
        let key = window_id.to_string();
        if hidden {
            self.app_hidden.insert(key);
        } else {
            self.app_hidden.remove(&key);
        }
    }

    /// Check if window is hidden by the application.
    pub fn is_app_hidden(&self, window_id: Window) -> bool {
        self.app_hidden.contains(&window_id.to_string())
    }

    /// Remove windows that no longer exist from state.
    pub fn cleanup_dead_windows(&mut self, live_windows: &[Window]) {
        let live_set: HashSet<String> = live_windows.iter().map(|id| id.to_string()).collect();
        self.windows.retain(|k, _| live_set.contains(k));
        self.app_hidden.retain(|k| live_set.contains(k));
        // Clean up stacking orders
        for order in self.stacking.values_mut() {
            order.retain(|k| live_set.contains(k));
        }
    }

    /// Remove a window from a desktop's stacking order
    pub fn remove_from_stacking(&mut self, window_id: Window, desktop: u32) {
        if let Some(order) = self.stacking.get_mut(&desktop) {
            let key = window_id.to_string();
            order.retain(|id| id != &key);
        }
    }

    /// Add a window to the end (top) of a desktop's stacking order
    pub fn add_to_stacking(&mut self, window_id: Window, desktop: u32) {
        let key = window_id.to_string();
        let order = self.stacking.entry(desktop).or_insert_with(Vec::new);

        // Remove if already present
        order.retain(|id| id != &key);

        // Add to top
        order.push(key);
    }

    /// Get the current desktop assignment for a window without modifying state
    pub fn get_window_desktop_assignment(&self, window_id: Window) -> Option<u32> {
        let key = window_id.to_string();
        self.windows.get(&key).copied()
    }

    /// Get all windows assigned to a specific desktop (0-indexed).
    /// Returns window IDs in stacking order (bottom to top) if available.
    pub fn windows_on_desktop(&self, desktop: u32) -> Vec<Window> {
        // Get stacking order for this desktop if available
        let stacking = self.stacking.get(&desktop);

        // Collect windows that should be visible on this desktop
        let mut result: Vec<Window> = Vec::new();

        if let Some(order) = stacking {
            // Use stacking order
            for id_str in order {
                if let Ok(id) = id_str.parse::<Window>() {
                    if let Some(&win_desktop) = self.windows.get(id_str) {
                        if win_desktop == desktop {
                            result.push(id);
                        }
                    }
                }
            }
        }

        // Also add any windows not in stacking order
        for (id_str, &win_desktop) in &self.windows {
            if win_desktop == desktop {
                if let Ok(id) = id_str.parse::<Window>() {
                    if !result.contains(&id) {
                        result.push(id);
                    }
                }
            }
        }

        result
    }

    fn state_path() -> Result<PathBuf> {
        Ok(PathBuf::from("/tmp/xpose/desktop_state.json"))
    }
}
