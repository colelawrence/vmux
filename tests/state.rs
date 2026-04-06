use std::time::{Duration, SystemTime};

use vmux::tmux::TmuxBellWindow;
use vmux::{AppState, TmuxSession};

fn make_sessions_with_attached(names: &[(&str, bool)]) -> Vec<TmuxSession> {
    names
        .iter()
        .map(|(name, attached)| TmuxSession {
            name: (*name).to_string(),
            windows: None,
            attached: *attached,
        })
        .collect()
}

fn make_sessions(names: &[&str]) -> Vec<TmuxSession> {
    make_sessions_with_attached(&names.iter().map(|n| (*n, false)).collect::<Vec<_>>())
}

#[test]
fn selection_wraps_around() {
    let sessions = make_sessions(&["a", "b", "c"]);
    let mut state = AppState::new(sessions);
    assert_eq!(state.selected, 0);

    state.move_up();
    assert_eq!(state.selected, 2);

    state.move_down();
    assert_eq!(state.selected, 0);
}

#[test]
fn prefers_attached_session_initially() {
    let sessions = make_sessions_with_attached(&[("a", false), ("b", true), ("c", false)]);
    let state = AppState::new(sessions);

    // Sessions are sorted by name, so the indices are: a -> 0, b -> 1, c -> 2.
    assert_eq!(state.selected, 1);
}

#[test]
fn recent_bells_are_tracked_per_session_and_expire() {
    let mut state = AppState::new(make_sessions(&["a", "b"]));
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

    state.observe_bell_windows(
        vec![
            TmuxBellWindow {
                session_name: "a".to_string(),
                window_id: "@1".to_string(),
            },
            TmuxBellWindow {
                session_name: "a".to_string(),
                window_id: "@2".to_string(),
            },
            TmuxBellWindow {
                session_name: "b".to_string(),
                window_id: "@3".to_string(),
            },
        ],
        observed_at,
    );

    assert_eq!(state.recent_bell_count("a"), 2);
    assert_eq!(state.recent_bell_count("b"), 1);

    state.observe_bell_windows(Vec::new(), observed_at + Duration::from_secs(121));

    assert_eq!(state.recent_bell_count("a"), 0);
    assert_eq!(state.recent_bell_count("b"), 0);
}
