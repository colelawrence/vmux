use crate::tmux::TmuxBellWindow;
use serde::{Deserialize, Serialize};
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const RECENT_ACTIVITY_TTL: Duration = Duration::from_secs(120);
const DEFAULT_LEDGER_RELATIVE_PATH: &str = ".cache/vmux/session-updates.jsonl";

#[derive(Debug)]
pub enum NotifyError {
    Usage(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    LedgerWrite(String),
}

impl fmt::Display for NotifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotifyError::Usage(msg) => write!(f, "notify usage error: {msg}"),
            NotifyError::Io(e) => write!(f, "notify io error: {e}"),
            NotifyError::Json(e) => write!(f, "notify payload parse error: {e}"),
            NotifyError::LedgerWrite(msg) => write!(f, "notify ledger error: {msg}"),
        }
    }
}

impl std::error::Error for NotifyError {}

impl From<std::io::Error> for NotifyError {
    fn from(err: std::io::Error) -> Self {
        NotifyError::Io(err)
    }
}

impl From<serde_json::Error> for NotifyError {
    fn from(err: serde_json::Error) -> Self {
        NotifyError::Json(err)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NotificationPayloadV1 {
    version: u8,
    title: String,
    subtitle: String,
    body: String,
    latest_assistant_message: String,
    platform: String,
    timestamp: u64,
    cwd: String,
    terminal: NotificationTerminal,
    tmux: Option<NotificationTmuxContext>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NotificationTerminal {
    bundle_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NotificationTmuxContext {
    session_name: String,
    window_id: String,
    window_index: i32,
    window_name: String,
    pane_id: String,
    client_name: String,
    client_pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NotificationLedgerRecord {
    version: u8,
    kind: String,
    source: String,
    timestamp: u64,
    title: String,
    subtitle: String,
    body: String,
    latest_assistant_message: String,
    cwd: String,
    platform: Option<String>,
    terminal_bundle_id: Option<String>,
    session: NotificationLedgerSession,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NotificationLedgerSession {
    session_id: Option<String>,
    session_name: Option<String>,
    window_id: Option<String>,
    window_index: Option<i32>,
    window_name: Option<String>,
    pane_id: Option<String>,
    client_name: Option<String>,
    client_pid: Option<u32>,
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_ledger_path() -> PathBuf {
    home_dir().join(DEFAULT_LEDGER_RELATIVE_PATH)
}

fn ledger_path_from_env() -> PathBuf {
    env::var_os("VMUX_NOTIFY_LEDGER_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(default_ledger_path)
}

enum TmuxSocketSpec {
    Named(String),
    Path(String),
}

fn tmux_socket_spec_from_env() -> Option<TmuxSocketSpec> {
    if let Ok(socket_name) = env::var("VMUX_TMUX_SOCKET") {
        let trimmed = socket_name.trim();
        if !trimmed.is_empty() {
            return Some(TmuxSocketSpec::Named(trimmed.to_string()));
        }
    }

    let tmux = env::var("TMUX").ok()?;
    let socket = tmux.split(',').next()?.trim();
    (!socket.is_empty()).then(|| TmuxSocketSpec::Path(socket.to_string()))
}

fn capture_live_tmux_context() -> Option<NotificationLedgerSession> {
    let socket = tmux_socket_spec_from_env()?;

    let mut cmd = Command::new("tmux");
    match socket {
        TmuxSocketSpec::Named(name) => {
            cmd.arg("-L").arg(name);
        }
        TmuxSocketSpec::Path(path) => {
            cmd.arg("-S").arg(path);
        }
    }
    cmd.arg("display-message").arg("-p");
    if let Ok(target) = env::var("TMUX_PANE") {
        let trimmed = target.trim();
        if !trimmed.is_empty() {
            cmd.arg("-t").arg(trimmed);
        }
    }
    cmd.arg("-F")
        .arg("#{session_id}\t#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{pane_id}\t#{client_name}\t#{client_pid}");

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8(output.stdout)
        .ok()?
        .trim_end_matches(['\r', '\n'])
        .to_string();
    let mut parts = raw.split('\t');
    let session_id = parts.next()?.to_string();
    let session_name = parts.next()?.to_string();
    let window_id = parts.next()?.to_string();
    let window_index = parts.next()?.parse::<i32>().ok();
    let window_name = parts.next()?.to_string();
    let pane_id = parts.next()?.to_string();
    let client_name = parts.next()?.to_string();
    let client_pid = parts.next()?.parse::<u32>().ok();

    Some(NotificationLedgerSession {
        session_id: Some(session_id),
        session_name: Some(session_name),
        window_id: Some(window_id),
        window_index,
        window_name: Some(window_name),
        pane_id: Some(pane_id),
        client_name: Some(client_name),
        client_pid,
    })
}

fn merge_tmux_context(
    payload_tmux: Option<NotificationTmuxContext>,
    live_tmux: Option<NotificationLedgerSession>,
) -> NotificationLedgerSession {
    let payload_tmux = payload_tmux.map(|tmux| NotificationLedgerSession {
        session_id: None,
        session_name: Some(tmux.session_name),
        window_id: Some(tmux.window_id),
        window_index: Some(tmux.window_index),
        window_name: Some(tmux.window_name),
        pane_id: Some(tmux.pane_id),
        client_name: Some(tmux.client_name),
        client_pid: tmux.client_pid,
    });

    live_tmux
        .or(payload_tmux)
        .unwrap_or(NotificationLedgerSession {
            session_id: None,
            session_name: None,
            window_id: None,
            window_index: None,
            window_name: None,
            pane_id: None,
            client_name: None,
            client_pid: None,
        })
}

fn ledger_record_from_payload(
    payload: NotificationPayloadV1,
    session: NotificationLedgerSession,
) -> NotificationLedgerRecord {
    NotificationLedgerRecord {
        version: payload.version,
        kind: "system-notification".to_string(),
        source: "vmux".to_string(),
        timestamp: payload.timestamp,
        title: payload.title,
        subtitle: payload.subtitle,
        body: payload.body,
        latest_assistant_message: payload.latest_assistant_message,
        cwd: payload.cwd,
        platform: Some(payload.platform),
        terminal_bundle_id: payload.terminal.bundle_id,
        session,
    }
}

fn append_ledger_record(path: &Path, record: &NotificationLedgerRecord) -> Result<(), NotifyError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| NotifyError::LedgerWrite(format!("failed to open {}: {e}", path.display())))?;

    let line = serde_json::to_string(record)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

pub fn run_notify(payload_path: &Path) -> Result<(), NotifyError> {
    if payload_path.as_os_str().is_empty() {
        return Err(NotifyError::Usage("payload path is required".to_string()));
    }

    let payload_text = fs::read_to_string(payload_path).map_err(|e| {
        NotifyError::LedgerWrite(format!(
            "failed to read payload {}: {e}",
            payload_path.display()
        ))
    })?;
    let payload: NotificationPayloadV1 = serde_json::from_str(&payload_text)?;

    let live_tmux = capture_live_tmux_context();
    if payload.tmux.is_none() && live_tmux.is_none() {
        return Err(NotifyError::Usage(
            "notify requires tmux context in payload or tmux environment".to_string(),
        ));
    }

    let session = merge_tmux_context(payload.tmux.clone(), live_tmux);
    let record = ledger_record_from_payload(payload, session);

    let ledger_path = ledger_path_from_env();
    append_ledger_record(&ledger_path, &record)?;
    Ok(())
}

pub fn load_recent_activity_windows(
    ledger_path: &Path,
    now: SystemTime,
) -> Result<Vec<(TmuxBellWindow, SystemTime)>, NotifyError> {
    let mut results = Vec::new();
    if !ledger_path.exists() {
        return Ok(results);
    }

    let file = fs::File::open(ledger_path).map_err(|e| {
        NotifyError::LedgerWrite(format!("failed to open {}: {e}", ledger_path.display()))
    })?;

    let reader = BufReader::new(file);
    for line in reader.lines() {
        let Ok(line) = line else {
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<NotificationLedgerRecord>(&line) else {
            continue;
        };
        if record.kind != "system-notification" {
            continue;
        }

        let Some(session_name) = record.session.session_name.clone() else {
            continue;
        };
        let Some(window_id) = record.session.window_id.clone() else {
            continue;
        };

        let event_time = SystemTime::UNIX_EPOCH + Duration::from_millis(record.timestamp);
        if now.duration_since(event_time).is_err() {
            continue;
        }
        if now
            .duration_since(event_time)
            .map(|age| age > RECENT_ACTIVITY_TTL)
            .unwrap_or(false)
        {
            continue;
        }

        results.push((
            TmuxBellWindow {
                session_name,
                window_id,
            },
            event_time,
        ));
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn load_recent_activity_windows_ignores_old_and_malformed_records() {
        let dir = tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        fs::write(
            &ledger_path,
            concat!(
                "{\"version\":1,\"kind\":\"system-notification\",\"source\":\"vmux\",\"timestamp\":100000,\"title\":\"a\",\"subtitle\":\"b\",\"body\":\"c\",\"latestAssistantMessage\":\"d\",\"cwd\":\"/tmp\",\"session\":{\"sessionId\":\"%1\",\"sessionName\":\"one\",\"windowId\":\"@1\",\"windowIndex\":0,\"windowName\":\"main\",\"paneId\":\"%1\",\"clientName\":\"zsh\",\"clientPid\":123}}\n",
                "not json\n",
                "{\"version\":1,\"kind\":\"system-notification\",\"source\":\"vmux\",\"timestamp\":1,\"title\":\"old\",\"subtitle\":\"b\",\"body\":\"c\",\"latestAssistantMessage\":\"d\",\"cwd\":\"/tmp\",\"session\":{\"sessionId\":\"%2\",\"sessionName\":\"two\",\"windowId\":\"@2\",\"windowIndex\":0,\"windowName\":\"main\",\"paneId\":\"%2\",\"clientName\":\"zsh\",\"clientPid\":123}}\n"
            ),
        )
        .unwrap();

        let now = SystemTime::UNIX_EPOCH + Duration::from_millis(100000 + 60_000);
        let windows = load_recent_activity_windows(&ledger_path, now).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].0.session_name, "one");
        assert_eq!(windows[0].0.window_id, "@1");
    }
}
