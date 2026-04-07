mod notify;
mod state;
pub mod tmux;
mod ui;

pub use crate::state::AppState;
pub use crate::tmux::{RealTmuxAdapter, TmuxAdapter, TmuxError, TmuxSession};

/// Top-level vmux entry point used by `main`.
///
/// This sets up the TUI, lets the user choose a session, then hands off
/// control to tmux by attaching or switching to the selected session.
pub fn run(adapter: &mut dyn TmuxAdapter) -> Result<(), VmuxError> {
    ui::run(adapter)
}

pub fn run_notify(payload_path: &std::path::Path) -> Result<(), VmuxError> {
    notify::run_notify(payload_path).map_err(VmuxError::Notify)
}

#[derive(Debug)]
pub enum VmuxError {
    Usage(String),
    Tmux(TmuxError),
    Notify(notify::NotifyError),
    Terminal(String),
}

impl std::fmt::Display for VmuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmuxError::Usage(msg) => write!(f, "usage error: {msg}"),
            VmuxError::Tmux(e) => write!(f, "tmux error: {e}"),
            VmuxError::Notify(e) => write!(f, "notify error: {e}"),
            VmuxError::Terminal(msg) => write!(f, "terminal error: {msg}"),
        }
    }
}

impl std::error::Error for VmuxError {}

impl From<TmuxError> for VmuxError {
    fn from(err: TmuxError) -> Self {
        VmuxError::Tmux(err)
    }
}
