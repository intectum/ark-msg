use std::fs;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use ark::client::sync;
use ark::context::create_client_context;
use ark::types::IdentityContext;
use ark_msg::paths::APPS_MSG;
use ark_msg::{convo, invite, message, reltime, tui};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ark-msg", about = "Simple messaging on top of ark")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new conversation with the given members.
    New {
        #[arg(long)] title: Option<String>,
        #[arg(long)] slug: Option<String>,
        #[arg(required = true)] members: Vec<String>,
    },
    /// List local conversations.
    List,
    /// Send a message. Body from positional arg, or `-i FILE`, or stdin.
    Send {
        convo: String,
        #[arg(short = 'i', long)] input: Option<String>,
        body: Option<String>,
    },
    /// Print messages in a conversation.
    Read {
        convo: String,
        #[arg(short = 'n', long)] last: Option<usize>,
    },
    /// Add a writer to a conversation.
    AddMember { convo: String, addr: String },
    /// Drop a member from a conversation.
    RemoveMember { convo: String, addr: String },
    /// Promote a member to owner.
    Promote { convo: String, addr: String },
    /// Demote an owner to writer.
    Demote { convo: String, addr: String },
    /// Sync apps/msg/ and auto-accept convo share proposals.
    Sync,
    /// Show members of a conversation.
    Members { convo: String },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("error: {}", e); ExitCode::FAILURE }
    }
}

fn run() -> io::Result<()> {
    let ctx = create_client_context()?;
    fs::create_dir_all(ctx.root.join(APPS_MSG))?;
    if std::env::args_os().len() <= 1 {
        return tui::run(ctx);
    }
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::New { title, slug, members } => cmd_new(&ctx, title, slug, members),
        Cmd::List => cmd_list(&ctx),
        Cmd::Send { convo, input, body } => cmd_send(&ctx, convo, input, body),
        Cmd::Read { convo, last } => cmd_read(&ctx, convo, last),
        Cmd::AddMember { convo, addr } => cmd_convo_op(&ctx, convo, |c, d| convo::add_member(c, d, &addr), "added"),
        Cmd::RemoveMember { convo, addr } => cmd_convo_op(&ctx, convo, |c, d| convo::remove_member(c, d, &addr), "removed"),
        Cmd::Promote { convo, addr } => cmd_convo_op(&ctx, convo, |c, d| convo::promote(c, d, &addr), "promoted"),
        Cmd::Demote { convo, addr } => cmd_convo_op(&ctx, convo, |c, d| convo::demote(c, d, &addr), "demoted"),
        Cmd::Sync => cmd_sync(&ctx),
        Cmd::Members { convo } => cmd_members(&ctx, convo),
    }
}

fn cmd_new(ctx: &IdentityContext, title: Option<String>, slug: Option<String>, members: Vec<String>) -> io::Result<()> {
    let title = title.unwrap_or_else(|| "Untitled".to_string());
    let dir = convo::create(ctx, &title, slug.as_deref(), &members)?;
    println!("{}", dir);
    Ok(())
}

fn cmd_list(ctx: &IdentityContext) -> io::Result<()> {
    let convos = convo::list(ctx)?;
    if convos.is_empty() {
        println!("(no conversations)");
        return Ok(());
    }
    for c in convos {
        println!("{}  members={} owners={}  {}",
            c.dir_name, c.member_count, c.owner_count,
            c.title.as_deref().unwrap_or("(no title)"));
    }
    Ok(())
}

fn cmd_send(ctx: &IdentityContext, convo_arg: String, input: Option<String>, body_arg: Option<String>) -> io::Result<()> {
    let dir = convo::resolve(ctx, &convo_arg)?;
    let body: Vec<u8> = if let Some(path) = input {
        fs::read(&path)?
    } else if let Some(s) = body_arg {
        s.into_bytes()
    } else {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        buf
    };
    let name = message::send(ctx, &dir, &body)?;
    println!("{}", name);
    Ok(())
}

fn cmd_read(ctx: &IdentityContext, convo_arg: String, last: Option<usize>) -> io::Result<()> {
    let dir = convo::resolve(ctx, &convo_arg)?;
    let msgs = message::read(ctx, &dir, last)?;
    let mut stdout = io::stdout().lock();
    for (summary, body) in msgs {
        writeln!(stdout, "[{}] {}", reltime::relative(&summary.modified), summary.sender)?;
        stdout.write_all(body.as_bytes())?;
        if !body.ends_with('\n') { writeln!(stdout)?; }
        writeln!(stdout)?;
    }
    Ok(())
}

fn cmd_convo_op(
    ctx: &IdentityContext,
    convo_arg: String,
    op: impl FnOnce(&IdentityContext, &str) -> io::Result<()>,
    verb: &str,
) -> io::Result<()> {
    let dir = convo::resolve(ctx, &convo_arg)?;
    op(ctx, &dir)?;
    println!("{} in {}", verb, dir);
    Ok(())
}

fn cmd_sync(ctx: &IdentityContext) -> io::Result<()> {
    sync(ctx, &ctx.root.join(APPS_MSG), false, true, |_| false, |_| false)?;
    let pending = invite::list(ctx).map(|v| v.len()).unwrap_or(0);
    if pending > 0 {
        println!("{} pending invite(s) — use TUI to join or dismiss", pending);
    }
    Ok(())
}

fn cmd_members(ctx: &IdentityContext, convo_arg: String) -> io::Result<()> {
    let dir = convo::resolve(ctx, &convo_arg)?;
    for m in convo::members(ctx, &dir)? {
        println!("{}", m);
    }
    Ok(())
}
