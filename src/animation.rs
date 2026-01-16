use std::time::{Duration, Instant};

use crate::layout::ThumbnailLayout;
use crate::window_finder::WindowInfo;

/// Animation configuration.
pub struct AnimationConfig {
    pub duration: Duration,
    pub fps: u32,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            duration: Duration::from_millis(500),
            fps: 60,
        }
    }
}

/// Interpolated layout for animation frames.
#[derive(Debug, Clone)]
pub struct AnimatedLayout {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub window_index: usize,
}

impl From<&ThumbnailLayout> for AnimatedLayout {
    fn from(layout: &ThumbnailLayout) -> Self {
        Self {
            x: layout.x,
            y: layout.y,
            width: layout.width,
            height: layout.height,
            window_index: layout.window_index,
        }
    }
}

/// Calculate starting layouts based on original window positions.
/// Windows start at their actual screen position, scaled to thumbnail size.
pub fn calculate_start_layouts(
    windows: &[WindowInfo],
    end_layouts: &[ThumbnailLayout],
    screen_width: u16,
    screen_height: u16,
) -> Vec<AnimatedLayout> {
    windows
        .iter()
        .zip(end_layouts.iter())
        .enumerate()
        .map(|(i, (window, end))| {
            // Scale factor from window size to thumbnail size (unused but kept for reference)
            let _scale_x = end.width as f64 / window.width.max(1) as f64;
            let _scale_y = end.height as f64 / window.height.max(1) as f64;

            // Start position: window's actual position, scaled
            // Center the scaled thumbnail at the window's center
            let window_center_x = window.x as f64 + window.width as f64 / 2.0;
            let window_center_y = window.y as f64 + window.height as f64 / 2.0;

            let start_x = (window_center_x - end.width as f64 / 2.0) as i16;
            let start_y = (window_center_y - end.height as f64 / 2.0) as i16;

            // Clamp to screen bounds
            let start_x = start_x.max(0).min(screen_width as i16 - end.width as i16);
            let start_y = start_y.max(0).min(screen_height as i16 - end.height as i16);

            AnimatedLayout {
                x: start_x,
                y: start_y,
                width: end.width,
                height: end.height,
                window_index: i,
            }
        })
        .collect()
}

/// Ease-out cubic function for smooth deceleration.
fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

/// Interpolate between start and end layouts.
pub fn interpolate_layouts(
    start: &[AnimatedLayout],
    end: &[ThumbnailLayout],
    progress: f64,
) -> Vec<AnimatedLayout> {
    let t = ease_out_cubic(progress.clamp(0.0, 1.0));

    start
        .iter()
        .zip(end.iter())
        .map(|(s, e)| {
            AnimatedLayout {
                x: lerp(s.x as f64, e.x as f64, t) as i16,
                y: lerp(s.y as f64, e.y as f64, t) as i16,
                width: e.width,  // Size stays constant
                height: e.height,
                window_index: s.window_index,
            }
        })
        .collect()
}

/// Linear interpolation.
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Animation state manager.
pub struct Animator {
    start_layouts: Vec<AnimatedLayout>,
    end_layouts: Vec<ThumbnailLayout>,
    start_time: Instant,
    duration: Duration,
    frame_duration: Duration,
}

impl Animator {
    pub fn new(
        start_layouts: Vec<AnimatedLayout>,
        end_layouts: Vec<ThumbnailLayout>,
        config: &AnimationConfig,
    ) -> Self {
        Self {
            start_layouts,
            end_layouts,
            start_time: Instant::now(),
            duration: config.duration,
            frame_duration: Duration::from_secs_f64(1.0 / config.fps as f64),
        }
    }

    /// Get current animation progress (0.0 to 1.0).
    pub fn progress(&self) -> f64 {
        let elapsed = self.start_time.elapsed();
        (elapsed.as_secs_f64() / self.duration.as_secs_f64()).min(1.0)
    }

    /// Check if animation is complete.
    pub fn is_complete(&self) -> bool {
        self.progress() >= 1.0
    }

    /// Get current interpolated layouts.
    pub fn current_layouts(&self) -> Vec<AnimatedLayout> {
        interpolate_layouts(&self.start_layouts, &self.end_layouts, self.progress())
    }

    /// Get the frame duration for timing.
    pub fn frame_duration(&self) -> Duration {
        self.frame_duration
    }

}
