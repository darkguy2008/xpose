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
    pub fn capture_window(&self, info: &WindowInfo) -> Result<CapturedWindow> {
        // Redirect window to off-screen storage
        composite::redirect_window(&self.conn, info.frame_window, composite::Redirect::AUTOMATIC)?;

        // Get pixmap with window contents
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
}
