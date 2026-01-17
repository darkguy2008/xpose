mod animation;
mod capture;
mod config;
mod connection;
mod error;
mod input;
mod layout;
mod renderer;
mod state;
mod window_finder;

use std::collections::HashSet;

use x11rb::connection::Connection;
use x11rb::protocol::damage as xdamage;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;

use std::thread;

use animation::{calculate_exit_layouts, calculate_start_layouts, AnimationConfig, Animator};
use capture::CapturedWindow;
use config::Config;
use connection::XConnection;
use error::Result;
use input::{InputAction, InputHandler};
use layout::{calculate_layout, LayoutConfig, ThumbnailLayout};
use renderer::OverviewWindow;
use state::WindowState;

fn main() {
    // Initialize logging to /tmp/xpose.log (recreated each run)
    let log_file = std::fs::File::create("/tmp/xpose.log").expect("Failed to create log file");

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .init();

    if let Err(e) = run() {
        log::error!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    log::info!("Starting xpose");

    // Load configuration
    let config = Config::load();
    let entrance_anim = AnimationConfig::new(config.entrance_duration());
    let exit_anim = AnimationConfig::new(config.exit_duration());

    // Connect to X server
    let xconn = XConnection::new()?;
    log::info!(
        "Connected to X server, screen {}x{}",
        xconn.screen_width,
        xconn.screen_height
    );

    // Find all windows (managed and skipped)
    // original_stacking_order contains frame window IDs in their X11 stacking order (bottom-to-top)
    let (mut windows, skipped_windows, original_stacking_order) =
        xconn.find_windows(&config.exclude_classes)?;

    if windows.is_empty() {
        log::info!("No windows to display");
        return Ok(());
    }

    // Load saved state and apply consistent ordering
    let mut window_state = WindowState::load();
    let current_hash = WindowState::compute_hash(&windows);

    if current_hash == window_state.window_set_hash {
        // Same window set - restore saved order
        log::debug!("Restoring saved window order");
        window_state.sort_windows(&mut windows);
    } else {
        // Window set changed - use new order and update state
        log::debug!("Window set changed, using fresh layout");
        window_state.update_from_windows(&windows);
        window_state.save();
    }

    // Capture window contents (managed windows)
    let mut captures: Vec<CapturedWindow> = Vec::new();
    for window in &windows {
        match xconn.capture_window(window) {
            Ok(capture) => captures.push(capture),
            Err(e) => log::warn!("Failed to capture window {:?}: {}", window.wm_name, e),
        }
    }

    if captures.is_empty() {
        log::info!("No windows could be captured");
        return Ok(());
    }

    // Capture skipped windows (for fade effect)
    let mut skipped_captures: Vec<CapturedWindow> = Vec::new();
    for window in &skipped_windows {
        match xconn.capture_window(window) {
            Ok(capture) => skipped_captures.push(capture),
            Err(e) => log::warn!("Failed to capture skipped window {:?}: {}", window.wm_name, e),
        }
    }

    // Calculate layout
    let config = LayoutConfig::default();
    let layouts = calculate_layout(
        &captures.iter().map(|c| c.info.clone()).collect::<Vec<_>>(),
        xconn.screen_width,
        xconn.screen_height,
        &config,
    );

    // Debug: print layout positions
    for (i, layout) in layouts.iter().enumerate() {
        log::debug!(
            "Layout {}: window_idx={}, pos=({}, {}), size={}x{}, window={:?}",
            i,
            layout.window_index,
            layout.x,
            layout.y,
            layout.width,
            layout.height,
            captures[layout.window_index].info.wm_name
        );
    }

    // Create overview window
    let overview = xconn.create_overview_window()?;

    // Map window and grab input before animation
    xconn.conn.map_window(overview.window)?;

    // Grab keyboard
    xconn.conn.grab_keyboard(
        true,
        overview.window,
        x11rb::CURRENT_TIME,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
    )?;

    // Grab pointer
    xconn.conn.grab_pointer(
        true,
        overview.window,
        (EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION).into(),
        GrabMode::ASYNC,
        GrabMode::ASYNC,
        overview.window,
        0u32,
        x11rb::CURRENT_TIME,
    )?;

    xconn.flush()?;

    // Run entrance animation
    let windows_info: Vec<_> = captures.iter().map(|c| c.info.clone()).collect();
    let start_layouts = calculate_start_layouts(
        &windows_info,
        &layouts,
        xconn.screen_width,
        xconn.screen_height,
    );

    let animator = Animator::new(start_layouts, layouts.clone(), &entrance_anim);

    // Build render order from original Z-order (bottom to top)
    let render_order: Vec<usize> = original_stacking_order
        .iter()
        .filter_map(|frame| captures.iter().position(|c| c.info.frame_window == *frame))
        .collect();

    // Animation loop - fade out skipped windows while animating managed windows
    while !animator.is_complete() {
        let progress = animator.progress();
        let current = animator.current_layouts();

        xconn.clear_overview(&overview)?;

        // Render skipped windows with fading opacity (1.0 → 0.0)
        let skip_opacity = 1.0 - progress;
        for capture in &skipped_captures {
            xconn.render_window_with_opacity(
                capture.picture,
                overview.picture,
                capture.info.x,
                capture.info.y,
                capture.info.width,
                capture.info.height,
                skip_opacity,
            )?;
        }

        // Render managed windows in original Z-order (bottom to top)
        for &idx in &render_order {
            let layout = &current[idx];
            xconn.render_thumbnail_animated(
                captures[idx].picture,
                overview.picture,
                captures[idx].info.width,
                captures[idx].info.height,
                layout,
            )?;
            xconn.draw_thumbnail_border_animated(&overview, layout, false)?;
        }

        xconn.present_overview(&overview)?;
        thread::sleep(animator.frame_duration());
    }

    // Render final static state
    render_all_thumbnails(&xconn, &captures, &layouts, &overview, None)?;
    xconn.present_overview(&overview)?;

    log::info!("Overview displayed, waiting for input");

    // Event loop
    let mut input_handler = InputHandler::new(layouts.clone());
    let mut selected_window: Option<usize> = None;
    let mut last_hovered: Option<usize> = None;
    let mut should_exit = false;

    // Track which windows have pending damage (for batching updates)
    let mut damaged_windows: HashSet<usize> = HashSet::new();

    loop {
        // Process all pending events (non-blocking after first)
        let event = xconn.conn.wait_for_event()?;
        let mut events = vec![event];

        // Collect any additional pending events to batch damage updates
        while let Some(event) = xconn.conn.poll_for_event()? {
            events.push(event);
        }

        let mut needs_present = false;

        for event in events {
            // Check if this is a DamageNotify event
            if let Event::DamageNotify(ref damage_event) = event {
                // Find which capture this damage belongs to
                if let Some(idx) = captures.iter().position(|c| c.damage == damage_event.damage) {
                    damaged_windows.insert(idx);
                    // Subtract damage to acknowledge it
                    xdamage::subtract(&xconn.conn, damage_event.damage, x11rb::NONE, x11rb::NONE)?;
                }
                continue;
            }

            let action = match event {
                Event::ButtonPress(ref e) => input_handler.handle_button_press(e),
                Event::KeyPress(ref e) => input_handler.handle_key_press(e),
                Event::MotionNotify(ref e) => input_handler.handle_motion(e),
                Event::Expose(_) => {
                    needs_present = true;
                    InputAction::None
                }
                _ => InputAction::None,
            };

            match action {
                InputAction::SelectWindow(index) => {
                    log::info!("Selected window {} (was hovering: {:?})", index, last_hovered);
                    if let Some(hover_idx) = last_hovered {
                        if hover_idx != index {
                            log::warn!("MISMATCH: Hovering {} but clicked {}", hover_idx, index);
                        }
                    }
                    selected_window = Some(index);
                    should_exit = true;
                }
                InputAction::Dismiss => {
                    log::info!("Dismissed");
                    should_exit = true;
                }
                InputAction::Hover(new_hover) => {
                    // Redraw affected thumbnails
                    if new_hover != last_hovered {
                        if let Some(idx) = new_hover {
                            log::debug!("Hovering window {}: {:?}", idx, captures[idx].info.wm_name);
                        }
                        // Clear old highlight
                        if let Some(old_idx) = last_hovered {
                            redraw_thumbnail(&xconn, &captures, &layouts, &overview, old_idx, false)?;
                        }
                        // Draw new highlight
                        if let Some(new_idx) = new_hover {
                            redraw_thumbnail(&xconn, &captures, &layouts, &overview, new_idx, true)?;
                        }
                        needs_present = true;
                        last_hovered = new_hover;
                    }
                }
                InputAction::None => {}
            }

            if should_exit {
                break;
            }
        }

        if should_exit {
            break;
        }

        // Process damaged windows - refresh and re-render
        if !damaged_windows.is_empty() {
            for &idx in &damaged_windows {
                if idx < captures.len() {
                    // Refresh the capture (get new pixmap with updated contents)
                    if let Err(e) = xconn.refresh_capture(&mut captures[idx]) {
                        log::warn!("Failed to refresh capture {}: {}", idx, e);
                        continue;
                    }

                    // Re-render this thumbnail
                    let highlighted = last_hovered == Some(idx);
                    redraw_thumbnail(&xconn, &captures, &layouts, &overview, idx, highlighted)?;
                }
            }
            damaged_windows.clear();
            needs_present = true;
        }

        if needs_present {
            xconn.present_overview(&overview)?;
        }
    }

    // Run exit animation - fade in skipped windows while animating managed windows back
    {
        let (exit_start, exit_end) = calculate_exit_layouts(&windows_info, &layouts);
        let exit_animator = Animator::new(exit_start, exit_end, &exit_anim);

        // Build render order: original Z-order (bottom to top), with selected window last
        // Map from original_stacking_order (frame IDs) to indices in captures array
        let mut render_order: Vec<usize> = Vec::new();
        for frame in &original_stacking_order {
            if let Some(idx) = captures.iter().position(|c| c.info.frame_window == *frame) {
                if Some(idx) != selected_window {
                    render_order.push(idx);
                }
            }
        }
        // Add selected window last (renders on top)
        if let Some(idx) = selected_window {
            render_order.push(idx);
        }

        while !exit_animator.is_complete() {
            let progress = exit_animator.progress();
            let current = exit_animator.current_layouts();

            xconn.clear_overview(&overview)?;

            // Render skipped windows with fading in opacity (0.0 → 1.0)
            for capture in &skipped_captures {
                xconn.render_window_with_opacity(
                    capture.picture,
                    overview.picture,
                    capture.info.x,
                    capture.info.y,
                    capture.info.width,
                    capture.info.height,
                    progress,
                )?;
            }

            // Render windows in original Z-order (bottom to top), selected window last
            for &idx in &render_order {
                let layout = &current[idx];
                xconn.render_thumbnail_animated(
                    captures[idx].picture,
                    overview.picture,
                    captures[idx].info.width,
                    captures[idx].info.height,
                    layout,
                )?;
            }

            xconn.present_overview(&overview)?;
            thread::sleep(exit_animator.frame_duration());
        }
    }

    // Cleanup
    log::debug!("Cleaning up");

    // Restore original window stacking order before raising selected window
    xconn.restore_stacking_order(&original_stacking_order)?;

    // Raise and focus selected window BEFORE destroying overview to avoid flicker
    if let Some(index) = selected_window {
        if index < captures.len() {
            let window_info = &captures[index].info;
            log::info!(
                "Raising window: {:?}",
                window_info.wm_name.as_deref().unwrap_or("(unnamed)")
            );
            xconn.raise_and_focus(window_info)?;
            xconn.sync()?; // Round-trip to ensure raise is fully processed
        }
    }

    // Log final Z-order for comparison
    xconn.log_current_zorder(&original_stacking_order)?;

    xconn.conn.ungrab_keyboard(x11rb::CURRENT_TIME)?;
    xconn.conn.ungrab_pointer(x11rb::CURRENT_TIME)?;
    xconn.sync()?; // Ensure ungrabs are processed
    xconn.destroy_overview(&overview)?;

    for capture in &captures {
        if let Err(e) = xconn.release_capture(capture) {
            log::warn!("Failed to release capture: {}", e);
        }
    }

    for capture in &skipped_captures {
        if let Err(e) = xconn.release_capture(capture) {
            log::warn!("Failed to release skipped capture: {}", e);
        }
    }

    xconn.flush()?;

    log::info!("Done");
    Ok(())
}

/// Render all thumbnails with optional highlight.
fn render_all_thumbnails(
    xconn: &XConnection,
    captures: &[CapturedWindow],
    layouts: &[ThumbnailLayout],
    overview: &OverviewWindow,
    highlighted: Option<usize>,
) -> Result<()> {
    for (i, (capture, layout)) in captures.iter().zip(layouts.iter()).enumerate() {
        xconn.render_thumbnail(
            capture.picture,
            overview.picture,
            capture.info.width,
            capture.info.height,
            layout,
        )?;
        xconn.draw_thumbnail_border(overview, layout, Some(i) == highlighted)?;
    }
    Ok(())
}

/// Redraw a single thumbnail (used for hover updates).
fn redraw_thumbnail(
    xconn: &XConnection,
    captures: &[CapturedWindow],
    layouts: &[ThumbnailLayout],
    overview: &OverviewWindow,
    index: usize,
    highlighted: bool,
) -> Result<()> {
    if index >= captures.len() || index >= layouts.len() {
        return Ok(());
    }

    let capture = &captures[index];
    let layout = &layouts[index];

    // Clear the area first
    xconn.clear_thumbnail_area(overview, layout)?;

    // Redraw thumbnail
    xconn.render_thumbnail(
        capture.picture,
        overview.picture,
        capture.info.width,
        capture.info.height,
        layout,
    )?;

    // Draw border with highlight state
    xconn.draw_thumbnail_border(overview, layout, highlighted)?;

    Ok(())
}
