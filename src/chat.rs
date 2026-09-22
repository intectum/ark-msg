use std::cmp::Reverse;
use std::io;

use time::OffsetDateTime;

use crate::message;
use crate::paths::{default_slug, get_chat_path, sanitize_slug, CHATS_ROOT};
use crate::types::Chat;

/// An id for a new chat: a slug and a timestamp, e.g.
/// `bob_2026-07-28T13-45-07.123Z`. Also names its dir in the local mirror.
/// `members` only feeds the default slug.
pub fn create_chat_id(slug: Option<&str>, members: &[String]) -> String {
    let slug = slug.map(sanitize_slug).unwrap_or_else(|| default_slug(members));

    format!("{}_{}", slug, ark::format_fs_safe(ark::now()))
}

/// Read a chat from the local mirror. A chat whose `chat.json` is missing or
/// unreadable is returned unnamed.
pub fn read_chat(ctx: &ark::Context, chat_id: &str) -> Chat {
    let path = get_chat_path(chat_id);

    // chat.json can lag the chat dir while syncing, so a chat without one
    // is read unnamed rather than failing.
    let name = ark::read(ctx, &format!("{}/chat.json", path)).ok()
        .and_then(|bytes| serde_json::from_slice::<Chat>(&bytes).ok())
        .and_then(|chat| chat.name);

    let mut last_activity = chat_id.split_once('_')
        .and_then(|(_, timestamp)| ark::parse_fs_safe(timestamp).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    if let Ok(mut messages) = message::list_messages(ctx, chat_id) {
        if let Some(latest) = messages.pop() {
            last_activity = latest.sent;
        }
    }

    Chat { id: chat_id.to_owned(), name, other_members: read_other_members(ctx, chat_id), last_activity }
}

/// A chat's members but self, from the local mirror alone: the 'all members'
/// group of a group chat, or the dir's members for a direct chat. Anything
/// missing or unreadable yields no members.
fn read_other_members(ctx: &ark::Context, chat_id: &str) -> Vec<String> {
    let path = get_chat_path(chat_id);

    let group_path = format!("{}/group.json", path);
    let mut members = if ark::exists(ctx, &group_path) {
        ark::read_identity(ctx, &group_path).ok().and_then(|group| group.members).unwrap_or_default()
    } else {
        ark::read_metadata_attributes(ctx, &path)
            .map(|metadata| metadata.members.into_iter().map(|member| member.address).collect())
            .unwrap_or_default()
    };

    members.retain(|address| *address != ctx.identity.address);

    members
}

/// What to call a chat: its name, or failing that its other members'
/// addresses, or failing that its id.
pub fn get_chat_display_name(chat: &Chat) -> String {
    if let Some(name) = &chat.name {
        return name.clone();
    }

    if chat.other_members.is_empty() {
        return chat.id.clone();
    }

    chat.other_members.join(", ")
}

/// Write a chat's `chat.json`.
pub fn write_chat(ctx: &ark::Context, path: &str, chat: &Chat) -> io::Result<()> {
    ark::write(ctx, path, &serde_json::to_vec_pretty(chat)?)
}

/// Enumerate chats under the local `apps/msg/chats/` mirror.
pub fn list_chats(ctx: &ark::Context) -> io::Result<Vec<Chat>> {
    if !ark::exists(ctx, CHATS_ROOT) {
        return Ok(Vec::new());
    }

    let mut chats = Vec::new();
    for path in ark::read_dir(ctx, CHATS_ROOT)? {
        if !ark::is_dir(ctx, &path) { continue; }

        chats.push(read_chat(ctx, ark::file_name(&path)));
    }

    chats.sort_by_key(|chat| Reverse(chat.last_activity));

    Ok(chats)
}

/// All member addresses of a chat, with the 'all members' group replaced by
/// the members it stands for.
pub fn get_chat_members(ctx: &ark::Context, chat_id: &str) -> io::Result<Vec<String>> {
    let mut addresses: Vec<String> = Vec::new();
    for member in ark::read_metadata_attributes(ctx, &get_chat_path(chat_id))?.members {
        let identity = ark::resolve_identity(ctx, &member.address)?;
        let member_addresses = identity.members.unwrap_or_else(|| vec![member.address]);
        for address in member_addresses {
            if !addresses.contains(&address) {
                addresses.push(address);
            }
        }
    }

    Ok(addresses)
}
