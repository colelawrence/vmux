use std::process::Command;

use vmux::tmux::{TmuxBellWindow, TmuxSession};
use vmux::{RealTmuxAdapter, TmuxAdapter};

struct FakeTmux {
    sessions: Vec<TmuxSession>,
    bell_windows: Vec<TmuxBellWindow>,
    last_client: std::cell::RefCell<Vec<String>>,
}

impl FakeTmux {
    fn new(names: &[&str]) -> Self {
        let sessions = names
            .iter()
            .map(|name| TmuxSession {
                name: (*name).to_string(),
                windows: None,
                attached: false,
            })
            .collect();
        Self {
            sessions,
            bell_windows: Vec::new(),
            last_client: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn with_bell_windows(names: &[&str], bell_windows: Vec<TmuxBellWindow>) -> Self {
        let mut fake = Self::new(names);
        fake.bell_windows = bell_windows;
        fake
    }
}

impl TmuxAdapter for FakeTmux {
    fn list_sessions(&mut self) -> Result<Vec<TmuxSession>, vmux::tmux::TmuxError> {
        Ok(self.sessions.clone())
    }

    fn list_bell_windows(&mut self) -> Result<Vec<TmuxBellWindow>, vmux::tmux::TmuxError> {
        Ok(self.bell_windows.clone())
    }

    fn build_client_command(
        &mut self,
        session_name: &str,
        _window_id: Option<&str>,
        _pane_id: Option<&str>,
    ) -> Result<std::process::Command, vmux::tmux::TmuxError> {
        self.last_client.borrow_mut().push(session_name.to_string());
        Ok(Command::new("tmux-client-placeholder"))
    }
}

#[test]
fn fake_tmux_adapter_records_client_build() {
    let mut fake = FakeTmux::new(&["one", "two"]);
    let sessions = fake.list_sessions().unwrap();
    assert_eq!(sessions.len(), 2);

    let _cmd = fake.build_client_command("two", None, None).unwrap();
    assert_eq!(fake.last_client.borrow().as_slice(), &["two".to_string()]);
}

#[test]
fn real_tmux_adapter_builds_exact_pane_target_command() {
    let mut adapter = RealTmuxAdapter::from_env();
    let cmd = adapter
        .build_client_command("demo", Some("@7"), Some("%9"))
        .expect("build client command");

    let args: Vec<String> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        vec![
            "attach-session".to_string(),
            "-t".to_string(),
            "demo".to_string(),
            ";".to_string(),
            "select-window".to_string(),
            "-t".to_string(),
            "@7".to_string(),
            ";".to_string(),
            "select-pane".to_string(),
            "-t".to_string(),
            "%9".to_string(),
        ]
    );
}

#[test]
fn fake_tmux_adapter_returns_bell_windows() {
    let mut fake = FakeTmux::with_bell_windows(
        &["one", "two"],
        vec![TmuxBellWindow {
            session_name: "one".to_string(),
            window_id: "@1".to_string(),
        }],
    );

    let bell_windows = fake.list_bell_windows().unwrap();
    assert_eq!(bell_windows.len(), 1);
    assert_eq!(bell_windows[0].session_name, "one");
    assert_eq!(bell_windows[0].window_id, "@1");
}

struct TmuxServerGuard {
    socket: String,
}

impl Drop for TmuxServerGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", self.socket.as_str(), "kill-server"])
            .status();
    }
}

/// Best-effort real tmux integration test for the adapter using an isolated
/// tmux server. Skips when `tmux` is not available.
#[test]
fn real_tmux_list_sessions_on_isolated_server() {
    // Check tmux availability.
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping real tmux adapter test: tmux not available");
        return;
    }

    let socket = format!("vmux-test-{}", std::process::id());
    let _guard = TmuxServerGuard {
        socket: socket.clone(),
    };

    // Start an isolated tmux server with a single session.
    let status = Command::new("tmux")
        .args([
            "-L",
            socket.as_str(),
            "new-session",
            "-d",
            "-s",
            "vmux_test",
        ])
        .status()
        .expect("failed to start tmux test server");
    if !status.success() {
        eprintln!("skipping real tmux adapter test: failed to start tmux server");
        return;
    }

    // Point the adapter at the isolated server via env var.
    std::env::set_var("VMUX_TMUX_SOCKET", socket.clone());
    let mut adapter = RealTmuxAdapter::from_env();
    let sessions = adapter.list_sessions().expect("list_sessions failed");

    assert!(sessions.iter().any(|s| s.name == "vmux_test"));
}
