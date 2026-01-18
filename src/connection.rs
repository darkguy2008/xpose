use x11rb::atom_manager;
use x11rb::connection::Connection;
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;
use x11rb::protocol::composite;
use x11rb::protocol::damage;
use x11rb::protocol::render::{self, Pictformat};
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::error::{Result, XposeError};

atom_manager! {
    pub Atoms: AtomsCookie {
        WM_STATE,
        WM_CLASS,
        WM_NAME,
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        WM_TRANSIENT_FOR,
        UTF8_STRING,
        // EWMH window type atoms
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_WINDOW_TYPE_TOOLBAR,
        _NET_WM_WINDOW_TYPE_MENU,
        _NET_WM_WINDOW_TYPE_UTILITY,
        _NET_WM_WINDOW_TYPE_SPLASH,
        _NET_WM_WINDOW_TYPE_DIALOG,
        _NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
        _NET_WM_WINDOW_TYPE_POPUP_MENU,
        _NET_WM_WINDOW_TYPE_TOOLTIP,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
        _NET_WM_WINDOW_TYPE_COMBO,
        _NET_WM_WINDOW_TYPE_DND,
        // EWMH window state atoms
        _NET_WM_STATE,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        // Root window background pixmap atoms
        _XROOTPMAP_ID,
        ESETROOT_PMAP_ID,
        // xpose virtual desktop atoms
        _XPOSE_NUM_DESKTOPS,
        _XPOSE_CURRENT_DESKTOP,
    }
}

pub struct XConnection {
    pub conn: RustConnection,
    #[allow(dead_code)]
    pub screen_num: usize,
    pub atoms: Atoms,
    pub root: Window,
    pub screen_width: u16,
    pub screen_height: u16,
    pub root_depth: u8,
    pub root_visual: Visualid,
    pub pict_format_rgb: Pictformat,
}

impl XConnection {
    pub fn new() -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let screen = &conn.setup().roots[screen_num];

        let atoms = Atoms::new(&conn)?.reply()?;

        // Initialize Composite extension
        let composite_version = composite::query_version(&conn, 0, 4)?.reply()?;
        if composite_version.major_version == 0 && composite_version.minor_version < 2 {
            return Err(XposeError::NoComposite);
        }
        log::info!(
            "Composite extension version {}.{}",
            composite_version.major_version,
            composite_version.minor_version
        );

        // Initialize Render extension and find picture format
        let render_version = render::query_version(&conn, 0, 11)?.reply()?;
        log::info!(
            "Render extension version {}.{}",
            render_version.major_version,
            render_version.minor_version
        );

        let pict_formats = render::query_pict_formats(&conn)?.reply()?;

        // Find a picture format matching root depth
        let pict_format_rgb = pict_formats
            .formats
            .iter()
            .find(|f| f.depth == screen.root_depth && f.type_ == render::PictType::DIRECT)
            .map(|f| f.id)
            .ok_or(XposeError::NoPictFormat)?;

        log::info!(
            "Using picture format {} for depth {}",
            pict_format_rgb,
            screen.root_depth
        );

        // Initialize Damage extension
        let damage_version = damage::query_version(&conn, 1, 1)?.reply()?;
        log::info!(
            "Damage extension version {}.{}",
            damage_version.major_version,
            damage_version.minor_version
        );

        Ok(Self {
            root: screen.root,
            screen_width: screen.width_in_pixels,
            screen_height: screen.height_in_pixels,
            root_depth: screen.root_depth,
            root_visual: screen.root_visual,
            conn,
            screen_num,
            atoms,
            pict_format_rgb,
        })
    }

    pub fn flush(&self) -> Result<()> {
        self.conn.flush()?;
        Ok(())
    }

    /// Sync with X server - ensures all previous requests are fully processed.
    pub fn sync(&self) -> Result<()> {
        // GetInputFocus is a cheap round-trip that forces the server to process all pending requests
        self.conn.get_input_focus()?.reply()?;
        Ok(())
    }

    pub fn generate_id(&self) -> Result<u32> {
        Ok(self.conn.generate_id()?)
    }

    /// Get the number of virtual desktops.
    pub fn get_num_desktops(&self) -> Result<Option<u32>> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms._XPOSE_NUM_DESKTOPS,
                AtomEnum::CARDINAL,
                0,
                1,
            )?
            .reply()?;

        Ok(reply.value32().and_then(|mut v| v.next()))
    }

    /// Get the current active desktop.
    pub fn get_current_desktop(&self) -> Result<Option<u32>> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms._XPOSE_CURRENT_DESKTOP,
                AtomEnum::CARDINAL,
                0,
                1,
            )?
            .reply()?;

        Ok(reply.value32().and_then(|mut v| v.next()))
    }

    /// Set the number of virtual desktops.
    pub fn set_num_desktops(&self, count: u32) -> Result<()> {
        self.conn.change_property32(
            PropMode::REPLACE,
            self.root,
            self.atoms._XPOSE_NUM_DESKTOPS,
            AtomEnum::CARDINAL,
            &[count],
        )?;
        Ok(())
    }

    /// Set the current active desktop.
    pub fn set_current_desktop(&self, desktop: u32) -> Result<()> {
        self.conn.change_property32(
            PropMode::REPLACE,
            self.root,
            self.atoms._XPOSE_CURRENT_DESKTOP,
            AtomEnum::CARDINAL,
            &[desktop],
        )?;
        Ok(())
    }

    /// Map a window (make it visible).
    pub fn map_window(&self, window: Window) -> Result<()> {
        self.conn.map_window(window)?;
        Ok(())
    }

    /// Unmap a window (hide it).
    pub fn unmap_window(&self, window: Window) -> Result<()> {
        self.conn.unmap_window(window)?;
        Ok(())
    }

    /// Get stacking order of all toplevel windows (bottom to top).
    pub fn get_stacking_order(&self) -> Result<Vec<Window>> {
        let tree = self.conn.query_tree(self.root)?.reply()?;
        Ok(tree.children)
    }

    /// Restack windows to match the given order (bottom to top).
    pub fn restack_windows(&self, order: &[Window]) -> Result<()> {
        // Raise each window in order, putting them above the previous one
        for (i, &window) in order.iter().enumerate() {
            if i == 0 {
                // First window: lower to bottom
                self.conn.configure_window(
                    window,
                    &ConfigureWindowAux::new().stack_mode(StackMode::BELOW),
                )?;
            } else {
                // Subsequent windows: raise above previous
                self.conn.configure_window(
                    window,
                    &ConfigureWindowAux::new()
                        .sibling(order[i - 1])
                        .stack_mode(StackMode::ABOVE),
                )?;
            }
        }
        Ok(())
    }
}
