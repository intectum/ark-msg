use std::fs;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use ark::client::sync;
use ark::context::create_client_context;
use ark::types::IdentityContext;
use ark_msg::chat::{get_chat_members, list_chats};
use ark_msg::direct::create_direct_chat;
use ark_msg::group::{add_group_chat_member, create_group_chat, demote_group_chat_member, promote_group_chat_member, remove_group_chat_member};
use ark_msg::message::{read_messages, send_message};
use ark_msg::paths::{chats_root, APPS_MSG};
use ark_msg::reltime::relative_time;
use ark_msg::tui::run_tui;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ark-msg", about = "Simple messaging on top of ark")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Direct chats: a fixed pair, owned by both members.
    Direct {
        #[command(subcommand)] cmd: DirectCmd,
    },
    /// Group chats: members can come and go.
    Group {
        #[command(subcommand)] cmd: GroupCmd,
    },
    /// List local chats.
    List,
    /// Print messages in a chat.
    Read {
        chat: String,
        #[arg(short = 'n', long)] last: Option<usize>,
    },
    /// Send a message. Body from positional arg, or `-i FILE`, or stdin.
    Send {
        chat: String,
        #[arg(short = 'i', long)] input: Option<String>,
        body: Option<String>,
    },
    /// Sync apps/msg/ and auto-accept chat share proposals.
    Sync,
    /// Show members of a chat.
    Members { chat: String },
}

#[derive(Subcommand)]
enum DirectCmd {
    /// Create a direct chat with the given member.
    Create {
        #[arg(long)] name: Option<String>,
        #[arg(long)] slug: Option<String>,
        addr: String,
    },
}

#[derive(Subcommand)]
enum GroupCmd {
    /// Create a group chat with the given members.
    Create {
        #[arg(long)] name: Option<String>,
        #[arg(long)] slug: Option<String>,
        #[arg(required = true)] members: Vec<String>,
    },
    /// Add a member.
    Add { chat: String, addr: String },
    /// Drop a member.
    Remove { chat: String, addr: String },
    /// Promote a member to owner.
    Promote { chat: String, addr: String },
    /// Demote an owner to member.
    Demote { chat: String, addr: String },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("error: {}", e); ExitCode::FAILURE }
    }
}

fn run() -> io::Result<()> {
    let cli = if std::env::args_os().len() > 1 { Some(Cli::parse()) } else { None };

    let ctx = create_client_context()?;
    fs::create_dir_all(ctx.root.join(APPS_MSG))?;
    let cli = match cli {
        Some(cli) => cli,
        None => return run_tui(ctx),
    };

    match cli.cmd {
        Cmd::Direct { cmd } => match cmd {
            DirectCmd::Create { name, slug, addr } =>
                create_direct_chat(&ctx, name.as_deref(), slug.as_deref(), &addr).map(|_| ()),
        },
        Cmd::Group { cmd } => match cmd {
            GroupCmd::Create { name, slug, members } =>
                create_group_chat(&ctx, name.as_deref(), slug.as_deref(), &members).map(|_| ()),
            GroupCmd::Add { chat, addr } =>
                resolve_chat(&ctx, &chat).and_then(|chat_id| add_group_chat_member(&ctx, &chat_id, &addr)),
            GroupCmd::Remove { chat, addr } =>
                resolve_chat(&ctx, &chat).and_then(|chat_id| remove_group_chat_member(&ctx, &chat_id, &addr)),
            GroupCmd::Promote { chat, addr } =>
                resolve_chat(&ctx, &chat).and_then(|chat_id| promote_group_chat_member(&ctx, &chat_id, &addr)),
            GroupCmd::Demote { chat, addr } =>
                resolve_chat(&ctx, &chat).and_then(|chat_id| demote_group_chat_member(&ctx, &chat_id, &addr)),
        },
        Cmd::List => list_chats_cli(&ctx),
        Cmd::Read { chat, last } =>
            resolve_chat(&ctx, &chat).and_then(|chat_id| read_messages_cli(&ctx, &chat_id, last)),
        Cmd::Send { chat, input, body } =>
            resolve_chat(&ctx, &chat).and_then(|chat_id| send_message_cli(&ctx, &chat_id, input, body)),
        Cmd::Sync => sync(&ctx, &ctx.root.join(APPS_MSG), false, true, |_| false, |_| false),
        Cmd::Members { chat } =>
            resolve_chat(&ctx, &chat).and_then(|chat_id| get_chat_members_cli(&ctx, &chat_id)),
    }
}

fn list_chats_cli(ctx: &IdentityContext) -> io::Result<()> {
    let chats = list_chats(ctx)?;
    if chats.is_empty() {
        println!("(no chats)");
        return Ok(());
    }

    for chat in chats {
        println!("{}  {}", chat.id, chat.name.as_deref().unwrap_or("(no name)"));
    }

    Ok(())
}

fn read_messages_cli(ctx: &IdentityContext, chat_id: &str, last: Option<usize>) -> io::Result<()> {
    let messages = read_messages(ctx, chat_id, last)?;

    let mut stdout = io::stdout().lock();
    for message in messages {
        writeln!(stdout, "[{}] {}", relative_time(message.sent), message.sender)?;
        stdout.write_all(message.body.as_bytes())?;
        if !message.body.ends_with('\n') { writeln!(stdout)?; }
        writeln!(stdout)?;
    }

    Ok(())
}

fn send_message_cli(ctx: &IdentityContext, chat_id: &str, input: Option<String>, body_arg: Option<String>) -> io::Result<()> {
    let body: Vec<u8> = if let Some(fs_path) = input {
        fs::read(&fs_path)?
    } else if let Some(s) = body_arg {
        s.into_bytes()
    } else {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        buf
    };

    send_message(ctx, chat_id, &body)?;

    Ok(())
}

fn get_chat_members_cli(ctx: &IdentityContext, chat_id: &str) -> io::Result<()> {
    for address in get_chat_members(ctx, chat_id)? {
        println!("{}", address);
    }

    Ok(())
}

/// Resolve a caller-provided `<chat>` argument to a full chat id.
/// Accepts an exact id, or a prefix that matches exactly one chat
/// (typically the slug portion before the timestamp suffix).
fn resolve_chat(ctx: &IdentityContext, arg: &str) -> io::Result<String> {
    let root = ctx.root.join(chats_root());
    if !root.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no chat matches '{}'", arg)));
    }
    let mut exact = None;
    let mut prefix: Vec<String> = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == arg { exact = Some(name.clone()); }
        if name.starts_with(arg) { prefix.push(name); }
    }
    if let Some(n) = exact { return Ok(n); }
    match prefix.len() {
        1 => Ok(prefix.remove(0)),
        0 => Err(io::Error::new(io::ErrorKind::NotFound, format!("no chat matches '{}'", arg))),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("ambiguous chat '{}': matches {:?}", arg, prefix))),
    }
}
