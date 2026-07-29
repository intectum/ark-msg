use std::fs;
use std::io;

use ark::client::put;
use ark::metadata::{has_metadata_attributes, read_metadata_attributes};
use ark::timestamp::{format_fs_safe, now};
use ark::types::{IdentityContext, Permissions};
use time::OffsetDateTime;

use crate::convo;
use crate::paths::convo_rel_path;

const MSG_PREFIX: &str = "msg-";
const MSG_SUFFIX: &str = ".md";

pub struct MessageSummary {
    pub file_name: String,
    pub sender: String,
    pub modified: OffsetDateTime,
}

/// Send a message: write body as a new `msg-<ts>.md` in the convo dir with
/// self as owner and all other convo members as readers.
pub fn send(ctx: &IdentityContext, dir_name: &str, body: &[u8]) -> io::Result<String> {
    let rel = convo_rel_path(dir_name);
    let local_dir = ctx.root.join(&rel);
    if !local_dir.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("conversation dir missing: {}", local_dir.display())));
    }

    let others: Vec<String> = convo::members(ctx, dir_name)?
        .into_iter()
        .filter(|a| a != &ctx.identity.address)
        .collect();

    let file_name = format!("{}{}{}", MSG_PREFIX, format_fs_safe(now()), MSG_SUFFIX);
    let file_path = local_dir.join(&file_name);
    fs::write(&file_path, body)?;
    let perms = Permissions { readers: others, ..Default::default() };
    put(ctx, &format!("/{}/{}", rel, file_name), Some(file_path.to_str().unwrap()), &perms, None, false)?;

    Ok(file_name)
}

/// List message summaries in the local convo dir, oldest first (filename
/// carries the ISO timestamp so lexical order = chronological).
pub fn list(ctx: &IdentityContext, dir_name: &str) -> io::Result<Vec<MessageSummary>> {
    let local_dir = ctx.root.join(convo_rel_path(dir_name));
    if !local_dir.exists() { return Ok(Vec::new()); }

    let mut msgs = Vec::new();
    for entry in fs::read_dir(&local_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(MSG_PREFIX) || !name.ends_with(MSG_SUFFIX) { continue; }
        let path = entry.path();
        if !has_metadata_attributes(&path)? {
            continue;
        }
        let meta = read_metadata_attributes(&path)?;
        msgs.push(MessageSummary { file_name: name, sender: meta.modified_by, modified: meta.modified });
    }
    msgs.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(msgs)
}

/// Read message bodies as UTF-8 (best-effort). Assumes `sync::run` has
/// already pulled files decrypted (`sync(ctx, ..., decrypt=true)`), so this
/// is a pure fs read.
pub fn read(ctx: &IdentityContext, dir_name: &str, last_n: Option<usize>) -> io::Result<Vec<(MessageSummary, String)>> {
    let local_dir = ctx.root.join(convo_rel_path(dir_name));
    let summaries = list(ctx, dir_name)?;
    let take = last_n.unwrap_or(summaries.len());
    let start = summaries.len().saturating_sub(take);

    let mut out = Vec::new();
    for summary in &summaries[start..] {
        let path = local_dir.join(&summary.file_name);
        let body = fs::read_to_string(&path).unwrap_or_else(|_| "<binary>".to_string());
        out.push((MessageSummary {
            file_name: summary.file_name.clone(),
            sender: summary.sender.clone(),
            modified: summary.modified,
        }, body));
    }
    Ok(out)
}
