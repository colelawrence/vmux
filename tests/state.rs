use vmux::{AppAction, AppState, TmuxSession};

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
fn confirm_returns_attach_when_non_empty() {
    let sessions = make_sessions(&["s1", "s2"]);
    let state = AppState::new(sessions);
    let action = state.confirm();
    assert_eq!(action, Some(AppAction::Attach(0)));
}

#[test]
fn confirm_none_when_empty() {
    let sessions: Vec<TmuxSession> = Vec::new();
    let state = AppState::new(sessions);
    assert!(state.confirm().is_none());
}

#[test]
fn prefers_attached_session_initially() {
    let sessions = make_sessions_with_attached(&[("a", false), ("b", true), ("c", false)]);
    let state = AppState::new(sessions);

    // Sessions are sorted by name, so the indices are: a -> 0, b -> 1, c -> 2.
    assert_eq!(state.selected, 1);
}
