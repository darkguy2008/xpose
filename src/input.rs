use x11rb::protocol::xproto::*;

use crate::layout::ThumbnailLayout;

/// Actions that can result from user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// User selected a window (by clicking on it).
    SelectWindow(usize),
    /// Mouse is hovering over a thumbnail (or None if not over any).
    Hover(Option<usize>),
    /// User wants to dismiss the overview (Escape or click outside).
    Dismiss,
    /// No action needed.
    None,
}

/// Handles mouse and keyboard input for the overview window.
pub struct InputHandler {
    layouts: Vec<ThumbnailLayout>,
    hovered_index: Option<usize>,
}

impl InputHandler {
    pub fn new(layouts: Vec<ThumbnailLayout>) -> Self {
        Self {
            layouts,
            hovered_index: None,
        }
    }

    /// Get the currently hovered thumbnail index.
    #[allow(dead_code)]
    pub fn hovered(&self) -> Option<usize> {
        self.hovered_index
    }

    /// Handle a button press event.
    pub fn handle_button_press(&mut self, event: &ButtonPressEvent) -> InputAction {
        log::debug!("Button press at ({}, {})", event.event_x, event.event_y);

        // Left mouse button
        if event.detail == 1 {
            if let Some(index) = self.find_thumbnail_at(event.event_x, event.event_y) {
                log::debug!("Hit thumbnail at index {}", index);
                return InputAction::SelectWindow(index);
            }
            log::debug!("No thumbnail hit, dismissing");
        }

        // Click outside any thumbnail dismisses the overview
        InputAction::Dismiss
    }

    /// Handle a key press event.
    pub fn handle_key_press(&self, event: &KeyPressEvent) -> InputAction {
        // Keycode 9 is typically Escape on most X11 keymaps
        // Keycode 36 is typically Enter/Return
        match event.detail {
            9 => InputAction::Dismiss, // Escape
            36 => {
                // Enter - select hovered window if any
                if let Some(index) = self.hovered_index {
                    InputAction::SelectWindow(index)
                } else {
                    InputAction::None
                }
            }
            _ => {
                log::debug!("Unhandled keycode: {}", event.detail);
                InputAction::None
            }
        }
    }

    /// Handle a pointer motion event.
    pub fn handle_motion(&mut self, event: &MotionNotifyEvent) -> InputAction {
        let new_hover = self.find_thumbnail_at(event.event_x, event.event_y);

        if new_hover != self.hovered_index {
            self.hovered_index = new_hover;
            return InputAction::Hover(new_hover);
        }

        InputAction::None
    }

    /// Find which thumbnail (if any) contains the given point.
    fn find_thumbnail_at(&self, x: i16, y: i16) -> Option<usize> {
        for layout in &self.layouts {
            let in_x = x >= layout.x && x < layout.x + layout.width as i16;
            let in_y = y >= layout.y && y < layout.y + layout.height as i16;
            log::trace!(
                "Layout {}: ({}, {}) {}x{} - in_x={}, in_y={}",
                layout.window_index, layout.x, layout.y, layout.width, layout.height, in_x, in_y
            );
            if in_x && in_y {
                return Some(layout.window_index);
            }
        }
        None
    }
}

// TODO: Future enhancements
// - Keyboard navigation (arrow keys to move between windows)
// - Number keys to select specific windows
// - Search/filter by window title
