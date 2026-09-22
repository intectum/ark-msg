use std::cmp::Reverse;
use std::io;

use time::OffsetDateTime;

use crate::message;
use crate::paths::{default_slug, get_chat_path, sanitize_slug, CHATS_ROOT};
use crate::types::{Chat, ChatMember};

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
///
/// Best effort, for naming a chat. `group.json` can lag the chat dir while
/// syncing, so a group chat read before it arrives yields no members.
/// [`get_chat_members`] resolves those.
fn read_other_members(ctx: &ark::Context, chat_id: &str) -> Vec<String> {
    let path = get_chat_path(chat_id);

    let group_path = format!("{}/group.json", path);
    let mut members: Vec<String> = if ark::exists(ctx, &group_path) {
        ark::read_identity(ctx, &group_path).ok().and_then(|group| group.members).unwrap_or_default()
    } else {
        ark::read_metadata_attributes(ctx, &path)
            .map(|metadata| metadata.members.into_iter()
                .filter(|member| !is_group_address(&member.address))
                .map(|member| member.address)
                .collect())
            .unwrap_or_default()
    };

    members.retain(|address| *address != ctx.identity.address);

    members
}

/// Whether an address is a group's rather than an account's. A group is an
/// identity at a path, e.g. `bob@host/apps/msg/chats/<chat_id>/group.json`.
fn is_group_address(address: &str) -> bool {
    address.contains('/')
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

/// All members of a chat, self included, with the 'all members' group
/// replaced by the members it stands for. Falls back to resolving a group
/// whose document has yet to arrive, which reaches its owner's server, so
/// this is not for the redraw path.
pub fn get_chat_members(ctx: &ark::Context, chat_id: &str) -> io::Result<Vec<ChatMember>> {
    let path = get_chat_path(chat_id);

    // The group is shared with the members it names, so it arrives alongside
    // the chat. Preferred over resolving its address: that answers from a
    // cache which is never refreshed, and so never sees a membership change.
    let group = ark::read_identity(ctx, &format!("{}/group.json", path)).ok();

    let mut members: Vec<ChatMember> = Vec::new();
    for dir_member in ark::read_metadata_attributes(ctx, &path)?.members {
        // Only a direct entry on the chat dir makes an owner; the group is
        // held as a writer, so those it stands for are ordinary members.
        let owner = dir_member.permission == ark::Permission::Owner;

        let addresses = match &group {
            Some(group) if group.address == dir_member.address =>
                group.members.clone().unwrap_or_else(|| vec![dir_member.address]),
            _ => {
                let identity = ark::resolve_identity(ctx, &dir_member.address)?;
                identity.members.unwrap_or_else(|| vec![dir_member.address])
            }
        };
        for address in addresses {
            match members.iter_mut().find(|member| member.address == address) {
                Some(member) => { member.owner |= owner; }
                None => members.push(ChatMember { address, owner }),
            }
        }
    }

    Ok(members)
}
