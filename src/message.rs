use std::fs;
use std::io;

use ark::client::{chmod_io, get_io, put_io, track_io};
use ark::metadata::{has_metadata_attributes, read_metadata_attributes};
use ark::types::IdentityContext;
use ark::util::now_iso_fs;

use crate::convo;
use crate::paths::convo_rel_path;

const MSG_PREFIX: &str = "msg-";
const MSG_SUFFIX: &str = ".md";

pub struct MessageSummary {
    pub file_name: String,
    pub sender: String,
    pub modified: String,
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

    let file_name = format!("{}{}{}", MSG_PREFIX, now_iso_fs(), MSG_SUFFIX);
    let file_path = local_dir.join(&file_name);
    fs::write(&file_path, body)?;
    track_io(ctx, file_path.to_str().unwrap(), None)?;

    let target = format!("/{}/{}", rel, file_name);
    // TODO: double-put — ark_friction.md#4. chmod on encrypted file needs an
    // existing file_key, only put mints one. Collapse when ark supports
    // chmod-mints-key or put_with_members.
    put_io(ctx, &target, file_path.to_str(), None)?;
    if !others.is_empty() {
        chmod_io(ctx, file_path.to_str().unwrap(), &[], &[], &others, &[])?;
        put_io(ctx, &target, file_path.to_str(), None)?;
    }

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

/// Read message bodies as UTF-8 (best-effort). For each message: if the local
/// file is still encrypted at rest, decrypt it into place via `get_io`.
pub fn read(ctx: &IdentityContext, dir_name: &str, last_n: Option<usize>) -> io::Result<Vec<(MessageSummary, String)>> {
    let rel = convo_rel_path(dir_name);
    let local_dir = ctx.root.join(&rel);
    let summaries = list(ctx, dir_name)?;
    let take = last_n.unwrap_or(summaries.len());
    let start = summaries.len().saturating_sub(take);

    let mut out = Vec::new();
    for summary in &summaries[start..] {
        let path = local_dir.join(&summary.file_name);
        let encrypted_locally = matches!(
            ark::metadata::read_local_metadata_attributes(&path)?.encrypted,
            Some(true)
        );
        if encrypted_locally {
            get_io(ctx, &format!("/{}/{}", rel, summary.file_name), path.to_str(), true)?;
        }
        let body = fs::read_to_string(&path).unwrap_or_else(|_| "<binary>".to_string());
        out.push((MessageSummary {
            file_name: summary.file_name.clone(),
            sender: summary.sender.clone(),
            modified: summary.modified.clone(),
        }, body));
    }
    Ok(out)
}
