use crate::recent_activity;
use crate::state::{AppState, RecentPane, SelectedPaneTarget};
use crate::tmux::{TmuxAdapter, TmuxSession};
use crate::VmuxError;
use crossterm::event::{self, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{execute, terminal};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Terminal;
use std::fs::File;
use std::io::{stdout, Read, Write};
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

struct HostInput {
    rx: Receiver<Vec<u8>>,
    _tx: Sender<Vec<u8>>, // kept alive for the reader thread
    pending: Vec<u8>,
}

impl HostInput {
    fn spawn() -> Result<Self, VmuxError> {
        let mut input = File::open("/dev/tty")
            .or_else(|_| File::open("/dev/stdin"))
            .map_err(|e| VmuxError::Terminal(format!("open terminal input failed: {e}")))?;
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let tx_reader = tx.clone();

        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match input.read(&mut buf) {
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

        Ok(Self {
            rx,
            _tx: tx,
            pending: Vec::new(),
        })
    }

    fn fill_pending(&mut self) {
        while let Ok(chunk) = self.rx.try_recv() {
            self.pending.extend_from_slice(&chunk);
        }
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
        session_id: &str,
        selected_pane: Option<&SelectedPaneTarget>,
        size: Rect,
    ) -> Result<Self, VmuxError> {
        let cmd = adapter
            .build_client_command(session_id, selected_pane.map(|pane| pane.pane_id.as_str()))
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

const RECENT_ACTIVITY_POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_RECENT_PANES_PER_SESSION: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarRow {
    Session {
        session_index: usize,
    },
    Pane {
        session_index: usize,
        pane: RecentPane,
    },
    Spacer,
}

fn session_label_line(session: &TmuxSession) -> Line<'static> {
    let mut spans = Vec::with_capacity(2);
    spans.push(Span::raw(session.name.clone()));
    if session.attached {
        spans.push(Span::raw(" (attached)"));
    }
    Line::from(spans)
}

fn pane_label_line(pane: &RecentPane) -> Line<'static> {
    Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            pane.title.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn build_sidebar_rows(state: &AppState) -> Vec<SidebarRow> {
    let mut rows = Vec::new();

    for (session_index, session) in state.sessions.iter().enumerate() {
        rows.push(SidebarRow::Session { session_index });
        for pane in state
            .recent_panes_for_session(&session.id)
            .into_iter()
            .take(MAX_RECENT_PANES_PER_SESSION)
        {
            rows.push(SidebarRow::Pane {
                session_index,
                pane,
            });
        }
        if session_index + 1 < state.sessions.len() {
            rows.push(SidebarRow::Spacer);
        }
    }

    rows
}

fn sidebar_item(row: &SidebarRow, sessions: &[TmuxSession]) -> ListItem<'static> {
    match row {
        SidebarRow::Session { session_index } => {
            ListItem::new(session_label_line(&sessions[*session_index]))
        }
        SidebarRow::Pane { pane, .. } => ListItem::new(pane_label_line(pane)),
        SidebarRow::Spacer => ListItem::new(Line::raw("")),
    }
}

fn selected_sidebar_row_index(state: &AppState, rows: &[SidebarRow]) -> Option<usize> {
    if let Some(selected_pane) = state.selected_pane_target() {
        if let Some(row_index) = rows.iter().position(|row| {
            matches!(
                row,
                SidebarRow::Pane { session_index, pane }
                    if *session_index == state.selected && pane.pane_id == selected_pane.pane_id
            )
        }) {
            return Some(row_index);
        }
    }

    rows.iter().position(|row| {
        matches!(
            row,
            SidebarRow::Session { session_index } if *session_index == state.selected
        )
    })
}

fn refresh_recent_activity(state: &mut AppState, now: SystemTime) -> Result<(), VmuxError> {
    let path = recent_activity::event_log_path_from_env();
    let panes = recent_activity::load_recent_panes(&path, now)
        .map_err(VmuxError::RecentActivity)?;
    state.observe_recent_panes(panes);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarMouseTarget {
    Row(usize),
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
    rows: &[SidebarRow],
    row_offset: usize,
) -> Option<SidebarMouseTarget> {
    if rect_contains(exit_button, mouse_col, mouse_row) {
        return Some(SidebarMouseTarget::Exit);
    }

    if rows.is_empty() || !rect_contains(sidebar_body, mouse_col, mouse_row) {
        return None;
    }

    let relative_row = mouse_row - sidebar_body.y;
    let row_index = row_offset.saturating_add(relative_row as usize);
    match rows.get(row_index) {
        Some(SidebarRow::Spacer) | None => None,
        Some(_) => Some(SidebarMouseTarget::Row(row_index)),
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

fn tmux_mouse_event_to_bytes(event: &MouseEvent, pane: Rect) -> Vec<u8> {
    let max_x = pane.width.saturating_sub(1);
    let max_y = pane.height.saturating_sub(1);
    let x = event
        .column
        .saturating_sub(pane.x)
        .min(max_x)
        .saturating_add(1);
    let y = event
        .row
        .saturating_sub(pane.y)
        .min(max_y)
        .saturating_add(1);
    let suffix = match event.kind {
        MouseEventKind::Down(_)
        | MouseEventKind::Drag(_)
        | MouseEventKind::Moved
        | MouseEventKind::ScrollUp
        | MouseEventKind::ScrollDown
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => 'M',
        MouseEventKind::Up(_) => 'm',
    };

    let cb = match event.kind {
        MouseEventKind::Down(button) => tmux_mouse_button_code(button),
        MouseEventKind::Up(_) => 3,
        MouseEventKind::Drag(button) => 0b0010_0000 | tmux_mouse_button_code(button),
        MouseEventKind::Moved => 0b0010_0000 | 0b11,
        MouseEventKind::ScrollUp => 0b0100_0000,
        MouseEventKind::ScrollDown => 0b0100_0001,
        MouseEventKind::ScrollLeft => 0b0100_0010,
        MouseEventKind::ScrollRight => 0b0100_0011,
    } | tmux_mouse_modifiers(event.modifiers);

    format!("\x1b[<{};{};{}{}", cb, x, y, suffix).into_bytes()
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

fn visible_cursor_position(screen: &vt100::Screen, pane: Rect) -> Option<(u16, u16)> {
    if pane.width == 0 || pane.height == 0 || screen.hide_cursor() {
        return None;
    }

    let (row, col) = screen.cursor_position();
    if row >= pane.height || col >= pane.width {
        return None;
    }

    Some((pane.x.saturating_add(col), pane.y.saturating_add(row)))
}

const SGR_MOUSE_PREFIX: &[u8] = b"\x1b[<";

fn mouse_prefix_suffix_len(bytes: &[u8]) -> usize {
    let max_len = bytes.len().min(SGR_MOUSE_PREFIX.len());
    for len in (2..=max_len).rev() {
        if bytes[bytes.len() - len..] == SGR_MOUSE_PREFIX[..len] {
            return len;
        }
    }
    0
}

fn find_mouse_prefix(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(SGR_MOUSE_PREFIX.len())
        .position(|window| window == SGR_MOUSE_PREFIX)
}

enum ParsedSgrMouseSequence {
    Complete { event: MouseEvent, consumed: usize },
    Incomplete,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseRoutingResult {
    Handled,
    NotHandled,
    Exit,
}

fn mouse_button_from_code(code: u16) -> Option<MouseButton> {
    match code & 0b11 {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Right),
        _ => None,
    }
}

fn mouse_modifiers_from_code(code: u16) -> KeyModifiers {
    let mut modifiers = KeyModifiers::empty();
    if code & 0b0000_0100 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if code & 0b0000_1000 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if code & 0b0001_0000 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    modifiers
}

fn parse_sgr_mouse_sequence(bytes: &[u8]) -> ParsedSgrMouseSequence {
    if !bytes.starts_with(SGR_MOUSE_PREFIX) {
        return ParsedSgrMouseSequence::Invalid;
    }

    let mut index = SGR_MOUSE_PREFIX.len();
    while index < bytes.len() {
        match bytes[index] {
            b'0'..=b'9' | b';' => index += 1,
            b'M' | b'm' => {
                let suffix = bytes[index];
                let body = match std::str::from_utf8(&bytes[SGR_MOUSE_PREFIX.len()..index]) {
                    Ok(body) => body,
                    Err(_) => return ParsedSgrMouseSequence::Invalid,
                };
                let mut parts = body.split(';');
                let Some(code) = parts.next().and_then(|part| part.parse::<u16>().ok()) else {
                    return ParsedSgrMouseSequence::Invalid;
                };
                let Some(column) = parts.next().and_then(|part| part.parse::<u16>().ok()) else {
                    return ParsedSgrMouseSequence::Invalid;
                };
                let Some(row) = parts.next().and_then(|part| part.parse::<u16>().ok()) else {
                    return ParsedSgrMouseSequence::Invalid;
                };
                if parts.next().is_some() || column == 0 || row == 0 {
                    return ParsedSgrMouseSequence::Invalid;
                }

                let kind = if code & 0b0100_0000 != 0 {
                    match code & 0b11 {
                        0 => MouseEventKind::ScrollUp,
                        1 => MouseEventKind::ScrollDown,
                        2 => MouseEventKind::ScrollLeft,
                        3 => MouseEventKind::ScrollRight,
                        _ => return ParsedSgrMouseSequence::Invalid,
                    }
                } else if suffix == b'm' {
                    let button = mouse_button_from_code(code).unwrap_or(MouseButton::Left);
                    MouseEventKind::Up(button)
                } else if code & 0b0010_0000 != 0 {
                    match mouse_button_from_code(code) {
                        Some(button) => MouseEventKind::Drag(button),
                        None if code & 0b11 == 0b11 => MouseEventKind::Moved,
                        None => return ParsedSgrMouseSequence::Invalid,
                    }
                } else {
                    let Some(button) = mouse_button_from_code(code) else {
                        return ParsedSgrMouseSequence::Invalid;
                    };
                    MouseEventKind::Down(button)
                };

                return ParsedSgrMouseSequence::Complete {
                    event: MouseEvent {
                        kind,
                        column: column - 1,
                        row: row - 1,
                        modifiers: mouse_modifiers_from_code(code),
                    },
                    consumed: index + 1,
                };
            }
            _ => return ParsedSgrMouseSequence::Invalid,
        }
    }

    ParsedSgrMouseSequence::Incomplete
}

fn send_raw_input_to_tmux(tmux_pane: &mut Option<TmuxPane>, bytes: &[u8]) -> Result<(), VmuxError> {
    if bytes.is_empty() {
        return Ok(());
    }

    if let Some(ref mut pane) = tmux_pane {
        pane.send_input(bytes)?;
    }
    Ok(())
}

fn process_host_mouse_event(
    mouse: &MouseEvent,
    pane_area: Rect,
    sidebar_area: Rect,
    sidebar_rows: &[SidebarRow],
    list_state: &ListState,
    state: &mut AppState,
    tmux_pane: &mut Option<TmuxPane>,
    tmux_mouse_captured: &mut bool,
) -> Result<MouseRoutingResult, VmuxError> {
    let sidebar_chunks = split_sidebar(sidebar_area);
    match sidebar_mouse_target_from_point(
        sidebar_chunks[0],
        sidebar_chunks[1],
        mouse.column,
        mouse.row,
        sidebar_rows,
        list_state.offset(),
    ) {
        Some(SidebarMouseTarget::Row(row_index))
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
        {
            if let Some(row) = sidebar_rows.get(row_index) {
                let mut changed = false;
                match row {
                    SidebarRow::Session { session_index } => {
                        changed = state.selected != *session_index
                            || state.selected_pane_target().is_some();
                        if changed {
                            state.select_session(*session_index);
                        }
                    }
                    SidebarRow::Pane {
                        session_index,
                        pane,
                    } => {
                        changed = state.selected != *session_index
                            || state
                                .selected_pane_target()
                                .map(|selected| selected.pane_id != pane.pane_id)
                                .unwrap_or(true);
                        if changed {
                            state.select_pane(*session_index, pane);
                        }
                    }
                    SidebarRow::Spacer => {}
                }

                if changed {
                    *tmux_pane = None;
                }
            }
            *tmux_mouse_captured = false;
            Ok(MouseRoutingResult::Handled)
        }
        Some(SidebarMouseTarget::Exit)
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
        {
            *tmux_mouse_captured = false;
            Ok(MouseRoutingResult::Exit)
        }
        _ if rect_contains(pane_area, mouse.column, mouse.row) || *tmux_mouse_captured => {
            let bytes = tmux_mouse_event_to_bytes(mouse, pane_area);
            send_raw_input_to_tmux(tmux_pane, &bytes)?;
            match mouse.kind {
                MouseEventKind::Down(_) => *tmux_mouse_captured = true,
                MouseEventKind::Up(_) => *tmux_mouse_captured = false,
                MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
                | MouseEventKind::Moved
                | MouseEventKind::Drag(_) => {}
            }
            Ok(MouseRoutingResult::Handled)
        }
        _ => {
            if matches!(mouse.kind, MouseEventKind::Up(_)) {
                *tmux_mouse_captured = false;
            }
            Ok(MouseRoutingResult::NotHandled)
        }
    }
}

fn drain_host_input(
    host_input: &mut HostInput,
    pane_area: Rect,
    sidebar_area: Rect,
    sidebar_rows: &[SidebarRow],
    list_state: &ListState,
    state: &mut AppState,
    tmux_pane: &mut Option<TmuxPane>,
    tmux_mouse_captured: &mut bool,
) -> Result<bool, VmuxError> {
    host_input.fill_pending();

    loop {
        if host_input.pending.starts_with(SGR_MOUSE_PREFIX) {
            match parse_sgr_mouse_sequence(&host_input.pending) {
                ParsedSgrMouseSequence::Complete { event, consumed } => {
                    let consumed_bytes = host_input.pending[..consumed].to_vec();
                    match process_host_mouse_event(
                        &event,
                        pane_area,
                        sidebar_area,
                        sidebar_rows,
                        list_state,
                        state,
                        tmux_pane,
                        tmux_mouse_captured,
                    )? {
                        MouseRoutingResult::Handled => {
                            host_input.pending.drain(..consumed);
                        }
                        MouseRoutingResult::NotHandled => {
                            send_raw_input_to_tmux(tmux_pane, &consumed_bytes)?;
                            host_input.pending.drain(..consumed);
                        }
                        MouseRoutingResult::Exit => {
                            host_input.pending.drain(..consumed);
                            return Ok(true);
                        }
                    }
                }
                ParsedSgrMouseSequence::Incomplete => break,
                ParsedSgrMouseSequence::Invalid => {
                    send_raw_input_to_tmux(tmux_pane, &host_input.pending[..1])?;
                    host_input.pending.drain(..1);
                }
            }
            continue;
        }

        let send_end = if let Some(index) = find_mouse_prefix(&host_input.pending) {
            index
        } else {
            host_input
                .pending
                .len()
                .saturating_sub(mouse_prefix_suffix_len(&host_input.pending))
        };

        if send_end == 0 {
            break;
        }

        send_raw_input_to_tmux(tmux_pane, &host_input.pending[..send_end])?;
        host_input.pending.drain(..send_end);
    }

    Ok(false)
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
    let mut host_input = HostInput::spawn()?;
    let initial_poll = SystemTime::now();
    refresh_recent_activity(&mut state, initial_poll)?;
    let mut last_activity_poll = Instant::now();

    // Initial tmux pane for the selected session.
    let mut tmux_pane: Option<TmuxPane> = None;
    let mut tmux_mouse_captured = false;
    let mut last_pane_area: Option<Rect> = None;

    loop {
        if last_activity_poll.elapsed() >= RECENT_ACTIVITY_POLL_INTERVAL {
            let poll_at = SystemTime::now();
            refresh_recent_activity(&mut state, poll_at)?;
            last_activity_poll = Instant::now();
        }

        let size = guard
            .terminal()
            .size()
            .map_err(|e| VmuxError::Terminal(e.to_string()))?;
        let area = Rect::new(0, 0, size.width, size.height);
        let chunks = split_area(area);
        let sidebar_rows = build_sidebar_rows(&state);

        if tmux_pane.is_none() && !state.sessions.is_empty() {
            let selected = &state.sessions[state.selected];
            // Size the embedded tmux client to the pane that actually renders tmux output.
            tmux_pane = Some(TmuxPane::spawn(
                adapter,
                &selected.id,
                state.selected_pane_target(),
                chunks[0],
            )?);
            last_pane_area = Some(chunks[0]);
        }

        if last_pane_area != Some(chunks[0]) {
            if let Some(ref mut pane) = tmux_pane {
                pane.resize(chunks[0].width, chunks[0].height)?;
            }
            last_pane_area = Some(chunks[0]);
        }

        let mut pane_exited = false;

        guard
            .terminal()
            .draw(|frame| {
                let sidebar_chunks = split_sidebar(chunks[1]);
                let selected_row = selected_sidebar_row_index(&state, &sidebar_rows).unwrap_or(0);
                sync_sidebar_list_offset(
                    &mut list_state,
                    selected_row,
                    sidebar_chunks[0].height as usize,
                    sidebar_rows.len(),
                );
                list_state.select(Some(selected_row));

                let sidebar_body = sidebar_chunks[0];
                let sidebar_exit = sidebar_chunks[1];

                let items: Vec<ListItem> = sidebar_rows
                    .iter()
                    .map(|row| sidebar_item(row, &state.sessions))
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
                    let screen = pane.parser.screen();
                    let lines = screen_to_lines(screen, chunks[0].width, chunks[0].height);
                    let paragraph = Paragraph::new(lines);
                    frame.render_widget(paragraph, chunks[0]);
                    if let Some((cursor_x, cursor_y)) = visible_cursor_position(screen, chunks[0]) {
                        frame.set_cursor_position((cursor_x, cursor_y));
                    }
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

        if drain_host_input(
            &mut host_input,
            chunks[0],
            chunks[1],
            &sidebar_rows,
            &list_state,
            &mut state,
            &mut tmux_pane,
            &mut tmux_mouse_captured,
        )? {
            break;
        }

        thread::sleep(Duration::from_millis(16));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

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
    fn session_label_line_shows_attached_suffix() {
        let session = TmuxSession {
            id: "demo".to_string(),
            name: "demo".to_string(),
            windows: Some(2),
            attached: true,
        };

        let line = session_label_line(&session);
        assert_eq!(line.spans[0].content, "demo");
        assert_eq!(line.spans[1].content, " (attached)");
    }

    #[test]
    fn build_sidebar_rows_groups_panes_under_sessions_with_spacers() {
        let mut state = AppState::new(vec![
            TmuxSession {
                id: "beta".to_string(),
                name: "beta".to_string(),
                windows: None,
                attached: false,
            },
            TmuxSession {
                id: "alpha".to_string(),
                name: "alpha".to_string(),
                windows: None,
                attached: true,
            },
        ]);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        state.observe_recent_panes(vec![
            RecentPane {
                session_id: "alpha".to_string(),
                pane_id: "%1".to_string(),
                title: "server logs".to_string(),
                observed_at: now,
            },
            RecentPane {
                session_id: "beta".to_string(),
                pane_id: "%2".to_string(),
                title: "worker".to_string(),
                observed_at: now,
            },
        ]);

        let rows = build_sidebar_rows(&state);
        assert_eq!(
            rows,
            vec![
                SidebarRow::Session { session_index: 0 },
                SidebarRow::Pane {
                    session_index: 0,
                    pane: RecentPane {
                        session_id: "alpha".to_string(),
                        pane_id: "%1".to_string(),
                        title: "server logs".to_string(),
                        observed_at: now,
                    },
                },
                SidebarRow::Spacer,
                SidebarRow::Session { session_index: 1 },
                SidebarRow::Pane {
                    session_index: 1,
                    pane: RecentPane {
                        session_id: "beta".to_string(),
                        pane_id: "%2".to_string(),
                        title: "worker".to_string(),
                        observed_at: now,
                    },
                },
            ]
        );
    }

    #[test]
    fn sidebar_mouse_target_hits_rows_and_exit_footer() {
        let sidebar_body = Rect::new(2, 1, 20, 5);
        let sidebar_exit = Rect::new(2, 6, 20, 1);
        let rows = vec![
            SidebarRow::Session { session_index: 0 },
            SidebarRow::Pane {
                session_index: 0,
                pane: RecentPane {
                    session_id: "alpha".to_string(),
                    pane_id: "%1".to_string(),
                    title: "server logs".to_string(),
                    observed_at: SystemTime::UNIX_EPOCH,
                },
            },
            SidebarRow::Spacer,
            SidebarRow::Session { session_index: 1 },
            SidebarRow::Pane {
                session_index: 1,
                pane: RecentPane {
                    session_id: "beta".to_string(),
                    pane_id: "%2".to_string(),
                    title: "worker".to_string(),
                    observed_at: SystemTime::UNIX_EPOCH,
                },
            },
        ];

        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 2, 1, &rows, 0),
            Some(SidebarMouseTarget::Row(0)),
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 10, 2, &rows, 0),
            Some(SidebarMouseTarget::Row(1)),
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 8, 4, &rows, 0),
            Some(SidebarMouseTarget::Row(3)),
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 8, 6, &rows, 0),
            Some(SidebarMouseTarget::Exit),
        );
    }

    #[test]
    fn sidebar_mouse_target_ignores_out_of_bounds_clicks_and_spacers() {
        let sidebar_body = Rect::new(2, 1, 20, 3);
        let sidebar_exit = Rect::new(2, 4, 20, 1);
        let rows = vec![
            SidebarRow::Session { session_index: 0 },
            SidebarRow::Spacer,
            SidebarRow::Session { session_index: 1 },
        ];

        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 1, 1, &rows, 0),
            None,
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 25, 1, &rows, 0),
            None,
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 10, 5, &rows, 0),
            None,
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 5, 2, &rows, 0),
            None,
        );
    }

    #[test]
    fn sidebar_mouse_target_applies_list_offset() {
        let sidebar_body = Rect::new(2, 1, 20, 3);
        let sidebar_exit = Rect::new(2, 4, 20, 1);
        let rows = vec![
            SidebarRow::Session { session_index: 0 },
            SidebarRow::Pane {
                session_index: 0,
                pane: RecentPane {
                    session_id: "alpha".to_string(),
                    pane_id: "%1".to_string(),
                    title: "server logs".to_string(),
                    observed_at: SystemTime::UNIX_EPOCH,
                },
            },
            SidebarRow::Spacer,
            SidebarRow::Session { session_index: 1 },
        ];

        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 8, 1, &rows, 1),
            Some(SidebarMouseTarget::Row(1)),
        );
        assert_eq!(
            sidebar_mouse_target_from_point(sidebar_body, sidebar_exit, 8, 2, &rows, 1),
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
    fn parse_sgr_mouse_sequence_decodes_press_release_move_and_modifiers() {
        match parse_sgr_mouse_sequence(b"\x1b[<0;26;4M") {
            ParsedSgrMouseSequence::Complete { event, consumed } => {
                assert_eq!(consumed, 10);
                assert_eq!(
                    event,
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: 25,
                        row: 3,
                        modifiers: KeyModifiers::empty(),
                    }
                );
            }
            _ => panic!("expected complete mouse press"),
        }

        match parse_sgr_mouse_sequence(b"\x1b[<3;26;4m") {
            ParsedSgrMouseSequence::Complete { event, .. } => {
                assert_eq!(
                    event,
                    MouseEvent {
                        kind: MouseEventKind::Up(MouseButton::Left),
                        column: 25,
                        row: 3,
                        modifiers: KeyModifiers::empty(),
                    }
                );
            }
            _ => panic!("expected complete mouse release"),
        }

        match parse_sgr_mouse_sequence(b"\x1b[<68;31;5M") {
            ParsedSgrMouseSequence::Complete { event, .. } => {
                assert_eq!(event.kind, MouseEventKind::ScrollUp);
                assert_eq!(event.column, 30);
                assert_eq!(event.row, 4);
                assert_eq!(event.modifiers, KeyModifiers::SHIFT);
            }
            _ => panic!("expected complete mouse scroll"),
        }

        match parse_sgr_mouse_sequence(b"\x1b[<35;31;5M") {
            ParsedSgrMouseSequence::Complete { event, .. } => {
                assert_eq!(event.kind, MouseEventKind::Moved);
                assert_eq!(event.column, 30);
                assert_eq!(event.row, 4);
            }
            _ => panic!("expected complete mouse move"),
        }
    }

    #[test]
    fn mouse_prefix_suffix_len_preserves_partial_mouse_prefix_without_swallowing_escape() {
        assert_eq!(mouse_prefix_suffix_len(b"hello"), 0);
        assert_eq!(mouse_prefix_suffix_len(b"\x1b"), 0);
        assert_eq!(mouse_prefix_suffix_len(b"\x1b["), 2);
        assert_eq!(mouse_prefix_suffix_len(b"abc\x1b[<"), 3);
        assert_eq!(find_mouse_prefix(b"abc\x1b[<0;1;1M"), Some(3));
        assert!(matches!(
            parse_sgr_mouse_sequence(b"\x1b[<0;1"),
            ParsedSgrMouseSequence::Incomplete
        ));
    }

    #[test]
    fn tmux_mouse_event_to_bytes_encodes_pane_relative_coordinates_and_clamps_capture() {
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
            b"\x1b[<0;2;3M".to_vec(),
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
            b"\x1b[<3;2;3m".to_vec(),
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
            b"\x1b[<68;7;4M".to_vec(),
        );
        assert_eq!(
            tmux_mouse_event_to_bytes(
                &MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: 30,
                    row: 4,
                    modifiers: KeyModifiers::ALT,
                },
                pane,
            ),
            b"\x1b[<43;7;4M".to_vec(),
        );
        assert_eq!(
            tmux_mouse_event_to_bytes(
                &MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    column: 999,
                    row: 999,
                    modifiers: KeyModifiers::empty(),
                },
                pane,
            ),
            b"\x1b[<3;50;10m".to_vec(),
        );
    }

    #[test]
    fn process_host_mouse_event_clears_capture_on_release_outside_pane() {
        let pane_area = Rect::new(0, 0, 76, 10);
        let sidebar_area = Rect::new(76, 0, 24, 10);
        let mut state = AppState::new(vec![TmuxSession {
            id: "demo".to_string(),
            name: "demo".to_string(),
            windows: None,
            attached: true,
        }]);
        let rows = build_sidebar_rows(&state);
        let list_state = ListState::default();
        let mut tmux_pane = None;
        let mut tmux_mouse_captured = true;

        let result = process_host_mouse_event(
            &MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 200,
                row: 200,
                modifiers: KeyModifiers::empty(),
            },
            pane_area,
            sidebar_area,
            &rows,
            &list_state,
            &mut state,
            &mut tmux_pane,
            &mut tmux_mouse_captured,
        )
        .expect("mouse release should succeed");

        assert_eq!(result, MouseRoutingResult::Handled);
        assert!(!tmux_mouse_captured);
    }

    #[test]
    fn process_host_mouse_event_reports_unclaimed_mouse_sequences() {
        let pane_area = Rect::new(0, 0, 76, 10);
        let sidebar_area = Rect::new(76, 0, 24, 10);
        let mut state = AppState::new(vec![TmuxSession {
            id: "demo".to_string(),
            name: "demo".to_string(),
            windows: None,
            attached: true,
        }]);
        let rows = build_sidebar_rows(&state);
        let list_state = ListState::default();
        let mut tmux_pane = None;
        let mut tmux_mouse_captured = false;

        let result = process_host_mouse_event(
            &MouseEvent {
                kind: MouseEventKind::Moved,
                column: 200,
                row: 200,
                modifiers: KeyModifiers::empty(),
            },
            pane_area,
            sidebar_area,
            &rows,
            &list_state,
            &mut state,
            &mut tmux_pane,
            &mut tmux_mouse_captured,
        )
        .expect("mouse routing should succeed");

        assert_eq!(result, MouseRoutingResult::NotHandled);
    }

    #[test]
    fn visible_cursor_position_tracks_tmux_cursor_visibility() {
        let mut parser = Parser::new(4, 8, 0);
        parser.process(b"ab\x1b[2;3H");
        assert_eq!(
            visible_cursor_position(parser.screen(), Rect::new(10, 20, 8, 4)),
            Some((12, 21)),
        );

        parser.process(b"\x1b[?25l");
        assert_eq!(
            visible_cursor_position(parser.screen(), Rect::new(10, 20, 8, 4)),
            None,
        );
    }
}
