use std::io;

use crate::chat::{create_chat_id, write_chat};
use crate::paths::get_chat_path;
use crate::types::Chat;

/// Create a group chat with `members` (the caller is always a member and its
/// owner) and share it. Returns the chat id (e.g.
/// `bob_2026-07-28T13-45-07.123Z`).
pub fn create_group_chat(
    ctx: &ark::Context,
    name: Option<&str>,
    slug: Option<&str>,
    members: &[String],
) -> io::Result<String> {
    let id = create_chat_id(slug, members);
    let mut all_members = vec![ctx.identity.address.clone()];
    all_members.extend(members.iter().cloned());

    let path = get_chat_path(&id);

    let group_path = format!("{}/group.json", path);
    let (group, _) = ark::create_client_identity(ctx, &group_path, &all_members)?;

    ark::put(
        ctx,
        &path,
        &ark::Permissions {
            owners: vec![ctx.identity.address.clone()],
            writers: vec![group.address.clone()],
            ..ark::Permissions::default()
        },
        None,
        false
    )?;

    let chat_json_path = format!("{}/chat.json", path);
    write_chat(ctx, &chat_json_path, &Chat {
        id: id.clone(),
        name: name.map(str::to_string),
        other_members: members.to_vec(),
        last_activity: ark::now()
    })?;
    ark::put(
        ctx,
        &chat_json_path,
        &ark::Permissions {
            owners: vec![ctx.identity.address.clone()],
            readers: vec![group.address.clone()],
            ..ark::Permissions::default()
        },
        None,
        false
    )?;

    Ok(id)
}

/// Add a member.
///
/// Only messages sent from now on reach them — existing ones are owned by
/// their senders and are not re-shared.
pub fn add_group_chat_member(ctx: &ark::Context, chat_id: &str, address: &str) -> io::Result<()> {
    if !is_group_chat(ctx, chat_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot add a member to a direct chat"));
    }

    let group_path = format!("{}/group.json", get_chat_path(chat_id));

    ark::change_identity_members(ctx, &group_path, &[address.to_string()], &[])
}

/// Drop a member.
pub fn remove_group_chat_member(ctx: &ark::Context, chat_id: &str, address: &str) -> io::Result<()> {
    if !is_group_chat(ctx, chat_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot remove a member from a direct chat"));
    }

    let path = get_chat_path(chat_id);

    let dir_members = ark::read_metadata_attributes(ctx, &path)
        .map(|metadata| metadata.members).unwrap_or_default();
    if dir_members.iter().any(|member| member.address == address) {
        demote_group_chat_member(ctx, chat_id, address)?;
    }

    let group_path = format!("{}/group.json", path);
    ark::change_identity_members(ctx, &group_path, &[], &[address.to_string()])?;

    Ok(())
}

/// Make a member an owner.
pub fn promote_group_chat_member(ctx: &ark::Context, chat_id: &str, address: &str) -> io::Result<()> {
    if !is_group_chat(ctx, chat_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot promote in a direct chat"));
    }

    let path = get_chat_path(chat_id);

    // Full puts rather than metadata-only ones: a promoted address may have no
    // entry on its server yet, and metadata-only writes have nothing to land on.
    ark::put(ctx, &path, &ark::owner(address), None, false)?;

    let chat_json_path = format!("{}/chat.json", path);
    if ark::exists(ctx, &chat_json_path) {
        ark::put(ctx, &chat_json_path, &ark::owner(address), None, false)?;
    }

    Ok(())
}

/// Make an owner an ordinary member.
pub fn demote_group_chat_member(ctx: &ark::Context, chat_id: &str, address: &str) -> io::Result<()> {
    if !is_group_chat(ctx, chat_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot demote in a direct chat"));
    }

    let path = get_chat_path(chat_id);

    ark::put_permissions(ctx, &path, &ark::drop(address))?;

    let chat_json_path = format!("{}/chat.json", path);
    if ark::exists(ctx, &chat_json_path) {
        ark::put_permissions(ctx, &chat_json_path, &ark::drop(address))?;
    }

    Ok(())
}

pub fn is_group_chat(ctx: &ark::Context, chat_id: &str) -> bool {
    ark::exists(ctx, &format!("{}/group.key", get_chat_path(chat_id)))
}
