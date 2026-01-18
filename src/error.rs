use thiserror::Error;

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum XposeError {
    #[error("X11 connection error: {0}")]
    Connection(#[from] x11rb::errors::ConnectError),

    #[error("X11 reply error: {0}")]
    Reply(#[from] x11rb::errors::ReplyError),

    #[error("X11 reply or ID error: {0}")]
    ReplyOrId(#[from] x11rb::errors::ReplyOrIdError),

    #[error("X11 connection error: {0}")]
    ConnectionError(#[from] x11rb::errors::ConnectionError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("No windows found to display")]
    NoWindows,

    #[error("Composite extension not available or version too old")]
    NoComposite,

    #[error("Render extension not available")]
    NoRender,

    #[error("Failed to find suitable picture format")]
    NoPictFormat,

    #[error("Damage extension not available")]
    NoDamage,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, XposeError>;
