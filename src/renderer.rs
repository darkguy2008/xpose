use x11rb::connection::Connection;
use x11rb::protocol::render::{self, Picture, PictOp, Transform};
use x11rb::protocol::xproto::*;
use crate::animation::AnimatedLayout;
use crate::capture::CapturedWindow;
use crate::connection::XConnection;
use crate::desktop_bar::DesktopPreviewLayout;
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
    pub bg_picture: Option<Picture>,
    pub font: Font,
}

impl XConnection {
    /// Attempt to get the root window background pixmap.
    /// Checks _XROOTPMAP_ID first, then ESETROOT_PMAP_ID.
    /// Returns None if no background is set.
    fn get_root_background_pixmap(&self) -> Result<Option<Pixmap>> {
        // Try _XROOTPMAP_ID first (most common - used by feh, nitrogen, hsetroot)
        let reply = self.conn.get_property(
            false,
            self.root,
            self.atoms._XROOTPMAP_ID,
            AtomEnum::PIXMAP,
            0,
            1,
        )?.reply()?;

        if reply.type_ != u32::from(AtomEnum::NONE) && !reply.value.is_empty() {
            if let Some(mut values) = reply.value32() {
                if let Some(pixmap_id) = values.next() {
                    if pixmap_id != 0 {
                        log::debug!("Found root background via _XROOTPMAP_ID: 0x{:x}", pixmap_id);
                        return Ok(Some(pixmap_id));
                    }
                }
            }
        }

        // Fallback to ESETROOT_PMAP_ID
        let reply = self.conn.get_property(
            false,
            self.root,
            self.atoms.ESETROOT_PMAP_ID,
            AtomEnum::PIXMAP,
            0,
            1,
        )?.reply()?;

        if reply.type_ != u32::from(AtomEnum::NONE) && !reply.value.is_empty() {
            if let Some(mut values) = reply.value32() {
                if let Some(pixmap_id) = values.next() {
                    if pixmap_id != 0 {
                        log::debug!("Found root background via ESETROOT_PMAP_ID: 0x{:x}", pixmap_id);
                        return Ok(Some(pixmap_id));
                    }
                }
            }
        }

        log::debug!("No root background pixmap found");
        Ok(None)
    }

    /// Create the fullscreen overview window.
    pub fn create_overview_window(&self) -> Result<OverviewWindow> {
        let window = self.generate_id()?;
        let pixmap = self.generate_id()?;
        let gc = self.generate_id()?;

        // Dark background color (fallback)
        let bg_color = 0x1a1a1a;

        // Try to get root background pixmap and create a picture from it
        let bg_picture = match self.get_root_background_pixmap() {
            Ok(Some(root_pixmap)) => {
                let pic = self.generate_id()?;
                match render::create_picture(
                    &self.conn,
                    pic,
                    root_pixmap,
                    self.pict_format_rgb,
                    &render::CreatePictureAux::new(),
                ) {
                    Ok(_) => {
                        log::info!("Created background picture from root wallpaper");
                        Some(pic)
                    }
                    Err(e) => {
                        log::warn!("Failed to create picture from root background: {}", e);
                        None
                    }
                }
            }
            Ok(None) => {
                log::info!("No root background found, using solid color");
                None
            }
            Err(e) => {
                log::warn!("Error getting root background: {}", e);
                None
            }
        };

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

        // Open a font for text rendering
        let font = self.generate_id()?;
        // Use "fixed" which is always available
        self.conn.open_font(font, b"fixed")?;
        log::info!("Opened font: fixed");

        // Create graphics context
        self.conn
            .create_gc(gc, window, &CreateGCAux::new().foreground(bg_color).font(font))?;

        // Create picture for the pixmap
        let picture = self.generate_id()?;
        render::create_picture(
            &self.conn,
            picture,
            pixmap,
            self.pict_format_rgb,
            &render::CreatePictureAux::new(),
        )?;

        // Fill pixmap with background (wallpaper or solid color)
        if let Some(bg_pic) = bg_picture {
            render::composite(
                &self.conn,
                PictOp::SRC,
                bg_pic,
                x11rb::NONE,
                picture,
                0, 0,
                0, 0,
                0, 0,
                self.screen_width,
                self.screen_height,
            )?;
        } else {
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
        }

        // Set the pixmap as the window's background so it shows immediately when mapped
        // (instead of flashing the background_pixel color first)
        self.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
        )?;

        self.conn.flush()?;

        Ok(OverviewWindow {
            window,
            pixmap,
            picture,
            gc,
            width: self.screen_width,
            height: self.screen_height,
            bg_picture,
            font,
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

    /// Draw window title label centered on thumbnail.
    pub fn draw_title_label(
        &self,
        overview: &OverviewWindow,
        layout: &ThumbnailLayout,
        title: &str,
    ) -> Result<()> {
        // Truncate title if too long
        let max_chars = 50;
        let display_title = if title.len() > max_chars {
            format!("{}...", &title[..max_chars - 3])
        } else {
            title.to_string()
        };

        // "fixed" font is 6x13 pixels per character
        let text_bytes = display_title.as_bytes();
        let char_width: u16 = 6;
        let text_width = (text_bytes.len() as u16) * char_width;
        let text_height: u16 = 13;
        let text_ascent: u16 = 11; // Baseline offset from top

        let padding_h: u16 = 16;
        let padding_v: u16 = 8;

        // Calculate label dimensions
        let label_width = text_width + padding_h * 2;
        let label_height = text_height + padding_v * 2;

        // Center label on thumbnail
        let label_x = layout.x + (layout.width as i16 - label_width as i16) / 2;
        let label_y = layout.y + (layout.height as i16 - label_height as i16) / 2;

        // Draw semi-transparent background rectangle
        let bg_color = 0x22_22_22; // Dark gray
        self.conn.change_gc(
            overview.gc,
            &ChangeGCAux::new().foreground(bg_color),
        )?;
        self.conn.poly_fill_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x: label_x,
                y: label_y,
                width: label_width,
                height: label_height,
            }],
        )?;

        // Draw border around label
        let border_color = 0x88_88_88;
        self.conn.change_gc(
            overview.gc,
            &ChangeGCAux::new().foreground(border_color).line_width(1),
        )?;
        self.conn.poly_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x: label_x,
                y: label_y,
                width: label_width,
                height: label_height,
            }],
        )?;

        // Draw text centered in the label
        let text_color = 0xFF_FF_FF; // White
        self.conn.change_gc(
            overview.gc,
            &ChangeGCAux::new().foreground(text_color).font(overview.font),
        )?;

        // Center text horizontally and vertically
        // X: label_x + padding
        // Y: baseline = label_y + padding_v + text_ascent
        let text_x = label_x + padding_h as i16;
        let text_y = label_y + padding_v as i16 + text_ascent as i16;

        self.conn.image_text8(
            overview.pixmap,
            overview.gc,
            text_x,
            text_y,
            text_bytes,
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

        let x = layout.x - border_width;
        let y = layout.y - border_width;
        let w = layout.width + 2 * border_width as u16;
        let h = layout.height + 2 * border_width as u16;

        if let Some(bg_pic) = overview.bg_picture {
            render::composite(
                &self.conn,
                PictOp::SRC,
                bg_pic,
                x11rb::NONE,
                overview.picture,
                x, y,
                0, 0,
                x, y,
                w,
                h,
            )?;
        } else {
            let bg_color = 0x1a1a1a;
            self.conn
                .change_gc(overview.gc, &ChangeGCAux::new().foreground(bg_color))?;
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
        }

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

    /// Render a window at its original position with opacity (for skipped windows fade effect).
    pub fn render_window_with_opacity(
        &self,
        src_picture: Picture,
        dst_picture: Picture,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        opacity: f64,
    ) -> Result<()> {
        if width == 0 || height == 0 || opacity <= 0.0 {
            return Ok(());
        }

        // Reset transform to identity (1:1 rendering)
        let transform = Transform {
            matrix11: double_to_fixed(1.0),
            matrix12: 0,
            matrix13: 0,
            matrix21: 0,
            matrix22: double_to_fixed(1.0),
            matrix23: 0,
            matrix31: 0,
            matrix32: 0,
            matrix33: double_to_fixed(1.0),
        };

        render::set_picture_transform(&self.conn, src_picture, transform)?;
        render::set_picture_filter(&self.conn, src_picture, b"nearest", &[])?;

        // Create solid fill for alpha mask
        let alpha = (opacity.clamp(0.0, 1.0) * 65535.0) as u16;
        let mask_picture = self.generate_id()?;
        render::create_solid_fill(
            &self.conn,
            mask_picture,
            render::Color {
                red: alpha,
                green: alpha,
                blue: alpha,
                alpha,
            },
        )?;

        // Composite with alpha mask using OVER operator
        render::composite(
            &self.conn,
            PictOp::OVER,
            src_picture,
            mask_picture,
            dst_picture,
            0,
            0,
            0,
            0,
            x,
            y,
            width,
            height,
        )?;

        // Free the mask picture
        render::free_picture(&self.conn, mask_picture)?;

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

    /// Clear entire overview pixmap to background (wallpaper or solid color).
    pub fn clear_overview(&self, overview: &OverviewWindow) -> Result<()> {
        if let Some(bg_pic) = overview.bg_picture {
            render::composite(
                &self.conn,
                PictOp::SRC,
                bg_pic,
                x11rb::NONE,
                overview.picture,
                0, 0,
                0, 0,
                0, 0,
                overview.width,
                overview.height,
            )?;
        } else {
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
        }

        Ok(())
    }

    /// Copy rendered content to window.
    pub fn present_overview(&self, overview: &OverviewWindow) -> Result<()> {
        // Keep overview above all other windows while visible.
        self.conn.configure_window(
            overview.window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )?;
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
        // Free the background picture if we created one
        if let Some(bg_pic) = overview.bg_picture {
            render::free_picture(&self.conn, bg_pic)?;
        }
        render::free_picture(&self.conn, overview.picture)?;
        self.conn.free_gc(overview.gc)?;
        self.conn.free_pixmap(overview.pixmap)?;
        self.conn.destroy_window(overview.window)?;
        self.conn.flush()?;
        Ok(())
    }

    /// Render the desktop bar background.
    pub fn render_desktop_bar_background(
        &self,
        overview: &OverviewWindow,
        bar_height: u16,
        bar_y_offset: i16,
    ) -> Result<()> {
        // Dark semi-transparent background
        let bg_color = 0x1a1a1a;
        self.conn
            .change_gc(overview.gc, &ChangeGCAux::new().foreground(bg_color))?;
        self.conn.poly_fill_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x: 0,
                y: bar_y_offset,
                width: overview.width,
                height: bar_height,
            }],
        )?;
        Ok(())
    }

    /// Render a desktop preview rectangle (simple version, no window content).
    #[allow(dead_code)]
    pub fn render_desktop_preview(
        &self,
        overview: &OverviewWindow,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        is_current: bool,
        is_hovered: bool,
    ) -> Result<()> {
        // Background color
        let bg_color = if is_current { 0x3a3a3a } else { 0x2a2a2a };
        self.conn
            .change_gc(overview.gc, &ChangeGCAux::new().foreground(bg_color))?;
        self.conn.poly_fill_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle { x, y, width, height }],
        )?;

        // Border
        let border_color = if is_current || is_hovered {
            0x4488FF // Highlight
        } else {
            0x444444 // Normal
        };
        let border_width: i16 = 2;
        self.conn.change_gc(
            overview.gc,
            &ChangeGCAux::new()
                .foreground(border_color)
                .line_width(border_width as u32),
        )?;
        self.conn.poly_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle { x, y, width, height }],
        )?;

        Ok(())
    }

    /// Render a desktop preview with wallpaper background and mini window thumbnails.
    pub fn render_desktop_preview_full(
        &self,
        overview: &OverviewWindow,
        preview: &DesktopPreviewLayout,
        captures: &[CapturedWindow],
        is_hovered: bool,
        y_offset: i16,
    ) -> Result<()> {
        let preview_x = preview.x;
        let preview_y = preview.y + y_offset;
        let preview_w = preview.width;
        let preview_h = preview.height;

        // 1. Render scaled wallpaper as background
        if let Some(bg_pic) = overview.bg_picture {
            self.render_wallpaper_scaled(
                bg_pic,
                overview.picture,
                preview_x,
                preview_y,
                preview_w,
                preview_h,
            )?;
        } else {
            // Fallback: solid color background
            let bg_color = if preview.is_current { 0x3a3a3a } else { 0x2a2a2a };
            self.conn
                .change_gc(overview.gc, &ChangeGCAux::new().foreground(bg_color))?;
            self.conn.poly_fill_rectangle(
                overview.pixmap,
                overview.gc,
                &[Rectangle {
                    x: preview_x,
                    y: preview_y,
                    width: preview_w,
                    height: preview_h,
                }],
            )?;
        }

        // 2. Render mini-window thumbnails
        for mini in &preview.mini_windows {
            // Find the capture by frame window ID
            if let Some(capture) = captures.iter().find(|c| c.info.frame_window == mini.window_id) {
                self.render_mini_thumbnail(
                    capture.picture,
                    overview.picture,
                    capture.info.width,
                    capture.info.height,
                    preview_x + mini.x,
                    preview_y + mini.y,
                    mini.width,
                    mini.height,
                )?;
            }
        }

        // 3. Draw border
        let border_color = if preview.is_current || is_hovered {
            0x4488FF
        } else {
            0x444444
        };
        let border_width: i16 = 2;
        self.conn.change_gc(
            overview.gc,
            &ChangeGCAux::new()
                .foreground(border_color)
                .line_width(border_width as u32),
        )?;
        self.conn.poly_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x: preview_x,
                y: preview_y,
                width: preview_w,
                height: preview_h,
            }],
        )?;

        Ok(())
    }

    /// Render wallpaper scaled to fit within a preview rectangle.
    fn render_wallpaper_scaled(
        &self,
        src_picture: Picture,
        dst_picture: Picture,
        dst_x: i16,
        dst_y: i16,
        dst_width: u16,
        dst_height: u16,
    ) -> Result<()> {
        // XRender transforms work in reverse: we specify how to map
        // destination coords back to source coords
        // scale = src_size / dst_size
        let scale_x = self.screen_width as f64 / dst_width as f64;
        let scale_y = self.screen_height as f64 / dst_height as f64;

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
            0, // Source position (transformed)
            0,
            0, // Mask position
            dst_x,
            dst_y,
            dst_width,
            dst_height,
        )?;

        // Reset transform to identity for other operations
        let identity = Transform {
            matrix11: double_to_fixed(1.0),
            matrix12: 0,
            matrix13: 0,
            matrix21: 0,
            matrix22: double_to_fixed(1.0),
            matrix23: 0,
            matrix31: 0,
            matrix32: 0,
            matrix33: double_to_fixed(1.0),
        };
        render::set_picture_transform(&self.conn, src_picture, identity)?;

        Ok(())
    }

    /// Render a mini window thumbnail at the specified position.
    fn render_mini_thumbnail(
        &self,
        src_picture: Picture,
        dst_picture: Picture,
        src_width: u16,
        src_height: u16,
        dst_x: i16,
        dst_y: i16,
        dst_width: u16,
        dst_height: u16,
    ) -> Result<()> {
        if dst_width == 0 || dst_height == 0 {
            return Ok(());
        }

        let scale_x = src_width as f64 / dst_width as f64;
        let scale_y = src_height as f64 / dst_height as f64;

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
        // Use bilinear filtering for smooth scaling
        render::set_picture_filter(&self.conn, src_picture, b"bilinear", &[])?;

        render::composite(
            &self.conn,
            PictOp::OVER, // OVER to handle window transparency
            src_picture,
            x11rb::NONE,
            dst_picture,
            0,
            0,
            0,
            0,
            dst_x,
            dst_y,
            dst_width,
            dst_height,
        )?;

        Ok(())
    }

    /// Render a window being dragged at a specific position and size.
    /// Used for drag feedback and snap/revert animations.
    pub fn render_dragged_window(
        &self,
        src_picture: Picture,
        dst_picture: Picture,
        src_width: u16,
        src_height: u16,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
    ) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        // Calculate scale factor (destination to source, for XRender inverse transform)
        let scale_x = src_width as f64 / width as f64;
        let scale_y = src_height as f64 / height as f64;

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
            PictOp::OVER,
            src_picture,
            x11rb::NONE,
            dst_picture,
            0,
            0,
            0,
            0,
            x,
            y,
            width,
            height,
        )?;

        Ok(())
    }

    /// Render the plus button.
    pub fn render_plus_button(
        &self,
        overview: &OverviewWindow,
        x: i16,
        y: i16,
        size: u16,
        is_hovered: bool,
    ) -> Result<()> {
        // Background circle (approximated with filled rectangle for now)
        let bg_color = if is_hovered { 0x555555 } else { 0x444444 };
        self.conn
            .change_gc(overview.gc, &ChangeGCAux::new().foreground(bg_color))?;
        self.conn.poly_fill_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x,
                y,
                width: size,
                height: size,
            }],
        )?;

        // Draw "+" symbol
        let plus_color = 0xCCCCCC;
        let line_width = 3u16;
        let margin = size / 4;
        self.conn.change_gc(
            overview.gc,
            &ChangeGCAux::new()
                .foreground(plus_color)
                .line_width(line_width as u32),
        )?;

        // Horizontal line
        let h_y = y + (size / 2) as i16;
        self.conn.poly_segment(
            overview.pixmap,
            overview.gc,
            &[Segment {
                x1: x + margin as i16,
                y1: h_y,
                x2: x + (size - margin) as i16,
                y2: h_y,
            }],
        )?;

        // Vertical line
        let v_x = x + (size / 2) as i16;
        self.conn.poly_segment(
            overview.pixmap,
            overview.gc,
            &[Segment {
                x1: v_x,
                y1: y + margin as i16,
                x2: v_x,
                y2: y + (size - margin) as i16,
            }],
        )?;

        Ok(())
    }

    /// Render a delete button (X) on a desktop preview.
    pub fn render_delete_button(
        &self,
        overview: &OverviewWindow,
        x: i16,
        y: i16,
        size: u16,
        is_hovered: bool,
    ) -> Result<()> {
        // Background (dark red, brighter when hovered)
        let bg_color = if is_hovered { 0xCC4444 } else { 0x884444 };
        self.conn
            .change_gc(overview.gc, &ChangeGCAux::new().foreground(bg_color))?;
        self.conn.poly_fill_rectangle(
            overview.pixmap,
            overview.gc,
            &[Rectangle {
                x,
                y,
                width: size,
                height: size,
            }],
        )?;

        // Draw "X" symbol
        let x_color = 0xFFFFFF;
        let line_width = 2u16;
        let margin = size / 4;
        self.conn.change_gc(
            overview.gc,
            &ChangeGCAux::new()
                .foreground(x_color)
                .line_width(line_width as u32),
        )?;

        // Diagonal lines forming X
        self.conn.poly_segment(
            overview.pixmap,
            overview.gc,
            &[
                Segment {
                    x1: x + margin as i16,
                    y1: y + margin as i16,
                    x2: x + (size - margin) as i16,
                    y2: y + (size - margin) as i16,
                },
                Segment {
                    x1: x + (size - margin) as i16,
                    y1: y + margin as i16,
                    x2: x + margin as i16,
                    y2: y + (size - margin) as i16,
                },
            ],
        )?;

        Ok(())
    }

    /// Render a desktop preview at an animated position/size (for zoom animation).
    /// This renders the wallpaper and mini-windows scaled to the given rectangle.
    pub fn render_desktop_preview_animated(
        &self,
        overview: &OverviewWindow,
        preview: &DesktopPreviewLayout,
        captures: &[CapturedWindow],
        dst_x: i16,
        dst_y: i16,
        dst_width: u16,
        dst_height: u16,
    ) -> Result<()> {
        if dst_width == 0 || dst_height == 0 {
            return Ok(());
        }

        // 1. Render scaled wallpaper as background
        if let Some(bg_pic) = overview.bg_picture {
            self.render_wallpaper_scaled(
                bg_pic,
                overview.picture,
                dst_x,
                dst_y,
                dst_width,
                dst_height,
            )?;
        } else {
            // Fallback: solid color background
            self.conn
                .change_gc(overview.gc, &ChangeGCAux::new().foreground(0x2a2a2a))?;
            self.conn.poly_fill_rectangle(
                overview.pixmap,
                overview.gc,
                &[Rectangle {
                    x: dst_x,
                    y: dst_y,
                    width: dst_width,
                    height: dst_height,
                }],
            )?;
        }

        // 2. Render mini-windows scaled proportionally
        // Calculate scale factors from original preview size to animated size
        let scale_x = dst_width as f64 / preview.width as f64;
        let scale_y = dst_height as f64 / preview.height as f64;

        for mini in &preview.mini_windows {
            // Find the capture by frame window ID
            if let Some(capture) = captures.iter().find(|c| c.info.frame_window == mini.window_id) {
                // Scale mini-window position and size
                let mini_x = dst_x + (mini.x as f64 * scale_x) as i16;
                let mini_y = dst_y + (mini.y as f64 * scale_y) as i16;
                let mini_w = (mini.width as f64 * scale_x) as u16;
                let mini_h = (mini.height as f64 * scale_y) as u16;

                self.render_mini_thumbnail(
                    capture.picture,
                    overview.picture,
                    capture.info.width,
                    capture.info.height,
                    mini_x,
                    mini_y,
                    mini_w,
                    mini_h,
                )?;
            }
        }

        Ok(())
    }
}

// TODO: Future enhancements
// - Animation support (fade-in/out)
// - Rounded corners using clip masks
// - Window title labels
// - Drop shadows
