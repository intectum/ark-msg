use std::io;

use crate::group::is_group_chat;
use crate::paths::get_chat_path;
use crate::types::Message;

const MSG_PREFIX: &str = "msg_";
const MSG_EXTENSION: &str = "md";

/// Send a message: write body as a new `msg_<ts>.md` in the chat dir with
/// self as owner, and the group (group chat) or the other members (direct
/// chat) as readers.
pub fn send_message(ctx: &ark::Context, chat_id: &str, body: &[u8]) -> io::Result<String> {
    let path = get_chat_path(chat_id);
    if !ark::exists(ctx, &path) {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("chat dir missing: {}", path)));
    }

    let file_name = format!("{}{}.{}", MSG_PREFIX, ark::format_fs_safe(ark::now()), MSG_EXTENSION);
    let file_path = ark::join_path(&path, &file_name);

    let metadata = ark::read_metadata_attributes(ctx, &path)?;
    let others = if is_group_chat(ctx, chat_id) {
        // The group member is the only writer
        metadata.members.iter().filter(|member| member.permission == ark::Permission::Writer).cloned().collect()
    } else {
        ark::without(&metadata.members, &ctx.identity.address)
    };

    ark::write(ctx, &file_path, body)?;
    ark::put(ctx, &file_path, &ark::assign(&others, ark::Permission::Reader), None, false)?;

    Ok(file_name)
}

/// List message summaries in the local chat dir, oldest first (filename
/// carries the ISO timestamp so lexical order = chronological).
pub fn list_messages(ctx: &ark::Context, chat_id: &str) -> io::Result<Vec<Message>> {
    let path = get_chat_path(chat_id);
    if !ark::exists(ctx, &path) { return Ok(Vec::new()); }

    let mut messages = Vec::new();
    for message_path in ark::read_dir(ctx, &path)? {
        let file_name = ark::file_name(&message_path);
        if !file_name.starts_with(MSG_PREFIX) { continue; }

        if !ark::has_metadata_attributes(ctx, &message_path)? { continue; }

        let metadata = ark::read_metadata_attributes(ctx, &message_path)?;
        messages.push(Message {
            id: file_name.to_string(),
            sender: metadata.modified_by,
            sent: metadata.modified,
            body: "".to_string()
        });
    }

    Ok(messages)
}

/// Read message bodies as UTF-8 (best-effort). Assumes `sync` has already
/// pulled files decrypted (`sync(ctx, ..., decrypt=true)`), so this is a pure
/// read.
pub fn read_messages(ctx: &ark::Context, chat_id: &str, last_n: Option<usize>) -> io::Result<Vec<Message>> {
    let path = get_chat_path(chat_id);
    let messages = list_messages(ctx, chat_id)?;

    let take = last_n.unwrap_or(messages.len());
    let start = messages.len().saturating_sub(take);

    let mut messages_with_bodies = Vec::new();
    for message in messages.into_iter().skip(start) {
        messages_with_bodies.push(Message {
            body: ark::read_to_string(ctx, &ark::join_path(&path, &message.id)).unwrap_or("<binary>".to_string()),
            ..message
        });
    }

    Ok(messages_with_bodies)
}
