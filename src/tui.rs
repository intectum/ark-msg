use std::cmp::Reverse;
use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use time::OffsetDateTime;

use crate::paths::{APPS_MSG, CHATS_ROOT};
use crate::types::{Chat, ChatMember, Invite, Message};
use crate::{chat, direct, group, invite, message, reltime};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;

enum UiEvent {
    Key(crossterm::event::KeyEvent),
    /// Watch stream reported a local/remote change under `apps/msg/`.
    LocalChanged,
    InvitesLoaded(Vec<Invite>),
    MembersLoaded { chat_id: String, members: Vec<ChatMember> },
    ChatCreated(String),
    /// A queued task finished, successfully or not.
    TaskDone,
    Error(String),
}

/// Work that talks to a server. Queued onto the task thread rather than run
/// on the UI loop, which a round trip would otherwise stall for seconds.
enum Task {
    Send { chat_id: String, body: String },
    CreateDirect { name: Option<String>, address: String },
    CreateGroup { name: Option<String>, members: Vec<String> },
    Accept(Invite),
    Reject(Invite),
    AddMember { chat_id: String, address: String },
    RemoveMember { chat_id: String, address: String },
    PromoteMember { chat_id: String, address: String },
    DemoteMember { chat_id: String, address: String },
}

impl Task {
    fn label(&self) -> &'static str {
        match self {
            Task::Send { .. } => "send",
            Task::CreateDirect { .. } | Task::CreateGroup { .. } => "create",
            Task::Accept(_) => "accept",
            Task::Reject(_) => "reject",
            Task::AddMember { .. } => "add",
            Task::RemoveMember { .. } => "remove",
            Task::PromoteMember { .. } => "promote",
            Task::DemoteMember { .. } => "demote",
        }
    }
}

enum Pane { Chats, Input, NewChat, Members }

/// A new chat being filled in. A direct chat takes one address and no name —
/// it falls back to the other member's address.
struct NewChat {
    group: bool,
    members: String,
    name: String,
    /// The field being typed into: 0 members, 1 name (group only).
    field: usize,
}

/// The member list of a chat, open over it. Only a group chat's members can
/// be changed, and only by an owner — a direct chat's pair is fixed.
struct Members {
    chat_id: String,
    group: bool,
    rows: Vec<ChatMember>,
    list_state: ListState,
    /// The address being typed in, while adding a member.
    adding: Option<String>,
}

enum ListRow {
    Chat(Chat),
    Invite(Invite),
}

impl ListRow {
    fn activity(&self) -> OffsetDateTime {
        match self {
            ListRow::Chat(c) => c.last_activity,
            ListRow::Invite(i) => i.proposed,
        }
    }
}

struct App {
    ctx: Arc<ark::Context>,
    chats: Vec<Chat>,
    invites: Vec<Invite>,
    rows: Vec<ListRow>,
    list_state: ListState,
    messages: Vec<Message>,
    input: String,
    pane: Pane,
    new_chat: Option<NewChat>,
    members: Option<Members>,
    error: Option<String>,
    quit: bool,
    /// Coalesces bursts of watch events into one reload per redraw tick.
    dirty: bool,
    /// Queued tasks yet to finish, shown in the status line.
    pending: usize,
    task_tx: Sender<Task>,
    member_trigger: Sender<String>,
}

impl App {
    fn new(ctx: Arc<ark::Context>, task_tx: Sender<Task>, member_trigger: Sender<String>) -> Self {
        let mut app = Self {
            ctx,
            chats: Vec::new(),
            invites: Vec::new(),
            rows: Vec::new(),
            list_state: ListState::default(),
            messages: Vec::new(),
            input: String::new(),
            pane: Pane::Chats,
            new_chat: None,
            members: None,
            error: None,
            quit: false,
            dirty: false,
            pending: 0,
            task_tx,
            member_trigger,
        };
        app.reload_chats();
        app.rebuild_rows();
        if !app.rows.is_empty() {
            app.list_state.select(Some(0));
            app.reload_messages();
        }
        app
    }

    /// Refresh local chat list (fast, disk-only).
    fn reload_chats(&mut self) {
        match chat::list_chats(&self.ctx) {
            Ok(v) => { self.chats = v; }
            Err(e) => { self.error = Some(format!("list error: {}", e)); }
        }
    }

    /// Combine chats + invites into `rows`, sort desc by activity, preserve
    /// selection by stable row key.
    fn rebuild_rows(&mut self) {
        let prev_key = self.selected_key();

        let mut rows: Vec<ListRow> = Vec::with_capacity(self.chats.len() + self.invites.len());
        rows.extend(self.chats.iter().cloned().map(ListRow::Chat));
        rows.extend(self.invites.iter().cloned().map(ListRow::Invite));
        rows.sort_by_key(|row| Reverse(row.activity()));  // most recent first
        self.rows = rows;

        let new_idx = prev_key
            .and_then(|k| self.rows.iter().position(|r| row_key(r) == k))
            .or(if self.rows.is_empty() { None } else { Some(0) });
        self.list_state.select(new_idx);
    }

    fn reload_messages(&mut self) {
        let Some(chat_id) = self.selected_chat_id() else {
            self.messages.clear();
            return;
        };
        match message::read_messages(&self.ctx, &chat_id, None) {
            Ok(msgs) => { self.messages = msgs; }
            Err(e) => { self.error = Some(format!("read error: {}", e)); }
        }
    }

    fn selected(&self) -> Option<&ListRow> {
        self.list_state.selected().and_then(|i| self.rows.get(i))
    }

    fn selected_key(&self) -> Option<String> {
        self.selected().map(row_key)
    }

    fn selected_chat_id(&self) -> Option<String> {
        match self.selected()? {
            ListRow::Chat(c) => Some(c.id.clone()),
            ListRow::Invite(_) => None,
        }
    }

    fn selected_invite(&self) -> Option<Invite> {
        match self.selected()? {
            ListRow::Invite(i) => Some(i.clone()),
            ListRow::Chat(_) => None,
        }
    }

    fn move_row(&mut self, delta: i32) {
        if self.rows.is_empty() { return; }
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(self.rows.len() as i32) as usize;
        self.list_state.select(Some(next));
        self.reload_messages();
    }

    /// Queue the typed message. It appears once the send lands and the reload
    /// it triggers picks the new file up.
    fn send(&mut self) {
        let body = self.input.trim().to_string();
        if body.is_empty() { return; }
        let Some(chat_id) = self.selected_chat_id() else { return; };

        self.input.clear();
        self.error = None;
        self.queue(Task::Send { chat_id, body });
    }

    fn queue(&mut self, task: Task) {
        self.pending += 1;
        if self.task_tx.send(task).is_err() {
            self.pending -= 1;
            self.error = Some("tasks stopped".to_string());
        }
    }

    fn open_new_chat(&mut self, group: bool) {
        self.new_chat = Some(NewChat { group, members: String::new(), name: String::new(), field: 0 });
        self.pane = Pane::NewChat;
        self.error = None;
    }

    fn new_chat_field(&mut self) -> &mut String {
        let new_chat = self.new_chat.as_mut().expect("new chat form");
        if new_chat.field == 0 { &mut new_chat.members } else { &mut new_chat.name }
    }

    /// Queue the chat being filled in. It is selected once created. A group
    /// chat with no name is left unnamed; it falls back to its members.
    fn create_chat(&mut self) {
        let Some(new_chat) = &self.new_chat else { return; };

        let group = new_chat.group;
        let members: Vec<String> = new_chat.members
            .split([',', ' '])
            .filter(|address| !address.is_empty())
            .map(str::to_string)
            .collect();
        let name = new_chat.name.trim();
        let name = if name.is_empty() { None } else { Some(name.to_string()) };

        let task = if members.is_empty() {
            self.error = Some("create error: an address is required".to_string());
            return;
        } else if group {
            Task::CreateGroup { name, members }
        } else if members.len() == 1 {
            Task::CreateDirect { name: None, address: members[0].clone() }
        } else {
            self.error = Some("create error: a direct chat takes one address".to_string());
            return;
        };

        self.new_chat = None;
        self.pane = Pane::Chats;
        self.error = None;
        self.queue(task);
    }

    /// Select a chat by id, once it is in the local mirror.
    fn select_chat(&mut self, chat_id: &str) {
        self.reload_chats();
        self.rebuild_rows();

        let key = format!("c:{}", chat_id);
        if let Some(index) = self.rows.iter().position(|row| row_key(row) == key) {
            self.list_state.select(Some(index));
        }

        self.reload_messages();
    }

    fn accept_invite(&mut self) {
        let Some(invite) = self.selected_invite() else { return; };

        // Dropped up front: the row goes as the invite is queued, rather than
        // lingering until the accept lands.
        self.invites.retain(|i| i.chat_id != invite.chat_id);
        self.rebuild_rows();
        self.error = None;
        self.queue(Task::Accept(invite));
    }

    fn reject_invite(&mut self) {
        let Some(invite) = self.selected_invite() else { return; };

        self.invites.retain(|i| i.chat_id != invite.chat_id);
        self.rebuild_rows();
        self.reload_messages();
        self.error = None;
        self.queue(Task::Reject(invite));
    }

    fn open_members(&mut self) {
        let Some(chat_id) = self.selected_chat_id() else { return; };

        self.members = Some(Members {
            group: group::is_group_chat(&self.ctx, &chat_id),
            chat_id,
            rows: Vec::new(),
            list_state: ListState::default(),
            adding: None,
        });
        self.reload_members();
        self.pane = Pane::Members;
        self.error = None;
    }

    fn close_members(&mut self) {
        self.members = None;
        self.pane = Pane::Chats;
    }

    /// Ask for the open chat's members. They arrive as `MembersLoaded`, as
    /// resolving the 'all members' group can reach its owner's server.
    fn reload_members(&mut self) {
        let Some(members) = &self.members else { return; };
        if self.member_trigger.send(members.chat_id.clone()).is_err() {
            self.error = Some("members stopped".to_string());
        }
    }

    /// Take a loaded member list, keeping the same address selected. A list
    /// for a chat whose pane has since closed or moved on is dropped.
    fn members_loaded(&mut self, chat_id: &str, rows: Vec<ChatMember>) {
        let Some(members) = &mut self.members else { return; };
        if members.chat_id != chat_id { return; }

        let prev_address = selected_member(members).map(|member| member.address.clone());
        members.rows = rows;

        let index = prev_address
            .and_then(|address| members.rows.iter().position(|member| member.address == address))
            .or(if members.rows.is_empty() { None } else { Some(0) });
        members.list_state.select(index);
    }

    fn move_member(&mut self, delta: i32) {
        let Some(members) = &mut self.members else { return; };
        if members.rows.is_empty() { return; }

        let cur = members.list_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(members.rows.len() as i32) as usize;
        members.list_state.select(Some(next));
    }

    fn member_add_field(&mut self) -> &mut String {
        self.members.as_mut().expect("member list").adding.as_mut().expect("add field")
    }

    /// Queue the address typed into the add field.
    fn add_member(&mut self) {
        let Some(members) = &self.members else { return; };
        let address = members.adding.clone().unwrap_or_default().trim().to_string();
        if address.is_empty() { return; }
        let chat_id = members.chat_id.clone();

        self.members.as_mut().expect("member list").adding = None;
        self.error = None;
        self.queue(Task::AddMember { chat_id, address });
    }

    /// Queue a drop of the selected member. Leaving a chat yourself is not
    /// the same thing, so self is not droppable here.
    fn remove_member(&mut self) {
        let Some(members) = &self.members else { return; };
        let Some(member) = selected_member(members) else { return; };
        if member.address == self.ctx.identity.address {
            self.error = Some("remove error: cannot remove yourself".to_string());
            return;
        }
        let (chat_id, address) = (members.chat_id.clone(), member.address.clone());

        self.error = None;
        self.queue(Task::RemoveMember { chat_id, address });
    }

    /// Queue whichever of promote/demote the selected member's current role
    /// calls for.
    fn toggle_member_owner(&mut self) {
        let Some(members) = &self.members else { return; };
        let Some(member) = selected_member(members) else { return; };
        let (chat_id, address) = (members.chat_id.clone(), member.address.clone());

        let task = match member.owner {
            true => Task::DemoteMember { chat_id, address },
            false => Task::PromoteMember { chat_id, address },
        };
        self.error = None;
        self.queue(task);
    }
}

fn selected_member(members: &Members) -> Option<&ChatMember> {
    members.list_state.selected().and_then(|index| members.rows.get(index))
}

fn row_key(row: &ListRow) -> String {
    match row {
        ListRow::Chat(c) => format!("c:{}", c.id),
        ListRow::Invite(i) => format!("i:{}", i.chat_id),
    }
}

pub fn run_tui(ctx: ark::Context) -> io::Result<()> {
    let ctx = Arc::new(ctx);

    let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>();
    spawn_input_reader(ui_tx.clone());
    spawn_file_watcher(ctx.clone(), ui_tx.clone());
    let invite_trigger = spawn_invite_loader(ctx.clone(), ui_tx.clone());
    spawn_proposal_watcher(ctx.clone(), ui_tx.clone(), invite_trigger.clone());
    let member_trigger = spawn_member_loader(ctx.clone(), ui_tx.clone());
    let task_tx = spawn_task_runner(ctx.clone(), ui_tx.clone(), invite_trigger);

    let mut app = App::new(ctx, task_tx, member_trigger);

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

/// Start `sync(watch=true)` in a background thread. `on_event` fires per
/// reconciled entry; we coalesce into a single `LocalChanged` per event and
/// let the UI dedup via its `dirty` flag. The watch call blocks until
/// unrecoverable error, at which point we report status and exit.
fn spawn_file_watcher(ctx: Arc<ark::Context>, ui_tx: Sender<UiEvent>) {
    thread::spawn(move || {
        // Both callbacks must be Fn + Send + Sync; wrap Sender in Arc<Mutex<_>>.
        let event_tx = Arc::new(Mutex::new(ui_tx.clone()));
        let error_tx = event_tx.clone();

        let on_event = move |_ev| {
            let _ = event_tx.lock().unwrap().send(UiEvent::LocalChanged);
            false
        };
        let on_error = move |e: io::Error| {
            let _ = error_tx.lock().unwrap().send(UiEvent::Error(format!("watch: {}", e)));
            false
        };

        if let Err(e) = ark::sync(&ctx, APPS_MSG, true, true, on_event, on_error) {
            let _ = ui_tx.send(UiEvent::Error(format!("watch died: {}", e)));
        }
    });
}

/// Reload invites as proposals for chats arrive. Errors reload too: the watch
/// stream reconnects on its own but drops any proposal logged while it was
/// down. The watch call blocks until unrecoverable error, at which point we
/// report status and exit.
fn spawn_proposal_watcher(ctx: Arc<ark::Context>, ui_tx: Sender<UiEvent>, invite_trigger: Sender<()>) {
    thread::spawn(move || {
        let error_tx = ui_tx.clone();
        let error_trigger = invite_trigger.clone();

        let on_proposal = move |_proposal| {
            let _ = invite_trigger.send(());
            false
        };
        let on_error = move |e: io::Error| {
            let _ = error_tx.send(UiEvent::Error(format!("proposals: {}", e)));
            let _ = error_trigger.send(());
            false
        };

        if let Err(e) = ark::watch_proposals(&ctx, CHATS_ROOT, on_proposal, on_error) {
            let _ = ui_tx.send(UiEvent::Error(format!("proposal watch died: {}", e)));
        }
    });
}

/// Load invites once, then on every trigger. Returns the trigger channel
/// callers use to reload (e.g. after accept/reject, or on a new proposal).
fn spawn_invite_loader(ctx: Arc<ark::Context>, ui_tx: Sender<UiEvent>) -> Sender<()> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        loop {
            match invite::list_invites(&ctx) {
                Ok(v) => {
                    if ui_tx.send(UiEvent::InvitesLoaded(v)).is_err() { break; }
                }
                Err(e) => {
                    let _ = ui_tx.send(UiEvent::Error(format!("invite fetch: {}", e)));
                }
            }
            if cmd_rx.recv().is_err() { break; }
        }
    });
    cmd_tx
}

/// Load a chat's members on every trigger. Returns the channel callers send
/// the chat id on. Kept off the UI thread: the 'all members' group of a chat
/// this account joined lives on its owner's server, so the first read of one
/// is a round trip.
fn spawn_member_loader(ctx: Arc<ark::Context>, ui_tx: Sender<UiEvent>) -> Sender<String> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for chat_id in cmd_rx {
            match chat::get_chat_members(&ctx, &chat_id) {
                Ok(members) => {
                    if ui_tx.send(UiEvent::MembersLoaded { chat_id, members }).is_err() { break; }
                }
                Err(e) => {
                    let _ = ui_tx.send(UiEvent::Error(format!("member fetch: {}", e)));
                }
            }
        }
    });
    cmd_tx
}

/// Run queued tasks one at a time, off the UI thread. Returns the channel
/// the UI queues them on.
fn spawn_task_runner(ctx: Arc<ark::Context>, ui_tx: Sender<UiEvent>, invite_trigger: Sender<()>) -> Sender<Task> {
    let (task_tx, task_rx) = mpsc::channel::<Task>();
    thread::spawn(move || {
        for task in task_rx {
            let label = task.label();
            if let Err(e) = run_task(&ctx, task, &ui_tx, &invite_trigger) {
                let _ = ui_tx.send(UiEvent::Error(format!("{} error: {}", label, e)));
            }
            if ui_tx.send(UiEvent::TaskDone).is_err() { break; }
        }
    });
    task_tx
}

fn run_task(ctx: &ark::Context, task: Task, ui_tx: &Sender<UiEvent>, invite_trigger: &Sender<()>) -> io::Result<()> {
    match task {
        Task::Send { chat_id, body } => { message::send_message(ctx, &chat_id, body.as_bytes())?; }
        Task::CreateDirect { name, address } => {
            let chat_id = direct::create_direct_chat(ctx, name.as_deref(), None, &address)?;
            let _ = ui_tx.send(UiEvent::ChatCreated(chat_id));
        }
        Task::CreateGroup { name, members } => {
            let chat_id = group::create_group_chat(ctx, name.as_deref(), None, &members)?;
            let _ = ui_tx.send(UiEvent::ChatCreated(chat_id));
        }
        Task::Accept(invite) => {
            invite::accept_invite(ctx, &invite)?;
            let _ = invite_trigger.send(());
        }
        Task::Reject(invite) => {
            invite::reject_invite(ctx, &invite)?;
            let _ = invite_trigger.send(());
        }
        Task::AddMember { chat_id, address } => group::add_group_chat_member(ctx, &chat_id, &address)?,
        Task::RemoveMember { chat_id, address } => group::remove_group_chat_member(ctx, &chat_id, &address)?,
        Task::PromoteMember { chat_id, address } => group::promote_group_chat_member(ctx, &chat_id, &address)?,
        Task::DemoteMember { chat_id, address } => group::demote_group_chat_member(ctx, &chat_id, &address)?,
    }

    Ok(())
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
            Ok(UiEvent::LocalChanged) => { app.dirty = true; }
            Ok(UiEvent::InvitesLoaded(v)) => {
                app.invites = v;
                app.rebuild_rows();
            }
            Ok(UiEvent::MembersLoaded { chat_id, members }) => app.members_loaded(&chat_id, members),
            Ok(UiEvent::ChatCreated(chat_id)) => app.select_chat(&chat_id),
            Ok(UiEvent::TaskDone) => {
                app.pending = app.pending.saturating_sub(1);
                app.dirty = true;
            }
            Ok(UiEvent::Error(s)) => { app.error = Some(s); }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if app.dirty {
            app.dirty = false;
            app.reload_chats();
            app.rebuild_rows();
            app.reload_messages();
            app.reload_members();
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
        Pane::Chats => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
            KeyCode::Char('j') | KeyCode::Down => app.move_row(1),
            KeyCode::Char('k') | KeyCode::Up => app.move_row(-1),
            KeyCode::Char('a') => app.accept_invite(),
            KeyCode::Char('d') => app.reject_invite(),
            KeyCode::Char('n') => app.open_new_chat(false),
            KeyCode::Char('g') => app.open_new_chat(true),
            KeyCode::Char('m') => app.open_members(),
            KeyCode::Tab | KeyCode::Enter | KeyCode::Char('i')
                if app.selected_chat_id().is_some() =>
            {
                app.pane = Pane::Input;
            }
            _ => {}
        },
        Pane::Input => match key.code {
            KeyCode::Esc => app.pane = Pane::Chats,
            KeyCode::Tab => app.pane = Pane::Chats,
            KeyCode::Enter => app.send(),
            // The arrows move between chats from either pane; j/k are text here.
            KeyCode::Down => app.move_row(1),
            KeyCode::Up => app.move_row(-1),
            KeyCode::Backspace => { app.input.pop(); }
            KeyCode::Char(c) => app.input.push(c),
            _ => {}
        }
        Pane::NewChat => {
            let Some(new_chat) = &app.new_chat else { return; };
            let (group, field) = (new_chat.group, new_chat.field);
            match key.code {
                KeyCode::Esc => {
                    app.new_chat = None;
                    app.pane = Pane::Chats;
                }
                // Only a group chat has a second field to move to.
                KeyCode::Tab if group => { app.new_chat.as_mut().unwrap().field = 1 - field; }
                KeyCode::Enter if group && field == 0 => { app.new_chat.as_mut().unwrap().field = 1; }
                KeyCode::Enter => app.create_chat(),
                KeyCode::Backspace => { app.new_chat_field().pop(); }
                KeyCode::Char(c) => app.new_chat_field().push(c),
                _ => {}
            }
        }
        Pane::Members => {
            let Some(members) = &app.members else { return; };
            let (group, adding) = (members.group, members.adding.is_some());

            // Typing an address takes every key but Esc and Enter, so the
            // list keys below are out of reach until the field is done with.
            if adding {
                match key.code {
                    KeyCode::Esc => { app.members.as_mut().expect("member list").adding = None; }
                    KeyCode::Enter => app.add_member(),
                    KeyCode::Backspace => { app.member_add_field().pop(); }
                    KeyCode::Char(c) => app.member_add_field().push(c),
                    _ => {}
                }
                return;
            }

            match key.code {
                KeyCode::Esc | KeyCode::Char('m') => app.close_members(),
                KeyCode::Char('j') | KeyCode::Down => app.move_member(1),
                KeyCode::Char('k') | KeyCode::Up => app.move_member(-1),
                KeyCode::Char('a') if group => {
                    app.members.as_mut().expect("member list").adding = Some(String::new());
                }
                KeyCode::Char('d') if group => app.remove_member(),
                KeyCode::Char('p') if group => app.toggle_member_owner(),
                _ => {}
            }
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

    if let Some(new_chat) = &app.new_chat {
        draw_new_chat(f, new_chat, chunks[0]);
    }
    if app.members.is_some() {
        draw_members(f, app, chunks[0]);
    }
}

fn draw_rows(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app.rows.iter().map(|r| match r {
        ListRow::Chat(c) => ListItem::new(chat::get_chat_display_name(c)),
        ListRow::Invite(i) => {
            let line = Line::from(vec![
                Span::styled("[invite] ", Style::default().fg(Color::Yellow)),
                Span::raw(i.chat_id.clone()),
            ]);
            ListItem::new(line)
        }
    }).collect();
    let focus = matches!(app.pane, Pane::Chats);
    // Dimmed while typing: the selection still marks the chat being read, but
    // the keys are going to the message box.
    let highlight_style = match focus {
        true => Style::default().add_modifier(Modifier::REVERSED),
        false => Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM),
    };
    let list = List::new(items)
        .block(block("Chats", focus))
        .style(if focus { Style::default() } else { Style::default().add_modifier(Modifier::DIM) })
        .highlight_style(highlight_style)
        .highlight_symbol("> ");
    let mut state = app.list_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(f: &mut ratatui::Frame, app: &App, area: Rect) {
    match app.selected() {
        Some(ListRow::Invite(i)) => draw_invite_card(f, i, area),
        Some(ListRow::Chat(c)) => draw_messages(f, app, c, area),
        None => {
            let para = Paragraph::new("").block(block("Messages", false));
            f.render_widget(para, area);
        }
    }
}

fn draw_invite_card(f: &mut ratatui::Frame, invite: &Invite, area: Rect) {
    let lines = vec![
        Line::from(vec![
            Span::styled("Chat: ", Style::default().fg(Color::DarkGray)),
            Span::raw(invite.chat_id.clone()),
        ]),
        Line::from(vec![
            Span::styled("From:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(invite.proposer.clone(), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("When:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(reltime::relative_time(invite.proposed)),
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

fn draw_messages(f: &mut ratatui::Frame, app: &App, chat: &Chat, area: Rect) {
    let title = chat::get_chat_display_name(chat);
    let lines: Vec<Line> = app.messages.iter().flat_map(|message| {
        let head = Line::from(vec![
            Span::styled(format!("[{}] ", reltime::relative_time(message.sent)), Style::default().fg(Color::DarkGray)),
            Span::styled(message.sender.clone(), Style::default().fg(Color::Cyan)),
        ]);
        let mut out = vec![head];
        for l in message.body.lines() { out.push(Line::from(l.to_string())); }
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

    if focus {
        // Inside the border, at the end of what has been typed.
        let last_column = area.x + area.width.saturating_sub(2);
        let column = (area.x + 1 + app.input.chars().count() as u16).min(last_column);
        f.set_cursor_position((column, area.y + 1));
    }
}

fn draw_new_chat(f: &mut ratatui::Frame, new_chat: &NewChat, area: Rect) {
    let width = area.width.saturating_sub(4).min(64);
    let height = if new_chat.group { 8 } else { 6 };
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height: height.min(area.height),
    };

    let mut lines = vec![field_line(
        if new_chat.group { "Members: " } else { "Address: " },
        &new_chat.members,
        new_chat.field == 0,
    )];
    if new_chat.group {
        lines.push(field_line("Name:    ", &new_chat.name, new_chat.field == 1));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if new_chat.group {
            "Members separated by spaces or commas. Name is optional."
        } else {
            "Named after the other account."
        },
        Style::default().fg(Color::DarkGray),
    )));

    let title = if new_chat.group { "New Group Chat" } else { "New Direct Chat" };
    let para = Paragraph::new(lines)
        .block(block(title, true))
        .wrap(Wrap { trim: false });
    f.render_widget(Clear, popup);
    f.render_widget(para, popup);
}

fn draw_members(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let Some(members) = &app.members else { return; };

    let width = area.width.saturating_sub(4).min(72);
    let add_height = if members.adding.is_some() { 1 } else { 0 };
    let height = (members.rows.len() as u16 + 2 + add_height).max(3).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let title = match app.selected() {
        Some(ListRow::Chat(chat)) => format!("Members — {}", chat::get_chat_display_name(chat)),
        _ => "Members".to_string(),
    };
    let block = block(&title, true);
    let inner = block.inner(popup);
    f.render_widget(Clear, popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(add_height)])
        .split(inner);

    // A chat always has this account as a member, so an empty list is one
    // still being loaded.
    if members.rows.is_empty() {
        let loading = Span::styled("(loading)", Style::default().fg(Color::DarkGray));
        f.render_widget(Paragraph::new(Line::from(loading)), chunks[0]);
    } else {
        let items: Vec<ListItem> = members.rows.iter().map(|member| {
            let mut spans = vec![Span::raw(member.address.clone())];
            if member.owner {
                spans.push(Span::styled("  owner", Style::default().fg(Color::Yellow)));
            }
            if member.address == app.ctx.identity.address {
                spans.push(Span::styled("  (you)", Style::default().fg(Color::DarkGray)));
            }
            ListItem::new(Line::from(spans))
        }).collect();
        let list = List::new(items)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        let mut state = members.list_state.clone();
        f.render_stateful_widget(list, chunks[0], &mut state);
    }

    if let Some(address) = &members.adding {
        f.render_widget(Paragraph::new(field_line("Add: ", address, true)), chunks[1]);
    }
}

fn field_line<'a>(label: &'a str, value: &'a str, focused: bool) -> Line<'a> {
    let label_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let cursor = if focused { "_" } else { "" };

    Line::from(vec![
        Span::styled(label, label_style),
        Span::raw(format!("{}{}", value, cursor)),
    ])
}

fn draw_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let addr = &app.ctx.identity.address;
    let is_invite = matches!(app.selected(), Some(ListRow::Invite(_)));
    let tail = if let Some(err) = &app.error {
        Span::styled(err.clone(), Style::default().fg(Color::Red))
    } else if app.pending > 0 {
        Span::styled(format!("working ({})", app.pending), Style::default().fg(Color::Yellow))
    } else {
        let is_group_form = app.new_chat.as_ref().is_some_and(|new_chat| new_chat.group);
        let members = app.members.as_ref();
        let is_adding = members.is_some_and(|members| members.adding.is_some());
        let is_group_members = members.is_some_and(|members| members.group);
        let owner_selected = members.and_then(selected_member).is_some_and(|member| member.owner);
        let hint = match app.pane {
            Pane::Chats if is_invite => "j/k select  a join  d dismiss  n/g new  q quit",
            Pane::Chats => "j/k select  Tab/i input  n direct  g group  m members  q quit",
            Pane::Input => "Enter send  Up/Down chat  Esc/Tab back",
            Pane::NewChat if is_group_form => "Tab field  Enter next/create  Esc cancel",
            Pane::NewChat => "Enter create  Esc cancel",
            Pane::Members if is_adding => "Enter add  Esc cancel",
            Pane::Members if is_group_members && owner_selected =>
                "j/k select  a add  d remove  p demote  Esc close",
            Pane::Members if is_group_members => "j/k select  a add  d remove  p promote  Esc close",
            Pane::Members => "j/k select  Esc close  (direct chat — members are fixed)",
        };
        Span::styled(hint, Style::default().fg(Color::DarkGray))
    };
    let line = Line::from(vec![
        Span::styled(format!(" {} ", addr), Style::default().fg(Color::Green)),
        Span::raw(" | "),
        tail,
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
