mod animation;
mod capture;
mod config;
mod connection;
mod desktop_bar;
mod error;
mod input;
mod layout;
mod renderer;
mod state;
mod window_finder;
mod xdeskie;

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use x11rb::connection::Connection;
use x11rb::protocol::damage as xdamage;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;

use std::thread;

// Animation constants
const REVERT_DURATION_MS: u64 = 200;
const SNAP_DURATION_MS: u64 = 150;
const GRID_TRANSITION_DURATION_MS: u64 = 250;

/// Animation mode: snap to desktop or revert to grid.
#[derive(Debug, Clone)]
enum AnimationMode {
    SnapToDesktop { desktop_idx: usize },
    RevertToGrid,
}

/// Animation state for drag revert or snap.
struct DragAnimation {
    mode: AnimationMode,
    window_index: usize,
    start_x: i16,
    start_y: i16,
    start_width: u16,
    start_height: u16,
    end_x: i16,
    end_y: i16,
    end_width: u16,
    end_height: u16,
    start_time: Instant,
    duration_ms: u64,
}

impl DragAnimation {
    fn progress(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_millis() as f64;
        let duration = self.duration_ms as f64;
        (elapsed / duration).min(1.0)
    }

    fn is_complete(&self) -> bool {
        self.progress() >= 1.0
    }

    fn current_position(&self) -> (i16, i16, u16, u16) {
        let t = self.progress();
        // Ease-out cubic
        let eased = 1.0 - (1.0 - t).powi(3);

        let x = self.start_x as f64 + (self.end_x - self.start_x) as f64 * eased;
        let y = self.start_y as f64 + (self.end_y - self.start_y) as f64 * eased;
        let w = self.start_width as f64 + (self.end_width as i32 - self.start_width as i32) as f64 * eased;
        let h = self.start_height as f64 + (self.end_height as i32 - self.start_height as i32) as f64 * eased;

        (x as i16, y as i16, w.max(1.0) as u16, h.max(1.0) as u16)
    }
}

/// Animation state for grid layout transitions when windows are removed.
struct GridTransitionAnimation {
    /// Map from window_index to (old_layout, new_layout)
    transitions: HashMap<usize, (ThumbnailLayout, ThumbnailLayout)>,
    start_time: Instant,
    duration_ms: u64,
}

impl GridTransitionAnimation {
    fn new(old_layouts: &[ThumbnailLayout], new_layouts: &[ThumbnailLayout], duration_ms: u64) -> Self {
        let mut transitions = HashMap::new();

        // Map new layouts by window_index for quick lookup
        let new_map: HashMap<usize, &ThumbnailLayout> = new_layouts
            .iter()
            .map(|l| (l.window_index, l))
            .collect();

        // For each window in old layouts, if it exists in new layouts, create transition
        for old_layout in old_layouts {
            if let Some(&new_layout) = new_map.get(&old_layout.window_index) {
                transitions.insert(
                    old_layout.window_index,
                    (old_layout.clone(), new_layout.clone()),
                );
            }
        }

        Self {
            transitions,
            start_time: Instant::now(),
            duration_ms,
        }
    }

    fn progress(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_millis() as f64;
        let duration = self.duration_ms as f64;
        (elapsed / duration).min(1.0)
    }

    fn is_complete(&self) -> bool {
        self.progress() >= 1.0
    }

    /// Get current interpolated layouts
    fn current_layouts(&self) -> Vec<ThumbnailLayout> {
        let t = self.progress();
        // Ease-out cubic for smooth deceleration
        let eased = 1.0 - (1.0 - t).powi(3);

        self.transitions
            .iter()
            .map(|(&window_index, (old, new))| {
                let x = old.x as f64 + (new.x - old.x) as f64 * eased;
                let y = old.y as f64 + (new.y - old.y) as f64 * eased;
                let width = old.width as f64 + (new.width as i32 - old.width as i32) as f64 * eased;
                let height = old.height as f64 + (new.height as i32 - old.height as i32) as f64 * eased;

                ThumbnailLayout {
                    x: x as i16,
                    y: y as i16,
                    width: width.max(1.0) as u16,
                    height: height.max(1.0) as u16,
                    window_index,
                }
            })
            .collect()
    }
}

/// Calculate drag scale factor and target size based on Y position.
/// Interpolates from drag start position (scale=1.0) to desktop preview bottom (scale=target_scale).
fn calculate_drag_scale_and_target(
    cursor_y: i16,
    drag_start_y: i16,
    layout: &ThumbnailLayout,
    desktop_bar: &Option<DesktopBar>,
    capture: &CapturedWindow,
) -> (f64, (u16, u16)) {
    // Calculate target size (what the window would be in a desktop preview)
    let aspect = capture.info.height as f64 / capture.info.width as f64;
    let target_width = 60u16; // Size in desktop preview
    let target_height = (60.0 * aspect) as u16;

    let Some(ref bar) = desktop_bar else {
        return (1.0, (target_width, target_height));
    };

    // Calculate scale ratio: how much smaller is the target compared to the grid thumbnail
    let target_scale = target_width as f64 / layout.width as f64;

    // Original position is where the user clicked (drag start), not thumbnail center
    let original_y = drag_start_y;

    // Target Y is the BOTTOM of the desktop previews (when cursor enters preview bounds)
    let preview_bottom_y = bar.preview_layouts.first()
        .map(|p| p.y + p.height as i16)
        .unwrap_or(bar.bar_height as i16);

    // If cursor is inside preview (at or above bottom), use minimum scale
    if cursor_y <= preview_bottom_y {
        return (target_scale, (target_width, target_height));
    }

    // If cursor is at or below original click position, use full scale
    if cursor_y >= original_y {
        return (1.0, (target_width, target_height));
    }

    // Interpolate between original click position and preview bottom
    // t = 0 at original_y, t = 1 at preview_bottom_y
    let total_distance = (original_y - preview_bottom_y) as f64;
    let current_distance = (original_y - cursor_y) as f64;
    let t = (current_distance / total_distance).clamp(0.0, 1.0);

    // Linear interpolation for smooth, even scaling
    // Interpolate scale: 1.0 at t=0, target_scale at t=1
    let scale = 1.0 - t * (1.0 - target_scale);

    (scale, (target_width, target_height))
}

/// Calculate dragged window size and position based on cursor and scale factor.
/// base_width/height is the original thumbnail size in the grid.
/// click_offset_x/y is the offset from thumbnail center to the original click point.
fn calculate_drag_rect(
    cursor_x: i16,
    cursor_y: i16,
    base_width: u16,
    base_height: u16,
    scale_factor: f64,
    click_offset_x: i16,
    click_offset_y: i16,
) -> (i16, i16, u16, u16) {
    let width = (base_width as f64 * scale_factor).max(1.0) as u16;
    let height = (base_height as f64 * scale_factor).max(1.0) as u16;

    // Scale the click offset proportionally so the click point stays under cursor
    let scaled_offset_x = (click_offset_x as f64 * scale_factor) as i16;
    let scaled_offset_y = (click_offset_y as f64 * scale_factor) as i16;

    // Position so the click point stays under cursor
    let x = cursor_x - (width / 2) as i16 - scaled_offset_x;
    let y = cursor_y - (height / 2) as i16 - scaled_offset_y;

    (x, y, width, height)
}

/// Recalculate grid layout for windows excluding removed ones.
/// Filters out removed windows, recalculates layout, and remaps indices.
fn recalculate_filtered_layout(
    captures: &[CapturedWindow],
    removed_windows: &HashSet<usize>,
    screen_width: u16,
    screen_height: u16,
    config: &LayoutConfig,
    top_reserved: u16,
) -> Vec<ThumbnailLayout> {
    // Filter out removed windows
    let filtered_captures: Vec<&CapturedWindow> = captures
        .iter()
        .enumerate()
        .filter(|(i, _)| !removed_windows.contains(i))
        .map(|(_, capture)| capture)
        .collect();

    if filtered_captures.is_empty() {
        return Vec::new();
    }

    // Create WindowInfo slice for layout calculation
    let filtered_infos: Vec<window_finder::WindowInfo> = filtered_captures
        .iter()
        .map(|c| c.info.clone())
        .collect();

    // Calculate new layout for filtered windows
    let new_layouts = calculate_layout(
        &filtered_infos,
        screen_width,
        screen_height,
        config,
        top_reserved,
    );

    // Remap indices back to original capture indices
    let filtered_indices: Vec<usize> = captures
        .iter()
        .enumerate()
        .filter(|(i, _)| !removed_windows.contains(i))
        .map(|(i, _)| i)
        .collect();

    new_layouts
        .into_iter()
        .enumerate()
        .map(|(new_idx, mut layout)| {
            layout.window_index = filtered_indices[new_idx];
            layout
        })
        .collect()
}

use animation::{calculate_exit_layouts, calculate_start_layouts, AnimationConfig, Animator};
use capture::CapturedWindow;
use config::Config;
use connection::XConnection;
use desktop_bar::{DesktopBar, BAR_HEIGHT};
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

    // Initialize desktop bar (if xdeskie is running)
    let mut desktop_bar = match (xconn.get_num_desktops()?, xconn.get_current_desktop()?) {
        (Some(num), Some(current)) if num > 0 => {
            log::info!("Desktop bar enabled: {} desktops, current={}", num, current);
            Some(DesktopBar::new(num, current, xconn.screen_width))
        }
        _ => {
            log::info!("Desktop bar disabled: xdeskie not running");
            None
        }
    };
    let bar_height = desktop_bar.as_ref().map_or(0, |_| BAR_HEIGHT);

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

    // Load xdeskie state for window-to-desktop mappings
    let xdeskie_state = xdeskie::XdeskieState::load();

    // Calculate mini-layouts for desktop previews if xdeskie is running
    if let (Some(ref mut bar), Some(ref state)) = (&mut desktop_bar, &xdeskie_state) {
        bar.calculate_mini_layouts(
            &captures,
            state,
            xconn.screen_width,
            xconn.screen_height,
        );
    }

    // Calculate layout
    let config = LayoutConfig::default();
    let mut layouts = calculate_layout(
        &captures.iter().map(|c| c.info.clone()).collect::<Vec<_>>(),
        xconn.screen_width,
        xconn.screen_height,
        &config,
        bar_height,
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

        // Render desktop bar (with slide-in animation)
        if let Some(ref bar) = desktop_bar {
            let bar_y_offset = (-(bar_height as f64) * (1.0 - progress)) as i16;
            render_desktop_bar(&xconn, &overview, bar, bar_y_offset, None, &captures)?;
        }

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
    if let Some(ref bar) = desktop_bar {
        render_desktop_bar(&xconn, &overview, bar, 0, None, &captures)?;
    }
    render_all_thumbnails(&xconn, &captures, &layouts, &overview, None, None)?;
    xconn.present_overview(&overview)?;

    log::info!("Overview displayed, waiting for input");

    // Event loop
    let mut input_handler = InputHandler::new(layouts.clone(), desktop_bar.clone());
    let mut selected_window: Option<usize> = None;
    let mut last_hovered: Option<usize> = None;
    let mut should_exit = false;

    // Track which windows have pending damage (for batching updates)
    let mut damaged_windows: HashSet<usize> = HashSet::new();

    // Drag animation state
    let mut drag_animation: Option<DragAnimation> = None;
    let mut last_drag_rect: Option<(i16, i16, u16, u16)> = None;
    let mut dragging_window_index: Option<usize> = None; // Window being dragged (to hide from grid)
    let mut removed_windows: HashSet<usize> = HashSet::new(); // Windows removed from grid (moved to desktops)

    // Grid transition animation state
    let mut grid_transition_animation: Option<GridTransitionAnimation> = None;

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
                Event::ButtonRelease(ref e) => input_handler.handle_button_release(e),
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
                InputAction::ActivateDesktop(idx) => {
                    log::info!("Activate desktop {} (UI only)", idx);
                    // TODO: Actually switch desktop when xdeskie integration is done
                }
                InputAction::ClickPlusButton => {
                    log::info!("Plus button clicked (UI only)");
                    // TODO: Add new desktop when xdeskie integration is done
                }
                InputAction::StartDrag(index) => {
                    log::info!("Started dragging window {}", index);
                    dragging_window_index = Some(index);

                    // Calculate and store click offset, then compute drag position
                    if let Some(drag) = input_handler.drag_state_mut() {
                        let capture = &captures[index];
                        let layout = &layouts[index];

                        // Calculate offset from thumbnail center to click point
                        let thumb_center_x = layout.x + (layout.width / 2) as i16;
                        let thumb_center_y = layout.y + (layout.height / 2) as i16;
                        let offset_x = drag.start_x - thumb_center_x;
                        let offset_y = drag.start_y - thumb_center_y;
                        drag.set_click_offset(offset_x, offset_y);

                        // Scale based on Y position relative to snap target size
                        let (scale, _) = calculate_drag_scale_and_target(
                            drag.current_y, drag.start_y, layout, &desktop_bar, &captures[index],
                        );
                        let rect = calculate_drag_rect(
                            drag.current_x, drag.current_y,
                            layout.width, layout.height, scale,
                            drag.click_offset_x, drag.click_offset_y,
                        );
                        last_drag_rect = Some(rect);

                        xconn.clear_overview(&overview)?;
                        if let Some(ref bar) = desktop_bar {
                            render_desktop_bar(&xconn, &overview, bar, 0, input_handler.hovered_desktop(), &captures)?;
                        }
                        render_all_thumbnails(&xconn, &captures, &layouts, &overview, last_hovered, dragging_window_index)?;
                        xconn.render_dragged_window(
                            capture.picture, overview.picture,
                            capture.info.width, capture.info.height,
                            rect.0, rect.1, rect.2, rect.3,
                        )?;
                    }
                    needs_present = true;
                }
                InputAction::DragMove(x, y) => {
                    // Calculate drag scale based on Y position
                    if let Some(drag) = input_handler.drag_state() {
                        let capture = &captures[drag.window_index];
                        let layout = &layouts[drag.window_index];

                        // Scale based on Y position relative to snap target size
                        let (scale, _) = calculate_drag_scale_and_target(
                            y, drag.start_y, layout, &desktop_bar, capture,
                        );
                        let rect = calculate_drag_rect(
                            x, y, layout.width, layout.height, scale,
                            drag.click_offset_x, drag.click_offset_y,
                        );
                        last_drag_rect = Some(rect);

                        xconn.clear_overview(&overview)?;
                        if let Some(ref bar) = desktop_bar {
                            render_desktop_bar(&xconn, &overview, bar, 0, input_handler.hovered_desktop(), &captures)?;
                        }
                        render_all_thumbnails(&xconn, &captures, &layouts, &overview, last_hovered, dragging_window_index)?;
                        xconn.render_dragged_window(
                            capture.picture, overview.picture,
                            capture.info.width, capture.info.height,
                            rect.0, rect.1, rect.2, rect.3,
                        )?;
                    }
                    needs_present = true;
                }
                InputAction::DropOnDesktop(window_idx, desktop_idx) => {
                    log::info!("Dropped window {} on desktop {} (UI only)", window_idx, desktop_idx);
                    // Start snap animation to desktop preview center
                    if let (Some(rect), Some(ref bar)) = (last_drag_rect, &desktop_bar) {
                        if let Some((target_x, target_y)) = bar.get_preview_center(desktop_idx) {
                            // Target size is small (preview size)
                            let capture = &captures[window_idx];
                            let aspect = capture.info.height as f64 / capture.info.width as f64;
                            let target_width = 60u16;
                            let target_height = (60.0 * aspect) as u16;

                            drag_animation = Some(DragAnimation {
                                mode: AnimationMode::SnapToDesktop { desktop_idx: desktop_idx as usize },
                                window_index: window_idx,
                                start_x: rect.0,
                                start_y: rect.1,
                                start_width: rect.2,
                                start_height: rect.3,
                                end_x: target_x - (target_width / 2) as i16,
                                end_y: target_y - (target_height / 2) as i16,
                                end_width: target_width,
                                end_height: target_height,
                                start_time: Instant::now(),
                                duration_ms: SNAP_DURATION_MS,
                            });
                        }
                    }
                    last_drag_rect = None;
                    needs_present = true;
                }
                InputAction::CancelDrag => {
                    log::debug!("Drag cancelled");
                    // Start revert animation back to grid position
                    if let Some(rect) = last_drag_rect {
                        // Find the window's grid layout position
                        if let Some(drag) = input_handler.drag_state() {
                            let layout = &layouts[drag.window_index];
                            drag_animation = Some(DragAnimation {
                                mode: AnimationMode::RevertToGrid,
                                window_index: drag.window_index,
                                start_x: rect.0,
                                start_y: rect.1,
                                start_width: rect.2,
                                start_height: rect.3,
                                end_x: layout.x,
                                end_y: layout.y,
                                end_width: layout.width,
                                end_height: layout.height,
                                start_time: Instant::now(),
                                duration_ms: REVERT_DURATION_MS,
                            });
                        }
                    }
                    last_drag_rect = None;
                    // Keep dragging_window_index set until animation completes
                    needs_present = true;
                }
                InputAction::HoverDesktop(desktop_idx) => {
                    log::debug!("Hover desktop: {:?}", desktop_idx);
                    // Redraw desktop bar with hover highlight
                    if let Some(ref bar) = desktop_bar {
                        render_desktop_bar(&xconn, &overview, bar, 0, desktop_idx, &captures)?;
                        needs_present = true;
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
            // Also re-render the desktop bar so mini-thumbnails update
            if let Some(ref bar) = desktop_bar {
                render_desktop_bar(&xconn, &overview, bar, 0, input_handler.hovered_desktop(), &captures)?;
            }
            damaged_windows.clear();
            needs_present = true;
        }

        // Process drag animation frames
        if let Some(ref anim) = drag_animation {
            let (ax, ay, aw, ah) = anim.current_position();
            let capture = &captures[anim.window_index];

            xconn.clear_overview(&overview)?;
            if let Some(ref bar) = desktop_bar {
                render_desktop_bar(&xconn, &overview, bar, 0, None, &captures)?;
            }
            // Hide the animating window from the grid during animation
            render_all_thumbnails(&xconn, &captures, &layouts, &overview, last_hovered, dragging_window_index)?;
            xconn.render_dragged_window(
                capture.picture, overview.picture,
                capture.info.width, capture.info.height,
                ax, ay, aw, ah,
            )?;
            needs_present = true;

            if anim.is_complete() {
                match anim.mode {
                    AnimationMode::SnapToDesktop { desktop_idx } => {
                        // Get window ID for xdeskie (use client window)
                        let window_id = captures[anim.window_index].info.client_window;

                        // Call xdeskie to actually move the window
                        // desktop_idx is 0-based in our UI, xdeskie uses 1-based
                        let xdeskie_desktop = desktop_idx + 1;
                        let result = std::process::Command::new("xdeskie")
                            .args(["move", &format!("0x{:x}", window_id), &xdeskie_desktop.to_string()])
                            .output();

                        match result {
                            Ok(output) if output.status.success() => {
                                log::info!("Moved window 0x{:x} to desktop {}", window_id, xdeskie_desktop);
                            }
                            Ok(output) => {
                                log::warn!("xdeskie move failed: {}", String::from_utf8_lossy(&output.stderr));
                            }
                            Err(e) => {
                                log::warn!("Failed to run xdeskie: {}", e);
                            }
                        }

                        // Reload xdeskie state and recalculate mini-layouts for desktop previews
                        if let Some(ref mut bar) = desktop_bar {
                            if let Some(state) = xdeskie::XdeskieState::load() {
                                bar.calculate_mini_layouts(
                                    &captures,
                                    &state,
                                    xconn.screen_width,
                                    xconn.screen_height,
                                );
                            }
                        }

                        // Window was dropped on desktop - remove it from grid
                        removed_windows.insert(anim.window_index);

                        // Store old layouts before recalculation
                        let old_layouts = layouts.clone();

                        // Recalculate layout for remaining windows
                        let new_layouts = recalculate_filtered_layout(
                            &captures,
                            &removed_windows,
                            xconn.screen_width,
                            xconn.screen_height,
                            &config,
                            bar_height,
                        );

                        // Start grid transition animation
                        grid_transition_animation = Some(GridTransitionAnimation::new(
                            &old_layouts,
                            &new_layouts,
                            GRID_TRANSITION_DURATION_MS,
                        ));

                        // Update layouts to new positions (animation will interpolate)
                        layouts = new_layouts;

                        // Update input handler with new layouts
                        input_handler.update_layouts(layouts.clone());

                        log::info!("Window {} removed from grid, moved to desktop {} - animating {} windows to new positions",
                                  anim.window_index, desktop_idx, layouts.len());
                    }
                    AnimationMode::RevertToGrid => {
                        // Window was dropped outside desktop - just return to grid
                        // (No removal, window already in layouts)
                    }
                }

                drag_animation = None;
                dragging_window_index = None;
            }
        }

        // Process grid transition animation frames
        if let Some(ref anim) = grid_transition_animation {
            let current_layouts = anim.current_layouts();

            xconn.clear_overview(&overview)?;
            if let Some(ref bar) = desktop_bar {
                render_desktop_bar(&xconn, &overview, bar, 0, None, &captures)?;
            }

            // Render thumbnails at interpolated positions
            for layout in &current_layouts {
                let capture = &captures[layout.window_index];
                xconn.render_thumbnail(
                    capture.picture,
                    overview.picture,
                    capture.info.width,
                    capture.info.height,
                    layout,
                )?;
            }
            needs_present = true;

            if anim.is_complete() {
                grid_transition_animation = None;
                // Final render with exact final positions
                xconn.clear_overview(&overview)?;
                if let Some(ref bar) = desktop_bar {
                    render_desktop_bar(&xconn, &overview, bar, 0, None, &captures)?;
                }
                render_all_thumbnails(&xconn, &captures, &layouts, &overview, last_hovered, None)?;
                needs_present = true;
            }
        }

        if needs_present {
            xconn.present_overview(&overview)?;
        }

        // Continue animation loop if animation is active
        if drag_animation.is_some() || grid_transition_animation.is_some() {
            thread::sleep(std::time::Duration::from_millis(16)); // ~60fps
            continue;
        }
    }

    // Run exit animation - fade in skipped windows while animating managed windows back
    {
        let (exit_start, exit_end) = calculate_exit_layouts(&windows_info, &layouts);
        let exit_animator = Animator::new(exit_start, exit_end, &exit_anim);

        // Build render order: original Z-order (bottom to top), with selected window last
        // Map from original_stacking_order (frame IDs) to indices in captures array
        // Filter out removed windows
        let mut render_order: Vec<usize> = Vec::new();
        for frame in &original_stacking_order {
            if let Some(idx) = captures.iter().position(|c| c.info.frame_window == *frame) {
                if !removed_windows.contains(&idx) && Some(idx) != selected_window {
                    render_order.push(idx);
                }
            }
        }
        // Add selected window last (renders on top) if it wasn't removed
        if let Some(idx) = selected_window {
            if !removed_windows.contains(&idx) {
                render_order.push(idx);
            }
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
                // Find the layout for this window index
                if let Some(layout) = current.iter().find(|l| l.window_index == idx) {
                    xconn.render_thumbnail_animated(
                        captures[idx].picture,
                        overview.picture,
                        captures[idx].info.width,
                        captures[idx].info.height,
                        layout,
                    )?;
                }
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

/// Render all thumbnails with optional highlight and optional exclusion.
fn render_all_thumbnails(
    xconn: &XConnection,
    captures: &[CapturedWindow],
    layouts: &[ThumbnailLayout],
    overview: &OverviewWindow,
    highlighted: Option<usize>,
    excluded: Option<usize>,
) -> Result<()> {
    // Iterate over layouts and use window_index to find the correct capture.
    // This is necessary because after windows are removed, layouts are filtered
    // but captures remain unchanged - layout.window_index maps back to captures.
    for layout in layouts {
        let idx = layout.window_index;
        // Skip excluded window (being dragged)
        if Some(idx) == excluded {
            continue;
        }
        let capture = &captures[idx];
        xconn.render_thumbnail(
            capture.picture,
            overview.picture,
            capture.info.width,
            capture.info.height,
            layout,
        )?;
        xconn.draw_thumbnail_border(overview, layout, Some(idx) == highlighted)?;
    }
    Ok(())
}

/// Redraw a single thumbnail (used for hover updates).
/// `window_index` is the index into captures array (the window_index from layouts).
fn redraw_thumbnail(
    xconn: &XConnection,
    captures: &[CapturedWindow],
    layouts: &[ThumbnailLayout],
    overview: &OverviewWindow,
    window_index: usize,
    highlighted: bool,
) -> Result<()> {
    if window_index >= captures.len() {
        return Ok(());
    }

    // Find the layout for this window_index (can't use array index after filtering)
    let layout = match layouts.iter().find(|l| l.window_index == window_index) {
        Some(l) => l,
        None => return Ok(()), // Window was removed from layouts
    };

    let capture = &captures[window_index];

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

/// Render the desktop bar with all previews and plus button.
fn render_desktop_bar(
    xconn: &XConnection,
    overview: &OverviewWindow,
    desktop_bar: &DesktopBar,
    bar_y_offset: i16,
    hovered_desktop: Option<u32>,
    captures: &[CapturedWindow],
) -> Result<()> {
    // Render bar background
    xconn.render_desktop_bar_background(overview, desktop_bar.bar_height, bar_y_offset)?;

    // Render desktop previews with wallpaper and mini-windows
    for preview in &desktop_bar.preview_layouts {
        let is_hovered = hovered_desktop == Some(preview.desktop_index);
        xconn.render_desktop_preview_full(
            overview,
            preview,
            captures,
            is_hovered,
            bar_y_offset,
        )?;
    }

    // Render plus button
    let pb = &desktop_bar.plus_button;
    xconn.render_plus_button(overview, pb.x, pb.y + bar_y_offset, pb.size, false)?;

    Ok(())
}
