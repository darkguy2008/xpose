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
    /// Whether the window was mapped (visible) when discovered
    pub is_mapped: bool,
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
    /// Returns (managed_windows, skipped_windows, original_stacking_order).
    /// Skipped windows are visible but filtered out (docks, panels, etc.) - used for fade effect.
    /// original_stacking_order contains the frame window IDs of managed windows in their
    /// original X11 stacking order (bottom-to-top), used to restore Z-order on exit.
    /// `exclude_classes` is a list of WM_CLASS values to exclude from the exposé view.
    #[allow(dead_code)]
    pub fn find_windows(
        &self,
        exclude_classes: &[String],
    ) -> Result<(Vec<WindowInfo>, Vec<WindowInfo>, Vec<Window>)> {
        let mut windows = Vec::new();
        let mut skipped = Vec::new();
        let mut original_stacking_order = Vec::new();

        // Get all children of root (these are TWM frame windows)
        // tree.children is in X11 stacking order (bottom-to-top)
        let tree = self.conn.query_tree(self.root)?.reply()?;

        for frame_window in tree.children {
            match self.examine_frame(frame_window) {
                Ok(ExamineResult::Managed(info)) => {
                    // Check if this window's class is in the exclude list
                    // WM_CLASS contains "instance class" (e.g. "xpad xpad" or "org.gnome.Boxes Org.gnome.Boxes")
                    let is_excluded = info.wm_class.as_ref().map_or(false, |class| {
                        exclude_classes.iter().any(|exc| {
                            // Check if any part of WM_CLASS matches the exclude pattern
                            class.split_whitespace().any(|part| part.eq_ignore_ascii_case(exc))
                        })
                    });

                    if is_excluded {
                        log::debug!(
                            "Excluding window by class: {:?} ({:?})",
                            info.wm_name,
                            info.wm_class
                        );
                        skipped.push(info);
                    } else {
                        log::debug!(
                            "Found window: {:?} ({:?}) frame=0x{:x} at {}x{}+{}+{}",
                            info.wm_name,
                            info.wm_class,
                            info.frame_window,
                            info.width,
                            info.height,
                            info.x,
                            info.y
                        );
                        // Save frame in original stacking order (bottom-to-top)
                        original_stacking_order.push(info.frame_window);
                        windows.push(info);
                    }
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

        // Log initial Z-order for debugging
        log::info!("=== INITIAL Z-ORDER (bottom to top) ===");
        for (i, win) in windows.iter().enumerate() {
            log::info!(
                "  [{}] frame=0x{:x} {}x{} {:?}",
                i,
                win.frame_window,
                win.width,
                win.height,
                win.wm_name.as_deref().unwrap_or("(unnamed)")
            );
        }
        log::info!("========================================");

        Ok((windows, skipped, original_stacking_order))
    }

    /// Enumerate ALL application windows including unmapped ones (for virtual desktop support).
    /// Returns (all_windows, skipped_windows, original_stacking_order).
    /// Windows have is_mapped set to indicate their visibility state.
    pub fn find_all_windows(
        &self,
        exclude_classes: &[String],
    ) -> Result<(Vec<WindowInfo>, Vec<WindowInfo>, Vec<Window>)> {
        let mut windows = Vec::new();
        let mut skipped = Vec::new();
        let mut original_stacking_order = Vec::new();

        // Get all children of root (these are TWM frame windows)
        let tree = self.conn.query_tree(self.root)?.reply()?;

        for frame_window in tree.children {
            match self.examine_frame_including_unmapped(frame_window) {
                Ok(ExamineResult::Managed(info)) => {
                    let is_excluded = info.wm_class.as_ref().map_or(false, |class| {
                        exclude_classes.iter().any(|exc| {
                            class.split_whitespace().any(|part| part.eq_ignore_ascii_case(exc))
                        })
                    });

                    if is_excluded {
                        log::debug!(
                            "Excluding window by class: {:?} ({:?})",
                            info.wm_name,
                            info.wm_class
                        );
                        skipped.push(info);
                    } else {
                        log::debug!(
                            "Found window: {:?} ({:?}) frame=0x{:x} mapped={} at {}x{}+{}+{}",
                            info.wm_name,
                            info.wm_class,
                            info.frame_window,
                            info.is_mapped,
                            info.width,
                            info.height,
                            info.x,
                            info.y
                        );
                        original_stacking_order.push(info.frame_window);
                        windows.push(info);
                    }
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
                    log::debug!("Error examining frame 0x{:x}: {}", frame_window, e);
                }
            }
        }

        log::info!(
            "Found {} application windows (including unmapped), {} skipped",
            windows.len(),
            skipped.len()
        );

        Ok((windows, skipped, original_stacking_order))
    }

    /// Examine a potential frame window to find the client window inside.
    /// Applies EWMH-based filtering to exclude non-application windows.
    #[allow(dead_code)]
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
                is_mapped: attrs.map_state == MapState::VIEWABLE,
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

    /// Examine a potential frame window including unmapped ones.
    /// Similar to examine_frame but doesn't skip unmapped windows.
    fn examine_frame_including_unmapped(&self, frame: Window) -> Result<ExamineResult> {
        // Get frame attributes
        let attrs = self.conn.get_window_attributes(frame)?.reply()?;

        // Skip override-redirect windows (menus, tooltips, popups)
        if attrs.override_redirect {
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
            // Skip windows in Withdrawn state (dead/closed windows not yet destroyed)
            if self.is_withdrawn(client) {
                log::debug!("Skipping withdrawn window: frame=0x{:x}", frame);
                return Ok(ExamineResult::Ignored);
            }
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
                is_mapped: attrs.map_state == MapState::VIEWABLE,
            };

            // Apply EWMH-based filtering on the client window
            if self.should_skip_window(client)? {
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

    /// Check if window is in Withdrawn state (should be ignored).
    /// WM_STATE values: WithdrawnState=0, NormalState=1, IconicState=3
    fn is_withdrawn(&self, window: Window) -> bool {
        let reply = match self.conn.get_property(
            false,
            window,
            self.atoms.WM_STATE,
            self.atoms.WM_STATE,
            0,
            2, // WM_STATE is 2 32-bit values: state, icon_window
        ) {
            Ok(cookie) => match cookie.reply() {
                Ok(r) => r,
                Err(_) => return false,
            },
            Err(_) => return false,
        };

        if reply.value_len == 0 {
            return false;
        }

        // First value is the state
        if let Some(state) = reply.value32().and_then(|mut v| v.next()) {
            // WithdrawnState = 0
            return state == 0;
        }
        false
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

        // Skip transient windows only if they have DIALOG type
        // (GTK apps use WM_TRANSIENT_FOR for legitimate windows like settings sheets)
        if self.is_transient(window)? && self.has_dialog_type(window)? {
            log::debug!("Skipping window 0x{:x}: transient dialog", window);
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

    /// Check if window has _NET_WM_WINDOW_TYPE_DIALOG.
    fn has_dialog_type(&self, window: Window) -> Result<bool> {
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

        if reply.type_ == u32::from(AtomEnum::NONE) || reply.value.is_empty() {
            return Ok(false);
        }

        if let Some(types) = reply.value32() {
            for window_type in types {
                if window_type == self.atoms._NET_WM_WINDOW_TYPE_DIALOG {
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

    /// Query and log the current Z-order of managed windows.
    /// Takes the list of frame windows we care about.
    pub fn log_current_zorder(&self, managed_frames: &[Window]) -> Result<()> {
        let tree = self.conn.query_tree(self.root)?.reply()?;

        log::info!("=== CURRENT Z-ORDER (bottom to top) ===");
        let mut idx = 0;
        for frame in &tree.children {
            if managed_frames.contains(frame) {
                // Get window name for logging
                if let Some(client) = self.find_client_window(*frame)? {
                    let name = self.get_wm_name(client).ok().flatten();
                    log::info!(
                        "  [{}] frame=0x{:x} {:?}",
                        idx,
                        frame,
                        name.as_deref().unwrap_or("(unnamed)")
                    );
                    idx += 1;
                }
            }
        }
        log::info!("========================================");
        Ok(())
    }

    /// Restore windows to their original stacking order.
    /// Takes the original stacking order (frame window IDs, bottom-to-top).
    pub fn restore_stacking_order(&self, original_order: &[Window]) -> Result<()> {
        if original_order.len() < 2 {
            return Ok(()); // Nothing to restack
        }

        log::debug!(
            "Restoring stacking order for {} windows (bottom-to-top): {:?}",
            original_order.len(),
            original_order.iter().map(|w| format!("0x{:x}", w)).collect::<Vec<_>>()
        );

        // Restack windows in order: each window goes ABOVE the previous one
        // This restores the original bottom-to-top order
        for i in 1..original_order.len() {
            let window = original_order[i];
            let sibling = original_order[i - 1];

            log::debug!(
                "Stacking 0x{:x} ABOVE 0x{:x}",
                window,
                sibling
            );

            self.conn.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .sibling(sibling)
                    .stack_mode(StackMode::ABOVE),
            )?;
        }

        self.conn.flush()?;
        log::debug!("Stacking order restored");
        Ok(())
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
