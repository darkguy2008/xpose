use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use crate::connection::XConnection;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub client_window: Window,
    pub frame_window: Window,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub wm_class: Option<String>,
    pub wm_name: Option<String>,
}

/// Result of examining a frame window.
enum ExamineResult {
    /// A managed application window.
    Managed(WindowInfo),
    /// A visible window that was skipped (dock, panel, etc.) - contains full info for rendering.
    Skipped(WindowInfo),
    /// Not a visible window (override-redirect, unmapped, tiny).
    Ignored,
}

impl XConnection {
    /// Enumerate all visible application windows.
    /// Returns (managed_windows, skipped_windows).
    /// Skipped windows are visible but filtered out (docks, panels, etc.) - used for fade effect.
    pub fn find_windows(&self) -> Result<(Vec<WindowInfo>, Vec<WindowInfo>)> {
        let mut windows = Vec::new();
        let mut skipped = Vec::new();

        // Get all children of root (these are TWM frame windows)
        let tree = self.conn.query_tree(self.root)?.reply()?;

        for frame_window in tree.children {
            match self.examine_frame(frame_window) {
                Ok(ExamineResult::Managed(info)) => {
                    log::debug!(
                        "Found window: {:?} ({:?}) at {}x{}+{}+{}",
                        info.wm_name,
                        info.wm_class,
                        info.width,
                        info.height,
                        info.x,
                        info.y
                    );
                    windows.push(info);
                }
                Ok(ExamineResult::Skipped(info)) => {
                    log::debug!(
                        "Skipped visible window: {:?} at {}x{}+{}+{}",
                        info.wm_name,
                        info.width,
                        info.height,
                        info.x,
                        info.y
                    );
                    skipped.push(info);
                }
                Ok(ExamineResult::Ignored) => {}
                Err(e) => {
                    // Window may have been destroyed, skip it
                    log::debug!("Error examining frame 0x{:x}: {}", frame_window, e);
                }
            }
        }

        log::info!(
            "Found {} application windows, {} skipped visible windows",
            windows.len(),
            skipped.len()
        );
        Ok((windows, skipped))
    }

    /// Examine a potential frame window to find the client window inside.
    /// Applies EWMH-based filtering to exclude non-application windows.
    fn examine_frame(&self, frame: Window) -> Result<ExamineResult> {
        // Get frame attributes
        let attrs = self.conn.get_window_attributes(frame)?.reply()?;

        // Skip override-redirect windows (menus, tooltips, popups)
        if attrs.override_redirect {
            return Ok(ExamineResult::Ignored);
        }

        // Skip unmapped windows
        if attrs.map_state != MapState::VIEWABLE {
            return Ok(ExamineResult::Ignored);
        }

        // Get frame geometry
        let geom = self.conn.get_geometry(frame)?.reply()?;

        // Skip tiny windows (1x1 placeholders used by some apps)
        if geom.width <= 1 || geom.height <= 1 {
            return Ok(ExamineResult::Ignored);
        }

        // Find client window with WM_STATE property
        if let Some(client) = self.find_client_window(frame)? {
            let wm_class = self.get_wm_class(client).ok().flatten();
            let wm_name = self.get_wm_name(client).ok().flatten();

            let info = WindowInfo {
                client_window: client,
                frame_window: frame,
                x: geom.x,
                y: geom.y,
                width: geom.width,
                height: geom.height,
                wm_class,
                wm_name,
            };

            // Apply EWMH-based filtering on the client window
            if self.should_skip_window(client)? {
                // This is a visible window but filtered by EWMH - track it for fade effect
                return Ok(ExamineResult::Skipped(info));
            }

            return Ok(ExamineResult::Managed(info));
        }

        Ok(ExamineResult::Ignored)
    }

    /// Depth-first search for a window with WM_STATE property.
    /// The WM_STATE property indicates a real client window managed by the WM.
    fn find_client_window(&self, window: Window) -> Result<Option<Window>> {
        // Check if this window has WM_STATE
        if self.has_wm_state(window)? {
            return Ok(Some(window));
        }

        // Search children
        let tree = self.conn.query_tree(window)?.reply()?;
        for child in tree.children {
            if let Some(client) = self.find_client_window(child)? {
                return Ok(Some(client));
            }
        }

        Ok(None)
    }

    /// Check if window has WM_STATE property (indicates real client).
    fn has_wm_state(&self, window: Window) -> Result<bool> {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms.WM_STATE,
                AtomEnum::ANY,
                0,
                0, // Just check existence
            )?
            .reply()?;

        Ok(reply.type_ != u32::from(AtomEnum::NONE))
    }

    /// Get WM_CLASS property (instance and class names).
    fn get_wm_class(&self, window: Window) -> Result<Option<String>> {
        let reply = self
            .conn
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)?
            .reply()?;

        if reply.type_ == u32::from(AtomEnum::NONE) || reply.value.is_empty() {
            return Ok(None);
        }

        // WM_CLASS is null-separated: "instance\0class\0"
        let value = String::from_utf8_lossy(&reply.value);
        Ok(Some(value.replace('\0', " ").trim().to_string()))
    }

    /// Get WM_NAME property (window title).
    fn get_wm_name(&self, window: Window) -> Result<Option<String>> {
        let reply = self
            .conn
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 256)?
            .reply()?;

        if reply.type_ == u32::from(AtomEnum::NONE) || reply.value.is_empty() {
            return Ok(None);
        }

        Ok(Some(String::from_utf8_lossy(&reply.value).to_string()))
    }

    /// Check if window should be skipped based on EWMH hints.
    /// Returns true if the window should NOT be shown in exposé.
    fn should_skip_window(&self, window: Window) -> Result<bool> {
        // Skip windows without WM_CLASS (minimal/broken apps like xdeskie)
        if !self.has_wm_class(window)? {
            log::debug!("Skipping window 0x{:x}: no WM_CLASS", window);
            return Ok(true);
        }

        // Skip non-normal window types (docks, toolbars, menus, etc.)
        if !self.is_normal_window_type(window)? {
            log::debug!("Skipping window 0x{:x}: non-normal window type", window);
            return Ok(true);
        }

        // Skip windows that want to be hidden from taskbar/pager
        if self.should_skip_by_state(window)? {
            log::debug!("Skipping window 0x{:x}: skip_taskbar/skip_pager state", window);
            return Ok(true);
        }

        // Skip transient windows (dialogs)
        if self.is_transient(window)? {
            log::debug!("Skipping window 0x{:x}: transient window", window);
            return Ok(true);
        }

        // Skip windows without decorations (sticky notes, etc.)
        if self.has_no_decorations(window)? {
            log::debug!("Skipping window 0x{:x}: no decorations", window);
            return Ok(true);
        }

        Ok(false)
    }

    /// Check if window has WM_CLASS property set.
    fn has_wm_class(&self, window: Window) -> Result<bool> {
        let reply = self
            .conn
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 0)?
            .reply()?;
        Ok(reply.type_ != u32::from(AtomEnum::NONE))
    }

    /// Check if window type is NORMAL (or unset, which defaults to NORMAL).
    /// Returns false for DOCK, DESKTOP, TOOLBAR, MENU, UTILITY, SPLASH, DIALOG, etc.
    fn is_normal_window_type(&self, window: Window) -> Result<bool> {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                0,
                32,
            )?
            .reply()?;

        // No _NET_WM_WINDOW_TYPE set = assume NORMAL
        if reply.type_ == u32::from(AtomEnum::NONE) || reply.value.is_empty() {
            return Ok(true);
        }

        // Check if any of the types is NORMAL
        if let Some(types) = reply.value32() {
            for window_type in types {
                // If NORMAL is in the list, show it
                if window_type == self.atoms._NET_WM_WINDOW_TYPE_NORMAL {
                    return Ok(true);
                }
                // Skip these types entirely
                if window_type == self.atoms._NET_WM_WINDOW_TYPE_DOCK
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_DESKTOP
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_TOOLBAR
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_MENU
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_UTILITY
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_SPLASH
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_DIALOG
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_DROPDOWN_MENU
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_POPUP_MENU
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_TOOLTIP
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_NOTIFICATION
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_COMBO
                    || window_type == self.atoms._NET_WM_WINDOW_TYPE_DND
                {
                    return Ok(false);
                }
            }
        }

        // Unknown type, assume normal
        Ok(true)
    }

    /// Check if window has _NET_WM_STATE_SKIP_TASKBAR or _NET_WM_STATE_SKIP_PAGER.
    fn should_skip_by_state(&self, window: Window) -> Result<bool> {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                0,
                32,
            )?
            .reply()?;

        if reply.type_ == u32::from(AtomEnum::NONE) || reply.value.is_empty() {
            return Ok(false);
        }

        if let Some(states) = reply.value32() {
            for state in states {
                if state == self.atoms._NET_WM_STATE_SKIP_TASKBAR
                    || state == self.atoms._NET_WM_STATE_SKIP_PAGER
                {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Check if window is transient for another window (dialog).
    fn is_transient(&self, window: Window) -> Result<bool> {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms.WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?;

        Ok(reply.type_ != u32::from(AtomEnum::NONE) && !reply.value.is_empty())
    }

    /// Check if window requests no decorations via _MOTIF_WM_HINTS.
    /// Format: flags(u32), functions(u32), decorations(u32), input_mode(i32), status(u32)
    /// If flags bit 1 (0x2) is set and decorations == 0, window wants no decorations.
    fn has_no_decorations(&self, window: Window) -> Result<bool> {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms._MOTIF_WM_HINTS,
                self.atoms._MOTIF_WM_HINTS,
                0,
                5,
            )?
            .reply()?;

        if reply.type_ == u32::from(AtomEnum::NONE) || reply.value.is_empty() {
            return Ok(false);
        }

        if let Some(mut values) = reply.value32() {
            let flags = values.next().unwrap_or(0);
            let _functions = values.next().unwrap_or(0);
            let decorations = values.next().unwrap_or(1);

            // MWM_HINTS_DECORATIONS = 0x2
            const MWM_HINTS_DECORATIONS: u32 = 0x2;

            if (flags & MWM_HINTS_DECORATIONS) != 0 && decorations == 0 {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Raise and focus a window.
    pub fn raise_and_focus(&self, window: &WindowInfo) -> Result<()> {
        log::debug!(
            "Raising frame 0x{:x}, client 0x{:x}",
            window.frame_window,
            window.client_window
        );

        // Raise BOTH frame and client windows to top of stack
        self.conn.configure_window(
            window.frame_window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )?;
        self.conn.configure_window(
            window.client_window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )?;

        // Map both windows in case they're iconified
        self.conn.map_window(window.frame_window)?;
        self.conn.map_window(window.client_window)?;
        self.conn.flush()?;

        // Send WM_TAKE_FOCUS if supported
        self.send_take_focus(window.client_window)?;

        // Set input focus
        self.conn.set_input_focus(
            InputFocus::POINTER_ROOT,
            window.client_window,
            x11rb::CURRENT_TIME,
        )?;
        self.conn.flush()?;

        log::debug!("Raise and focus complete");
        Ok(())
    }

    /// Send WM_TAKE_FOCUS client message if the window supports it.
    fn send_take_focus(&self, window: Window) -> Result<()> {
        let wm_protocols = self
            .conn
            .intern_atom(false, b"WM_PROTOCOLS")?
            .reply()?
            .atom;
        let wm_take_focus = self
            .conn
            .intern_atom(false, b"WM_TAKE_FOCUS")?
            .reply()?
            .atom;

        // Check if window supports WM_TAKE_FOCUS
        let prop = self
            .conn
            .get_property(false, window, wm_protocols, AtomEnum::ATOM, 0, 32)?
            .reply()?;

        let supports_take_focus = prop
            .value32()
            .map(|atoms| atoms.into_iter().any(|a| a == wm_take_focus))
            .unwrap_or(false);

        if !supports_take_focus {
            log::debug!("Window does not support WM_TAKE_FOCUS");
            return Ok(());
        }

        // Send the message
        let event = ClientMessageEvent::new(
            32,
            window,
            wm_protocols,
            [wm_take_focus, x11rb::CURRENT_TIME, 0, 0, 0],
        );

        self.conn
            .send_event(false, window, EventMask::NO_EVENT, event)?;
        log::debug!("Sent WM_TAKE_FOCUS");

        Ok(())
    }
}
