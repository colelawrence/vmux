use std::fs;
use std::process::Command;

use serde_json::Value;

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

#[test]
fn notify_mode_appends_ledger_with_tmux_context() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping vmux notify test: tmux not available");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = format!("vmux-notify-{}", std::process::id());
    let _guard = TmuxServerGuard {
        socket: socket.clone(),
    };

    let status = Command::new("tmux")
        .args([
            "-L",
            socket.as_str(),
            "new-session",
            "-d",
            "-s",
            "vmux_notify_test",
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

    let expected_session_id = Command::new("tmux")
        .args([
            "-L",
            socket.as_str(),
            "display-message",
            "-p",
            "-t",
            pane_id.as_str(),
            "-F",
            "#{session_id}",
        ])
        .output()
        .expect("query session id");
    assert!(expected_session_id.status.success());
    let expected_session_id = String::from_utf8(expected_session_id.stdout)
        .expect("utf8")
        .trim()
        .to_string();

    let payload_path = dir.path().join("payload.json");
    fs::write(
        &payload_path,
        format!(
            r#"{{
  "version":1,
  "title":"Demo",
  "subtitle":"Pi Coding Agent",
  "body":"Working",
  "latestAssistantMessage":"Hello there",
  "platform":"darwin",
  "timestamp":1234567890,
  "cwd":"/tmp",
  "terminal":{{"bundleId":null}},
  "tmux":{{
    "sessionName":"vmux_notify_test",
    "windowId":"@0",
    "windowIndex":0,
    "windowName":"main",
    "paneId":"{pane_id}",
    "clientName":"zsh",
    "clientPid":1234
  }}
}}"#,
            pane_id = pane_id
        ),
    )
    .expect("write payload");

    let ledger_path = dir.path().join("ledger.jsonl");

    let mut cmd = Command::new(vmux_bin());
    cmd.arg("notify").arg(&payload_path);
    cmd.env("VMUX_TMUX_SOCKET", socket.as_str());
    cmd.env("TMUX", format!("{socket},1,0"));
    cmd.env("TMUX_PANE", pane_id.as_str());
    cmd.env("VMUX_NOTIFY_LEDGER_PATH", &ledger_path);

    let output = cmd.output().expect("run vmux notify");
    if !output.status.success() {
        panic!(
            "vmux notify failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let ledger = fs::read_to_string(&ledger_path).expect("read ledger");
    let line = ledger
        .lines()
        .find(|line| !line.trim().is_empty())
        .expect("ledger line");
    let record: Value = serde_json::from_str(line).expect("parse ledger record");
    assert_eq!(record["kind"], "system-notification");
    assert_eq!(record["session"]["sessionName"], "vmux_notify_test");
    assert_eq!(record["session"]["windowId"], "@0");
    assert_eq!(record["session"]["sessionId"], expected_session_id);
}
