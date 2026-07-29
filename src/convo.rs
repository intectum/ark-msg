use std::fs;
use std::io;
use std::path::PathBuf;

use ark::client::{put, put_permissions};
use ark::metadata::{drop, has_metadata_attributes, owner, read_metadata_attributes, reader, writer};
use ark::timestamp;
use ark::types::{IdentityContext, Permission, Permissions};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::message;
use crate::paths::{convo_rel_path, convos_root, default_slug, make_dir_name, sanitize_slug};

const CONVERSATION_JSON: &str = "conversation.json";

#[derive(Serialize, Deserialize, Clone)]
pub struct ConversationDoc {
    pub title: String,
}

#[derive(Clone)]
pub struct ConvoSummary {
    pub dir_name: String,
    pub title: Option<String>,
    pub member_count: usize,
    pub owner_count: usize,
    pub last_activity: OffsetDateTime,
}

/// Create a conversation dir with `members` as writers and the caller as
/// owner, then write and share a `conversation.json`. Returns the dir name
/// (e.g. `2026-07-28T13-45-07.123Z_bob`).
pub fn create(
    ctx: &IdentityContext,
    title: &str,
    slug: Option<&str>,
    members: &[String],
) -> io::Result<String> {
    let slug = slug.map(sanitize_slug).unwrap_or_else(|| default_slug(members));
    let dir_name = make_dir_name(&slug);
    let rel = convo_rel_path(&dir_name);

    let local_dir = ctx.root.join(&rel);
    fs::create_dir_all(&local_dir)?;
    let dir_perms = Permissions { writers: members.to_vec(), ..Default::default() };
    put(ctx, &format!("/{}", rel), Some(local_dir.to_str().unwrap()), &dir_perms, None, false)?;

    let doc = ConversationDoc { title: title.to_string() };
    let json_path = local_dir.join(CONVERSATION_JSON);
    fs::write(&json_path, serde_json::to_vec_pretty(&doc)?)?;
    let json_perms = Permissions { readers: members.to_vec(), ..Default::default() };
    put(ctx, &format!("/{}/{}", rel, CONVERSATION_JSON), Some(json_path.to_str().unwrap()), &json_perms, None, false)?;

    Ok(dir_name)
}

/// Enumerate conversations under the local `apps/msg/convos/` mirror.
pub fn list(ctx: &IdentityContext) -> io::Result<Vec<ConvoSummary>> {
    let root = ctx.root.join(convos_root());
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut summaries = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();

        let title = read_title(&path);
        let (member_count, owner_count) = count_members(&path).unwrap_or((0, 0));
        let last_activity = last_activity(ctx, &dir_name);

        summaries.push(ConvoSummary { dir_name, title, member_count, owner_count, last_activity });
    }
    summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    Ok(summaries)
}

/// Resolve a caller-provided `<convo>` argument to a full dir name.
/// Accepts an exact dir name, or a suffix that matches exactly one convo
/// (typically the slug portion after the timestamp prefix).
pub fn resolve(ctx: &IdentityContext, arg: &str) -> io::Result<String> {
    let root = ctx.root.join(convos_root());
    if !root.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no conversation matches '{}'", arg)));
    }
    let mut exact = None;
    let mut suffix: Vec<String> = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == arg { exact = Some(name.clone()); }
        if name.ends_with(arg) { suffix.push(name); }
    }
    if let Some(n) = exact { return Ok(n); }
    match suffix.len() {
        1 => Ok(suffix.remove(0)),
        0 => Err(io::Error::new(io::ErrorKind::NotFound, format!("no conversation matches '{}'", arg))),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("ambiguous conversation '{}': matches {:?}", arg, suffix))),
    }
}

pub fn add_member(ctx: &IdentityContext, dir_name: &str, addr: &str) -> io::Result<()> {
    let rel = convo_rel_path(dir_name);
    put_permissions(ctx, &format!("/{}", rel), &writer(addr))?;

    let json_rel = format!("{}/{}", rel, CONVERSATION_JSON);
    if ctx.root.join(&json_rel).exists() {
        put_permissions(ctx, &format!("/{}", json_rel), &reader(addr))?;
    }
    Ok(())
}

pub fn remove_member(ctx: &IdentityContext, dir_name: &str, addr: &str) -> io::Result<()> {
    let rel = convo_rel_path(dir_name);
    put_permissions(ctx, &format!("/{}", rel), &drop(addr))?;

    let json_rel = format!("{}/{}", rel, CONVERSATION_JSON);
    if ctx.root.join(&json_rel).exists() {
        put_permissions(ctx, &format!("/{}", json_rel), &drop(addr))?;
    }
    Ok(())
}

pub fn promote(ctx: &IdentityContext, dir_name: &str, addr: &str) -> io::Result<()> {
    let rel = convo_rel_path(dir_name);
    put_permissions(ctx, &format!("/{}", rel), &owner(addr))?;

    let json_rel = format!("{}/{}", rel, CONVERSATION_JSON);
    if ctx.root.join(&json_rel).exists() {
        put_permissions(ctx, &format!("/{}", json_rel), &owner(addr))?;
    }
    Ok(())
}

pub fn demote(ctx: &IdentityContext, dir_name: &str, addr: &str) -> io::Result<()> {
    let rel = convo_rel_path(dir_name);
    if addr == ctx.identity.address {
        let meta = read_metadata_attributes(&ctx.root.join(&rel))?;
        let other_owners = meta.members.iter()
            .filter(|m| m.permission == Permission::Owner && m.address != ctx.identity.address)
            .count();
        if other_owners == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot demote self: no other owner"));
        }
    }
    put_permissions(ctx, &format!("/{}", rel), &writer(addr))?;

    let json_rel = format!("{}/{}", rel, CONVERSATION_JSON);
    if ctx.root.join(&json_rel).exists() {
        put_permissions(ctx, &format!("/{}", json_rel), &reader(addr))?;
    }
    Ok(())
}

/// Return the writer+owner set of the convo dir (excludes any `*` public
/// member).
pub fn members(ctx: &IdentityContext, dir_name: &str) -> io::Result<Vec<String>> {
    let path = ctx.root.join(convo_rel_path(dir_name));
    let meta = read_metadata_attributes(&path)?;
    Ok(meta.members.into_iter()
        .filter(|m| m.address != "*")
        .map(|m| m.address)
        .collect())
}

fn read_title(dir: &PathBuf) -> Option<String> {
    let bytes = fs::read(dir.join(CONVERSATION_JSON)).ok()?;
    let doc: ConversationDoc = serde_json::from_slice(&bytes).ok()?;
    Some(doc.title)
}

/// Newest message timestamp in the convo dir, else convo creation time
/// (ark timestamp prefix on dir_name), else UNIX_EPOCH.
fn last_activity(ctx: &IdentityContext, dir_name: &str) -> OffsetDateTime {
    if let Ok(mut msgs) = message::list(ctx, dir_name) {
        if let Some(latest) = msgs.pop() {
            return latest.modified;
        }
    }
    dir_name.split_once('_')
        .and_then(|(stamp, _)| timestamp::parse_fs_safe(stamp).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn count_members(dir: &PathBuf) -> io::Result<(usize, usize)> {
    if !has_metadata_attributes(dir)? { return Ok((0, 0)); }
    let meta = read_metadata_attributes(dir)?;
    let members = meta.members.iter().filter(|m| m.address != "*").count();
    let owners = meta.members.iter()
        .filter(|m| m.address != "*" && m.permission == Permission::Owner)
        .count();
    Ok((members, owners))
}
