use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::Path;

use ark::identity::resolve_identity;
use ark::metadata::read_metadata_attributes;
use ark::timestamp;
use ark::types::IdentityContext;
use time::OffsetDateTime;

use crate::message;
use crate::paths::{chats_root, default_slug, get_chat_fs_path, sanitize_slug};
use crate::types::Chat;

/// An id for a new chat: a slug and a timestamp, e.g.
/// `bob_2026-07-28T13-45-07.123Z`. Also names its dir in the local mirror.
/// `members` only feeds the default slug.
pub fn create_chat_id(slug: Option<&str>, members: &[String]) -> String {
    let slug = slug.map(sanitize_slug).unwrap_or_else(|| default_slug(members));

    format!("{}_{}", slug, timestamp::format_fs_safe(timestamp::now()))
}

/// Read a chat from the local mirror. A chat whose `chat.json` is missing or
/// unreadable is returned unnamed.
pub fn read_chat(ctx: &IdentityContext, chat_id: &str) -> Chat {
    // chat.json can lag the chat dir while syncing, so a chat without one
    // is read unnamed rather than failing.
    let name = fs::read(get_chat_fs_path(&ctx.root, chat_id).join("chat.json")).ok()
        .and_then(|bytes| serde_json::from_slice::<Chat>(&bytes).ok())
        .and_then(|chat| chat.name);

    let mut last_activity = chat_id.split_once('_')
        .and_then(|(_, timestamp)| timestamp::parse_fs_safe(timestamp).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    if let Ok(mut messages) = message::list_messages(ctx, chat_id) {
        if let Some(latest) = messages.pop() {
            last_activity = latest.sent;
        }
    }

    Chat { id: chat_id.to_owned(), name, last_activity }
}

/// Write a chat's `chat.json`.
pub fn write_chat(path: &Path, chat: &Chat) -> io::Result<()> {
    fs::write(path, serde_json::to_vec_pretty(chat)?)
}

/// Enumerate chats under the local `apps/msg/chats/` mirror.
pub fn list_chats(ctx: &IdentityContext) -> io::Result<Vec<Chat>> {
    let fs_path = ctx.root.join(chats_root());
    if !fs_path.exists() {
        return Ok(Vec::new());
    }

    let mut chats = Vec::new();
    for entry in fs::read_dir(&fs_path)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }

        let id = entry.file_name().to_string_lossy().into_owned();
        chats.push(read_chat(ctx, &id));
    }

    chats.sort_by_key(|chat| Reverse(chat.last_activity));

    Ok(chats)
}

/// All member addresses of a chat, with the 'all members' group replaced by
/// the members it stands for.
pub fn get_chat_members(ctx: &IdentityContext, chat_id: &str) -> io::Result<Vec<String>> {
    let local_dir = get_chat_fs_path(&ctx.root, chat_id);

    let mut addresses: Vec<String> = Vec::new();
    for member in read_metadata_attributes(&local_dir)?.members {
        let identity = resolve_identity(ctx, &member.address)?;
        let member_addresses = identity.members.unwrap_or_else(|| vec![member.address]);
        for address in member_addresses {
            if !addresses.contains(&address) {
                addresses.push(address);
            }
        }
    }

    Ok(addresses)
}
