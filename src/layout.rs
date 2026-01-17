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

    // Screen center for distance calculations (ripple effect)
    let screen_center_x = screen_width as f64 / 2.0;
    let screen_center_y = screen_height as f64 / 2.0;

    let cell_assignments = if rows == 1 {
        // Single row: sort by X position, use distance-from-center as tiebreaker
        let mut indexed: Vec<(usize, f64, f64)> = windows
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let cx = w.x as f64 + w.width as f64 / 2.0;
                let cy = w.y as f64 + w.height as f64 / 2.0;
                (i, cx, cy)
            })
            .collect();

        // Threshold: 10% of screen width - positions within this are "similar"
        let x_threshold = screen_width as f64 * 0.10;

        indexed.sort_by(|a, b| {
            let (_, x_a, y_a) = *a;
            let (_, x_b, y_b) = *b;
            let x_diff = (x_a - x_b).abs();

            if x_diff < x_threshold {
                // Similar positions: use distance from screen center (ripple effect)
                // Closer to center = smaller sort value = inner position
                let dist_a = distance_from_point(x_a, y_a, screen_center_x, screen_center_y);
                let dist_b = distance_from_point(x_b, y_b, screen_center_x, screen_center_y);
                dist_a.partial_cmp(&dist_b).unwrap()
            } else {
                // Different positions: use X coordinate
                x_a.partial_cmp(&x_b).unwrap()
            }
        });

        log::debug!("=== SINGLE ROW SORTING (by X, distance tiebreaker) ===");
        log::debug!("Screen center: ({:.1}, {:.1}), X threshold: {:.1}", screen_center_x, screen_center_y, x_threshold);
        for (win_idx, x, y) in &indexed {
            let dist = distance_from_point(*x, *y, screen_center_x, screen_center_y);
            log::debug!(
                "  window {} at ({:.1}, {:.1}), dist from center={:.1}",
                win_idx, x, y, dist
            );
        }
        log::debug!("=====================================================");

        let mut assignments = vec![0; count];
        for (cell_idx, (win_idx, _, _)) in indexed.iter().enumerate() {
            assignments[*win_idx] = cell_idx;
        }
        assignments
    } else {
        // Multi-row: assign windows to rows by Y position, then sort within rows by X
        let mut indexed: Vec<(usize, f64, f64)> = windows
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let center_x = w.x as f64 + w.width as f64 / 2.0;
                let center_y = w.y as f64 + w.height as f64 / 2.0;
                (i, center_x, center_y)
            })
            .collect();

        // Thresholds for "similar" positions
        let y_threshold = screen_height as f64 * 0.10;
        let x_threshold = screen_width as f64 * 0.10;

        // Sort by Y to determine row assignment, use distance-from-center as tiebreaker
        indexed.sort_by(|a, b| {
            let (_, x_a, y_a) = *a;
            let (_, x_b, y_b) = *b;
            let y_diff = (y_a - y_b).abs();

            if y_diff < y_threshold {
                // Similar Y positions: use distance from center (ripple effect)
                let dist_a = distance_from_point(x_a, y_a, screen_center_x, screen_center_y);
                let dist_b = distance_from_point(x_b, y_b, screen_center_x, screen_center_y);
                dist_a.partial_cmp(&dist_b).unwrap()
            } else {
                y_a.partial_cmp(&y_b).unwrap()
            }
        });

        log::debug!("Grid: {}x{} for {} windows", cols, rows, count);
        log::debug!("Screen center: ({:.1}, {:.1})", screen_center_x, screen_center_y);
        log::debug!("Y threshold: {:.1}, X threshold: {:.1}", y_threshold, x_threshold);
        log::debug!("=== WINDOW POSITIONS (sorted by Y, distance tiebreaker) ===");
        for (i, (win_idx, x, y)) in indexed.iter().enumerate() {
            let dist = distance_from_point(*x, *y, screen_center_x, screen_center_y);
            log::debug!(
                "  [{}] window {} at (x={:.1}, y={:.1}) dist={:.1}",
                i, win_idx, x, y, dist
            );
        }
        log::debug!("==========================================================");

        // Assign windows to rows based on Y quantiles
        let mut row_buckets: Vec<Vec<(usize, f64, f64)>> = vec![Vec::new(); rows];

        for (i, &(win_idx, x, y)) in indexed.iter().enumerate() {
            // Calculate which row this window belongs to based on its position in sorted list
            // Distribute windows as evenly as possible across rows
            let row = (i * rows) / count;
            log::debug!(
                "Window {} (sorted_pos={}, y={:.1}) → ROW {} (formula: {}*{}/{} = {})",
                win_idx, i, y, row, i, rows, count, row
            );
            row_buckets[row].push((win_idx, x, y));
        }

        // Sort each row by X position and assign to cells
        let mut assignments = vec![0; count];

        for (row, mut bucket) in row_buckets.into_iter().enumerate() {
            log::debug!("=== ROW {} ({} windows) ===", row, bucket.len());

            // Sort this row by X (left to right), use distance-from-center as tiebreaker
            bucket.sort_by(|a, b| {
                let (_, x_a, y_a) = *a;
                let (_, x_b, y_b) = *b;
                let x_diff = (x_a - x_b).abs();

                if x_diff < x_threshold {
                    // Similar X positions: use distance from center (ripple effect)
                    let dist_a = distance_from_point(x_a, y_a, screen_center_x, screen_center_y);
                    let dist_b = distance_from_point(x_b, y_b, screen_center_x, screen_center_y);
                    dist_a.partial_cmp(&dist_b).unwrap()
                } else {
                    x_a.partial_cmp(&x_b).unwrap()
                }
            });

            log::debug!("Windows in row {} after X-sort (distance tiebreaker):", row);
            for (pos, (win_idx, x, y)) in bucket.iter().enumerate() {
                let dist = distance_from_point(*x, *y, screen_center_x, screen_center_y);
                log::debug!(
                    "  [{}] window {} at (x={:.1}, y={:.1}) dist={:.1}",
                    pos, win_idx, x, y, dist
                );
            }

            // Assign to cells - position in row + row offset
            for (position, (win_idx, x, y)) in bucket.into_iter().enumerate() {
                let cell_idx = row * cols + position;  // FIX: Respect grid structure
                let col = cell_idx % cols;
                let row_check = cell_idx / cols;

                log::debug!(
                    "  Assigning window {} (x={:.1}, y={:.1}) → cell {} = (col={}, row={})",
                    win_idx, x, y, cell_idx, col, row_check
                );

                assignments[win_idx] = cell_idx;
            }
        }

        log::debug!("=== FINAL CELL ASSIGNMENTS ===");
        for (win_idx, &cell_idx) in assignments.iter().enumerate() {
            let col = cell_idx % cols;
            let row = cell_idx / cols;
            log::debug!(
                "  Window {} → cell {} = (col={}, row={})",
                win_idx, cell_idx, col, row
            );
        }
        log::debug!("================================");

        assignments
    };

    // Count windows per row to determine which rows need centering
    let mut windows_per_row = vec![0usize; rows];
    for &cell_idx in &cell_assignments {
        let row = cell_idx / cols;
        windows_per_row[row] += 1;
    }

    log::debug!("Windows per row: {:?}", windows_per_row);

    // Build layouts based on assignments
    let mut layouts = Vec::with_capacity(count);

    for (i, window) in windows.iter().enumerate() {
        let cell_idx = cell_assignments[i];
        let col = cell_idx % cols;
        let row = cell_idx / cols;

        // Center any row that has fewer items than columns
        let row_window_count = windows_per_row[row];
        let row_offset_x = if row_window_count < cols {
            let row_width = (row_window_count as u16 * cell_width) +
                ((row_window_count as u16).saturating_sub(1) * config.padding);
            (grid_width.saturating_sub(row_width)) / 2
        } else {
            0
        };

        // Calculate cell position (centered grid + row centering for partial rows)
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

/// Calculate Euclidean distance from a point to another point.
fn distance_from_point(x: f64, y: f64, target_x: f64, target_y: f64) -> f64 {
    let dx = x - target_x;
    let dy = y - target_y;
    (dx * dx + dy * dy).sqrt()
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
