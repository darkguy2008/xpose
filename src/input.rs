use x11rb::protocol::xproto::*;

use crate::desktop_bar::{DesktopBar, DesktopBarHit};
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
    /// User clicked on a desktop preview.
    ActivateDesktop(u32),
    /// User clicked the plus button.
    ClickPlusButton,
    /// User started dragging a window.
    StartDrag(usize),
    /// Drag position updated.
    DragMove(i16, i16),
    /// Window dropped on a desktop.
    DropOnDesktop(usize, u32),
    /// Drag was cancelled.
    CancelDrag,
    /// Mouse hovering over desktop preview.
    HoverDesktop(Option<u32>),
}

/// Tracks the state of a drag operation.
#[derive(Debug, Clone)]
pub struct DragState {
    pub window_index: usize,
    pub start_x: i16,
    pub start_y: i16,
    pub current_x: i16,
    pub current_y: i16,
    pub is_active: bool,
    /// Offset from thumbnail center to click point (set when drag starts).
    pub click_offset_x: i16,
    pub click_offset_y: i16,
}

impl DragState {
    const DRAG_THRESHOLD: i16 = 5;

    pub fn new(window_index: usize, x: i16, y: i16) -> Self {
        Self {
            window_index,
            start_x: x,
            start_y: y,
            current_x: x,
            current_y: y,
            is_active: false,
            click_offset_x: 0,
            click_offset_y: 0,
        }
    }

    /// Set the click offset from thumbnail center.
    pub fn set_click_offset(&mut self, offset_x: i16, offset_y: i16) {
        self.click_offset_x = offset_x;
        self.click_offset_y = offset_y;
    }

    /// Update drag position, returns true if drag became active.
    pub fn update(&mut self, x: i16, y: i16) -> bool {
        self.current_x = x;
        self.current_y = y;

        if !self.is_active {
            let dx = (x - self.start_x).abs();
            let dy = (y - self.start_y).abs();
            if dx > Self::DRAG_THRESHOLD || dy > Self::DRAG_THRESHOLD {
                self.is_active = true;
                return true;
            }
        }
        false
    }
}

/// Handles mouse and keyboard input for the overview window.
pub struct InputHandler {
    layouts: Vec<ThumbnailLayout>,
    desktop_bar: Option<DesktopBar>,
    hovered_index: Option<usize>,
    hovered_desktop: Option<u32>,
    drag_state: Option<DragState>,
}

impl InputHandler {
    pub fn new(layouts: Vec<ThumbnailLayout>, desktop_bar: Option<DesktopBar>) -> Self {
        Self {
            layouts,
            desktop_bar,
            hovered_index: None,
            hovered_desktop: None,
            drag_state: None,
        }
    }

    /// Get the currently hovered thumbnail index.
    #[allow(dead_code)]
    pub fn hovered(&self) -> Option<usize> {
        self.hovered_index
    }

    /// Get the currently hovered desktop index.
    #[allow(dead_code)]
    pub fn hovered_desktop(&self) -> Option<u32> {
        self.hovered_desktop
    }

    /// Check if a drag is currently active.
    pub fn is_dragging(&self) -> bool {
        self.drag_state.as_ref().map_or(false, |d| d.is_active)
    }

    /// Get the current drag state.
    pub fn drag_state(&self) -> Option<&DragState> {
        self.drag_state.as_ref()
    }

    /// Get mutable access to the current drag state.
    pub fn drag_state_mut(&mut self) -> Option<&mut DragState> {
        self.drag_state.as_mut()
    }

    /// Update the layouts used for hit-testing.
    /// Called when the grid layout is recalculated (e.g., after removing windows).
    pub fn update_layouts(&mut self, new_layouts: Vec<ThumbnailLayout>) {
        self.layouts = new_layouts;
    }

    /// Update the desktop bar (e.g., after adding a new desktop).
    pub fn update_desktop_bar(&mut self, new_bar: Option<DesktopBar>) {
        self.desktop_bar = new_bar;
    }

    /// Handle a button press event.
    pub fn handle_button_press(&mut self, event: &ButtonPressEvent) -> InputAction {
        log::debug!("Button press at ({}, {})", event.event_x, event.event_y);

        // Left mouse button only
        if event.detail != 1 {
            return InputAction::Dismiss;
        }

        // Check desktop bar first
        if let Some(ref bar) = self.desktop_bar {
            if bar.contains_point(event.event_x, event.event_y) {
                match bar.hit_test(event.event_x, event.event_y) {
                    DesktopBarHit::Desktop(idx) => {
                        log::info!("Clicked desktop {}", idx);
                        return InputAction::ActivateDesktop(idx);
                    }
                    DesktopBarHit::PlusButton => {
                        log::info!("Clicked plus button");
                        return InputAction::ClickPlusButton;
                    }
                    DesktopBarHit::None => {
                        log::debug!("Clicked in bar area but not on element");
                        return InputAction::None;
                    }
                }
            }
        }

        // Check window thumbnails - start potential drag
        if let Some(index) = self.find_thumbnail_at(event.event_x, event.event_y) {
            log::debug!("Starting potential drag on thumbnail {}", index);
            self.drag_state = Some(DragState::new(index, event.event_x, event.event_y));
            return InputAction::None; // Wait to see if drag or click
        }

        // Click outside any element dismisses
        log::debug!("No element hit, dismissing");
        InputAction::Dismiss
    }

    /// Handle a button release event.
    pub fn handle_button_release(&mut self, event: &ButtonReleaseEvent) -> InputAction {
        if let Some(drag) = self.drag_state.take() {
            if drag.is_active {
                // Check if dropping on a desktop
                if let Some(ref bar) = self.desktop_bar {
                    if let DesktopBarHit::Desktop(desktop_idx) =
                        bar.hit_test(event.event_x, event.event_y)
                    {
                        log::info!(
                            "Dropped window {} on desktop {}",
                            drag.window_index,
                            desktop_idx
                        );
                        return InputAction::DropOnDesktop(drag.window_index, desktop_idx);
                    }
                }
                log::debug!("Drag cancelled (not dropped on desktop)");
                return InputAction::CancelDrag;
            } else {
                // Was a click, not a drag
                log::debug!("Selected window {} (click)", drag.window_index);
                return InputAction::SelectWindow(drag.window_index);
            }
        }
        InputAction::None
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
        // Update drag state if active
        if let Some(ref mut drag) = self.drag_state {
            let became_active = drag.update(event.event_x, event.event_y);
            if became_active {
                return InputAction::StartDrag(drag.window_index);
            }
            if drag.is_active {
                // Update hover state for desktop bar during drag
                if let Some(ref bar) = self.desktop_bar {
                    let new_hover = match bar.hit_test(event.event_x, event.event_y) {
                        DesktopBarHit::Desktop(idx) => Some(idx),
                        _ => None,
                    };
                    if new_hover != self.hovered_desktop {
                        self.hovered_desktop = new_hover;
                    }
                }
                return InputAction::DragMove(event.event_x, event.event_y);
            }
        }

        // Check desktop bar hover
        if let Some(ref bar) = self.desktop_bar {
            if bar.contains_point(event.event_x, event.event_y) {
                let new_hover = match bar.hit_test(event.event_x, event.event_y) {
                    DesktopBarHit::Desktop(idx) => Some(idx),
                    _ => None,
                };
                if new_hover != self.hovered_desktop {
                    self.hovered_desktop = new_hover;
                    return InputAction::HoverDesktop(new_hover);
                }
                return InputAction::None;
            } else if self.hovered_desktop.is_some() {
                self.hovered_desktop = None;
                return InputAction::HoverDesktop(None);
            }
        }

        // Check thumbnail hover
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
                layout.window_index,
                layout.x,
                layout.y,
                layout.width,
                layout.height,
                in_x,
                in_y
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
