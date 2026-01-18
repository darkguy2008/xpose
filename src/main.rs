mod animation;
mod capture;
mod config;
mod connection;
mod desktop;
mod desktop_bar;
mod error;
mod input;
mod layout;
mod renderer;
mod state;
mod window_finder;

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

use animation::{AnimatedLayout, AnimationConfig, Animator};
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
    // Initialize logging to /tmp/xpose.log (append mode)
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("/tmp/xpose.log")
        .expect("Failed to open log file");

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .init();

    if let Err(e) = run() {
        log::error!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    log::info!("========================================");
    log::info!("Starting xpose - new session");
    log::info!("========================================");

    // Load configuration
    let config = Config::load();
    let animation_speed = if config.animation_speed > 0.0 {
        config.animation_speed
    } else {
        1.0
    };
    let entrance_anim = AnimationConfig::new(scale_duration(config.entrance_duration(), animation_speed));
    let exit_anim = AnimationConfig::new(scale_duration(config.exit_duration(), animation_speed));
    let snap_duration_ms = scale_duration_ms(SNAP_DURATION_MS, animation_speed);
    let revert_duration_ms = scale_duration_ms(REVERT_DURATION_MS, animation_speed);
    let grid_transition_duration_ms = scale_duration_ms(GRID_TRANSITION_DURATION_MS, animation_speed);

    // Connect to X server
    let xconn = XConnection::new()?;
    log::info!(
        "Connected to X server, screen {}x{}",
        xconn.screen_width,
        xconn.screen_height
    );

    // Load desktop state (always enabled now)
    let mut desktop_state = desktop::DesktopState::load()?;

    // Sync from X properties if they exist (for compatibility)
    desktop_state.sync_from_x(&xconn)?;

    log::info!(
        "Desktop state: {} desktops, current={}",
        desktop_state.desktops,
        desktop_state.current
    );

    // Initialize desktop bar
    let mut desktop_bar = Some(DesktopBar::new(
        desktop_state.desktops,
        desktop_state.current,
        xconn.screen_width,
    ));
    let bar_height = BAR_HEIGHT;

    // Find ALL windows including unmapped ones (for virtual desktop support)
    // original_stacking_order contains frame window IDs in their X11 stacking order (bottom-to-top)
    let (mut windows, skipped_windows, original_stacking_order) =
        xconn.find_all_windows(&config.exclude_classes)?;

    // Log existing window assignments from loaded state
    log::info!("Loaded desktop state has {} window assignments:", desktop_state.windows.len());
    for (key, &desktop) in &desktop_state.windows {
        log::info!("  Window 0x{} -> desktop {}", key, desktop);
    }

    // Assign any new windows to the current desktop
    // Windows that were already tracked keep their assignments
    log::info!("Processing {} windows (current desktop = {}):", windows.len(), desktop_state.current);
    for info in &windows {
        let was_known = desktop_state.windows.contains_key(&info.frame_window.to_string());
        let assigned = desktop_state.get_window_desktop(info.frame_window, desktop_state.current);
        if !was_known {
            log::info!("  NEW: {:?} (0x{:x}) -> desktop {}",
                info.wm_name.as_deref().unwrap_or("?"), info.frame_window, assigned);
        } else {
            log::info!("  existing: {:?} (0x{:x}) on desktop {}",
                info.wm_name.as_deref().unwrap_or("?"), info.frame_window, assigned);
        }
    }
    desktop_state.save()?;

    if windows.is_empty() {
        log::info!("No windows to display");
        return Ok(());
    }

    let current_desktop = desktop_state.current;
    let current_window_ids: HashSet<Window> = desktop_state
        .windows_on_desktop(current_desktop)
        .into_iter()
        .collect();

    // Create the overview window (but don't map it yet - wait until captures are complete)
    let overview = xconn.create_overview_window()?;

    // Grab the X server while restacking and mapping to avoid intermediate paints.
    xconn.conn.grab_server()?;

    // Move windows from OTHER desktops off-screen BEFORE mapping to prevent flicker.
    // When these windows get mapped, they'll be invisible because they're off-screen.
    let offscreen_x = -(xconn.screen_width as i32 * 2);
    for info in &windows {
        if !current_window_ids.contains(&info.frame_window) {
            xconn.conn.configure_window(
                info.frame_window,
                &ConfigureWindowAux::new().x(offscreen_x),
            )?;
        }
    }

    // Keep all windows below the overview to avoid visible flashes while mapping.
    for info in &windows {
        xconn.conn.configure_window(
            info.frame_window,
            &ConfigureWindowAux::new()
                .sibling(overview.window)
                .stack_mode(StackMode::BELOW),
        )?;
        xconn.conn.configure_window(
            info.client_window,
            &ConfigureWindowAux::new()
                .sibling(overview.window)
                .stack_mode(StackMode::BELOW),
        )?;
    }
    // Map all windows so we can capture them (they will be unmapped on exit as needed)
    let mapped_any = desktop::map_all_windows(&xconn, &windows)?;
    xconn.flush()?;
    log::info!("Mapped all {} windows for live capture", windows.len());
    // Give X server time to process all maps and make windows ready for capture
    xconn.sync()?;
    if mapped_any {
        // Extra delay for windows that were unmapped - they need time to redraw
        std::thread::sleep(std::time::Duration::from_millis(50));
        xconn.sync()?;
    }
    xconn.conn.ungrab_server()?;
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
    // Failed captures get placeholders that we'll try to upgrade during animation
    let mut captures: Vec<CapturedWindow> = Vec::new();
    let mut placeholder_indices: HashSet<usize> = HashSet::new();
    for window in &windows {
        match xconn.capture_window(window) {
            Ok(capture) => captures.push(capture),
            Err(e) => {
                log::debug!("Capture failed for {:?}, using placeholder: {}", window.wm_name, e);
                // Create placeholder so window still appears in layout
                match xconn.create_placeholder_capture(window) {
                    Ok(placeholder) => {
                        placeholder_indices.insert(captures.len());
                        captures.push(placeholder);
                    }
                    Err(e2) => log::warn!("Failed to create placeholder for {:?}: {}", window.wm_name, e2),
                }
            }
        }
    }

    if captures.is_empty() {
        log::info!("No windows could be captured");
        xconn.destroy_overview(&overview)?;
        return Ok(());
    }

    // Capture skipped windows (for fade effect) - no placeholders needed
    let mut skipped_captures: Vec<CapturedWindow> = Vec::new();
    for window in &skipped_windows {
        match xconn.capture_window(window) {
            Ok(capture) => skipped_captures.push(capture),
            Err(e) => log::debug!("Skipped window {:?} not captured: {}", window.wm_name, e),
        }
    }

    // NOTE: Windows from other desktops stay off-screen during xpose's run.
    // They'll be restored on exit via restore_window_visibility (which unmaps them anyway).
    // The exit animation uses stored WindowInfo positions, not current window positions.

    // Update stacking order for the CURRENT desktop only from the X11 stacking order.
    // Other desktops keep their saved stacking orders since X11 only knows the
    // accurate stacking for mapped (visible) windows.
    let current_desktop_stacking: Vec<Window> = original_stacking_order
        .iter()
        .copied()
        .filter(|&frame| current_window_ids.contains(&frame))
        .collect();
    log::info!("Updating stacking for current desktop {} with {} windows:", current_desktop, current_desktop_stacking.len());
    for (i, &frame) in current_desktop_stacking.iter().enumerate() {
        let name = captures.iter()
            .find(|c| c.info.frame_window == frame)
            .and_then(|c| c.info.wm_name.as_deref())
            .unwrap_or("?");
        log::info!("  [{}] {:?} (0x{:x})", i, name, frame);
    }
    desktop_state.stacking.insert(
        current_desktop,
        current_desktop_stacking.iter().map(|id| id.to_string()).collect(),
    );

    // Log stacking for other desktops (from saved state)
    for desk in 0..desktop_state.desktops {
        if desk != current_desktop {
            if let Some(order) = desktop_state.stacking.get(&desk) {
                log::info!("Desktop {} stacking (from saved state): {} windows", desk, order.len());
                for (i, id_str) in order.iter().enumerate() {
                    if let Ok(frame) = id_str.parse::<Window>() {
                        let name = captures.iter()
                            .find(|c| c.info.frame_window == frame)
                            .and_then(|c| c.info.wm_name.as_deref())
                            .unwrap_or("?");
                        log::info!("  [{}] {:?} (0x{:x})", i, name, frame);
                    }
                }
            } else {
                log::info!("Desktop {} has NO saved stacking order", desk);
            }
        }
    }

    // Calculate mini-layouts for desktop previews using desktop state
    if let Some(ref mut bar) = desktop_bar {
        bar.calculate_mini_layouts(
            &captures,
            &desktop_state,
            xconn.screen_width,
            xconn.screen_height,
        );
    }

    // Calculate layout for windows on the current desktop only
    let config = LayoutConfig::default();
    let grid_indices: Vec<usize> = captures
        .iter()
        .enumerate()
        .filter(|(_, capture)| current_window_ids.contains(&capture.info.frame_window))
        .map(|(i, _)| i)
        .collect();
    let grid_infos: Vec<window_finder::WindowInfo> = grid_indices
        .iter()
        .map(|&idx| captures[idx].info.clone())
        .collect();
    let mut layouts = calculate_layout(
        &grid_infos,
        xconn.screen_width,
        xconn.screen_height,
        &config,
        bar_height,
    );
    for (layout, &capture_idx) in layouts.iter_mut().zip(grid_indices.iter()) {
        layout.window_index = capture_idx;
    }

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

    // Run entrance animation
    let start_layouts: Vec<AnimatedLayout> = grid_infos
        .iter()
        .zip(grid_indices.iter())
        .map(|(info, &capture_idx)| AnimatedLayout {
            x: info.x,
            y: info.y,
            width: info.width,
            height: info.height,
            window_index: capture_idx,
        })
        .collect();

    let animator = Animator::new(start_layouts, layouts.clone(), &entrance_anim);

    // Build render order from original Z-order (bottom to top)
    let render_order: Vec<usize> = original_stacking_order
        .iter()
        .filter_map(|frame| {
            let capture_idx = captures.iter().position(|c| c.info.frame_window == *frame)?;
            layouts.iter().position(|l| l.window_index == capture_idx)
        })
        .collect();

    // Render first frame before starting the animation loop.
    {
        let current = animator.current_layouts();
        xconn.clear_overview(&overview)?;
        if let Some(ref bar) = desktop_bar {
            let bar_y_offset = -(bar_height as i16);
            render_desktop_bar(&xconn, &overview, bar, bar_y_offset, None, &captures)?;
        }
        // Render skipped windows at full opacity (matches progress=0 in animation loop).
        for capture in &skipped_captures {
            xconn.render_window_with_opacity(
                capture.picture,
                overview.picture,
                capture.info.x,
                capture.info.y,
                capture.info.width,
                capture.info.height,
                1.0,
            )?;
        }
        for &layout_idx in &render_order {
            let layout = &current[layout_idx];
            let idx = layout.window_index;
            xconn.render_thumbnail_animated(
                captures[idx].picture,
                overview.picture,
                captures[idx].info.width,
                captures[idx].info.height,
                layout,
            )?;
            xconn.draw_thumbnail_border_animated(&overview, layout, false)?;
        }

        // Now map the overview window - content is fully rendered so no flash
        xconn.conn.map_window(overview.window)?;
        xconn.present_overview(&overview)?;
    }

    // Grab input before animation
    xconn.conn.grab_keyboard(
        true,
        overview.window,
        x11rb::CURRENT_TIME,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
    )?;
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
        for &layout_idx in &render_order {
            let layout = &current[layout_idx];
            let idx = layout.window_index;
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
    let mut selected_desktop: Option<u32> = None;
    let mut last_hovered: Option<usize> = None;
    let mut should_exit = false;

    // Track which windows have pending damage (for batching updates)
    let mut damaged_windows: HashSet<usize> = HashSet::new();

    // Drag animation state
    let mut drag_animation: Option<DragAnimation> = None;
    let mut last_drag_rect: Option<(i16, i16, u16, u16)> = None;
    let mut dragging_window_index: Option<usize> = None; // Window being dragged (to hide from grid)
    let mut removed_windows: HashSet<usize> = captures
        .iter()
        .enumerate()
        .filter(|(_, capture)| {
            !desktop_state.is_visible_on(capture.info.frame_window, current_desktop)
        })
        .map(|(i, _)| i)
        .collect();

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
                    log::info!("Switching to desktop {}", idx);
                    // Update state (windows stay mapped while xpose is active for live capture)
                    desktop_state.current = idx;
                    desktop_state.sync_to_x(&xconn)?;
                    desktop_state.save()?;

                    // Store selected desktop for zoom animation on exit
                    selected_desktop = Some(idx);
                    should_exit = true;
                }
                InputAction::ClickPlusButton => {
                    log::info!("Adding new desktop");
                    let new_count = desktop_state.desktops + 1;
                    desktop::set_desktop_count(&xconn, &mut desktop_state, &windows, new_count)?;

                    // Recreate desktop bar with new desktop count
                    desktop_bar = Some(DesktopBar::new(
                        desktop_state.desktops,
                        desktop_state.current,
                        xconn.screen_width,
                    ));
                    if let Some(ref mut bar) = desktop_bar {
                        bar.calculate_mini_layouts(
                            &captures,
                            &desktop_state,
                            xconn.screen_width,
                            xconn.screen_height,
                        );
                    }
                    input_handler.update_desktop_bar(desktop_bar.clone());

                    // Redraw
                    xconn.clear_overview(&overview)?;
                    if let Some(ref bar) = desktop_bar {
                        render_desktop_bar(&xconn, &overview, bar, 0, input_handler.hovered_desktop(), &captures)?;
                    }
                    render_all_thumbnails(&xconn, &captures, &layouts, &overview, last_hovered, dragging_window_index)?;
                    needs_present = true;
                }
                InputAction::StartDrag(index) => {
                    if let Some(layout) = find_layout(&layouts, index) {
                        log::info!("Started dragging window {}", index);
                        dragging_window_index = Some(index);

                        // Calculate and store click offset, then compute drag position
                        if let Some(drag) = input_handler.drag_state_mut() {
                            let capture = &captures[index];

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
                    }
                    needs_present = true;
                }
                InputAction::DragMove(x, y) => {
                    // Calculate drag scale based on Y position
                    if let Some(drag) = input_handler.drag_state() {
                        let Some(layout) = find_layout(&layouts, drag.window_index) else {
                            continue;
                        };
                        let capture = &captures[drag.window_index];

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
                                duration_ms: snap_duration_ms,
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
                            if let Some(layout) = find_layout(&layouts, drag.window_index) {
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
                                duration_ms: revert_duration_ms,
                                });
                            }
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

        // Try to upgrade placeholder captures to real ones
        if !placeholder_indices.is_empty() {
            let mut upgraded: Vec<usize> = Vec::new();
            for &idx in &placeholder_indices {
                if idx < captures.len() && xconn.try_upgrade_placeholder(&mut captures[idx]) {
                    upgraded.push(idx);
                    // Re-render this thumbnail with real content
                    let highlighted = last_hovered == Some(idx);
                    redraw_thumbnail(&xconn, &captures, &layouts, &overview, idx, highlighted)?;
                }
            }
            if !upgraded.is_empty() {
                for idx in upgraded {
                    placeholder_indices.remove(&idx);
                }
                // Re-render desktop bar with updated captures
                if let Some(ref bar) = desktop_bar {
                    render_desktop_bar(&xconn, &overview, bar, 0, input_handler.hovered_desktop(), &captures)?;
                }
                needs_present = true;
            }
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
                        // Get window ID (use frame window for state tracking)
                        let window_id = captures[anim.window_index].info.frame_window;

                        // Move window using integrated desktop manager (0-indexed)
                        match desktop::move_window(&xconn, &mut desktop_state, window_id, desktop_idx as u32) {
                            Ok(()) => {
                                log::info!("Moved window 0x{:x} to desktop {}", window_id, desktop_idx);
                            }
                            Err(e) => {
                                log::warn!("Failed to move window: {}", e);
                            }
                        }

                        // Recalculate mini-layouts for desktop previews
                        if let Some(ref mut bar) = desktop_bar {
                            bar.calculate_mini_layouts(
                                &captures,
                                &desktop_state,
                                xconn.screen_width,
                                xconn.screen_height,
                            );
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
                            grid_transition_duration_ms,
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

    // Run exit animation
    if let Some(desktop_idx) = selected_desktop {
        // Desktop zoom animation - scale the selected desktop preview to full screen
        if let Some(ref bar) = desktop_bar {
            if let Some(preview) = bar.preview_layouts.iter().find(|p| p.desktop_index == desktop_idx) {
                // Log what mini_windows we're about to animate
                log::info!("Desktop zoom animation for desktop {}", desktop_idx);
                log::info!("Preview mini_windows ({}):", preview.mini_windows.len());
                for (i, mini) in preview.mini_windows.iter().enumerate() {
                    let name = captures.iter()
                        .find(|c| c.info.frame_window == mini.window_id)
                        .and_then(|c| c.info.wm_name.as_deref())
                        .unwrap_or("?");
                    log::info!("  [{}] {:?} (0x{:x})", i, name, mini.window_id);
                }

                // Log the original stacking order (filtered to this desktop)
                log::info!("Original stacking order (windows on desktop {}):", desktop_idx);
                for (i, &frame) in original_stacking_order.iter().enumerate() {
                    if desktop_state.is_visible_on(frame, desktop_idx) {
                        let name = captures.iter()
                            .find(|c| c.info.frame_window == frame)
                            .and_then(|c| c.info.wm_name.as_deref())
                            .unwrap_or("?");
                        log::info!("  [{}] {:?} (0x{:x})", i, name, frame);
                    }
                }

                let exit_animator = Animator::new(vec![], vec![], &exit_anim);

                // Start position: preview in the bar
                let start_x = preview.x as f64;
                let start_y = preview.y as f64;
                let start_w = preview.width as f64;
                let start_h = preview.height as f64;

                // End position: full screen
                let end_x = 0.0_f64;
                let end_y = 0.0_f64;
                let end_w = xconn.screen_width as f64;
                let end_h = xconn.screen_height as f64;

                while !exit_animator.is_complete() {
                    let progress = exit_animator.progress();

                    // Interpolate position and size
                    let cur_x = (start_x + (end_x - start_x) * progress) as i16;
                    let cur_y = (start_y + (end_y - start_y) * progress) as i16;
                    let cur_w = (start_w + (end_w - start_w) * progress) as u16;
                    let cur_h = (start_h + (end_h - start_h) * progress) as u16;

                    xconn.clear_overview(&overview)?;
                    xconn.render_desktop_preview_animated(
                        &overview,
                        preview,
                        &captures,
                        cur_x,
                        cur_y,
                        cur_w,
                        cur_h,
                    )?;
                    xconn.present_overview(&overview)?;
                    thread::sleep(exit_animator.frame_duration());
                }
            }
        }
    } else {
        // Normal window exit animation - fade in skipped windows while animating managed windows back
        let exit_start: Vec<AnimatedLayout> =
            layouts.iter().map(AnimatedLayout::from).collect();
        let exit_end: Vec<ThumbnailLayout> = grid_infos
            .iter()
            .zip(grid_indices.iter())
            .map(|(info, &capture_idx)| ThumbnailLayout {
                x: info.x,
                y: info.y,
                width: info.width,
                height: info.height,
                window_index: capture_idx,
            })
            .collect();
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

    // Restore window visibility based on current desktop (unmap windows on other desktops)
    desktop::restore_window_visibility(&xconn, &desktop_state, &windows)?;
    log::info!("Restored window visibility for desktop {}", desktop_state.current);

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

fn scale_duration(duration: std::time::Duration, speed: f64) -> std::time::Duration {
    let speed = if speed > 0.0 { speed } else { 1.0 };
    let scaled = duration.as_secs_f64() / speed;
    std::time::Duration::from_secs_f64(scaled.max(0.0))
}

fn scale_duration_ms(ms: u64, speed: f64) -> u64 {
    let speed = if speed > 0.0 { speed } else { 1.0 };
    let scaled = (ms as f64 / speed).max(1.0);
    scaled.round() as u64
}

fn find_layout(layouts: &[ThumbnailLayout], window_index: usize) -> Option<&ThumbnailLayout> {
    layouts.iter().find(|l| l.window_index == window_index)
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

    // Draw title label when highlighted
    if highlighted {
        let title = capture.info.wm_name.as_deref().unwrap_or("(untitled)");
        xconn.draw_title_label(overview, layout, title)?;
    }

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
