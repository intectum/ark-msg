use std::fs;
use std::io;

use ark::client::put;
use ark::metadata::{has_metadata_attributes, read_metadata_attributes};
use ark::permissions::{assign, without};
use ark::timestamp::{format_fs_safe, now};
use ark::types::{IdentityContext, Permission};

use crate::group::is_group_chat;
use crate::paths::{get_chat_ark_path, get_chat_fs_path};
use crate::types::Message;

const MSG_PREFIX: &str = "msg_";
const MSG_EXTENSION: &str = "md";

/// Send a message: write body as a new `msg_<ts>.md` in the chat dir with
/// self as owner, and the group (group chat) or the other members (direct
/// chat) as readers.
pub fn send_message(ctx: &IdentityContext, chat_id: &str, body: &[u8]) -> io::Result<String> {
    let fs_path = get_chat_fs_path(&ctx.root, chat_id);
    if !fs_path.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("chat dir missing: {}", fs_path.display())));
    }

    let file_name = format!("{}{}.{}", MSG_PREFIX, format_fs_safe(now()), MSG_EXTENSION);
    let file_fs_path = fs_path.join(&file_name);

    let metadata = read_metadata_attributes(&fs_path)?;
    let others = if is_group_chat(ctx, chat_id) {
        // The group member is the only writer
        metadata.members.iter().filter(|member| member.permission == Permission::Writer).cloned().collect()
    } else {
        without(&metadata.members, &ctx.identity.address)
    };

    fs::write(&file_fs_path, body)?;
    put(
        ctx,
        &format!("{}/{}", get_chat_ark_path(chat_id), file_name),
        Some(file_fs_path.to_str().unwrap()),
        &assign(&others, Permission::Reader),
        None,
        false
    )?;

    Ok(file_name)
}

/// List message summaries in the local chat dir, oldest first (filename
/// carries the ISO timestamp so lexical order = chronological).
pub fn list_messages(ctx: &IdentityContext, chat_id: &str) -> io::Result<Vec<Message>> {
    let fs_path = get_chat_fs_path(&ctx.root, chat_id);
    if !fs_path.exists() { return Ok(Vec::new()); }

    let mut messages = Vec::new();
    for entry in fs::read_dir(&fs_path)? {
        let entry = entry?;

        let file_name = entry.file_name().to_string_lossy().into_owned();
        if !file_name.starts_with(MSG_PREFIX) { continue; }

        let path = entry.path();
        if !has_metadata_attributes(&path)? { continue; }

        let metadata = read_metadata_attributes(&path)?;
        messages.push(Message {
            id: file_name,
            sender: metadata.modified_by,
            sent: metadata.modified,
            body: "".to_string()
        });
    }

    messages.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(messages)
}

/// Read message bodies as UTF-8 (best-effort). Assumes `sync::run` has
/// already pulled files decrypted (`sync(ctx, ..., decrypt=true)`), so this
/// is a pure fs read.
pub fn read_messages(ctx: &IdentityContext, chat_id: &str, last_n: Option<usize>) -> io::Result<Vec<Message>> {
    let fs_path = get_chat_fs_path(&ctx.root, chat_id);
    let messages = list_messages(ctx, chat_id)?;

    let take = last_n.unwrap_or(messages.len());
    let start = messages.len().saturating_sub(take);

    let mut messages_with_bodies = Vec::new();
    for message in messages.into_iter().skip(start) {
        messages_with_bodies.push(Message {
            body: fs::read_to_string(fs_path.join(&message.id)).unwrap_or("<binary>".to_string()),
            ..message
        });
    }

    Ok(messages_with_bodies)
}
