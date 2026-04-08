use std::time::{Duration, SystemTime};

use vmux::{AppState, RecentPane, TmuxSession};

fn make_sessions_with_attached(names: &[(&str, bool)]) -> Vec<TmuxSession> {
    names
        .iter()
        .map(|(name, attached)| TmuxSession {
            id: (*name).to_string(),
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

    assert_eq!(state.selected, 1);
}

#[test]
fn recent_panes_are_tracked_per_session() {
    let mut state = AppState::new(make_sessions(&["a", "b"]));
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

    state.observe_recent_panes(vec![
        RecentPane {
            session_id: "a".to_string(),
            pane_id: "%1".to_string(),
            title: "server".to_string(),
            observed_at,
        },
        RecentPane {
            session_id: "a".to_string(),
            pane_id: "%2".to_string(),
            title: "worker".to_string(),
            observed_at,
        },
        RecentPane {
            session_id: "b".to_string(),
            pane_id: "%3".to_string(),
            title: "docs".to_string(),
            observed_at,
        },
    ]);

    assert_eq!(state.recent_panes_for_session("a").len(), 2);
    assert_eq!(state.recent_panes_for_session("b").len(), 1);
}

#[test]
fn selecting_pane_tracks_exact_pane() {
    let mut state = AppState::new(make_sessions(&["a", "b"]));
    let pane = RecentPane {
        session_id: "b".to_string(),
        pane_id: "%42".to_string(),
        title: "logs".to_string(),
        observed_at: SystemTime::UNIX_EPOCH,
    };

    state.select_pane(1, &pane);

    assert_eq!(state.selected, 1);
    let selected = state.selected_pane_target().expect("selected pane target");
    assert_eq!(selected.pane_id, "%42");

    state.select_session(0);
    assert!(state.selected_pane_target().is_none());
}

#[test]
fn selected_pane_clears_when_recent_snapshot_no_longer_contains_it() {
    let mut state = AppState::new(make_sessions(&["a", "b"]));
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let pane = RecentPane {
        session_id: "b".to_string(),
        pane_id: "%42".to_string(),
        title: "logs".to_string(),
        observed_at,
    };

    state.observe_recent_panes(vec![pane.clone()]);
    state.select_pane(1, &pane);
    state.observe_recent_panes(Vec::new());

    assert!(state.selected_pane_target().is_none());
}

#[test]
fn recent_panes_follow_session_id_even_if_session_name_changes() {
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let mut state = AppState::new(vec![TmuxSession {
        id: "$1".to_string(),
        name: "renamed".to_string(),
        windows: None,
        attached: true,
    }]);

    state.observe_recent_panes(vec![RecentPane {
        session_id: "$1".to_string(),
        pane_id: "%1".to_string(),
        title: "server".to_string(),
        observed_at,
    }]);

    assert_eq!(state.recent_panes_for_session("$1").len(), 1);
    assert_eq!(state.recent_panes_for_session("renamed").len(), 0);
}
