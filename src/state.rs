use crate::tmux::TmuxSession;

#[derive(Debug, Clone)]
pub struct AppState {
    pub sessions: Vec<TmuxSession>,
    pub selected: usize,
}

impl AppState {
    pub fn new(mut sessions: Vec<TmuxSession>) -> Self {
        // Sort sessions by name for a stable, deterministic order.
        sessions.sort_by(|a, b| a.name.cmp(&b.name));

        // Prefer the attached session as the initial selection when present.
        let selected = sessions
            .iter()
            .position(|s| s.attached)
            .unwrap_or(0);

        Self { sessions, selected }
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
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
