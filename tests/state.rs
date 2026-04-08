use std::time::{Duration, SystemTime};

use vmux::{AppState, RecentPane, TmuxSession};

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
fn recent_panes_are_tracked_per_session_and_expire() {
    let mut state = AppState::new(make_sessions(&["a", "b"]));
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

    state.observe_recent_panes(
        vec![
            RecentPane {
                session_name: "a".to_string(),
                window_id: "@1".to_string(),
                pane_id: "%1".to_string(),
                title: "server".to_string(),
                observed_at,
            },
            RecentPane {
                session_name: "a".to_string(),
                window_id: "@2".to_string(),
                pane_id: "%2".to_string(),
                title: "worker".to_string(),
                observed_at,
            },
            RecentPane {
                session_name: "b".to_string(),
                window_id: "@3".to_string(),
                pane_id: "%3".to_string(),
                title: "docs".to_string(),
                observed_at,
            },
        ],
        observed_at,
    );

    assert_eq!(state.recent_panes_for_session("a").len(), 2);
    assert_eq!(state.recent_panes_for_session("b").len(), 1);

    state.observe_recent_panes(Vec::new(), observed_at + Duration::from_secs(121));

    assert!(state.recent_panes_for_session("a").is_empty());
    assert!(state.recent_panes_for_session("b").is_empty());
}

#[test]
fn selecting_pane_tracks_exact_window_and_pane() {
    let mut state = AppState::new(make_sessions(&["a", "b"]));
    let pane = RecentPane {
        session_name: "b".to_string(),
        window_id: "@9".to_string(),
        pane_id: "%42".to_string(),
        title: "logs".to_string(),
        observed_at: SystemTime::UNIX_EPOCH,
    };

    state.select_pane(1, &pane);

    assert_eq!(state.selected, 1);
    let selected = state.selected_pane_target().expect("selected pane target");
    assert_eq!(selected.window_id, "@9");
    assert_eq!(selected.pane_id, "%42");

    state.select_session(0);
    assert!(state.selected_pane_target().is_none());
}
