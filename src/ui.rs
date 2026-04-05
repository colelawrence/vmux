use crate::state::AppState;
use crate::tmux::TmuxAdapter;
use crate::VmuxError;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Terminal;
use std::io::{stdout, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;
use vt100::{Color as VtColor, Parser};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

struct TerminalCleanup {
    raw_enabled: bool,
    alt_screen_enabled: bool,
    mouse_enabled: bool,
}

impl TerminalCleanup {
    fn new() -> Self {
        Self {
            raw_enabled: false,
            alt_screen_enabled: false,
            mouse_enabled: false,
        }
    }
}

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if self.mouse_enabled {
            let mut stdout = stdout();
            let _ = execute!(stdout, event::DisableMouseCapture);
        }
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

        execute!(stdout, event::EnableMouseCapture)
            .map_err(|e| VmuxError::Terminal(e.to_string()))?;
        cleanup.mouse_enabled = true;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostInputAction {
    Quit,
    ToggleFocus,
    MoveUp,
    MoveDown,
    ForwardToTmux,
    Ignore,
}

fn classify_key_event(focus: Focus, key: &crossterm::event::KeyEvent) -> HostInputAction {
    use crossterm::event::KeyModifiers;

    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('q')) {
        return HostInputAction::Quit;
    }

    match focus {
        Focus::Sidebar => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => HostInputAction::Quit,
            KeyCode::Tab | KeyCode::Enter => HostInputAction::ToggleFocus,
            KeyCode::Up | KeyCode::Char('k') => HostInputAction::MoveUp,
            KeyCode::Down | KeyCode::Char('j') => HostInputAction::MoveDown,
            _ => HostInputAction::Ignore,
        },
        Focus::Pane => match key.code {
            KeyCode::Tab => HostInputAction::ToggleFocus,
            _ => HostInputAction::ForwardToTmux,
        },
    }
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
        self.parser.set_size(height, width);
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

fn split_sidebar(area: Rect) -> [Rect; 2] {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    [chunks[0], chunks[1]]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarMouseTarget {
    Session(usize),
    Exit,
}

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn sidebar_mouse_target_from_point(
    sidebar_body: Rect,
    exit_button: Rect,
    mouse_col: u16,
    mouse_row: u16,
    sessions_len: usize,
    session_offset: usize,
) -> Option<SidebarMouseTarget> {
    if rect_contains(exit_button, mouse_col, mouse_row) {
        return Some(SidebarMouseTarget::Exit);
    }

    if sessions_len == 0 || !rect_contains(sidebar_body, mouse_col, mouse_row) {
        return None;
    }

    let relative_row = mouse_row - sidebar_body.y;
    let session_index = session_offset.saturating_add(relative_row as usize);
    if session_index < sessions_len {
        Some(SidebarMouseTarget::Session(session_index))
    } else {
        None
    }
}

fn sync_sidebar_list_offset(
    list_state: &mut ListState,
    selected: usize,
    visible_height: usize,
    items_len: usize,
) {
    if visible_height == 0 || items_len == 0 {
        return;
    }

    let max_offset = items_len.saturating_sub(visible_height);
    let mut offset = list_state.offset().min(max_offset);
    if selected < offset {
        offset = selected;
    }
    if selected >= offset.saturating_add(visible_height) {
        offset = selected.saturating_add(1).saturating_sub(visible_height);
    }

    *list_state.offset_mut() = offset.min(max_offset);
}

fn vt100_color_to_ratatui(color: VtColor) -> Color {
    match color {
        VtColor::Default => Color::Reset,
        VtColor::Idx(index) => Color::Indexed(index),
        VtColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

fn style_from_cell(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();

    match cell.fgcolor() {
        VtColor::Default => {}
        color => style = style.fg(vt100_color_to_ratatui(color)),
    }
    match cell.bgcolor() {
        VtColor::Default => {}
        color => style = style.bg(vt100_color_to_ratatui(color)),
    }

    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }

    style
}

fn screen_to_lines(screen: &vt100::Screen, width: u16, height: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(height as usize);

    for row in 0..height {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(width as usize);
        let mut current_style: Option<Style> = None;
        let mut current_text = String::with_capacity(width as usize);

        let push_run = |spans: &mut Vec<Span<'static>>, style: &mut Option<Style>, text: &mut String| {
            if let Some(style) = style.take() {
                spans.push(Span::styled(std::mem::take(text), style));
            }
        };

        for col in 0..width {
            let Some(cell) = screen.cell(row, col) else {
                if let Some(style) = current_style {
                    current_text.push(' ');
                    current_style = Some(style);
                } else {
                    current_style = Some(Style::default());
                    current_text.push(' ');
                }
                continue;
            };

            // vt100 stores wide glyphs in the leading cell and marks the trailing cell as a continuation.
            // Skipping the continuation keeps the visible width aligned with the terminal buffer.
            if cell.is_wide_continuation() {
                continue;
            }

            let cell_style = style_from_cell(cell);
            let cell_text = if cell.has_contents() {
                cell.contents()
            } else {
                " ".to_string()
            };

            match current_style {
                Some(style) if style == cell_style => {
                    current_text.push_str(&cell_text);
                }
                Some(_) => {
                    push_run(&mut spans, &mut current_style, &mut current_text);
                    current_style = Some(cell_style);
                    current_text = cell_text;
                }
                None => {
                    current_style = Some(cell_style);
                    current_text = cell_text;
                }
            }
        }

        push_run(&mut spans, &mut current_style, &mut current_text);
        lines.push(Line::from(spans));
    }

    lines
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
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        KeyCode::F(5) => b"\x1b[15~".to_vec(),
        KeyCode::F(6) => b"\x1b[17~".to_vec(),
        KeyCode::F(7) => b"\x1b[18~".to_vec(),
        KeyCode::F(8) => b"\x1b[19~".to_vec(),
        KeyCode::F(9) => b"\x1b[20~".to_vec(),
        KeyCode::F(10) => b"\x1b[21~".to_vec(),
        KeyCode::F(11) => b"\x1b[23~".to_vec(),
        KeyCode::F(12) => b"\x1b[24~".to_vec(),
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
    let mut focus = Focus::Pane;

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
                let sidebar_chunks = split_sidebar(chunks[0]);
                sync_sidebar_list_offset(
                    &mut list_state,
                    state.selected,
                    sidebar_chunks[0].height as usize,
                    state.sessions.len(),
                );
                list_state.select(Some(state.selected));

                let sidebar_body = sidebar_chunks[0];
                let sidebar_exit = sidebar_chunks[1];

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
                    .highlight_style(
                        Style::default()
                            .fg(Color::Cyan)
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("▶ ");

                frame.render_stateful_widget(list, sidebar_body, &mut list_state);

                // The full footer row is clickable; the centered label is just the visual affordance.
                let exit_button = Paragraph::new("[ Exit vmux ]")
                    .alignment(Alignment::Center)
                    .style(
                        Style::default()
                            .fg(Color::LightRed)
                            .bg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    );
                frame.render_widget(exit_button, sidebar_exit);

                if let Some(ref mut pane) = tmux_pane {
                    pane_exited = pane.pump();

                    // Render the tmux screen into the right-hand pane.
                    // TODO: if this becomes a hot path, avoid rebuilding the entire pane every frame.
                    let lines = screen_to_lines(pane.parser.screen(), chunks[1].width, chunks[1].height);
                    let paragraph = Paragraph::new(lines);
                    frame.render_widget(paragraph, chunks[1]);
                } else {
                    let paragraph = Paragraph::new("no session");
                    frame.render_widget(paragraph, chunks[1]);
                }
            })
            .map_err(|e| VmuxError::Terminal(e.to_string()))?;

        if pane_exited {
            // A live embedded tmux client is part of the split-view contract.
            return Err(VmuxError::Terminal(
                "embedded tmux client exited".to_string(),
            ));
        }

        // Test-only escape hatch: let smoke tests stop after the first successful draw.
        if cfg!(debug_assertions)
            && std::env::var("VMUX_TEST_ONESHOT").as_deref() == Ok("1")
        {
            break;
        }

        if event::poll(Duration::from_millis(50)).map_err(|e| VmuxError::Terminal(e.to_string()))?
        {
            match event::read().map_err(|e| VmuxError::Terminal(e.to_string()))? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match classify_key_event(focus, &key) {
                        HostInputAction::Quit => break,
                        HostInputAction::ToggleFocus => {
                            focus = match focus {
                                Focus::Sidebar => Focus::Pane,
                                Focus::Pane => Focus::Sidebar,
                            };
                        }
                        HostInputAction::MoveUp => {
                            state.move_up();
                            tmux_pane = None; // respawn for the new selection
                        }
                        HostInputAction::MoveDown => {
                            state.move_down();
                            tmux_pane = None;
                        }
                        HostInputAction::ForwardToTmux => {
                            if let Some(ref mut pane) = tmux_pane {
                                if let Some(bytes) = key_event_to_bytes(&key) {
                                    pane.send_input(&bytes)?;
                                }
                            }
                        }
                        HostInputAction::Ignore => {}
                    }
                }
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row,
                    ..
                }) => {
                    // Mouse hit testing uses the latest drawn sidebar geometry; resize updates are reflected on the next frame.
                    let sidebar_chunks = split_sidebar(chunks[0]);
                    match sidebar_mouse_target_from_point(
                        sidebar_chunks[0],
                        sidebar_chunks[1],
                        column,
                        row,
                        state.sessions.len(),
                        list_state.offset(),
                    ) {
                        Some(SidebarMouseTarget::Session(index)) => {
                            // A single click both selects the session and returns keyboard focus to the pane.
                            if index != state.selected {
                                state.selected = index;
                                tmux_pane = None;
                            }
                            focus = Focus::Pane;
                        }
                        Some(SidebarMouseTarget::Exit) => break,
                        None => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn screen_to_lines_preserves_ansi_colors() {
        let mut parser = Parser::new(1, 2, 0);
        parser.process(b"\x1b[1;38;5;196;48;2;10;20;30mX\x1b[0mY");

        let lines = screen_to_lines(parser.screen(), 2, 1);
        let first = &lines[0].spans[0];
        assert_eq!(first.content, "X");
        assert_eq!(first.style.fg, Some(Color::Indexed(196)));
        assert_eq!(first.style.bg, Some(Color::Rgb(10, 20, 30)));
        assert!(first.style.add_modifier.contains(Modifier::BOLD));

        let second = &lines[0].spans[1];
        assert_eq!(second.content, "Y");
        assert_eq!(second.style, Style::default());
    }

    #[test]
    fn parser_resize_preserves_screen_contents() {
        let mut parser = Parser::new(1, 4, 0);
        parser.process(b"ABCD");
        parser.set_size(2, 6);

        let contents = parser.screen().contents_formatted();
        assert!(
            contents.windows(4).any(|window| window == b"ABCD"),
            "resize should preserve the rendered buffer"
        );
    }

    #[test]
    fn sidebar_mouse_target_hits_session_rows_and_exit_footer() {
        let sidebar_body = Rect::new(2, 1, 20, 5);
        let sidebar_exit = Rect::new(2, 6, 20, 1);

        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 2, 1, 5, 0),
            Some(SidebarMouseTarget::Session(0)),
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 10, 5, 5, 0),
            Some(SidebarMouseTarget::Session(4)),
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 8, 6, 5, 0),
            Some(SidebarMouseTarget::Exit),
        );
    }

    #[test]
    fn sidebar_mouse_target_ignores_out_of_bounds_clicks() {
        let sidebar_body = Rect::new(2, 1, 20, 3);
        let sidebar_exit = Rect::new(2, 4, 20, 1);

        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 1, 1, 3, 0),
            None,
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 25, 1, 3, 0),
            None,
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 10, 5, 3, 0),
            None,
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 5, 3, 0, 0),
            None,
        );
    }

    #[test]
    fn sidebar_mouse_target_applies_list_offset() {
        let sidebar_body = Rect::new(2, 1, 20, 3);
        let sidebar_exit = Rect::new(2, 4, 20, 1);

        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 8, 2, 10, 3),
            Some(SidebarMouseTarget::Session(4)),
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 8, 3, 4, 3),
            None,
        );
    }

    #[test]
    fn sidebar_list_offset_keeps_selected_row_visible() {
        let mut state = ListState::default().with_offset(0);
        sync_sidebar_list_offset(&mut state, 5, 3, 10);
        assert_eq!(state.offset(), 3);
        sync_sidebar_list_offset(&mut state, 1, 3, 10);
        assert_eq!(state.offset(), 1);
    }

    #[test]
    fn sidebar_key_routing_matches_host_contract() {
        assert_eq!(
            classify_key_event(
                Focus::Sidebar,
                &KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
            ),
            HostInputAction::Quit,
        );
        assert_eq!(
            classify_key_event(
                Focus::Sidebar,
                &KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
            ),
            HostInputAction::Quit,
        );
        assert_eq!(
            classify_key_event(
                Focus::Sidebar,
                &KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
            ),
            HostInputAction::ToggleFocus,
        );
        assert_eq!(
            classify_key_event(Focus::Sidebar, &KeyEvent::new(KeyCode::Up, KeyModifiers::empty())),
            HostInputAction::MoveUp,
        );
        assert_eq!(
            classify_key_event(
                Focus::Sidebar,
                &KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
            ),
            HostInputAction::MoveDown,
        );
    }

    #[test]
    fn pane_key_routing_passes_through_most_keys() {
        assert_eq!(
            classify_key_event(
                Focus::Pane,
                &KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
            ),
            HostInputAction::ForwardToTmux,
        );
        assert_eq!(
            classify_key_event(Focus::Pane, &KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
            HostInputAction::ForwardToTmux,
        );
        assert_eq!(
            classify_key_event(Focus::Pane, &KeyEvent::new(KeyCode::Tab, KeyModifiers::empty())),
            HostInputAction::ToggleFocus,
        );
        assert_eq!(
            classify_key_event(
                Focus::Pane,
                &KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
            ),
            HostInputAction::ForwardToTmux,
        );
        assert_eq!(
            classify_key_event(
                Focus::Pane,
                &KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
            ),
            HostInputAction::Quit,
        );
    }

    #[test]
    fn key_event_to_bytes_matches_tmux_friendly_sequences() {
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty())),
            Some(vec![b'a']),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Char('A'), KeyModifiers::CONTROL)),
            Some(vec![1]),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            Some(vec![26]),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
            Some(b"\r".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Tab, KeyModifiers::empty())),
            Some(b"\t".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty())),
            Some(b"\x1b[Z".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty())),
            Some(vec![0x7f]),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Insert, KeyModifiers::empty())),
            Some(b"\x1b[2~".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Delete, KeyModifiers::empty())),
            Some(b"\x1b[3~".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Home, KeyModifiers::empty())),
            Some(b"\x1b[H".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::End, KeyModifiers::empty())),
            Some(b"\x1b[F".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::PageUp, KeyModifiers::empty())),
            Some(b"\x1b[5~".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::PageDown, KeyModifiers::empty())),
            Some(b"\x1b[6~".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Left, KeyModifiers::empty())),
            Some(b"\x1b[D".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Right, KeyModifiers::empty())),
            Some(b"\x1b[C".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Up, KeyModifiers::empty())),
            Some(b"\x1b[A".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Down, KeyModifiers::empty())),
            Some(b"\x1b[B".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::F(1), KeyModifiers::empty())),
            Some(b"\x1bOP".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::F(4), KeyModifiers::empty())),
            Some(b"\x1bOS".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::F(12), KeyModifiers::empty())),
            Some(b"\x1b[24~".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
            Some(b"\x1b".to_vec()),
        );
    }
}
