use std::process::Command;

use vmux::tmux::TmuxSession;
use vmux::{RealTmuxAdapter, TmuxAdapter};

struct FakeTmux {
    sessions: Vec<TmuxSession>,
    attached_to: std::cell::RefCell<Vec<String>>,
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
        Self { sessions, attached_to: std::cell::RefCell::new(Vec::new()) }
    }
}

impl TmuxAdapter for FakeTmux {
    fn list_sessions(&mut self) -> Result<Vec<TmuxSession>, vmux::tmux::TmuxError> {
        Ok(self.sessions.clone())
    }

    fn attach_or_switch(&mut self, session_name: &str) -> Result<(), vmux::tmux::TmuxError> {
        self.attached_to.borrow_mut().push(session_name.to_string());
        Ok(())
    }
}

#[test]
fn fake_tmux_adapter_records_attach() {
    let mut fake = FakeTmux::new(&["one", "two"]);
    let sessions = fake.list_sessions().unwrap();
    assert_eq!(sessions.len(), 2);

    fake.attach_or_switch("two").unwrap();
    assert_eq!(fake.attached_to.borrow().as_slice(), &["two".to_string()]);
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
        .args(["-L", socket.as_str(), "new-session", "-d", "-s", "vmux_test"])
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
