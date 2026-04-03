use crate::state::{AppAction, AppState};
use crate::tmux::TmuxAdapter;
use crate::VmuxError;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Terminal;
use std::io::stdout;
use std::time::Duration;

struct TerminalCleanup {
    raw_enabled: bool,
    alt_screen_enabled: bool,
}

impl TerminalCleanup {
    fn new() -> Self {
        Self {
            raw_enabled: false,
            alt_screen_enabled: false,
        }
    }
}

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if self.alt_screen_enabled {
            let mut stdout = stdout();
            let _ = execute!(stdout, terminal::LeaveAlternateScreen);
        }
        if self.raw_enabled {
            let _ = disable_raw_mode();
        }
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    #[allow(dead_code)]
    cleanup: TerminalCleanup,
}

impl TerminalGuard {
    fn init() -> Result<Self, VmuxError> {
        let mut cleanup = TerminalCleanup::new();
        enable_raw_mode().map_err(|e| VmuxError::Terminal(e.to_string()))?;
        cleanup.raw_enabled = true;

        let mut stdout = stdout();
        execute!(stdout, terminal::EnterAlternateScreen)
            .map_err(|e| VmuxError::Terminal(e.to_string()))?;
        cleanup.alt_screen_enabled = true;

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).map_err(|e| VmuxError::Terminal(e.to_string()))?;
        Ok(Self { terminal, cleanup })
    }

    fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<std::io::Stdout>> {
        &mut self.terminal
    }
}

pub fn run(adapter: &mut dyn TmuxAdapter) -> Result<(), VmuxError> {
    let sessions = adapter.list_sessions()?;
    if sessions.is_empty() {
        // Nothing to do; just exit cleanly.
        return Ok(());
    }

    // Test hook: when enabled, bypass the interactive TUI and immediately
    // attach/switch to the initially selected session. This still uses the
    // same AppState selection logic (including attached-session preference),
    // but avoids crossterm event handling.
    if cfg!(debug_assertions) && std::env::var("VMUX_TEST_AUTOSELECT").as_deref() == Ok("1") {
        let state = AppState::new(sessions.clone());
        if let Some(AppAction::Attach(idx)) = state.confirm() {
            let session = &state.sessions[idx];
            adapter.attach_or_switch(&session.name)?;
        }
        return Ok(());
    }

    let mut guard = TerminalGuard::init()?;
    let mut state = AppState::new(sessions.clone());
    let mut list_state = ListState::default();

    loop {
        list_state.select(Some(state.selected));
        guard
            .terminal()
            .draw(|frame| {
                let size = frame.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(1)
                    .constraints([Constraint::Min(1)].as_ref())
                    .split(size);

                let items: Vec<ListItem> = state
                    .sessions
                    .iter()
                    .map(|s| {
                        let mut line = s.name.clone();
                        if s.attached {
                            line.push_str(" (attached)");
                        }
                        ListItem::new(line)
                    })
                    .collect();

                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title("vmux sessions"))
                    .highlight_style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("> ");

                frame.render_stateful_widget(list, chunks[0], &mut list_state);
            })
            .map_err(|e| VmuxError::Terminal(e.to_string()))?;

        if event::poll(Duration::from_millis(200)).map_err(|e| VmuxError::Terminal(e.to_string()))? {
            match event::read().map_err(|e| VmuxError::Terminal(e.to_string()))? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Up | KeyCode::Char('k') => state.move_up(),
                    KeyCode::Down | KeyCode::Char('j') => state.move_down(),
                    KeyCode::Enter => {
                        if let Some(AppAction::Attach(idx)) = state.confirm() {
                            let session = &state.sessions[idx];
                            // Drop the guard so the terminal is restored before handing off
                            // control to tmux. The Drop implementation is best-effort and
                            // runs on all exit paths (including panic).
                            drop(guard);
                            adapter.attach_or_switch(&session.name)?;
                            return Ok(());
                        }
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {
                    // Just redraw on next loop iteration.
                }
                _ => {}
            }
        }
    }

    // User quit without attaching. `guard` is dropped here, restoring the terminal.
    Ok(())
}
