# Virtual Desktop Bar - Implementation Progress

## Completed Features

### 1. Desktop Bar UI (Core)
- **Desktop bar at top of screen** with slide-in animation during entrance
- **Centered desktop previews** showing placeholder rectangles for each virtual desktop
- **Current desktop highlighting** with different border color
- **Plus button** on right edge
- **xdeskie integration** via `_XDESKIE_NUM_DESKTOPS` and `_XDESKIE_CURRENT_DESKTOP` atoms
- **Graceful fallback** if xdeskie not running (bar doesn't appear)

### 2. Click Handling
- Click on desktop preview → logs "Activate desktop X (UI only)"
- Click on plus button → logs "Plus button clicked (UI only)"
- Hover highlighting on desktop previews

### 3. Drag and Drop
- **Drag starts** → original thumbnail disappears from grid (no ghosting)
- **Click-anchored positioning** → thumbnail stays where you clicked, doesn't jump to center on cursor
- **Progressive scaling** → window shrinks linearly as it moves from click position toward desktop previews
- **Scale target** → final scale reached when cursor enters desktop preview bounds (not bar edge)
- **Snap animation** → when dropped on desktop, animates into the preview
- **Revert animation** → when dropped elsewhere, animates back to grid position

### 4. Persistent Window Removal (UI-Only)
- **Window removal** → when snap animation completes, window is permanently removed from grid
- **Grid recalculation** → remaining windows automatically reposition/rescale to fill the gap
- **Optimal layout** → grid dimensions recalculate (e.g., 3×2 → 2×2 when window removed)
- **Smooth transitions** → remaining windows animate to new positions over 250ms with ease-out cubic
- **Hit-testing updates** → input handler receives updated layouts for correct click detection
- **Exit animation support** → removed windows excluded from exit animation rendering

## Files Modified

| File | Changes |
|------|---------|
| `src/desktop_bar.rs` | DesktopBar struct, layout calculation, hit testing, get_preview_center() |
| `src/connection.rs` | Added xdeskie atoms and query methods |
| `src/layout.rs` | Added `top_reserved` param to reserve space for bar |
| `src/renderer.rs` | Added bar/preview/plus button/dragged window rendering |
| `src/input.rs` | Extended InputAction enum, DragState with click_offset fields, desktop bar interactions, update_layouts() method |
| `src/main.rs` | DragAnimation struct with AnimationMode, drag handling, scale calculations, animation loop, removed_windows tracking, recalculate_filtered_layout() helper |

## Key Code Locations

### Drag Scale Calculation
`src/main.rs` - `calculate_drag_scale_and_target()` function:
- Interpolates from drag start position (scale=1.0) to desktop preview bottom (scale=target_scale)
- Uses linear interpolation for smooth, even scaling

### Drag Positioning
`src/main.rs` - `calculate_drag_rect()` function:
- Uses click offset to keep thumbnail anchored to where user clicked
- Offset scales proportionally as thumbnail shrinks

### Drag Animation
`src/main.rs` - `DragAnimation` struct:
- Handles both snap (to desktop) and revert (to grid) animations
- 150ms for snap, 200ms for revert
- Uses ease-out cubic interpolation

### Desktop Bar Rendering
`src/main.rs` - `render_desktop_bar()` helper function
`src/renderer.rs` - Individual render methods for bar background, previews, plus button

### Window Removal System
`src/main.rs:28-32` - `AnimationMode` enum to track snap vs. revert animations
`src/main.rs:225-279` - `recalculate_filtered_layout()` helper function:
- Filters out removed windows
- Recalculates grid layout for remaining windows
- Remaps window indices to maintain capture array references

`src/main.rs:521` - `removed_windows: HashSet<usize>` tracks removed window indices
`src/main.rs:785-817` - Animation completion handler:
- Detects snap completion → adds to removed set
- Stores old layouts and recalculates new layouts
- Starts grid transition animation for smooth repositioning

`src/main.rs:842-856` - Exit animation render order filtering:
- Excludes removed windows from exit animation
- Uses `window_index` lookup instead of array indexing

`src/input.rs:133-137` - `update_layouts()` method refreshes hit-testing

### Grid Transition Animation
`src/main.rs:76-145` - `GridTransitionAnimation` struct:
- Maps window indices to (old_layout, new_layout) transitions
- Interpolates position and size with ease-out cubic easing
- Duration: 250ms (GRID_TRANSITION_DURATION_MS)
- Returns current interpolated layouts per frame

`src/main.rs:829-864` - Animation frame processing:
- Renders thumbnails at interpolated positions
- Clears and re-renders entire grid each frame
- Completes with final render at exact target positions

### 5. Live Desktop Previews
- **Wallpaper rendering** → scaled wallpaper shown in each desktop preview
- **Mini window thumbnails** → windows rendered at proportional positions within previews
- **xdeskie state integration** → reads `/tmp/xdeskie/state.json` for window-to-desktop mappings
- **Sticky window support** → windows on desktop 0 appear in all previews

## Files Modified (Live Previews)

| File | Changes |
|------|---------|
| `src/xdeskie.rs` | **NEW** - Parse xdeskie state file for window-to-desktop mappings |
| `src/desktop_bar.rs` | Added `MiniWindowLayout` struct, `calculate_mini_layouts()` method |
| `src/renderer.rs` | Added `render_desktop_preview_full()`, `render_wallpaper_scaled()`, `render_mini_thumbnail()` |
| `src/main.rs` | Integrated xdeskie state loading, updated render_desktop_bar() to use new rendering |

## Key Code Locations (Live Previews)

### xdeskie State Parsing
`src/xdeskie.rs` - `XdeskieState::load()` function:
- Reads `/tmp/xdeskie/state.json`
- Provides `windows_on_desktop(desktop)` method to get window IDs for a desktop

### Mini Layout Calculation
`src/desktop_bar.rs` - `DesktopBar::calculate_mini_layouts()`:
- Scale factors: `142/screen_width` for X, `80/screen_height` for Y
- Maps screen positions to mini-thumbnail positions within previews

### Full Preview Rendering
`src/renderer.rs` - `render_desktop_preview_full()`:
1. Renders scaled wallpaper via `render_wallpaper_scaled()`
2. Renders mini-thumbnails via `render_mini_thumbnail()`
3. Draws border with highlight for current/hovered

## Still TODO (UI Only)

1. **Polish** - fine-tune animation timings, colors, sizes if needed

## Future Work (Functionality)

When migrating xdeskie into xpose:
1. Actually switch desktops on click
2. Actually move windows between desktops on drop
3. Add new desktop on plus button click
4. Show actual window content in desktop previews (per-desktop window grouping)
