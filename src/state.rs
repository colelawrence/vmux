use crate::tmux::TmuxSession;
use std::collections::HashMap;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentPane {
    pub session_id: String,
    pub pane_id: String,
    pub title: String,
    pub observed_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPaneTarget {
    pub pane_id: String,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub sessions: Vec<TmuxSession>,
    pub selected: usize,
    selected_pane: Option<SelectedPaneTarget>,
    recent_panes: HashMap<String, RecentPane>,
}

impl AppState {
    pub fn new(mut sessions: Vec<TmuxSession>) -> Self {
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        let selected = sessions.iter().position(|session| session.attached).unwrap_or(0);

        Self {
            sessions,
            selected,
            selected_pane: None,
            recent_panes: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn observe_recent_panes(&mut self, panes: Vec<RecentPane>) {
        self.recent_panes = panes
            .into_iter()
            .map(|pane| (Self::recent_pane_key(&pane.session_id, &pane.pane_id), pane))
            .collect();

        if let Some(selected_pane) = self.selected_pane.as_ref() {
            let session_id = self
                .sessions
                .get(self.selected)
                .map(|session| session.id.as_str())
                .unwrap_or_default();
            let key = Self::recent_pane_key(session_id, &selected_pane.pane_id);
            if !self.recent_panes.contains_key(&key) {
                self.selected_pane = None;
            }
        }
    }

    pub fn recent_panes_for_session(&self, session_id: &str) -> Vec<RecentPane> {
        let mut panes: Vec<RecentPane> = self
            .recent_panes
            .values()
            .filter(|pane| pane.session_id == session_id)
            .cloned()
            .collect();

        panes.sort_by(|a, b| {
            b.observed_at
                .cmp(&a.observed_at)
                .then_with(|| a.title.cmp(&b.title))
                .then_with(|| a.pane_id.cmp(&b.pane_id))
        });
        panes
    }

    pub fn selected_pane_target(&self) -> Option<&SelectedPaneTarget> {
        self.selected_pane.as_ref()
    }

    pub fn select_session(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        self.selected = index;
        self.selected_pane = None;
    }

    pub fn select_pane(&mut self, session_index: usize, pane: &RecentPane) {
        if session_index >= self.sessions.len() {
            return;
        }
        self.selected = session_index;
        self.selected_pane = Some(SelectedPaneTarget {
            pane_id: pane.pane_id.clone(),
        });
    }

    fn recent_pane_key(session_id: &str, pane_id: &str) -> String {
        format!("{session_id}:{pane_id}")
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
        self.selected_pane = None;
    }

    pub fn move_down(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.sessions.len();
        self.selected_pane = None;
    }
}
