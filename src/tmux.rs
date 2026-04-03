use std::env;
use std::fmt;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSession {
    pub name: String,
    pub windows: Option<u32>,
    pub attached: bool,
}

#[derive(Debug)]
pub enum TmuxError {
    Io(std::io::Error),
    Utf8(std::string::FromUtf8Error),
    TmuxFailed(String),
    Parse(String),
}

impl fmt::Display for TmuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TmuxError::Io(e) => write!(f, "tmux io error: {e}"),
            TmuxError::Utf8(e) => write!(f, "tmux utf8 error: {e}"),
            TmuxError::TmuxFailed(msg) => write!(f, "tmux failed: {msg}"),
            TmuxError::Parse(msg) => write!(f, "tmux parse error: {msg}"),
        }
    }
}

impl std::error::Error for TmuxError {}

impl From<std::io::Error> for TmuxError {
    fn from(e: std::io::Error) -> Self {
        TmuxError::Io(e)
    }
}

impl From<std::string::FromUtf8Error> for TmuxError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        TmuxError::Utf8(e)
    }
}

pub trait TmuxAdapter {
    fn list_sessions(&mut self) -> Result<Vec<TmuxSession>, TmuxError>;
    fn attach_or_switch(&mut self, session_name: &str) -> Result<(), TmuxError>;
}

/// Real tmux adapter that shells out to the `tmux` binary.
pub struct RealTmuxAdapter {
    /// Optional tmux socket name (from `VMUX_TMUX_SOCKET` env var).
    socket_name: Option<String>,
}

impl RealTmuxAdapter {
    pub fn from_env() -> Self {
        let socket_name = env::var("VMUX_TMUX_SOCKET").ok();
        Self { socket_name }
    }

    fn base_command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        if let Some(ref socket) = self.socket_name {
            cmd.arg("-L").arg(socket);
        }
        cmd
    }

    fn inside_tmux(&self) -> bool {
        env::var("TMUX").is_ok()
    }
}

impl TmuxAdapter for RealTmuxAdapter {
    fn list_sessions(&mut self) -> Result<Vec<TmuxSession>, TmuxError> {
        // Format: name:windows:attached_flag
        let mut cmd = self.base_command();
        cmd.arg("list-sessions")
            .arg("-F")
            .arg("#S:#{session_windows}:#{?session_attached,1,0}");
        let output = cmd.output()?;
        if !output.status.success() {
            return Err(TmuxError::TmuxFailed(format!(
                "tmux list-sessions exited with status {status}",
                status = output.status
            )));
        }
        let stdout = String::from_utf8(output.stdout)?;
        let mut sessions = Vec::new();
        for line in stdout.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() < 3 {
                return Err(TmuxError::Parse(format!(
                    "unexpected list-sessions line: {line}"
                )));
            }
            let name = parts[0].to_string();
            let windows = parts[1].parse::<u32>().ok();
            let attached = match parts[2] {
                "1" => true,
                "0" => false,
                other => {
                    return Err(TmuxError::Parse(format!(
                        "unexpected attached flag '{other}' in line: {line}"
                    )))
                }
            };
            sessions.push(TmuxSession {
                name,
                windows,
                attached,
            });
        }
        Ok(sessions)
    }

    fn attach_or_switch(&mut self, session_name: &str) -> Result<(), TmuxError> {
        let mut cmd = self.base_command();
        if self.inside_tmux() {
            cmd.arg("switch-client").arg("-t").arg(session_name);
        } else {
            cmd.arg("attach-session").arg("-t").arg(session_name);
        }
        let status = cmd.status()?;
        if !status.success() {
            return Err(TmuxError::TmuxFailed(format!(
                "tmux attach/switch exited with status {status}"
            )));
        }
        Ok(())
    }
}
