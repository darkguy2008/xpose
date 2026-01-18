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
#[allow(dead_code)]
pub fn switch_next(xconn: &XConnection, state: &mut DesktopState, windows: &[WindowInfo]) -> Result<u32> {
    let next = (state.current + 1) % state.desktops;
    switch_to_desktop(xconn, state, windows, next)?;
    Ok(next)
}

/// Switch to the previous desktop (wraps around).
#[allow(dead_code)]
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
/// Returns true if any window was newly mapped.
pub fn map_all_windows(xconn: &XConnection, windows: &[WindowInfo]) -> Result<bool> {
    let mut mapped_any = false;
    for info in windows {
        if !info.is_mapped {
            xconn.map_window(info.frame_window)?;
            mapped_any = true;
        }
    }
    xconn.flush()?;
    Ok(mapped_any)
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

/// Delete a specific desktop and move its windows to an adjacent desktop.
/// - If deleting the first desktop (index 0), windows move to the new desktop 0 (was 1)
/// - Otherwise, windows move to the previous desktop (index - 1)
/// Returns error if only 1 desktop exists.
pub fn delete_desktop(
    xconn: &XConnection,
    state: &mut DesktopState,
    desktop_to_delete: u32,
) -> Result<()> {
    if state.desktops <= 1 {
        return Err(crate::error::XposeError::Other(
            "Cannot delete the last desktop".to_string(),
        ));
    }

    if desktop_to_delete >= state.desktops {
        return Err(crate::error::XposeError::Other(format!(
            "Desktop {} does not exist",
            desktop_to_delete
        )));
    }

    // Determine target desktop for window migration
    // After deletion, all indices >= deleted shift down by 1
    // So if deleting 0, windows go to what will become 0 (currently 1)
    // If deleting N > 0, windows go to N-1
    let target_desktop = if desktop_to_delete == 0 { 0 } else { desktop_to_delete - 1 };

    log::info!(
        "Deleting desktop {}, moving windows to desktop {}",
        desktop_to_delete,
        target_desktop
    );

    // Move all windows from deleted desktop to target
    for (_, desktop) in state.windows.iter_mut() {
        if *desktop == desktop_to_delete {
            *desktop = target_desktop;
        }
    }

    // Shift all desktop assignments > deleted index down by 1
    for (_, desktop) in state.windows.iter_mut() {
        if *desktop > desktop_to_delete {
            *desktop -= 1;
        }
    }

    // Merge deleted desktop's stacking into target
    if let Some(deleted_stacking) = state.stacking.remove(&desktop_to_delete) {
        let target_stacking = state.stacking.entry(target_desktop).or_default();
        target_stacking.extend(deleted_stacking);
    }

    // Shift stacking order keys
    let old_stacking = std::mem::take(&mut state.stacking);
    for (desk, order) in old_stacking {
        let new_key = if desk > desktop_to_delete {
            desk - 1
        } else {
            desk
        };
        state.stacking.insert(new_key, order);
    }

    // Update desktop count
    state.desktops -= 1;

    // Adjust current desktop if necessary
    if state.current == desktop_to_delete {
        state.current = target_desktop.min(state.desktops - 1);
    } else if state.current > desktop_to_delete {
        state.current -= 1;
    }

    state.sync_to_x(xconn)?;
    state.save()?;

    Ok(())
}

/// Reorder desktops by moving `from_desktop` to position `to_position`.
/// `to_position` is the index the desktop will be inserted BEFORE.
/// If `to_position >= num_desktops`, it's inserted at the end.
/// All window assignments and stacking orders are updated accordingly.
pub fn reorder_desktop(
    xconn: &XConnection,
    state: &mut DesktopState,
    from_desktop: u32,
    to_position: u32,
) -> Result<()> {
    if from_desktop >= state.desktops {
        return Err(crate::error::XposeError::Other(format!(
            "Invalid source desktop {}",
            from_desktop
        )));
    }

    // Clamp to_position to valid range
    let to_position = to_position.min(state.desktops);

    // No change needed if staying in same position
    if from_desktop == to_position || from_desktop + 1 == to_position {
        return Ok(());
    }

    log::info!(
        "Reordering desktop {} to position {}",
        from_desktop,
        to_position
    );

    // Build mapping from old indices to new indices
    let mut index_map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();

    if from_desktop < to_position {
        // Moving right: from shifts to to-1, everything between shifts left
        for i in 0..state.desktops {
            if i == from_desktop {
                index_map.insert(i, to_position - 1);
            } else if i > from_desktop && i < to_position {
                index_map.insert(i, i - 1);
            } else {
                index_map.insert(i, i);
            }
        }
    } else {
        // Moving left: from shifts to to, everything between shifts right
        for i in 0..state.desktops {
            if i == from_desktop {
                index_map.insert(i, to_position);
            } else if i >= to_position && i < from_desktop {
                index_map.insert(i, i + 1);
            } else {
                index_map.insert(i, i);
            }
        }
    }

    // Update window assignments
    for (_, desktop) in state.windows.iter_mut() {
        if let Some(&new_idx) = index_map.get(desktop) {
            *desktop = new_idx;
        }
    }

    // Update stacking orders
    let old_stacking = std::mem::take(&mut state.stacking);
    for (old_idx, order) in old_stacking {
        if let Some(&new_idx) = index_map.get(&old_idx) {
            state.stacking.insert(new_idx, order);
        }
    }

    // Update current desktop
    if let Some(&new_current) = index_map.get(&state.current) {
        state.current = new_current;
    }

    state.sync_to_x(xconn)?;
    state.save()?;

    Ok(())
}
