use std::fs;
use std::io;
use std::path::PathBuf;

use ark::client::{chmod_io, put_io, track_io};
use ark::metadata::{has_metadata_attributes, read_metadata_attributes};
use ark::types::{IdentityContext, Permission};
use serde::{Deserialize, Serialize};

use crate::paths::{convo_rel_path, convos_root, default_slug, make_dir_name, now_unix_secs, sanitize_slug};

const CONVERSATION_JSON: &str = "conversation.json";

#[derive(Serialize, Deserialize, Clone)]
pub struct ConversationDoc {
    pub title: String,
}

pub struct ConvoSummary {
    pub dir_name: String,
    pub title: Option<String>,
    pub member_count: usize,
    pub owner_count: usize,
}

/// Create a conversation dir with `members` as writers and the caller as
/// owner, then write and share a `conversation.json`. Returns the dir name
/// (e.g. `bob-1699999999`).
pub fn create(
    ctx: &IdentityContext,
    title: &str,
    slug: Option<&str>,
    members: &[String],
) -> io::Result<String> {
    let slug = slug.map(sanitize_slug).unwrap_or_else(|| default_slug(members));
    let dir_name = make_dir_name(&slug, now_unix_secs());
    let rel = convo_rel_path(&dir_name);

    let local_dir = ctx.root.join(&rel);
    fs::create_dir_all(&local_dir)?;
    track_io(ctx, local_dir.to_str().unwrap(), None)?;

    if !members.is_empty() {
        chmod_io(ctx, local_dir.to_str().unwrap(), &[], members, &[], &[])?;
    }
    put_io(ctx, &format!("/{}/", rel), local_dir.to_str(), None)?;

    let doc = ConversationDoc { title: title.to_string() };
    let json_path = local_dir.join(CONVERSATION_JSON);
    let json_target = format!("/{}/{}", rel, CONVERSATION_JSON);
    fs::write(&json_path, serde_json::to_vec_pretty(&doc)?)?;
    track_io(ctx, json_path.to_str().unwrap(), None)?;
    // TODO: double-put — ark_friction.md#4. chmod on encrypted file needs an
    // existing file_key, only put mints one. Collapse when ark supports
    // chmod-mints-key or put_with_members.
    put_io(ctx, &json_target, json_path.to_str(), None)?;
    if !members.is_empty() {
        chmod_io(ctx, json_path.to_str().unwrap(), &[], &[], members, &[])?;
        put_io(ctx, &json_target, json_path.to_str(), None)?;
    }

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

        summaries.push(ConvoSummary { dir_name, title, member_count, owner_count });
    }
    summaries.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
    Ok(summaries)
}

/// Resolve a caller-provided `<convo>` argument to a full dir name.
/// Accepts an exact dir name, or a prefix that matches exactly one convo.
pub fn resolve(ctx: &IdentityContext, arg: &str) -> io::Result<String> {
    let root = ctx.root.join(convos_root());
    if !root.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no conversation matches '{}'", arg)));
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
        0 => Err(io::Error::new(io::ErrorKind::NotFound, format!("no conversation matches '{}'", arg))),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("ambiguous conversation '{}': matches {:?}", arg, prefix))),
    }
}

pub fn add_member(ctx: &IdentityContext, dir_name: &str, addr: &str) -> io::Result<()> {
    let rel = convo_rel_path(dir_name);
    let local_dir = ctx.root.join(&rel);
    chmod_io(ctx, local_dir.to_str().unwrap(), &[], &[addr.to_string()], &[], &[])?;
    put_io(ctx, &format!("/{}/", rel), local_dir.to_str(), None)?;

    let json_path = local_dir.join(CONVERSATION_JSON);
    if json_path.exists() {
        chmod_io(ctx, json_path.to_str().unwrap(), &[], &[], &[addr.to_string()], &[])?;
        put_io(ctx, &format!("/{}/{}", rel, CONVERSATION_JSON), json_path.to_str(), None)?;
    }
    Ok(())
}

pub fn remove_member(ctx: &IdentityContext, dir_name: &str, addr: &str) -> io::Result<()> {
    let rel = convo_rel_path(dir_name);
    let local_dir = ctx.root.join(&rel);
    chmod_io(ctx, local_dir.to_str().unwrap(), &[], &[], &[], &[addr.to_string()])?;
    put_io(ctx, &format!("/{}/", rel), local_dir.to_str(), None)?;

    let json_path = local_dir.join(CONVERSATION_JSON);
    if json_path.exists() {
        chmod_io(ctx, json_path.to_str().unwrap(), &[], &[], &[], &[addr.to_string()])?;
        put_io(ctx, &format!("/{}/{}", rel, CONVERSATION_JSON), json_path.to_str(), None)?;
    }
    Ok(())
}

pub fn promote(ctx: &IdentityContext, dir_name: &str, addr: &str) -> io::Result<()> {
    let rel = convo_rel_path(dir_name);
    let local_dir = ctx.root.join(&rel);
    chmod_io(ctx, local_dir.to_str().unwrap(), &[addr.to_string()], &[], &[], &[])?;
    put_io(ctx, &format!("/{}/", rel), local_dir.to_str(), None)?;

    let json_path = local_dir.join(CONVERSATION_JSON);
    if json_path.exists() {
        chmod_io(ctx, json_path.to_str().unwrap(), &[addr.to_string()], &[], &[], &[])?;
        put_io(ctx, &format!("/{}/{}", rel, CONVERSATION_JSON), json_path.to_str(), None)?;
    }
    Ok(())
}

pub fn demote(ctx: &IdentityContext, dir_name: &str, addr: &str) -> io::Result<()> {
    let rel = convo_rel_path(dir_name);
    let local_dir = ctx.root.join(&rel);
    if addr == ctx.identity.address {
        let meta = read_metadata_attributes(&local_dir)?;
        let other_owners = meta.members.iter()
            .filter(|m| m.permission == Permission::Owner && m.address != ctx.identity.address)
            .count();
        if other_owners == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot demote self: no other owner"));
        }
    }
    chmod_io(ctx, local_dir.to_str().unwrap(), &[], &[addr.to_string()], &[], &[])?;
    put_io(ctx, &format!("/{}/", rel), local_dir.to_str(), None)?;

    let json_path = local_dir.join(CONVERSATION_JSON);
    if json_path.exists() {
        // Convo writer = JSON reader (title read-only for non-owners).
        chmod_io(ctx, json_path.to_str().unwrap(), &[], &[], &[addr.to_string()], &[])?;
        put_io(ctx, &format!("/{}/{}", rel, CONVERSATION_JSON), json_path.to_str(), None)?;
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

fn count_members(dir: &PathBuf) -> io::Result<(usize, usize)> {
    if !has_metadata_attributes(dir)? { return Ok((0, 0)); }
    let meta = read_metadata_attributes(dir)?;
    let members = meta.members.iter().filter(|m| m.address != "*").count();
    let owners = meta.members.iter()
        .filter(|m| m.address != "*" && m.permission == Permission::Owner)
        .count();
    Ok((members, owners))
}

