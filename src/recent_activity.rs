//! Recent-activity event log.
//!
//! Contract:
//! - the event log is append-only JSONL
//! - only `notify` and `clear` events are valid
//! - `(session_id, pane_id)` is the canonical identity
//! - current sidebar state is reconstructed by replaying valid events in file order
//! - malformed, truncated, or unknown-version lines are ignored line-by-line
//!
//! This module intentionally has no compatibility path for older schemas.

use crate::state::RecentPane;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub(crate) const RECENT_ACTIVITY_TTL: Duration = Duration::from_secs(120);
pub(crate) const DEFAULT_EVENT_LOG_RELATIVE_PATH: &str = ".cache/vmux/recent-activity.jsonl";

/// Schema version for on-disk recent-activity events.
const EVENT_LOG_VERSION: u8 = 1;
/// Rotate the active event log once it grows beyond this size.
const EVENT_LOG_ROTATE_BYTES: u64 = 64 * 1024;
/// Keep at most this many rotated segments alongside the active log.
const EVENT_LOG_MAX_SEGMENTS: usize = 4;

#[derive(Debug)]
pub enum RecentActivityError {
    Usage(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    EventLogWrite(String),
}

impl fmt::Display for RecentActivityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecentActivityError::Usage(message) => write!(f, "recent activity usage error: {message}"),
            RecentActivityError::Io(error) => write!(f, "recent activity io error: {error}"),
            RecentActivityError::Json(error) => write!(f, "recent activity payload parse error: {error}"),
            RecentActivityError::EventLogWrite(message) => write!(f, "recent activity event log error: {message}"),
        }
    }
}

impl std::error::Error for RecentActivityError {}

impl From<std::io::Error> for RecentActivityError {
    fn from(error: std::io::Error) -> Self {
        RecentActivityError::Io(error)
    }
}

impl From<serde_json::Error> for RecentActivityError {
    fn from(error: serde_json::Error) -> Self {
        RecentActivityError::Json(error)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NotifyPayload {
    session_id: String,
    pane_id: String,
    pane_display_text: String,
    notify_time: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ClearPayload {
    session_id: String,
    pane_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NotifyEvent {
    version: u8,
    event: String,
    session_id: String,
    pane_id: String,
    pane_display_text: String,
    notify_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ClearEvent {
    version: u8,
    event: String,
    session_id: String,
    pane_id: String,
}

#[derive(Debug, Clone)]
enum RecentPaneEvent {
    Notify(NotifyEvent),
    Clear(ClearEvent),
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_event_log_path() -> PathBuf {
    home_dir().join(DEFAULT_EVENT_LOG_RELATIVE_PATH)
}

pub(crate) fn event_log_path_from_env() -> PathBuf {
    env::var_os("VMUX_RECENT_ACTIVITY_LOG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(default_event_log_path)
}

fn require_non_empty(field_name: &str, value: String) -> Result<String, RecentActivityError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RecentActivityError::Usage(format!(
            "{field_name} must be a non-empty string"
        )));
    }
    Ok(trimmed.to_string())
}

fn notify_event_from_payload(payload: NotifyPayload) -> Result<NotifyEvent, RecentActivityError> {
    Ok(NotifyEvent {
        version: EVENT_LOG_VERSION,
        event: "notify".to_string(),
        session_id: require_non_empty("sessionId", payload.session_id)?,
        pane_id: require_non_empty("paneId", payload.pane_id)?,
        pane_display_text: require_non_empty("paneDisplayText", payload.pane_display_text)?,
        notify_time: payload.notify_time,
    })
}

fn clear_event_from_payload(payload: ClearPayload) -> Result<ClearEvent, RecentActivityError> {
    Ok(ClearEvent {
        version: EVENT_LOG_VERSION,
        event: "clear".to_string(),
        session_id: require_non_empty("sessionId", payload.session_id)?,
        pane_id: require_non_empty("paneId", payload.pane_id)?,
    })
}

fn append_event_line(path: &Path, line: &str) -> Result<(), RecentActivityError> {
    // Once the append succeeds the event is committed. Rotation is best-effort
    // housekeeping so a rotation failure never invalidates a committed event.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            RecentActivityError::EventLogWrite(format!("failed to open {}: {error}", path.display()))
        })?;

    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;

    let _ = maybe_rotate_event_log(path);
    Ok(())
}

fn rotated_event_log_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "recent-activity".to_string());
    let extension = path.extension().map(|value| value.to_string_lossy().into_owned());
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    let file_name = match extension {
        Some(extension) if !extension.is_empty() => {
            format!("{stem}.{timestamp:020}-{pid}.{extension}")
        }
        _ => format!("{stem}.{timestamp:020}-{pid}"),
    };
    parent.join(file_name)
}

fn rotated_segment_paths(path: &Path) -> Result<Vec<PathBuf>, RecentActivityError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        return Ok(Vec::new());
    }

    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = path.extension().map(|value| value.to_string_lossy().into_owned());

    let mut segments = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let candidate = entry.path();
        if candidate == path {
            continue;
        }
        let Some(name) = candidate.file_name().map(|value| value.to_string_lossy()) else {
            continue;
        };

        let matches = match extension.as_deref() {
            Some(extension) if !extension.is_empty() => {
                name.starts_with(&format!("{stem}.")) && name.ends_with(&format!(".{extension}"))
            }
            _ => name.starts_with(&format!("{stem}.")),
        };
        if matches {
            segments.push(candidate);
        }
    }

    segments.sort();
    Ok(segments)
}

fn replay_paths(path: &Path) -> Result<Vec<PathBuf>, RecentActivityError> {
    let mut paths = rotated_segment_paths(path)?;
    if path.exists() {
        paths.push(path.to_path_buf());
    }
    Ok(paths)
}

fn maybe_rotate_event_log(path: &Path) -> Result<(), RecentActivityError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(RecentActivityError::Io(error)),
    };

    if metadata.len() <= EVENT_LOG_ROTATE_BYTES {
        return Ok(());
    }

    let rotated_path = rotated_event_log_path(path);
    match fs::rename(path, &rotated_path) {
        Ok(()) => prune_old_segments(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RecentActivityError::Io(error)),
    }

    Ok(())
}

fn prune_old_segments(path: &Path) -> Result<(), RecentActivityError> {
    let segments = rotated_segment_paths(path)?;
    if segments.len() <= EVENT_LOG_MAX_SEGMENTS {
        return Ok(());
    }

    for segment in &segments[..segments.len() - EVENT_LOG_MAX_SEGMENTS] {
        let _ = fs::remove_file(segment);
    }
    Ok(())
}

pub fn run_notify(payload_path: &Path) -> Result<(), RecentActivityError> {
    if payload_path.as_os_str().is_empty() {
        return Err(RecentActivityError::Usage("payload path is required".to_string()));
    }

    let payload_text = fs::read_to_string(payload_path).map_err(|error| {
        RecentActivityError::EventLogWrite(format!(
            "failed to read payload {}: {error}",
            payload_path.display()
        ))
    })?;
    let payload: NotifyPayload = serde_json::from_str(&payload_text)?;
    let event = notify_event_from_payload(payload)?;
    let line = serde_json::to_string(&event)?;
    append_event_line(&event_log_path_from_env(), &line)
}

pub fn run_clear(payload_path: &Path) -> Result<(), RecentActivityError> {
    if payload_path.as_os_str().is_empty() {
        return Err(RecentActivityError::Usage("payload path is required".to_string()));
    }

    let payload_text = fs::read_to_string(payload_path).map_err(|error| {
        RecentActivityError::EventLogWrite(format!(
            "failed to read payload {}: {error}",
            payload_path.display()
        ))
    })?;
    let payload: ClearPayload = serde_json::from_str(&payload_text)?;
    let event = clear_event_from_payload(payload)?;
    let line = serde_json::to_string(&event)?;
    append_event_line(&event_log_path_from_env(), &line)
}

fn recent_pane_event_from_line(line: &str) -> Option<RecentPaneEvent> {
    // Replay is crash-tolerant by construction: malformed or truncated lines are
    // dropped in isolation so the rest of the event log remains readable.
    let value: Value = serde_json::from_str(line).ok()?;
    let version = value.get("version")?.as_u64()? as u8;
    if version != EVENT_LOG_VERSION {
        return None;
    }

    match value.get("event")?.as_str()? {
        "notify" => serde_json::from_value::<NotifyEvent>(value)
            .ok()
            .map(RecentPaneEvent::Notify),
        "clear" => serde_json::from_value::<ClearEvent>(value)
            .ok()
            .map(RecentPaneEvent::Clear),
        _ => None,
    }
}

pub fn load_recent_panes(
    event_log_path: &Path,
    now: SystemTime,
) -> Result<Vec<RecentPane>, RecentActivityError> {
    let mut panes_by_key: HashMap<String, RecentPane> = HashMap::new();

    for path in replay_paths(event_log_path)? {
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(RecentActivityError::EventLogWrite(format!(
                    "failed to open {}: {error}",
                    path.display()
                )))
            }
        };

        let reader = BufReader::new(file);
        for line in reader.lines() {
            let Ok(line) = line else {
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            match recent_pane_event_from_line(&line) {
                Some(RecentPaneEvent::Notify(event)) => {
                    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_millis(event.notify_time);
                    let key = format!("{}:{}", event.session_id, event.pane_id);
                    panes_by_key.insert(
                        key,
                        RecentPane {
                            session_id: event.session_id,
                            pane_id: event.pane_id,
                            title: event.pane_display_text,
                            observed_at,
                        },
                    );
                }
                Some(RecentPaneEvent::Clear(event)) => {
                    panes_by_key.remove(&format!("{}:{}", event.session_id, event.pane_id));
                }
                None => {}
            }
        }
    }

    let mut results: Vec<RecentPane> = panes_by_key
        .into_values()
        .filter(|pane| match now.duration_since(pane.observed_at) {
            Ok(age) => age <= RECENT_ACTIVITY_TTL,
            Err(_) => false,
        })
        .collect();

    results.sort_by(|a, b| {
        b.observed_at
            .cmp(&a.observed_at)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.pane_id.cmp(&b.pane_id))
    });
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn load_recent_panes_replays_notify_and_clear_events() {
        let dir = tempdir().unwrap();
        let event_log_path = dir.path().join("recent-activity.jsonl");
        fs::write(
            &event_log_path,
            concat!(
                "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$1\",\"paneId\":\"%1\",\"paneDisplayText\":\"server logs\",\"notifyTime\":100000}\n",
                "{\"version\":1,\"event\":\"clear\",\"sessionId\":\"$1\",\"paneId\":\"%1\"}\n",
                "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$1\",\"paneId\":\"%2\",\"paneDisplayText\":\"worker\",\"notifyTime\":120000}\n"
            ),
        )
        .unwrap();

        let now = SystemTime::UNIX_EPOCH + Duration::from_millis(180000);
        let panes = load_recent_panes(&event_log_path, now).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].session_id, "$1");
        assert_eq!(panes[0].pane_id, "%2");
        assert_eq!(panes[0].title, "worker");
    }

    #[test]
    fn load_recent_panes_latest_notify_wins_for_same_identity() {
        let dir = tempdir().unwrap();
        let event_log_path = dir.path().join("recent-activity.jsonl");
        fs::write(
            &event_log_path,
            concat!(
                "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$1\",\"paneId\":\"%1\",\"paneDisplayText\":\"old title\",\"notifyTime\":100000}\n",
                "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$1\",\"paneId\":\"%1\",\"paneDisplayText\":\"new title\",\"notifyTime\":101000}\n"
            ),
        )
        .unwrap();

        let now = SystemTime::UNIX_EPOCH + Duration::from_millis(161000);
        let panes = load_recent_panes(&event_log_path, now).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].title, "new title");
    }

    #[test]
    fn load_recent_panes_ignores_old_malformed_and_unknown_records() {
        let dir = tempdir().unwrap();
        let event_log_path = dir.path().join("recent-activity.jsonl");
        fs::write(
            &event_log_path,
            concat!(
                "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$1\",\"paneId\":\"%1\",\"paneDisplayText\":\"notification title\",\"notifyTime\":100000}\n",
                "not json\n",
                "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$2\",\"paneId\":\"%2\",\"paneDisplayText\":\"old\",\"notifyTime\":1}\n",
                "{\"version\":99,\"event\":\"notify\",\"sessionId\":\"$3\",\"paneId\":\"%3\",\"paneDisplayText\":\"wrong version\",\"notifyTime\":100000}\n",
                "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$broken\",\"paneId\":\"%3\""
            ),
        )
        .unwrap();

        let now = SystemTime::UNIX_EPOCH + Duration::from_millis(160000);
        let panes = load_recent_panes(&event_log_path, now).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].session_id, "$1");
        assert_eq!(panes[0].pane_id, "%1");
        assert_eq!(panes[0].title, "notification title");
    }

    #[test]
    fn load_recent_panes_reads_rotated_segments_before_active_file() {
        let dir = tempdir().unwrap();
        let event_log_path = dir.path().join("recent-activity.jsonl");
        let rotated_path = dir.path().join("recent-activity.00000000000000000001-1.jsonl");
        fs::write(
            &rotated_path,
            "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$1\",\"paneId\":\"%1\",\"paneDisplayText\":\"from rotated\",\"notifyTime\":100000}\n",
        )
        .unwrap();
        fs::write(
            &event_log_path,
            "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$1\",\"paneId\":\"%1\",\"paneDisplayText\":\"from active\",\"notifyTime\":100001}\n",
        )
        .unwrap();

        let now = SystemTime::UNIX_EPOCH + Duration::from_millis(160001);
        let panes = load_recent_panes(&event_log_path, now).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].title, "from active");
    }

    #[test]
    fn notify_payload_requires_non_empty_identity_and_display_text() {
        let error = notify_event_from_payload(NotifyPayload {
            session_id: "  ".to_string(),
            pane_id: "%1".to_string(),
            pane_display_text: "server logs".to_string(),
            notify_time: 1000,
        })
        .expect_err("empty session id should fail");
        assert!(matches!(error, RecentActivityError::Usage(_)));

        let error = notify_event_from_payload(NotifyPayload {
            session_id: "$1".to_string(),
            pane_id: "%1".to_string(),
            pane_display_text: "   ".to_string(),
            notify_time: 1000,
        })
        .expect_err("empty display text should fail");
        assert!(matches!(error, RecentActivityError::Usage(_)));
    }

    #[test]
    fn load_recent_panes_ignores_future_events() {
        let dir = tempdir().unwrap();
        let event_log_path = dir.path().join("recent-activity.jsonl");
        fs::write(
            &event_log_path,
            "{\"version\":1,\"event\":\"notify\",\"sessionId\":\"$1\",\"paneId\":\"%1\",\"paneDisplayText\":\"future\",\"notifyTime\":200000}\n",
        )
        .unwrap();

        let now = SystemTime::UNIX_EPOCH + Duration::from_millis(100000);
        let panes = load_recent_panes(&event_log_path, now).unwrap();
        assert!(panes.is_empty());
    }
}
