use x11rb::connection::Connection;
use x11rb::protocol::composite;
use x11rb::protocol::damage::{self, Damage, ReportLevel};
use x11rb::protocol::render::{self, Picture};
use x11rb::protocol::xproto::*;

use crate::connection::XConnection;
use crate::error::Result;
use crate::window_finder::WindowInfo;

#[derive(Debug)]
pub struct CapturedWindow {
    pub info: WindowInfo,
    pub pixmap: Pixmap,
    pub picture: Picture,
    pub damage: Damage,
}

impl XConnection {
    /// Capture window contents to a pixmap using XComposite.
    /// Returns immediately - no retries. Use retry_capture for failed windows.
    pub fn capture_window(&self, info: &WindowInfo) -> Result<CapturedWindow> {
        // Redirect window to off-screen storage
        composite::redirect_window(&self.conn, info.frame_window, composite::Redirect::AUTOMATIC)?;

        // Try to get pixmap with window contents (single attempt)
        let pixmap = self.generate_id()?;
        composite::name_window_pixmap(&self.conn, info.frame_window, pixmap)?;

        // Get actual pixmap geometry (may differ from frame geometry)
        let pixmap_geom = self.conn.get_geometry(pixmap)?.reply()?;

        // Create XRender picture from pixmap
        let picture = self.generate_id()?;
        render::create_picture(
            &self.conn,
            picture,
            pixmap,
            self.pict_format_rgb,
            &render::CreatePictureAux::new(),
        )?;

        // Create damage tracking for real-time updates
        // ReportLevel::NON_EMPTY gives us simple "something changed" notifications
        let damage_id = self.generate_id()?;
        damage::create(&self.conn, damage_id, info.frame_window, ReportLevel::NON_EMPTY)?;

        self.conn.flush()?;

        // Update info with actual pixmap dimensions
        let mut captured_info = info.clone();
        captured_info.width = pixmap_geom.width;
        captured_info.height = pixmap_geom.height;

        log::debug!(
            "Captured window {:?} -> pixmap 0x{:x} ({}x{}), picture 0x{:x}, damage 0x{:x}",
            info.wm_name,
            pixmap,
            pixmap_geom.width,
            pixmap_geom.height,
            picture,
            damage_id
        );

        Ok(CapturedWindow {
            info: captured_info,
            pixmap,
            picture,
            damage: damage_id,
        })
    }

    /// Release captured resources.
    pub fn release_capture(&self, capture: &CapturedWindow) -> Result<()> {
        // Destroy damage tracking
        damage::destroy(&self.conn, capture.damage)?;

        // Free picture
        render::free_picture(&self.conn, capture.picture)?;

        // Free pixmap
        self.conn.free_pixmap(capture.pixmap)?;

        // Unredirect window
        composite::unredirect_window(
            &self.conn,
            capture.info.frame_window,
            composite::Redirect::AUTOMATIC,
        )?;

        Ok(())
    }

    /// Re-capture window pixmap after damage (window content changed).
    /// This creates a new pixmap/picture from the current window contents.
    pub fn refresh_capture(&self, capture: &mut CapturedWindow) -> Result<()> {
        // Free old picture and pixmap
        render::free_picture(&self.conn, capture.picture)?;
        self.conn.free_pixmap(capture.pixmap)?;

        // Get new pixmap with updated window contents
        let pixmap = self.generate_id()?;
        composite::name_window_pixmap(&self.conn, capture.info.frame_window, pixmap)?;

        // Create new picture
        let picture = self.generate_id()?;
        render::create_picture(
            &self.conn,
            picture,
            pixmap,
            self.pict_format_rgb,
            &render::CreatePictureAux::new(),
        )?;

        // Ensure commands are sent to server
        self.conn.flush()?;

        capture.pixmap = pixmap;
        capture.picture = picture;

        Ok(())
    }

    /// Create a placeholder capture for a window that failed to capture.
    /// Uses a solid black picture. Can be upgraded later via try_upgrade_placeholder.
    pub fn create_placeholder_capture(&self, info: &WindowInfo) -> Result<CapturedWindow> {
        // Redirect window (needed for later retry)
        let _ = composite::redirect_window(&self.conn, info.frame_window, composite::Redirect::AUTOMATIC);

        // Create a small pixmap filled with black as placeholder
        let pixmap = self.generate_id()?;
        self.conn.create_pixmap(
            self.root_depth,
            pixmap,
            self.root,
            info.width.max(1),
            info.height.max(1),
        )?;

        // Fill with black
        let gc = self.generate_id()?;
        self.conn.create_gc(
            gc,
            pixmap,
            &x11rb::protocol::xproto::CreateGCAux::new().foreground(0x222222),
        )?;
        self.conn.poly_fill_rectangle(
            pixmap,
            gc,
            &[x11rb::protocol::xproto::Rectangle {
                x: 0,
                y: 0,
                width: info.width.max(1),
                height: info.height.max(1),
            }],
        )?;
        self.conn.free_gc(gc)?;

        // Create picture from placeholder pixmap
        let picture = self.generate_id()?;
        render::create_picture(
            &self.conn,
            picture,
            pixmap,
            self.pict_format_rgb,
            &render::CreatePictureAux::new(),
        )?;

        // Create damage tracking (even for placeholder)
        let damage_id = self.generate_id()?;
        damage::create(&self.conn, damage_id, info.frame_window, ReportLevel::NON_EMPTY)?;

        self.conn.flush()?;

        log::debug!(
            "Created placeholder for {:?} ({}x{})",
            info.wm_name,
            info.width,
            info.height
        );

        Ok(CapturedWindow {
            info: info.clone(),
            pixmap,
            picture,
            damage: damage_id,
        })
    }

    /// Try to upgrade a placeholder capture to a real capture.
    /// Returns true if successful, false if window still not ready.
    pub fn try_upgrade_placeholder(&self, capture: &mut CapturedWindow) -> bool {
        // Try to get actual window pixmap
        let new_pixmap = match self.generate_id() {
            Ok(id) => id,
            Err(_) => return false,
        };

        if composite::name_window_pixmap(&self.conn, capture.info.frame_window, new_pixmap).is_err() {
            return false;
        }

        // Check if pixmap is valid
        match self.conn.get_geometry(new_pixmap) {
            Ok(cookie) => match cookie.reply() {
                Ok(geom) => {
                    // Success! Create new picture and replace placeholder
                    let new_picture = match self.generate_id() {
                        Ok(id) => id,
                        Err(_) => {
                            let _ = self.conn.free_pixmap(new_pixmap);
                            return false;
                        }
                    };

                    if render::create_picture(
                        &self.conn,
                        new_picture,
                        new_pixmap,
                        self.pict_format_rgb,
                        &render::CreatePictureAux::new(),
                    ).is_err() {
                        let _ = self.conn.free_pixmap(new_pixmap);
                        return false;
                    }

                    // Free old placeholder resources
                    let _ = render::free_picture(&self.conn, capture.picture);
                    let _ = self.conn.free_pixmap(capture.pixmap);

                    // Update capture with real content
                    capture.pixmap = new_pixmap;
                    capture.picture = new_picture;
                    capture.info.width = geom.width;
                    capture.info.height = geom.height;

                    let _ = self.conn.flush();

                    log::info!("Upgraded placeholder to real capture: {:?}", capture.info.wm_name);
                    true
                }
                Err(_) => {
                    let _ = self.conn.free_pixmap(new_pixmap);
                    false
                }
            },
            Err(_) => {
                let _ = self.conn.free_pixmap(new_pixmap);
                false
            }
        }
    }
}
