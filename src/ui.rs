use crate::notify;
use crate::state::AppState;
use crate::tmux::{TmuxAdapter, TmuxSession};
use crate::VmuxError;
use crossterm::event::{self, Event, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{execute, terminal};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Terminal;
use std::io::{stdout, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use vt100::{Color as VtColor, Parser};

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

impl TmuxPane {
    fn spawn(
        adapter: &mut dyn TmuxAdapter,
        session_name: &str,
        size: Rect,
    ) -> Result<Self, VmuxError> {
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

const RECENT_BELL_POLL_INTERVAL: Duration = Duration::from_millis(500);

fn session_label_line(session: &TmuxSession, recent_bell_count: usize) -> Line<'static> {
    let mut spans = Vec::with_capacity(4);
    spans.push(Span::raw(session.name.clone()));
    if session.attached {
        spans.push(Span::raw(" (attached)"));
    }
    if recent_bell_count > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("!{recent_bell_count}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn refresh_recent_activity(state: &mut AppState, now: SystemTime) -> Result<(), VmuxError> {
    let path = recent_activity_ledger_path();
    let windows = notify::load_recent_activity_windows(&path, now).map_err(VmuxError::Notify)?;
    state.observe_bell_windows_at(windows, now);
    Ok(())
}

fn split_area(area: Rect) -> [Rect; 2] {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(24)])
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

fn default_recent_activity_ledger_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cache/vmux/session-updates.jsonl")
}

fn recent_activity_ledger_path() -> PathBuf {
    std::env::var_os("VMUX_NOTIFY_LEDGER_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(default_recent_activity_ledger_path)
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

fn tmux_mouse_button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn tmux_mouse_modifiers(modifiers: crossterm::event::KeyModifiers) -> u16 {
    let mut code = 0;
    if modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
        code |= 0b0000_0100;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::ALT) {
        code |= 0b0000_1000;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
        code |= 0b0001_0000;
    }
    code
}

fn tmux_mouse_event_to_bytes(event: &MouseEvent, pane: Rect) -> Option<Vec<u8>> {
    if !rect_contains(pane, event.column, event.row) {
        return None;
    }

    let x = event.column.saturating_sub(pane.x).saturating_add(1);
    let y = event.row.saturating_sub(pane.y).saturating_add(1);
    let suffix = match event.kind {
        MouseEventKind::Down(_)
        | MouseEventKind::Drag(_)
        | MouseEventKind::ScrollUp
        | MouseEventKind::ScrollDown
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => 'M',
        MouseEventKind::Up(_) => 'm',
        MouseEventKind::Moved => return None,
    };

    let cb = match event.kind {
        MouseEventKind::Down(button) => tmux_mouse_button_code(button),
        MouseEventKind::Up(_) => 3,
        MouseEventKind::Drag(button) => 0b0010_0000 | tmux_mouse_button_code(button),
        MouseEventKind::Moved => return None,
        MouseEventKind::ScrollUp => 0b0100_0000,
        MouseEventKind::ScrollDown => 0b0100_0001,
        MouseEventKind::ScrollLeft => 0b0100_0010,
        MouseEventKind::ScrollRight => 0b0100_0011,
    } | tmux_mouse_modifiers(event.modifiers);

    Some(format!("\x1b[<{};{};{}{}", cb, x, y, suffix).into_bytes())
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

        let push_run =
            |spans: &mut Vec<Span<'static>>, style: &mut Option<Style>, text: &mut String| {
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

fn control_code(c: char) -> Option<u8> {
    if !c.is_ascii() {
        return None;
    }

    if c == '?' {
        return Some(0x7f);
    }

    Some((c as u8) & 0x1f)
}

fn key_modifier_value(modifiers: crossterm::event::KeyModifiers) -> Option<u8> {
    use crossterm::event::KeyModifiers;

    let shift = u8::from(modifiers.contains(KeyModifiers::SHIFT));
    let alt_like = u8::from(modifiers.intersects(
        KeyModifiers::ALT | KeyModifiers::META | KeyModifiers::SUPER | KeyModifiers::HYPER,
    ));
    let control = u8::from(modifiers.contains(KeyModifiers::CONTROL));

    if shift == 0 && alt_like == 0 && control == 0 {
        None
    } else {
        Some(1 + shift + (alt_like * 2) + (control * 4))
    }
}

fn has_escape_prefix(modifiers: crossterm::event::KeyModifiers) -> bool {
    use crossterm::event::KeyModifiers;

    modifiers.intersects(
        KeyModifiers::ALT | KeyModifiers::META | KeyModifiers::SUPER | KeyModifiers::HYPER,
    )
}

fn modified_csi_sequence(
    parameter: u8,
    final_char: char,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<Vec<u8>> {
    key_modifier_value(modifiers)
        .map(|modifier| format!("\x1b[{parameter};{modifier}{final_char}").into_bytes())
}

fn key_event_to_bytes(key: &crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyModifiers};

    let bytes =
        match key.code {
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut bytes = vec![control_code(c)?];
                if has_escape_prefix(key.modifiers) {
                    bytes.insert(0, 0x1b);
                }
                bytes
            }
            KeyCode::Char(c) => {
                let mut bytes = c.to_string().into_bytes();
                if has_escape_prefix(key.modifiers) {
                    bytes.insert(0, 0x1b);
                }
                bytes
            }
            KeyCode::Enter => b"\r".to_vec(),
            KeyCode::Tab => b"\t".to_vec(),
            KeyCode::BackTab => {
                if let Some(modifier) = key_modifier_value(key.modifiers) {
                    if modifier == 2 {
                        b"\x1b[Z".to_vec()
                    } else {
                        format!("\x1b[1;{modifier}Z").into_bytes()
                    }
                } else {
                    b"\x1b[Z".to_vec()
                }
            }
            KeyCode::Backspace => vec![0x7f],
            KeyCode::Insert => {
                modified_csi_sequence(2, '~', key.modifiers).unwrap_or_else(|| b"\x1b[2~".to_vec())
            }
            KeyCode::Delete => {
                modified_csi_sequence(3, '~', key.modifiers).unwrap_or_else(|| b"\x1b[3~".to_vec())
            }
            KeyCode::Home => {
                modified_csi_sequence(1, 'H', key.modifiers).unwrap_or_else(|| b"\x1b[H".to_vec())
            }
            KeyCode::End => {
                modified_csi_sequence(1, 'F', key.modifiers).unwrap_or_else(|| b"\x1b[F".to_vec())
            }
            KeyCode::PageUp => {
                modified_csi_sequence(5, '~', key.modifiers).unwrap_or_else(|| b"\x1b[5~".to_vec())
            }
            KeyCode::PageDown => {
                modified_csi_sequence(6, '~', key.modifiers).unwrap_or_else(|| b"\x1b[6~".to_vec())
            }
            KeyCode::Left => {
                modified_csi_sequence(1, 'D', key.modifiers).unwrap_or_else(|| b"\x1b[D".to_vec())
            }
            KeyCode::Right => {
                modified_csi_sequence(1, 'C', key.modifiers).unwrap_or_else(|| b"\x1b[C".to_vec())
            }
            KeyCode::Up => {
                modified_csi_sequence(1, 'A', key.modifiers).unwrap_or_else(|| b"\x1b[A".to_vec())
            }
            KeyCode::Down => {
                modified_csi_sequence(1, 'B', key.modifiers).unwrap_or_else(|| b"\x1b[B".to_vec())
            }
            KeyCode::F(1) => {
                modified_csi_sequence(1, 'P', key.modifiers).unwrap_or_else(|| b"\x1bOP".to_vec())
            }
            KeyCode::F(2) => {
                modified_csi_sequence(1, 'Q', key.modifiers).unwrap_or_else(|| b"\x1bOQ".to_vec())
            }
            KeyCode::F(3) => {
                modified_csi_sequence(1, 'R', key.modifiers).unwrap_or_else(|| b"\x1bOR".to_vec())
            }
            KeyCode::F(4) => {
                modified_csi_sequence(1, 'S', key.modifiers).unwrap_or_else(|| b"\x1bOS".to_vec())
            }
            KeyCode::F(5) => modified_csi_sequence(15, '~', key.modifiers)
                .unwrap_or_else(|| b"\x1b[15~".to_vec()),
            KeyCode::F(6) => modified_csi_sequence(17, '~', key.modifiers)
                .unwrap_or_else(|| b"\x1b[17~".to_vec()),
            KeyCode::F(7) => modified_csi_sequence(18, '~', key.modifiers)
                .unwrap_or_else(|| b"\x1b[18~".to_vec()),
            KeyCode::F(8) => modified_csi_sequence(19, '~', key.modifiers)
                .unwrap_or_else(|| b"\x1b[19~".to_vec()),
            KeyCode::F(9) => modified_csi_sequence(20, '~', key.modifiers)
                .unwrap_or_else(|| b"\x1b[20~".to_vec()),
            KeyCode::F(10) => modified_csi_sequence(21, '~', key.modifiers)
                .unwrap_or_else(|| b"\x1b[21~".to_vec()),
            KeyCode::F(11) => modified_csi_sequence(23, '~', key.modifiers)
                .unwrap_or_else(|| b"\x1b[23~".to_vec()),
            KeyCode::F(12) => modified_csi_sequence(24, '~', key.modifiers)
                .unwrap_or_else(|| b"\x1b[24~".to_vec()),
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
    let initial_poll = SystemTime::now();
    state.observe_bell_windows(adapter.list_bell_windows()?, initial_poll);
    refresh_recent_activity(&mut state, initial_poll)?;
    let mut last_bell_poll = Instant::now();

    // Initial tmux pane for the selected session.
    let mut tmux_pane: Option<TmuxPane> = None;
    let mut tmux_mouse_captured = false;

    loop {
        if last_bell_poll.elapsed() >= RECENT_BELL_POLL_INTERVAL {
            let poll_at = SystemTime::now();
            state.observe_bell_windows(adapter.list_bell_windows()?, poll_at);
            refresh_recent_activity(&mut state, poll_at)?;
            last_bell_poll = Instant::now();
        }

        let size = guard
            .terminal()
            .size()
            .map_err(|e| VmuxError::Terminal(e.to_string()))?;
        let area = Rect::new(0, 0, size.width, size.height);
        let chunks = split_area(area);

        if tmux_pane.is_none() && !state.sessions.is_empty() {
            let selected = &state.sessions[state.selected];
            // Size the embedded tmux client to the pane that actually renders tmux output.
            tmux_pane = Some(TmuxPane::spawn(adapter, &selected.name, chunks[0])?);
        }

        let mut pane_exited = false;

        guard
            .terminal()
            .draw(|frame| {
                let sidebar_chunks = split_sidebar(chunks[1]);
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
                    .map(|s| ListItem::new(session_label_line(s, state.recent_bell_count(&s.name))))
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

                    // Render the tmux screen into the left-hand pane.
                    // TODO: if this becomes a hot path, avoid rebuilding the entire pane every frame.
                    let lines =
                        screen_to_lines(pane.parser.screen(), chunks[0].width, chunks[0].height);
                    let paragraph = Paragraph::new(lines);
                    frame.render_widget(paragraph, chunks[0]);
                } else {
                    let paragraph = Paragraph::new("no session");
                    frame.render_widget(paragraph, chunks[0]);
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
        if cfg!(debug_assertions) && std::env::var("VMUX_TEST_ONESHOT").as_deref() == Ok("1") {
            break;
        }

        if event::poll(Duration::from_millis(50)).map_err(|e| VmuxError::Terminal(e.to_string()))? {
            match event::read().map_err(|e| VmuxError::Terminal(e.to_string()))? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(ref mut pane) = tmux_pane {
                        if let Some(bytes) = key_event_to_bytes(&key) {
                            pane.send_input(&bytes)?;
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    // Mouse hit testing uses the latest drawn sidebar geometry; resize updates are reflected on the next frame.
                    let sidebar_chunks = split_sidebar(chunks[1]);
                    match sidebar_mouse_target_from_point(
                        sidebar_chunks[0],
                        sidebar_chunks[1],
                        mouse.column,
                        mouse.row,
                        state.sessions.len(),
                        list_state.offset(),
                    ) {
                        Some(SidebarMouseTarget::Session(index))
                            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
                        {
                            if index != state.selected {
                                state.selected = index;
                                tmux_pane = None;
                            }
                            tmux_mouse_captured = false;
                        }
                        Some(SidebarMouseTarget::Exit)
                            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
                        {
                            break;
                        }
                        _ if rect_contains(chunks[0], mouse.column, mouse.row)
                            || tmux_mouse_captured =>
                        {
                            if let Some(ref mut pane) = tmux_pane {
                                if let Some(bytes) = tmux_mouse_event_to_bytes(&mouse, chunks[0]) {
                                    pane.send_input(&bytes)?;
                                    match mouse.kind {
                                        MouseEventKind::Down(_) => tmux_mouse_captured = true,
                                        MouseEventKind::Up(_) => tmux_mouse_captured = false,
                                        MouseEventKind::ScrollUp
                                        | MouseEventKind::ScrollDown
                                        | MouseEventKind::ScrollLeft
                                        | MouseEventKind::ScrollRight
                                        | MouseEventKind::Moved
                                        | MouseEventKind::Drag(_) => {}
                                    }
                                }
                            }
                        }
                        _ => {
                            if matches!(mouse.kind, MouseEventKind::Up(_)) {
                                tmux_mouse_captured = false;
                            }
                        }
                    }
                }
                Event::Resize(cols, rows) => {
                    let area = Rect::new(0, 0, cols, rows);
                    let chunks = split_area(area);
                    if let Some(ref mut pane) = tmux_pane {
                        pane.resize(chunks[0].width, chunks[0].height)?;
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
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

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
    fn split_area_places_sidebar_on_right() {
        let chunks = split_area(Rect::new(0, 0, 100, 10));
        assert_eq!(chunks[0], Rect::new(0, 0, 76, 10));
        assert_eq!(chunks[1], Rect::new(76, 0, 24, 10));
    }

    #[test]
    fn session_label_line_shows_recent_bell_badge() {
        let session = TmuxSession {
            name: "demo".to_string(),
            windows: Some(2),
            attached: true,
        };

        let line = session_label_line(&session, 2);
        assert_eq!(line.spans[0].content, "demo");
        assert_eq!(line.spans[1].content, " (attached)");
        assert_eq!(line.spans[2].content, " ");
        assert_eq!(line.spans[3].content, "!2");
        assert_eq!(line.spans[3].style.fg, Some(Color::Yellow));
        assert!(line.spans[3].style.add_modifier.contains(Modifier::BOLD));
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
            key_event_to_bytes(&KeyEvent::new(KeyCode::Char('['), KeyModifiers::CONTROL)),
            Some(vec![27]),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)),
            Some(b"\x1ba".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)),
            Some(b"\x1b[1;3A".to_vec()),
        );
        assert_eq!(
            key_event_to_bytes(&KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL)),
            Some(b"\x1b[1;5C".to_vec()),
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

    #[test]
    fn tmux_mouse_event_to_bytes_encodes_pane_relative_coordinates() {
        let pane = Rect::new(24, 1, 50, 10);

        assert_eq!(
            tmux_mouse_event_to_bytes(
                &MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 25,
                    row: 3,
                    modifiers: KeyModifiers::empty(),
                },
                pane,
            ),
            Some(b"\x1b[<0;2;3M".to_vec()),
        );
        assert_eq!(
            tmux_mouse_event_to_bytes(
                &MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    column: 25,
                    row: 3,
                    modifiers: KeyModifiers::empty(),
                },
                pane,
            ),
            Some(b"\x1b[<3;2;3m".to_vec()),
        );
        assert_eq!(
            tmux_mouse_event_to_bytes(
                &MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    column: 30,
                    row: 4,
                    modifiers: KeyModifiers::SHIFT,
                },
                pane,
            ),
            Some(b"\x1b[<68;7;4M".to_vec()),
        );
    }
}
