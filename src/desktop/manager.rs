use x11rb::protocol::xproto::Window;

use crate::connection::XConnection;
use crate::error::Result;
use crate::window_finder::WindowInfo;

use super::DesktopState;

/// Switch to a specific desktop (0-indexed internally).
///
/// This handles:
/// - Detecting newly appeared windows and assigning them to current desktop
/// - Cleaning up dead windows from state
/// - Mapping/unmapping windows based on target desktop visibility
/// - Preserving window stacking order per desktop
pub fn switch_to_desktop(
    xconn: &XConnection,
    state: &mut DesktopState,
    windows: &[WindowInfo],
    target: u32,
) -> Result<()> {
    let window_ids: Vec<Window> = windows.iter().map(|i| i.frame_window).collect();

    detect_new_windows(state, windows);
    state.cleanup_dead_windows(&window_ids);

    // Save current desktop's stacking order before switching
    save_stacking_order(xconn, state, state.current)?;

    update_window_visibility(xconn, state, windows, target)?;

    // Restore target desktop's stacking order
    restore_stacking_order(xconn, state, target)?;

    state.current = target;
    state.sync_to_x(xconn)?;
    state.save()?;

    Ok(())
}

/// Detect newly appeared windows and assign them to current desktop.
fn detect_new_windows(state: &mut DesktopState, windows: &[WindowInfo]) {
    for info in windows {
        let key = info.frame_window.to_string();
        let is_new = !state.windows.contains_key(&key);

        if is_new {
            // Assign new window to current desktop
            state.get_window_desktop(info.frame_window, state.current);

            // If already hidden on arrival, mark as app-hidden
            if !info.is_mapped {
                state.set_app_hidden(info.frame_window, true);
            }
        }
    }
}

/// Update window visibility based on target desktop.
fn update_window_visibility(
    xconn: &XConnection,
    state: &DesktopState,
    windows: &[WindowInfo],
    target: u32,
) -> Result<()> {
    for info in windows {
        if state.is_visible_on(info.frame_window, target) {
            xconn.map_window(info.frame_window)?;
        } else {
            xconn.unmap_window(info.frame_window)?;
        }
    }
    xconn.flush()?;
    Ok(())
}

/// Save the current stacking order for a desktop.
pub fn save_stacking_order(xconn: &XConnection, state: &mut DesktopState, desktop: u32) -> Result<()> {
    let stacking = xconn.get_stacking_order()?;

    // Filter to only windows visible on this desktop
    let desktop_stacking: Vec<String> = stacking
        .into_iter()
        .filter(|&id| state.is_visible_on(id, desktop))
        .map(|id| id.to_string())
        .collect();

    state.stacking.insert(desktop, desktop_stacking);
    Ok(())
}

/// Restore the stacking order for a desktop.
fn restore_stacking_order(xconn: &XConnection, state: &DesktopState, desktop: u32) -> Result<()> {
    if let Some(order) = state.stacking.get(&desktop) {
        // Convert string IDs back to Window and filter out any that no longer exist
        let current_windows: std::collections::HashSet<Window> = xconn
            .get_stacking_order()?
            .into_iter()
            .collect();

        let order: Vec<Window> = order
            .iter()
            .filter_map(|s| s.parse::<Window>().ok())
            .filter(|id| current_windows.contains(id))
            .collect();

        xconn.restack_windows(&order)?;
    }
    Ok(())
}

/// Switch to the next desktop (wraps around).
pub fn switch_next(xconn: &XConnection, state: &mut DesktopState, windows: &[WindowInfo]) -> Result<u32> {
    let next = (state.current + 1) % state.desktops;
    switch_to_desktop(xconn, state, windows, next)?;
    Ok(next)
}

/// Switch to the previous desktop (wraps around).
pub fn switch_prev(xconn: &XConnection, state: &mut DesktopState, windows: &[WindowInfo]) -> Result<u32> {
    let prev = if state.current == 0 {
        state.desktops - 1
    } else {
        state.current - 1
    };
    switch_to_desktop(xconn, state, windows, prev)?;
    Ok(prev)
}

/// Move a window to a specific desktop (0-indexed).
///
/// Note: When xpose is active, all windows are mapped for live capture,
/// so this only updates the state without changing actual visibility.
pub fn move_window(
    xconn: &XConnection,
    state: &mut DesktopState,
    window_id: Window,
    desktop: u32,
) -> Result<()> {
    if desktop >= state.desktops {
        return Err(crate::error::XposeError::Other(format!(
            "Invalid desktop {}. Valid range: 0-{}",
            desktop,
            state.desktops - 1
        )));
    }

    // Get the current desktop assignment before changing it
    let old_desktop = state.get_window_desktop_assignment(window_id);

    // If we're currently on one of the affected desktops, save its stacking order first
    let current_desktop = state.current;
    let is_source_current = old_desktop.map(|d| d == current_desktop).unwrap_or(false);
    let is_dest_current = desktop == current_desktop;

    if is_source_current || is_dest_current {
        save_stacking_order(xconn, state, current_desktop)?;
    }

    // Remove window from old desktop's stacking order
    if let Some(old_desk) = old_desktop {
        state.remove_from_stacking(window_id, old_desk);
    }

    // Update window's desktop assignment
    state.set_window_desktop(window_id, desktop);
    state.set_app_hidden(window_id, false);

    // Add window to new desktop's stacking order
    state.add_to_stacking(window_id, desktop);

    // Note: We do NOT change window visibility here because xpose keeps all windows
    // mapped for live capture. Visibility will be restored when xpose exits.

    state.save()?;

    Ok(())
}

/// Set the number of desktops, relocating windows if necessary.
pub fn set_desktop_count(
    xconn: &XConnection,
    state: &mut DesktopState,
    windows: &[WindowInfo],
    count: u32,
) -> Result<()> {
    if count == 0 {
        return Err(crate::error::XposeError::Other(
            "Desktop count must be at least 1".to_string(),
        ));
    }

    // Move windows from removed desktops to the last valid one (0-indexed)
    if count < state.desktops {
        let max_valid = count - 1;
        for win_desktop in state.windows.values_mut() {
            if *win_desktop >= count {
                *win_desktop = max_valid;
            }
        }
    }

    state.desktops = count;

    // Switch to last valid desktop if current is now invalid
    if state.current >= count {
        let new_current = count - 1;
        switch_to_desktop(xconn, state, windows, new_current)?;
    }

    state.sync_to_x(xconn)?;
    state.save()?;

    Ok(())
}

/// Map all windows (used when xpose starts to enable live capture).
pub fn map_all_windows(xconn: &XConnection, windows: &[WindowInfo]) -> Result<()> {
    let mut mapped_any = false;
    for info in windows {
        if !info.is_mapped {
            xconn.map_window(info.frame_window)?;
            mapped_any = true;
        }
    }
    xconn.flush()?;
    // Give X server time to process all maps and make windows ready for capture
    xconn.sync()?;
    if mapped_any {
        // Extra delay for windows that were unmapped - they need time to redraw
        std::thread::sleep(std::time::Duration::from_millis(50));
        xconn.sync()?;
    }
    Ok(())
}

/// Restore window visibility based on current desktop (used when xpose exits).
pub fn restore_window_visibility(
    xconn: &XConnection,
    state: &DesktopState,
    windows: &[WindowInfo],
) -> Result<()> {
    for info in windows {
        if !state.is_visible_on(info.frame_window, state.current) {
            xconn.unmap_window(info.frame_window)?;
        }
    }
    xconn.flush()?;
    Ok(())
}
