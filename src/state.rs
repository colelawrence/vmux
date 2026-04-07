use crate::tmux::{TmuxBellWindow, TmuxSession};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

const RECENT_BELL_TTL: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
struct RecentBell {
    session_name: String,
    observed_at: SystemTime,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub sessions: Vec<TmuxSession>,
    pub selected: usize,
    recent_bells: HashMap<String, RecentBell>,
}

impl AppState {
    pub fn new(mut sessions: Vec<TmuxSession>) -> Self {
        // Sort sessions by name for a stable, deterministic order.
        sessions.sort_by(|a, b| a.name.cmp(&b.name));

        // Prefer the attached session as the initial selection when present.
        let selected = sessions.iter().position(|s| s.attached).unwrap_or(0);

        Self {
            sessions,
            selected,
            recent_bells: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn observe_bell_windows(&mut self, windows: Vec<TmuxBellWindow>, observed_at: SystemTime) {
        self.observe_bell_windows_at(
            windows
                .into_iter()
                .map(|window| (window, observed_at))
                .collect(),
            observed_at,
        )
    }

    pub fn observe_bell_windows_at(
        &mut self,
        windows: Vec<(TmuxBellWindow, SystemTime)>,
        prune_at: SystemTime,
    ) {
        self.prune_recent_bells(prune_at);

        for (window, observed_at) in windows {
            let key = Self::bell_key(&window.session_name, &window.window_id);
            self.recent_bells.insert(
                key,
                RecentBell {
                    session_name: window.session_name,
                    observed_at,
                },
            );
        }
    }

    pub fn recent_bell_count(&self, session_name: &str) -> usize {
        self.recent_bells
            .values()
            .filter(|bell| bell.session_name == session_name)
            .count()
    }

    fn bell_key(session_name: &str, window_id: &str) -> String {
        format!("{session_name}:{window_id}")
    }

    fn prune_recent_bells(&mut self, observed_at: SystemTime) {
        self.recent_bells.retain(
            |_, bell| match observed_at.duration_since(bell.observed_at) {
                Ok(age) => age <= RECENT_BELL_TTL,
                Err(_) => true,
            },
        );
    }

    pub fn move_up(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.sessions.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.sessions.len();
    }
}
