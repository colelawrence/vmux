use crate::state::AppState;
use crate::tmux::TmuxAdapter;
use crate::VmuxError;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Terminal;
use std::io::{stdout, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;
use vt100::Parser;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

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

struct TmuxPane {
    parser: Parser,
    rx: Receiver<Vec<u8>>,
    _tx: Sender<Vec<u8>>, // kept alive for the reader thread
    master: Box<dyn MasterPty + Send>,
    _child: Box<dyn Child + Send>,
    _writer: Box<dyn Write + Send>,
    exited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Pane,
}

impl TmuxPane {
    fn spawn(adapter: &mut dyn TmuxAdapter, session_name: &str, size: Rect) -> Result<Self, VmuxError> {
        let cmd = adapter
            .build_client_command(session_name)
            .map_err(VmuxError::Tmux)?;

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: size.height,
                cols: size.width,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| VmuxError::Terminal(format!("openpty failed: {e}")))?;

        let mut builder = CommandBuilder::new(cmd.get_program());
        for arg in cmd.get_args() {
            builder.arg(arg);
        }

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| VmuxError::Terminal(format!("spawn tmux client failed: {e}")))?;

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| VmuxError::Terminal(format!("clone pty reader failed: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| VmuxError::Terminal(format!("take pty writer failed: {e}")))?;
        let master = pair.master;

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let tx_reader = tx.clone();

        // Reader thread: forward all bytes from the PTY into an mpsc channel.
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx_reader.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let parser = Parser::new(size.height as u16, size.width as u16, 0);

        Ok(Self {
            parser,
            rx,
            _tx: tx,
            master,
            _child: child,
            _writer: writer,
            exited: false,
        })
    }

    fn pump(&mut self) -> bool {
        while let Ok(chunk) = self.rx.try_recv() {
            self.parser.process(&chunk);
        }

        if !self.exited {
            match self._child.try_wait() {
                Ok(Some(_)) => {
                    self.exited = true;
                    return true;
                }
                Ok(None) => {}
                Err(_) => {
                    self.exited = true;
                    return true;
                }
            }
        }

        false
    }

    fn resize(&mut self, width: u16, height: u16) -> Result<(), VmuxError> {
        self.master
            .resize(PtySize {
                rows: height,
                cols: width,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| VmuxError::Terminal(format!("resize tmux pane failed: {e}")))?;
        self.parser = Parser::new(height as u16, width as u16, 0);
        Ok(())
    }

    fn send_input(&mut self, bytes: &[u8]) -> Result<(), VmuxError> {
        self._writer
            .write_all(bytes)
            .map_err(|e| VmuxError::Terminal(format!("write to tmux pane failed: {e}")))?;
        self._writer
            .flush()
            .map_err(|e| VmuxError::Terminal(format!("flush tmux pane failed: {e}")))?;
        Ok(())
    }
}

impl Drop for TmuxPane {
    fn drop(&mut self) {
        let _ = self._child.kill();
        let _ = self._child.wait();
    }
}

fn split_area(area: Rect) -> [Rect; 2] {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(1)])
        .split(area);
    [chunks[0], chunks[1]]
}

fn key_event_to_bytes(key: &crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyModifiers};

    let bytes = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let ch = c.to_ascii_lowercase();
            if ('a'..='z').contains(&ch) {
                vec![(ch as u8) - b'a' + 1]
            } else {
                return None;
            }
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        _ => return None,
    };

    Some(bytes)
}

pub fn run(adapter: &mut dyn TmuxAdapter) -> Result<(), VmuxError> {
    let sessions = adapter.list_sessions()?;
    if sessions.is_empty() {
        // Nothing to do; just exit cleanly.
        return Ok(());
    }

    let mut guard = TerminalGuard::init()?;
    let mut state = AppState::new(sessions.clone());
    let mut list_state = ListState::default();
    let mut focus = Focus::Sidebar;

    // Initial tmux pane for the selected session.
    let mut tmux_pane: Option<TmuxPane> = None;

    loop {
        let size = guard
            .terminal()
            .size()
            .map_err(|e| VmuxError::Terminal(e.to_string()))?;
        let area = Rect::new(0, 0, size.width, size.height);
        let chunks = split_area(area);

        if tmux_pane.is_none() && !state.sessions.is_empty() {
            let selected = &state.sessions[state.selected];
            tmux_pane = Some(TmuxPane::spawn(adapter, &selected.name, chunks[1])?);
        }

        let mut pane_exited = false;

        guard
            .terminal()
            .draw(|frame| {
                list_state.select(Some(state.selected));

                let sidebar_title = match focus {
                    Focus::Sidebar => "sessions (sidebar)",
                    Focus::Pane => "sessions",
                };

                let items: Vec<ListItem> = state
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(index, s)| {
                        let mut line = if index == state.selected {
                            format!("> {}", s.name)
                        } else {
                            format!("  {}", s.name)
                        };
                        if s.attached {
                            line.push_str(" (attached)");
                        }
                        if index == state.selected {
                            line.push_str(" *");
                        }
                        ListItem::new(line)
                    })
                    .collect();

                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(sidebar_title))
                    .highlight_style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("> ");

                frame.render_stateful_widget(list, chunks[0], &mut list_state);

                if let Some(ref mut pane) = tmux_pane {
                    pane_exited = pane.pump();

                    // Render the tmux screen into the right-hand pane.
                    let mut lines = Vec::new();
                    let screen = pane.parser.screen().clone();
                    for row in 0..chunks[1].height {
                        let mut line = String::new();
                        for col in 0..chunks[1].width {
                            if let Some(cell) = screen.cell(row, col) {
                                let ch = cell.contents().to_string();
                                if ch.is_empty() {
                                    line.push(' ');
                                } else {
                                    line.push_str(&ch);
                                }
                            } else {
                                line.push(' ');
                            }
                        }
                        lines.push(line);
                    }

                    let title = match focus {
                        Focus::Sidebar => "tmux (live)",
                        Focus::Pane => "tmux (focused)",
                    };
                    let paragraph = Paragraph::new(lines.join("\n")).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(title),
                    );
                    frame.render_widget(paragraph, chunks[1]);
                } else {
                    let paragraph = Paragraph::new("no session").block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("tmux"),
                    );
                    frame.render_widget(paragraph, chunks[1]);
                }
            })
            .map_err(|e| VmuxError::Terminal(e.to_string()))?;

        if pane_exited {
            return Err(VmuxError::Terminal(
                "embedded tmux client exited".to_string(),
            ));
        }

        if cfg!(debug_assertions)
            && std::env::var("VMUX_TEST_ONESHOT").as_deref() == Ok("1")
        {
            break;
        }

        if event::poll(Duration::from_millis(50)).map_err(|e| VmuxError::Terminal(e.to_string()))?
        {
            match event::read().map_err(|e| VmuxError::Terminal(e.to_string()))? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('q'))
                    {
                        break;
                    }

                    match focus {
                        Focus::Sidebar => match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Tab | KeyCode::Enter => {
                                focus = Focus::Pane;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                state.move_up();
                                tmux_pane = None; // respawn for the new selection
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                state.move_down();
                                tmux_pane = None;
                            }
                            _ => {}
                        },
                        Focus::Pane => match key.code {
                            KeyCode::Tab => {
                                focus = Focus::Sidebar;
                            }
                            _ => {
                                if let Some(ref mut pane) = tmux_pane {
                                    if let Some(bytes) = key_event_to_bytes(&key) {
                                        pane.send_input(&bytes)?;
                                    }
                                }
                            }
                        },
                    }
                }
                Event::Resize(cols, rows) => {
                    let area = Rect::new(0, 0, cols, rows);
                    let chunks = split_area(area);
                    if let Some(ref mut pane) = tmux_pane {
                        pane.resize(chunks[1].width, chunks[1].height)?;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}
