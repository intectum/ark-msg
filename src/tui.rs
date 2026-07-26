use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ark::types::IdentityContext;

use crate::{convo, message, reltime, sync};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;

const SYNC_INTERVAL: Duration = Duration::from_secs(5);

enum UiEvent {
    Key(crossterm::event::KeyEvent),
    SyncStarted,
    SyncDone(String),
}

enum Pane { Convos, Input }

struct App {
    ctx: Arc<IdentityContext>,
    convos: Vec<convo::ConvoSummary>,
    convo_state: ListState,
    messages: Vec<(message::MessageSummary, String)>,
    input: String,
    pane: Pane,
    status: String,
    quit: bool,
    sync_trigger: Sender<()>,
}

impl App {
    fn new(ctx: Arc<IdentityContext>, sync_trigger: Sender<()>) -> Self {
        let mut app = Self {
            ctx,
            convos: Vec::new(),
            convo_state: ListState::default(),
            messages: Vec::new(),
            input: String::new(),
            pane: Pane::Convos,
            status: "starting...".into(),
            quit: false,
            sync_trigger,
        };
        app.reload_convos();
        if !app.convos.is_empty() {
            app.convo_state.select(Some(0));
            // Messages already-decrypted on disk are cheap to load. If any are
            // still encrypted-at-rest, `read` falls back to a decrypt round
            // trip; sync worker calls `ensure_decrypted_all` after each sync
            // to keep that path cold.
            app.reload_messages();
        }
        app
    }

    fn reload_convos(&mut self) {
        match convo::list(&self.ctx) {
            Ok(v) => { self.convos = v; }
            Err(e) => { self.status = format!("list error: {}", e); }
        }
    }

    fn reload_messages(&mut self) {
        let Some(dir) = self.selected_dir() else {
            self.messages.clear();
            return;
        };
        match message::read(&self.ctx, &dir, None) {
            Ok(msgs) => { self.messages = msgs; }
            Err(e) => { self.status = format!("read error: {}", e); }
        }
    }

    fn selected_dir(&self) -> Option<String> {
        self.convo_state.selected().and_then(|i| self.convos.get(i)).map(|c| c.dir_name.clone())
    }

    fn move_convo(&mut self, delta: i32) {
        if self.convos.is_empty() { return; }
        let cur = self.convo_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(self.convos.len() as i32) as usize;
        self.convo_state.select(Some(next));
        self.reload_messages();
    }

    fn send(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() { return; }
        let Some(dir) = self.selected_dir() else {
            self.status = "no conversation selected".into();
            return;
        };
        match message::send(&self.ctx, &dir, text.as_bytes()) {
            Ok(_) => {
                self.input.clear();
                self.status = "sent".into();
                self.reload_messages();
            }
            Err(e) => { self.status = format!("send error: {}", e); }
        }
    }

    fn trigger_sync(&self) {
        let _ = self.sync_trigger.send(());
    }
}

pub fn run(ctx: IdentityContext) -> io::Result<()> {
    silence_stderr();

    let ctx = Arc::new(ctx);

    let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>();
    spawn_input_reader(ui_tx.clone());
    let sync_trigger = spawn_sync_worker(ctx.clone(), ui_tx.clone());

    let mut app = App::new(ctx, sync_trigger);
    app.trigger_sync();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut app, ui_rx);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn spawn_input_reader(tx: Sender<UiEvent>) {
    thread::spawn(move || {
        loop {
            if event::poll(Duration::from_millis(200)).unwrap_or(false) {
                if let Ok(Event::Key(k)) = event::read() {
                    if k.kind == KeyEventKind::Press && tx.send(UiEvent::Key(k)).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

fn spawn_sync_worker(ctx: Arc<IdentityContext>, ui_tx: Sender<UiEvent>) -> Sender<()> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        loop {
            let _ = ui_tx.send(UiEvent::SyncStarted);
            let status = match sync::run(&ctx) {
                Ok(r) if r.accepted.is_empty() => "synced".to_string(),
                Ok(r) => format!("synced (auto-accepted {})", r.accepted.len()),
                Err(e) => format!("sync error: {}", e),
            };
            if ui_tx.send(UiEvent::SyncDone(status)).is_err() { break; }
            // Either wait SYNC_INTERVAL or wake early on manual trigger.
            let _ = cmd_rx.recv_timeout(SYNC_INTERVAL);
        }
    });
    cmd_tx
}

fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    rx: Receiver<UiEvent>,
) -> io::Result<()> {
    terminal.draw(|f| ui(f, app))?;
    while !app.quit {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(UiEvent::Key(key)) => handle_key(app, key),
            Ok(UiEvent::SyncStarted) => { app.status = "syncing...".into(); }
            Ok(UiEvent::SyncDone(msg)) => {
                app.status = msg;
                app.reload_convos();
                app.reload_messages();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        terminal.draw(|f| ui(f, app))?;
    }
    Ok(())
}

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    match app.pane {
        Pane::Convos => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
            KeyCode::Char('j') | KeyCode::Down => app.move_convo(1),
            KeyCode::Char('k') | KeyCode::Up => app.move_convo(-1),
            KeyCode::Char('r') => app.trigger_sync(),
            KeyCode::Tab | KeyCode::Enter | KeyCode::Char('i') => app.pane = Pane::Input,
            _ => {}
        },
        Pane::Input => match key.code {
            KeyCode::Esc => app.pane = Pane::Convos,
            KeyCode::Tab => app.pane = Pane::Convos,
            KeyCode::Enter => app.send(),
            KeyCode::Backspace => { app.input.pop(); }
            KeyCode::Char(c) => app.input.push(c),
            _ => {}
        }
    }
}

fn ui(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(20)])
        .split(chunks[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(body[1]);

    draw_convos(f, app, body[0]);
    draw_messages(f, app, right[0]);
    draw_input(f, app, right[1]);
    draw_status(f, app, chunks[1]);
}

fn draw_convos(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app.convos.iter().map(|c| {
        let title = c.title.as_deref().unwrap_or("(no title)");
        let line = format!("{}  ({}m)", title, c.member_count);
        ListItem::new(line)
    }).collect();
    let focus = matches!(app.pane, Pane::Convos);
    let block = block("Conversations", focus);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    let mut state = app.convo_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_messages(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let title = match app.convos.get(app.convo_state.selected().unwrap_or(usize::MAX)) {
        Some(c) => c.title.clone().unwrap_or_else(|| c.dir_name.clone()),
        None => "Messages".into(),
    };
    let lines: Vec<Line> = app.messages.iter().flat_map(|(s, body)| {
        let head = Line::from(vec![
            Span::styled(format!("[{}] ", reltime::relative(&s.modified)), Style::default().fg(Color::DarkGray)),
            Span::styled(s.sender.clone(), Style::default().fg(Color::Cyan)),
        ]);
        let mut out = vec![head];
        for l in body.lines() { out.push(Line::from(l.to_string())); }
        out.push(Line::from(""));
        out
    }).collect();

    let msg_count = app.messages.len();
    let visible = area.height.saturating_sub(2) as usize;
    let total_lines = lines.len();
    let scroll = total_lines.saturating_sub(visible) as u16;

    let title_str = format!("{} ({})", title, msg_count);
    let para = Paragraph::new(lines)
        .block(block(&title_str, false))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);
}

fn draw_input(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let focus = matches!(app.pane, Pane::Input);
    let text = if app.input.is_empty() && !focus {
        "(press Tab or i to type, Enter to send)".to_string()
    } else {
        app.input.clone()
    };
    let style = if app.input.is_empty() && !focus {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    let para = Paragraph::new(Line::from(Span::styled(text, style)))
        .block(block("Message", focus));
    f.render_widget(para, area);
}

fn draw_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let addr = &app.ctx.identity.address;
    let hint = match app.pane {
        Pane::Convos => "j/k select  r sync  Tab/i input  q quit",
        Pane::Input => "Enter send  Esc/Tab back",
    };
    let line = Line::from(vec![
        Span::styled(format!(" {} ", addr), Style::default().fg(Color::Green)),
        Span::raw(" | "),
        Span::raw(&app.status),
        Span::raw("  "),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn block(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default().borders(Borders::ALL).title(title.to_string()).border_style(style)
}

// Ark's client emits progress via eprintln! (see ark_friction.md#12). Those
// writes corrupt the ratatui frame. Silence fd 2 for the TUI's lifetime by
// pointing it at /dev/null.
fn silence_stderr() {
    unsafe {
        let path = b"/dev/null\0";
        let fd = libc::open(path.as_ptr() as *const _, libc::O_WRONLY);
        if fd >= 0 {
            libc::dup2(fd, libc::STDERR_FILENO);
            libc::close(fd);
        }
    }
}
