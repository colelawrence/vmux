use std::env;
use std::fmt;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSession {
    pub id: String,
    pub name: String,
    pub windows: Option<u32>,
    pub attached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxBellWindow {
    pub session_name: String,
    pub window_id: String,
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
    fn from(error: std::io::Error) -> Self {
        TmuxError::Io(error)
    }
}

impl From<std::string::FromUtf8Error> for TmuxError {
    fn from(error: std::string::FromUtf8Error) -> Self {
        TmuxError::Utf8(error)
    }
}

pub trait TmuxAdapter {
    fn list_sessions(&mut self) -> Result<Vec<TmuxSession>, TmuxError>;
    fn list_bell_windows(&mut self) -> Result<Vec<TmuxBellWindow>, TmuxError>;
    fn build_client_command(
        &mut self,
        session_id: &str,
        pane_id: Option<&str>,
    ) -> Result<Command, TmuxError>;
}

pub struct RealTmuxAdapter {
    socket_name: Option<String>,
}

impl RealTmuxAdapter {
    pub fn from_env() -> Self {
        let socket_name = env::var("VMUX_TMUX_SOCKET").ok();
        Self { socket_name }
    }

    fn base_command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        if let Some(socket) = self.socket_name.as_ref() {
            cmd.arg("-L").arg(socket);
        }
        cmd
    }
}

impl TmuxAdapter for RealTmuxAdapter {
    fn list_sessions(&mut self) -> Result<Vec<TmuxSession>, TmuxError> {
        let mut cmd = self.base_command();
        cmd.arg("list-sessions")
            .arg("-F")
            .arg("#{session_id}\t#S\t#{session_windows}\t#{?session_attached,1,0}");
        let output = cmd.output()?;
        if !output.status.success() {
            return Err(TmuxError::TmuxFailed(format!(
                "tmux list-sessions exited with status {}",
                output.status
            )));
        }

        let stdout = String::from_utf8(output.stdout)?;
        let mut sessions = Vec::new();
        for line in stdout.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() != 4 {
                return Err(TmuxError::Parse(format!(
                    "unexpected list-sessions line: {line}"
                )));
            }
            let attached = match parts[3] {
                "1" => true,
                "0" => false,
                other => {
                    return Err(TmuxError::Parse(format!(
                        "unexpected attached flag '{other}' in line: {line}"
                    )))
                }
            };
            sessions.push(TmuxSession {
                id: parts[0].to_string(),
                name: parts[1].to_string(),
                windows: parts[2].parse::<u32>().ok(),
                attached,
            });
        }
        Ok(sessions)
    }

    fn list_bell_windows(&mut self) -> Result<Vec<TmuxBellWindow>, TmuxError> {
        let mut cmd = self.base_command();
        cmd.arg("list-windows")
            .arg("-a")
            .arg("-F")
            .arg("#S:#{window_id}:#{window_bell_flag}");
        let output = cmd.output()?;
        if !output.status.success() {
            return Err(TmuxError::TmuxFailed(format!(
                "tmux list-windows exited with status {}",
                output.status
            )));
        }

        let stdout = String::from_utf8(output.stdout)?;
        let mut windows = Vec::new();
        for line in stdout.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() != 3 {
                return Err(TmuxError::Parse(format!(
                    "unexpected list-windows line: {line}"
                )));
            }
            if parts[2] != "1" {
                continue;
            }
            windows.push(TmuxBellWindow {
                session_name: parts[0].to_string(),
                window_id: parts[1].to_string(),
            });
        }
        Ok(windows)
    }

    fn build_client_command(
        &mut self,
        session_id: &str,
        pane_id: Option<&str>,
    ) -> Result<Command, TmuxError> {
        let mut cmd = self.base_command();
        cmd.arg("attach-session").arg("-t").arg(session_id);
        if let Some(pane_id) = pane_id {
            cmd.arg(";").arg("select-pane").arg("-t").arg(pane_id);
        }
        Ok(cmd)
    }
}
