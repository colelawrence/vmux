pub mod tmux;
mod state;
mod ui;

pub use crate::state::{AppState, AppAction};
pub use crate::tmux::{RealTmuxAdapter, TmuxAdapter, TmuxError, TmuxSession};

/// Top-level vmux entry point used by `main`.
///
/// This sets up the TUI, lets the user choose a session, then hands off
/// control to tmux by attaching or switching to the selected session.
pub fn run(adapter: &mut dyn TmuxAdapter) -> Result<(), VmuxError> {
    ui::run(adapter)
}

#[derive(Debug)]
pub enum VmuxError {
    Tmux(TmuxError),
    Terminal(String),
}

impl std::fmt::Display for VmuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmuxError::Tmux(e) => write!(f, "tmux error: {e}"),
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
