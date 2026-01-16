use x11rb::connection::Connection;
use x11rb::protocol::render::{self, Picture, PictOp, Transform};
use x11rb::protocol::xproto::*;
use crate::animation::AnimatedLayout;
use crate::connection::XConnection;
use crate::error::Result;
use crate::layout::ThumbnailLayout;

// Fixed-point conversion for XRender transforms (16.16 format)
const FIXED_SHIFT: i32 = 16;

fn double_to_fixed(d: f64) -> i32 {
    (d * (1 << FIXED_SHIFT) as f64) as i32
}

pub struct OverviewWindow {
    pub window: Window,
    pub pixmap: Pixmap,
    pub picture: Picture,
    pub gc: Gcontext,
    pub width: u16,
    pub height: u16,
}

impl XConnection {
    /// Create the fullscreen overview window.
    pub fn create_overview_window(&self) -> Result<OverviewWindow> {
        let window = self.generate_id()?;
        let pixmap = self.generate_id()?;
        let gc = self.generate_id()?;

        // Dark background color (0x1a1a1a)
        let bg_color = 0x1a1a1a;

        // Create fullscreen window
        self.conn.create_window(
            self.root_depth,
            window,
            self.root,
            0,
            0,
            self.screen_width,
            self.screen_height,
            0,
            WindowClass::INPUT_OUTPUT,
            self.root_visual,
            &CreateWindowAux::new()
                .background_pixel(bg_color)
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::KEY_PRESS
                        | EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE
                        | EventMask::POINTER_MOTION,
                )
                .override_redirect(1), // Don't let WM manage us
        )?;

        // Create backing pixmap
        self.conn.create_pixmap(
            self.root_depth,
            pixmap,
            window,
            self.screen_width,
            self.screen_height,
        )?;

        // Create graphics context
        self.conn
            .create_gc(gc, window, &CreateGCAux::new().foreground(bg_color))?;

        // Fill pixmap with background color
        self.conn.poly_fill_rectangle(
            pixmap,
            gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: self.screen_width,
                height: self.screen_height,
            }],
        )?;

        // Create picture for the pixmap
        let picture = self.generate_id()?;
        render::create_picture(
            &self.conn,
            picture,
            pixmap,
            self.pict_format_rgb,
            &render::CreatePictureAux::new(),
        )?;

        self.conn.flush()?;

        Ok(OverviewWindow {
            window,
            pixmap,
            picture,
            gc,
            width: self.screen_width,
            height: self.screen_height,
        })
    }

    /// Render a scaled thumbnail using XRender.
    pub fn render_thumbnail(
        &self,
        src_picture: Picture,
        dst_picture: Picture,
        src_width: u16,
        src_height: u16,
        layout: &ThumbnailLayout,
    ) -> Result<()> {
        if layout.width == 0 || layout.height == 0 {
            return Ok(());
        }

        // Calculate scale factor (destination to source, for XRender inverse transform)
        let scale_x = src_width as f64 / layout.width as f64;
        let scale_y = src_height as f64 / layout.height as f64;

        // Create transform matrix for scaling
        let transform = Transform {
            matrix11: double_to_fixed(scale_x),
            matrix12: 0,
            matrix13: 0,
            matrix21: 0,
            matrix22: double_to_fixed(scale_y),
            matrix23: 0,
            matrix31: 0,
            matrix32: 0,
            matrix33: double_to_fixed(1.0),
        };

        // Apply transform to source picture
        render::set_picture_transform(&self.conn, src_picture, transform)?;

        // Set filter for smooth scaling
        // TODO: Add option for "nearest" for faster but pixelated scaling
        render::set_picture_filter(&self.conn, src_picture, b"bilinear", &[])?;

        // Composite source to destination
        render::composite(
            &self.conn,
            PictOp::SRC,
            src_picture,
            x11rb::NONE, // No mask
            dst_picture,
            0,
            0, // Source position (transformed)
            0,
            0, // Mask position
            layout.x,
            layout.y,
            layout.width,
            layout.height,
        )?;

        Ok(())
    }

    /// Draw border around thumbnail.
    pub fn draw_thumbnail_border(
        &self,
        overview: &OverviewWindow,
        layout: &ThumbnailLayout,
        highlighted: bool,
    ) -> Result<()> {
        let border_width: i16 = 3;

        // Choose border color based on highlight state
        let color = if highlighted {
            // Bright cyan for highlighted
            0x44_88_FF
        } else {
            // Dark gray for normal
            0x44_44_44
        };

        // Set foreground color for drawing
        self.conn.change_gc(
            overview.gc,
            &ChangeGCAux::new().foreground(color).line_width(border_width as u32),
        )?;

        // Draw rectangle outline
        let x = layout.x - border_width;
        let y = layout.y - border_width;
        let w = layout.width + 2 * border_width as u16;
        let h = layout.height + 2 * border_width as u16;

        self.conn.poly_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x,
                y,
                width: w,
                height: h,
            }],
        )?;

        Ok(())
    }

    /// Clear thumbnail area (for redraw).
    pub fn clear_thumbnail_area(
        &self,
        overview: &OverviewWindow,
        layout: &ThumbnailLayout,
    ) -> Result<()> {
        let border_width: i16 = 5;
        let bg_color = 0x1a1a1a;

        self.conn
            .change_gc(overview.gc, &ChangeGCAux::new().foreground(bg_color))?;

        let x = layout.x - border_width;
        let y = layout.y - border_width;
        let w = layout.width + 2 * border_width as u16;
        let h = layout.height + 2 * border_width as u16;

        self.conn.poly_fill_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x,
                y,
                width: w,
                height: h,
            }],
        )?;

        Ok(())
    }

    /// Render a scaled thumbnail at animated position.
    pub fn render_thumbnail_animated(
        &self,
        src_picture: Picture,
        dst_picture: Picture,
        src_width: u16,
        src_height: u16,
        layout: &AnimatedLayout,
    ) -> Result<()> {
        if layout.width == 0 || layout.height == 0 {
            return Ok(());
        }

        let scale_x = src_width as f64 / layout.width as f64;
        let scale_y = src_height as f64 / layout.height as f64;

        let transform = Transform {
            matrix11: double_to_fixed(scale_x),
            matrix12: 0,
            matrix13: 0,
            matrix21: 0,
            matrix22: double_to_fixed(scale_y),
            matrix23: 0,
            matrix31: 0,
            matrix32: 0,
            matrix33: double_to_fixed(1.0),
        };

        render::set_picture_transform(&self.conn, src_picture, transform)?;
        render::set_picture_filter(&self.conn, src_picture, b"bilinear", &[])?;

        render::composite(
            &self.conn,
            PictOp::SRC,
            src_picture,
            x11rb::NONE,
            dst_picture,
            0,
            0,
            0,
            0,
            layout.x,
            layout.y,
            layout.width,
            layout.height,
        )?;

        Ok(())
    }

    /// Draw border around animated thumbnail.
    pub fn draw_thumbnail_border_animated(
        &self,
        overview: &OverviewWindow,
        layout: &AnimatedLayout,
        highlighted: bool,
    ) -> Result<()> {
        let border_width: i16 = 3;

        let color = if highlighted {
            0x44_88_FF
        } else {
            0x44_44_44
        };

        self.conn.change_gc(
            overview.gc,
            &ChangeGCAux::new().foreground(color).line_width(border_width as u32),
        )?;

        let x = layout.x - border_width;
        let y = layout.y - border_width;
        let w = layout.width + 2 * border_width as u16;
        let h = layout.height + 2 * border_width as u16;

        self.conn.poly_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x,
                y,
                width: w,
                height: h,
            }],
        )?;

        Ok(())
    }

    /// Clear entire overview pixmap to background color.
    pub fn clear_overview(&self, overview: &OverviewWindow) -> Result<()> {
        let bg_color = 0x1a1a1a;

        self.conn
            .change_gc(overview.gc, &ChangeGCAux::new().foreground(bg_color))?;

        self.conn.poly_fill_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: overview.width,
                height: overview.height,
            }],
        )?;

        Ok(())
    }

    /// Copy rendered content to window.
    pub fn present_overview(&self, overview: &OverviewWindow) -> Result<()> {
        self.conn.copy_area(
            overview.pixmap,
            overview.window,
            overview.gc,
            0,
            0,
            0,
            0,
            overview.width,
            overview.height,
        )?;
        self.conn.flush()?;
        Ok(())
    }

    /// Destroy overview window and free resources.
    pub fn destroy_overview(&self, overview: &OverviewWindow) -> Result<()> {
        render::free_picture(&self.conn, overview.picture)?;
        self.conn.free_gc(overview.gc)?;
        self.conn.free_pixmap(overview.pixmap)?;
        self.conn.destroy_window(overview.window)?;
        self.conn.flush()?;
        Ok(())
    }
}

// TODO: Future enhancements
// - Animation support (fade-in/out)
// - Rounded corners using clip masks
// - Window title labels
// - Drop shadows
