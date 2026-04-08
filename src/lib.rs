mod recent_activity;
mod state;
pub mod tmux;
mod ui;

pub use crate::recent_activity::RecentActivityError;
pub use crate::state::{AppState, RecentPane, SelectedPaneTarget};
pub use crate::tmux::{RealTmuxAdapter, TmuxAdapter, TmuxError, TmuxSession};

/// Top-level vmux entry point used by `main`.
///
/// This sets up the TUI, lets the user choose a session, then hands off
/// control to tmux by attaching or switching to the selected session.
pub fn run(adapter: &mut dyn TmuxAdapter) -> Result<(), VmuxError> {
    ui::run(adapter)
}

/// Append a `notify` event to the recent-activity event log.
pub fn run_notify(payload_path: &std::path::Path) -> Result<(), VmuxError> {
    recent_activity::run_notify(payload_path).map_err(VmuxError::RecentActivity)
}

/// Append a `clear` event to the recent-activity event log.
pub fn run_clear(payload_path: &std::path::Path) -> Result<(), VmuxError> {
    recent_activity::run_clear(payload_path).map_err(VmuxError::RecentActivity)
}

#[derive(Debug)]
pub enum VmuxError {
    Usage(String),
    Tmux(TmuxError),
    RecentActivity(recent_activity::RecentActivityError),
    Terminal(String),
}

impl std::fmt::Display for VmuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmuxError::Usage(msg) => write!(f, "usage error: {msg}"),
            VmuxError::Tmux(e) => write!(f, "tmux error: {e}"),
            VmuxError::RecentActivity(e) => write!(f, "recent activity error: {e}"),
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
