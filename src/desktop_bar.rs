/// Desktop bar UI for virtual desktop management (Mission Control style).

use x11rb::protocol::xproto::Window;

use crate::capture::CapturedWindow;
use crate::desktop::DesktopState;

// Layout constants
pub const BAR_HEIGHT: u16 = 120;
const PREVIEW_PADDING: u16 = 15;
const PREVIEW_HEIGHT: u16 = 80;
const PREVIEW_WIDTH: u16 = (PREVIEW_HEIGHT as f64 * 16.0 / 9.0) as u16; // ~142
const PLUS_BUTTON_SIZE: u16 = 40;
const PLUS_BUTTON_MARGIN: u16 = 20;


/// Result of hit testing the desktop bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopBarHit {
    None,
    Desktop(u32),
    PlusButton,
}

/// Layout for a mini-window thumbnail within a desktop preview.
#[derive(Debug, Clone)]
pub struct MiniWindowLayout {
    pub window_id: Window,   // X11 window ID for lookup in captures
    pub x: i16,              // X position within preview (relative to preview origin)
    pub y: i16,              // Y position within preview (relative to preview origin)
    pub width: u16,          // Scaled width
    pub height: u16,         // Scaled height
}

/// Layout rectangle for a desktop preview in the bar.
#[derive(Debug, Clone)]
pub struct DesktopPreviewLayout {
    pub desktop_index: u32,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub is_current: bool,
    pub mini_windows: Vec<MiniWindowLayout>,  // Windows to render in this preview
}

/// Layout for the plus button.
#[derive(Debug, Clone, Copy)]
pub struct PlusButtonLayout {
    pub x: i16,
    pub y: i16,
    pub size: u16,
}

/// Complete desktop bar state and layout.
#[derive(Clone)]
pub struct DesktopBar {
    pub num_desktops: u32,
    pub current_desktop: u32,
    pub preview_layouts: Vec<DesktopPreviewLayout>,
    pub plus_button: PlusButtonLayout,
    pub bar_height: u16,
}

impl DesktopBar {
    /// Create desktop bar from xdeskie properties.
    pub fn new(num_desktops: u32, current_desktop: u32, screen_width: u16) -> Self {
        let bar_height = BAR_HEIGHT;

        // Calculate preview dimensions (16:9 aspect ratio)
        let preview_height = PREVIEW_HEIGHT;
        let preview_width = (preview_height as f64 * 16.0 / 9.0) as u16;

        // Calculate total width of all previews + padding
        let total_previews_width = (num_desktops as u16 * preview_width)
            + ((num_desktops.saturating_sub(1)) as u16 * PREVIEW_PADDING);

        // Center the previews horizontally
        let start_x = (screen_width.saturating_sub(total_previews_width)) / 2;
        let preview_y = (bar_height.saturating_sub(preview_height)) / 2;

        // Build preview layouts
        let mut preview_layouts = Vec::with_capacity(num_desktops as usize);
        for i in 0..num_desktops {
            let x = start_x as i16 + (i as u16 * (preview_width + PREVIEW_PADDING)) as i16;
            preview_layouts.push(DesktopPreviewLayout {
                desktop_index: i,
                x,
                y: preview_y as i16,
                width: preview_width,
                height: preview_height,
                is_current: i == current_desktop,
                mini_windows: Vec::new(),
            });
        }

        // Plus button on the right edge
        let plus_button = PlusButtonLayout {
            x: (screen_width - PLUS_BUTTON_MARGIN - PLUS_BUTTON_SIZE) as i16,
            y: ((bar_height - PLUS_BUTTON_SIZE) / 2) as i16,
            size: PLUS_BUTTON_SIZE,
        };

        Self {
            num_desktops,
            current_desktop,
            preview_layouts,
            plus_button,
            bar_height,
        }
    }

    /// Check if a point is within the bar area.
    pub fn contains_point(&self, _x: i16, y: i16) -> bool {
        y >= 0 && y < self.bar_height as i16
    }

    /// Hit test: returns which element (if any) is at the given coordinates.
    pub fn hit_test(&self, x: i16, y: i16) -> DesktopBarHit {
        // Check plus button first
        let pb = &self.plus_button;
        if x >= pb.x && x < pb.x + pb.size as i16 && y >= pb.y && y < pb.y + pb.size as i16 {
            return DesktopBarHit::PlusButton;
        }

        // Check desktop previews
        for preview in &self.preview_layouts {
            if x >= preview.x
                && x < preview.x + preview.width as i16
                && y >= preview.y
                && y < preview.y + preview.height as i16
            {
                return DesktopBarHit::Desktop(preview.desktop_index);
            }
        }

        DesktopBarHit::None
    }

    /// Get the center position of a desktop preview (for snap animation target).
    pub fn get_preview_center(&self, desktop_index: u32) -> Option<(i16, i16)> {
        self.preview_layouts
            .iter()
            .find(|p| p.desktop_index == desktop_index)
            .map(|p| (p.x + (p.width / 2) as i16, p.y + (p.height / 2) as i16))
    }

    /// Calculate mini-window layouts for all desktop previews.
    /// Takes window captures and desktop state to determine which windows
    /// appear on which desktop, and calculates their scaled positions.
    pub fn calculate_mini_layouts(
        &mut self,
        captures: &[CapturedWindow],
        desktop_state: &DesktopState,
        screen_width: u16,
        screen_height: u16,
    ) {
        // Scale factors for screen -> preview mapping
        let scale_x = PREVIEW_WIDTH as f64 / screen_width as f64;
        let scale_y = PREVIEW_HEIGHT as f64 / screen_height as f64;

        log::debug!(
            "Calculating mini layouts: scale_x={:.4}, scale_y={:.4}",
            scale_x,
            scale_y
        );

        for preview in &mut self.preview_layouts {
            preview.mini_windows.clear();

            // Get window IDs for this desktop (0-indexed)
            let window_ids = desktop_state.windows_on_desktop(preview.desktop_index);
            log::info!(
                "Desktop {} preview: desktop_state.windows_on_desktop({}) returned {} windows",
                preview.desktop_index,
                preview.desktop_index,
                window_ids.len()
            );

            for window_id in window_ids {
                // Find the capture for this window (try both client and frame)
                let capture = captures.iter().find(|c| {
                    c.info.client_window == window_id || c.info.frame_window == window_id
                });

                if let Some(cap) = capture {
                    log::info!(
                        "  Desktop {}: Adding window {:?} (0x{:x})",
                        preview.desktop_index,
                        cap.info.wm_name,
                        window_id
                    );
                    // Scale window position and size to preview coordinates
                    let mini_x = (cap.info.x as f64 * scale_x) as i16;
                    let mini_y = (cap.info.y as f64 * scale_y) as i16;
                    let mini_w = (cap.info.width as f64 * scale_x).max(4.0) as u16;
                    let mini_h = (cap.info.height as f64 * scale_y).max(3.0) as u16;

                    preview.mini_windows.push(MiniWindowLayout {
                        window_id: cap.info.frame_window,
                        x: mini_x,
                        y: mini_y,
                        width: mini_w,
                        height: mini_h,
                    });

                    log::debug!(
                        "Desktop {}: window {:?} at ({}, {}) {}x{}",
                        preview.desktop_index,
                        cap.info.wm_name,
                        mini_x,
                        mini_y,
                        mini_w,
                        mini_h
                    );
                }
            }

            log::debug!(
                "Desktop {} has {} mini windows",
                preview.desktop_index,
                preview.mini_windows.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_desktop_bar_layout() {
        let bar = DesktopBar::new(4, 0, 1920);

        assert_eq!(bar.num_desktops, 4);
        assert_eq!(bar.current_desktop, 0);
        assert_eq!(bar.preview_layouts.len(), 4);
        assert!(bar.preview_layouts[0].is_current);
        assert!(!bar.preview_layouts[1].is_current);
    }

    #[test]
    fn test_hit_test_desktop() {
        let bar = DesktopBar::new(4, 0, 1920);
        let preview = &bar.preview_layouts[0];

        // Hit inside first preview
        let hit = bar.hit_test(preview.x + 5, preview.y + 5);
        assert_eq!(hit, DesktopBarHit::Desktop(0));
    }

    #[test]
    fn test_hit_test_plus_button() {
        let bar = DesktopBar::new(4, 0, 1920);
        let pb = &bar.plus_button;

        let hit = bar.hit_test(pb.x + 5, pb.y + 5);
        assert_eq!(hit, DesktopBarHit::PlusButton);
    }

    #[test]
    fn test_hit_test_none() {
        let bar = DesktopBar::new(4, 0, 1920);

        // Hit in empty area
        let hit = bar.hit_test(5, 5);
        assert_eq!(hit, DesktopBarHit::None);
    }

    #[test]
    fn test_contains_point() {
        let bar = DesktopBar::new(4, 0, 1920);

        assert!(bar.contains_point(100, 50));
        assert!(!bar.contains_point(100, 150));
        assert!(!bar.contains_point(100, -10));
    }

    #[test]
    fn test_get_preview_center() {
        let bar = DesktopBar::new(4, 0, 1920);

        // First desktop should have a center
        let center = bar.get_preview_center(0);
        assert!(center.is_some());

        // Non-existent desktop
        let none = bar.get_preview_center(99);
        assert!(none.is_none());
    }
}
