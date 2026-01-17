use crate::window_finder::WindowInfo;

#[derive(Debug, Clone)]
pub struct ThumbnailLayout {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub window_index: usize,
}

pub struct LayoutConfig {
    pub padding: u16,
    pub margin: u16,
    pub max_scale: f64,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            padding: 20,
            margin: 50,
            max_scale: 0.9,
        }
    }
}

/// Calculate thumbnail layouts for all windows in a grid.
/// Windows are assigned to grid positions based on their screen location
/// to preserve spatial relationships (Apple-style layout).
pub fn calculate_layout(
    windows: &[WindowInfo],
    screen_width: u16,
    screen_height: u16,
    config: &LayoutConfig,
) -> Vec<ThumbnailLayout> {
    if windows.is_empty() {
        return Vec::new();
    }

    let available_width = screen_width.saturating_sub(2 * config.margin);
    let available_height = screen_height.saturating_sub(2 * config.margin);

    // Calculate optimal grid dimensions
    let count = windows.len();
    let (cols, rows) = optimal_grid(count, available_width, available_height);

    // Calculate cell size
    let total_h_padding = (cols as u16).saturating_sub(1) * config.padding;
    let total_v_padding = (rows as u16).saturating_sub(1) * config.padding;

    let cell_width = available_width.saturating_sub(total_h_padding) / cols as u16;
    let cell_height = available_height.saturating_sub(total_v_padding) / rows as u16;

    // Grid dimensions for cell center calculations
    let grid_width = (cols as u16 * cell_width) + ((cols as u16).saturating_sub(1) * config.padding);
    let grid_height = (rows as u16 * cell_height) + ((rows as u16).saturating_sub(1) * config.padding);
    let grid_offset_x = (screen_width.saturating_sub(grid_width)) / 2;
    let grid_offset_y = (screen_height.saturating_sub(grid_height)) / 2;

    let windows_in_last_row = count % cols;
    let last_row_cols = if windows_in_last_row == 0 { cols } else { windows_in_last_row };

    let cell_assignments = if rows == 1 {
        // Single row: sort by X position to preserve left-to-right ordering
        let mut indexed: Vec<(usize, f64)> = windows
            .iter()
            .enumerate()
            .map(|(i, w)| (i, w.x as f64 + w.width as f64 / 2.0))
            .collect();
        indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

        let mut assignments = vec![0; count];
        for (cell_idx, (win_idx, _)) in indexed.iter().enumerate() {
            assignments[*win_idx] = cell_idx;
        }
        assignments
    } else {
        // Multi-row: use spatial matching (greedy assignment to nearest cell)
        let window_centers: Vec<(usize, f64, f64)> = windows
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let center_x = (w.x as f64 + w.width as f64 / 2.0) / screen_width as f64;
                let center_y = (w.y as f64 + w.height as f64 / 2.0) / screen_height as f64;
                (i, center_x.clamp(0.0, 1.0), center_y.clamp(0.0, 1.0))
            })
            .collect();

        let cell_centers: Vec<(usize, f64, f64)> = (0..count)
            .map(|cell_idx| {
                let col = cell_idx % cols;
                let row = cell_idx / cols;

                // Account for last row centering
                let row_offset_x = if row == rows - 1 && last_row_cols < cols {
                    let last_row_width = (last_row_cols as u16 * cell_width) +
                        ((last_row_cols as u16).saturating_sub(1) * config.padding);
                    (grid_width.saturating_sub(last_row_width)) / 2
                } else {
                    0
                };

                // Actual cell center in screen coordinates
                let cell_x = grid_offset_x + row_offset_x +
                    col as u16 * (cell_width + config.padding) + cell_width / 2;
                let cell_y = grid_offset_y +
                    row as u16 * (cell_height + config.padding) + cell_height / 2;

                // Normalize to [0,1]
                (cell_idx, cell_x as f64 / screen_width as f64, cell_y as f64 / screen_height as f64)
            })
            .collect();

        let mut assignments: Vec<usize> = vec![usize::MAX; count];
        let mut used_cells: Vec<bool> = vec![false; count];

        for _ in 0..count {
            let mut best_dist = f64::MAX;
            let mut best_window = 0;
            let mut best_cell = 0;

            for &(win_idx, wx, wy) in &window_centers {
                if assignments[win_idx] != usize::MAX {
                    continue;
                }
                for &(cell_idx, cx, cy) in &cell_centers {
                    if used_cells[cell_idx] {
                        continue;
                    }
                    let dist = (wx - cx).powi(2) + (wy - cy).powi(2);
                    if dist < best_dist {
                        best_dist = dist;
                        best_window = win_idx;
                        best_cell = cell_idx;
                    }
                }
            }

            assignments[best_window] = best_cell;
            used_cells[best_cell] = true;
        }
        assignments
    };

    // Build layouts based on assignments
    let mut layouts = Vec::with_capacity(count);

    for (i, window) in windows.iter().enumerate() {
        let cell_idx = cell_assignments[i];
        let col = cell_idx % cols;
        let row = cell_idx / cols;

        // For the last row, center it if it has fewer items
        let row_offset_x = if row == rows - 1 && last_row_cols < cols {
            let last_row_width = (last_row_cols as u16 * cell_width) +
                ((last_row_cols as u16).saturating_sub(1) * config.padding);
            (grid_width.saturating_sub(last_row_width)) / 2
        } else {
            0
        };

        // Calculate cell position (centered grid + row centering for last row)
        let cell_x = grid_offset_x as i16 + row_offset_x as i16 +
            (col as u16 * (cell_width + config.padding)) as i16;
        let cell_y = grid_offset_y as i16 +
            (row as u16 * (cell_height + config.padding)) as i16;

        // Scale window to fit in cell while preserving aspect ratio
        let (thumb_width, thumb_height) = scale_to_fit(
            window.width,
            window.height,
            cell_width,
            cell_height,
            config.max_scale,
        );

        // Center thumbnail within cell
        let x = cell_x + ((cell_width.saturating_sub(thumb_width)) / 2) as i16;
        let y = cell_y + ((cell_height.saturating_sub(thumb_height)) / 2) as i16;

        layouts.push(ThumbnailLayout {
            x,
            y,
            width: thumb_width,
            height: thumb_height,
            window_index: i,
        });
    }

    layouts
}

/// Calculate optimal grid dimensions for N windows.
fn optimal_grid(count: usize, width: u16, height: u16) -> (usize, usize) {
    if count == 0 {
        return (1, 1);
    }

    let aspect_ratio = width as f64 / height as f64;

    // Calculate cols to roughly match screen aspect ratio
    let cols_f = (count as f64 * aspect_ratio).sqrt();
    let cols = (cols_f.ceil() as usize).max(1);
    let rows = ((count + cols - 1) / cols).max(1); // Ceiling division

    (cols, rows)
}

/// Scale dimensions to fit within bounds while preserving aspect ratio.
fn scale_to_fit(
    src_width: u16,
    src_height: u16,
    max_width: u16,
    max_height: u16,
    max_scale: f64,
) -> (u16, u16) {
    if src_width == 0 || src_height == 0 {
        return (max_width, max_height);
    }

    let scale_x = max_width as f64 / src_width as f64;
    let scale_y = max_height as f64 / src_height as f64;
    let scale = scale_x.min(scale_y).min(max_scale);

    let width = (src_width as f64 * scale) as u16;
    let height = (src_height as f64 * scale) as u16;

    (width.max(1), height.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_optimal_grid() {
        // 1 window -> 1x1
        assert_eq!(optimal_grid(1, 1920, 1080), (1, 1));
        // 2 windows on widescreen -> 2x1
        assert_eq!(optimal_grid(2, 1920, 1080), (2, 1));
        // 4 windows -> 2x2 or 3x2 depending on aspect
        let (cols, rows) = optimal_grid(4, 1920, 1080);
        assert!(cols * rows >= 4);
    }

    #[test]
    fn test_scale_to_fit() {
        // 1920x1080 into 400x300 cell
        let (w, h) = scale_to_fit(1920, 1080, 400, 300, 1.0);
        // Should scale down proportionally
        assert!(w <= 400);
        assert!(h <= 300);
        // Aspect ratio preserved (16:9)
        let ratio = w as f64 / h as f64;
        assert!((ratio - 16.0 / 9.0).abs() < 0.1);
    }
}
