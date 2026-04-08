use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use expectrl::session::Session;

fn vmux_bin() -> String {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_vmux") {
        return path;
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{manifest_dir}/target/debug/vmux")
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

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

fn unique_socket(prefix: &str) -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}-{nonce}")
}

fn query_tmux(socket: &str, target: &str, format: &str) -> String {
    let output = Command::new("tmux")
        .args([
            "-L",
            socket,
            "display-message",
            "-p",
            "-t",
            target,
            "-F",
            format,
        ])
        .output()
        .expect("query tmux");
    assert!(output.status.success(), "tmux query should succeed");
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

fn setup_tmux_session(window_name: &str) -> Option<(tempfile::TempDir, String, TmuxServerGuard, String, String)> {
    if !tmux_available() {
        eprintln!("skipping vmux recent-activity system test: tmux not available");
        return None;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = unique_socket("vmux-recent");
    let guard = TmuxServerGuard {
        socket: socket.clone(),
    };

    let status = Command::new("tmux")
        .args([
            "-L",
            socket.as_str(),
            "new-session",
            "-d",
            "-s",
            "vmux_recent_test",
            "-n",
            window_name,
        ])
        .status()
        .expect("start tmux server");
    assert!(status.success(), "isolated tmux server should start");

    let pane_id = Command::new("tmux")
        .args([
            "-L",
            socket.as_str(),
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}",
        ])
        .output()
        .expect("list panes");
    assert!(pane_id.status.success());
    let pane_id = String::from_utf8(pane_id.stdout)
        .expect("utf8")
        .trim()
        .to_string();
    assert!(!pane_id.is_empty());

    let session_id = query_tmux(&socket, &pane_id, "#{session_id}");
    Some((dir, socket, guard, pane_id, session_id))
}

fn write_notify_payload(payload_path: &Path, session_id: &str, pane_id: &str, display_text: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    fs::write(
        payload_path,
        format!(
            r#"{{
  "sessionId":"{session_id}",
  "paneId":"{pane_id}",
  "paneDisplayText":"{display_text}",
  "notifyTime":{now}
}}"#,
        ),
    )
    .expect("write payload");
}

fn write_clear_payload(payload_path: &Path, session_id: &str, pane_id: &str) {
    fs::write(
        payload_path,
        format!(
            r#"{{
  "sessionId":"{session_id}",
  "paneId":"{pane_id}"
}}"#,
        ),
    )
    .expect("write payload");
}

fn run_vmux_mode(mode: &str, payload_path: &Path, event_log_path: &Path) {
    let mut cmd = Command::new(vmux_bin());
    cmd.arg(mode).arg(payload_path);
    cmd.env("VMUX_RECENT_ACTIVITY_LOG_PATH", event_log_path);

    let output = cmd.output().expect("run vmux mode");
    if !output.status.success() {
        panic!(
            "vmux {mode} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn render_vmux_once(event_log_path: &Path, socket: &str) -> String {
    let mut cmd = Command::new(vmux_bin());
    cmd.env("VMUX_RECENT_ACTIVITY_LOG_PATH", event_log_path);
    cmd.env("VMUX_TMUX_SOCKET", socket);
    cmd.env("VMUX_TEST_ONESHOT", "1");
    cmd.env("TERM", "xterm-256color");
    cmd.env_remove("TMUX");

    let mut pty = Session::spawn(cmd).expect("spawn vmux in pty");
    let mut transcript = String::new();
    pty.read_to_string(&mut transcript)
        .expect("read vmux transcript");
    transcript
}

#[test]
fn notify_event_is_rendered_in_recent_activity_sidebar() {
    let display_text = "RECENT_VISIBLE";
    let Some((dir, socket, _guard, pane_id, session_id)) = setup_tmux_session(display_text) else {
        return;
    };

    let payload_path = dir.path().join("payload.json");
    write_notify_payload(&payload_path, &session_id, &pane_id, display_text);
    let event_log_path = dir.path().join("recent-activity.jsonl");

    run_vmux_mode("notify", &payload_path, &event_log_path);
    let transcript = render_vmux_once(&event_log_path, &socket);

    assert!(
        transcript.contains("vmux_recent_test"),
        "sidebar should render the tmux session name: {transcript:?}"
    );
    assert!(
        transcript.contains(display_text),
        "sidebar should render the recent pane display text: {transcript:?}"
    );
}

#[test]
fn clear_event_removes_recent_activity_from_sidebar() {
    let display_text = "RECENT_CLEARED";
    let Some((dir, socket, _guard, pane_id, session_id)) = setup_tmux_session(display_text) else {
        return;
    };

    let notify_payload_path = dir.path().join("notify.json");
    write_notify_payload(&notify_payload_path, &session_id, &pane_id, display_text);
    let clear_payload_path = dir.path().join("clear.json");
    write_clear_payload(&clear_payload_path, &session_id, &pane_id);
    let event_log_path = dir.path().join("recent-activity.jsonl");

    run_vmux_mode("notify", &notify_payload_path, &event_log_path);
    run_vmux_mode("clear", &clear_payload_path, &event_log_path);
    let transcript = render_vmux_once(&event_log_path, &socket);

    assert!(
        transcript.contains("vmux_recent_test"),
        "session row should still render: {transcript:?}"
    );
    assert!(
        !transcript.contains(display_text),
        "cleared pane should not render in the sidebar: {transcript:?}"
    );
}
