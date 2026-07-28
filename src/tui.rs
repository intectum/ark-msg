use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ark::client::{accept_proposal, reject_proposal, sync};
use ark::types::IdentityContext;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::paths::APPS_MSG;
use crate::{convo, invite, message, reltime};
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

enum ListRow {
    Convo(convo::ConvoSummary),
    Invite(invite::InviteSummary),
}

impl ListRow {
    fn activity(&self) -> OffsetDateTime {
        match self {
            ListRow::Convo(c) => c.last_activity,
            ListRow::Invite(i) => i.modified,
        }
    }
}

struct App {
    ctx: Arc<IdentityContext>,
    rows: Vec<ListRow>,
    list_state: ListState,
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
            rows: Vec::new(),
            list_state: ListState::default(),
            messages: Vec::new(),
            input: String::new(),
            pane: Pane::Convos,
            status: "starting...".into(),
            quit: false,
            sync_trigger,
        };
        app.reload_rows();
        if !app.rows.is_empty() {
            app.list_state.select(Some(0));
            app.reload_messages();
        }
        app
    }

    fn reload_rows(&mut self) {
        let prev_key = self.selected_key();

        let mut rows: Vec<ListRow> = Vec::new();
        match convo::list(&self.ctx) {
            Ok(v) => rows.extend(v.into_iter().map(ListRow::Convo)),
            Err(e) => { self.status = format!("list error: {}", e); }
        }
        match invite::list(&self.ctx) {
            Ok(v) => rows.extend(v.into_iter().map(ListRow::Invite)),
            Err(e) => { self.status = format!("invite list error: {}", e); }
        }
        rows.sort_by(|a, b| b.activity().cmp(&a.activity()));  // most recent first
        self.rows = rows;

        // Preserve selection across reloads: match by dir/proposal key, else
        // clamp to available range.
        let new_idx = prev_key
            .and_then(|k| self.rows.iter().position(|r| row_key(r) == k))
            .or(if self.rows.is_empty() { None } else { Some(0) });
        self.list_state.select(new_idx);
    }

    fn reload_messages(&mut self) {
        let Some(dir) = self.selected_convo_dir() else {
            self.messages.clear();
            return;
        };
        match message::read(&self.ctx, &dir, None) {
            Ok(msgs) => { self.messages = msgs; }
            Err(e) => { self.status = format!("read error: {}", e); }
        }
    }

    fn selected(&self) -> Option<&ListRow> {
        self.list_state.selected().and_then(|i| self.rows.get(i))
    }

    fn selected_key(&self) -> Option<String> {
        self.selected().map(row_key)
    }

    fn selected_convo_dir(&self) -> Option<String> {
        match self.selected()? {
            ListRow::Convo(c) => Some(c.dir_name.clone()),
            ListRow::Invite(_) => None,
        }
    }

    fn selected_invite_id(&self) -> Option<String> {
        match self.selected()? {
            ListRow::Invite(i) => Some(i.proposal_id.clone()),
            ListRow::Convo(_) => None,
        }
    }

    fn move_row(&mut self, delta: i32) {
        if self.rows.is_empty() { return; }
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(self.rows.len() as i32) as usize;
        self.list_state.select(Some(next));
        self.reload_messages();
    }

    fn send(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() { return; }
        let Some(dir) = self.selected_convo_dir() else {
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

    fn accept_invite(&mut self) {
        let Some(id) = self.selected_invite_id() else { return; };
        match accept_proposal(&self.ctx, &id, false) {
            Ok(()) => {
                self.status = "joined — syncing...".into();
                self.trigger_sync();
            }
            Err(e) => { self.status = format!("accept error: {}", e); }
        }
    }

    fn reject_invite(&mut self) {
        let Some(id) = self.selected_invite_id() else { return; };
        match reject_proposal(&self.ctx, &id) {
            Ok(()) => {
                self.status = "invite dismissed".into();
                self.reload_rows();
                self.reload_messages();
            }
            Err(e) => { self.status = format!("reject error: {}", e); }
        }
    }

    fn trigger_sync(&self) {
        let _ = self.sync_trigger.send(());
    }
}

fn row_key(row: &ListRow) -> String {
    match row {
        ListRow::Convo(c) => format!("c:{}", c.dir_name),
        ListRow::Invite(i) => format!("i:{}", i.proposal_id),
    }
}

pub fn run(ctx: IdentityContext) -> io::Result<()> {
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
        let path = ctx.root.join(APPS_MSG);
        loop {
            let _ = ui_tx.send(UiEvent::SyncStarted);
            let status = match sync(&ctx, &path, false, true, |_| false, |_| false) {
                Ok(()) => "synced".to_string(),
                Err(e) => format!("sync error: {}", e),
            };
            if ui_tx.send(UiEvent::SyncDone(status)).is_err() { break; }
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
                app.reload_rows();
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
            KeyCode::Char('j') | KeyCode::Down => app.move_row(1),
            KeyCode::Char('k') | KeyCode::Up => app.move_row(-1),
            KeyCode::Char('r') => app.trigger_sync(),
            KeyCode::Char('a') => app.accept_invite(),
            KeyCode::Char('d') => app.reject_invite(),
            KeyCode::Tab | KeyCode::Enter | KeyCode::Char('i')
                if app.selected_convo_dir().is_some() =>
            {
                app.pane = Pane::Input;
            }
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

    draw_rows(f, app, body[0]);
    draw_detail(f, app, right[0]);
    draw_input(f, app, right[1]);
    draw_status(f, app, chunks[1]);
}

fn draw_rows(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app.rows.iter().map(|r| match r {
        ListRow::Convo(c) => {
            let title = c.title.as_deref().unwrap_or("(no title)");
            ListItem::new(format!("{}  ({}m)", title, c.member_count))
        }
        ListRow::Invite(i) => {
            let line = Line::from(vec![
                Span::styled("[invite] ", Style::default().fg(Color::Yellow)),
                Span::raw(i.dir_name.clone()),
            ]);
            ListItem::new(line)
        }
    }).collect();
    let focus = matches!(app.pane, Pane::Convos);
    let block = block("Conversations", focus);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    let mut state = app.list_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(f: &mut ratatui::Frame, app: &App, area: Rect) {
    match app.selected() {
        Some(ListRow::Invite(i)) => draw_invite_card(f, i, area),
        Some(ListRow::Convo(c)) => draw_messages(f, app, c, area),
        None => {
            let para = Paragraph::new("").block(block("Messages", false));
            f.render_widget(para, area);
        }
    }
}

fn draw_invite_card(f: &mut ratatui::Frame, invite: &invite::InviteSummary, area: Rect) {
    let modified_iso = invite.modified.format(&Rfc3339).unwrap_or_default();
    let lines = vec![
        Line::from(vec![
            Span::styled("Convo: ", Style::default().fg(Color::DarkGray)),
            Span::raw(invite.dir_name.clone()),
        ]),
        Line::from(vec![
            Span::styled("From:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(invite.proposer.clone(), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("When:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(reltime::relative(&modified_iso)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Press  a  to Join   d  to Dismiss",
            Style::default().fg(Color::Yellow),
        )),
    ];
    let para = Paragraph::new(lines)
        .block(block("Invitation", false))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn draw_messages(f: &mut ratatui::Frame, app: &App, convo: &convo::ConvoSummary, area: Rect) {
    let title = convo.title.clone().unwrap_or_else(|| convo.dir_name.clone());
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
    let is_invite = matches!(app.selected(), Some(ListRow::Invite(_)));
    let (text, style) = if is_invite {
        ("(invite selected — press a to join)".to_string(),
         Style::default().fg(Color::DarkGray))
    } else if app.input.is_empty() && !focus {
        ("(press Tab or i to type, Enter to send)".to_string(),
         Style::default().fg(Color::DarkGray))
    } else {
        (app.input.clone(), Style::default())
    };
    let para = Paragraph::new(Line::from(Span::styled(text, style)))
        .block(block("Message", focus));
    f.render_widget(para, area);
}

fn draw_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let addr = &app.ctx.identity.address;
    let is_invite = matches!(app.selected(), Some(ListRow::Invite(_)));
    let hint = match app.pane {
        Pane::Convos if is_invite => "j/k select  a join  d dismiss  r sync  q quit",
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
