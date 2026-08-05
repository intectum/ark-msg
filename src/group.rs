use std::io;

use ark::client::{change_identity_members, create_identity, put, put_permissions};
use ark::metadata::read_metadata_attributes;
use ark::permissions::{drop, owner};
use ark::timestamp;
use ark::types::{IdentityContext, Permissions};

use crate::chat::{create_chat_id, write_chat};
use crate::paths::{get_chat_ark_path, get_chat_fs_path};
use crate::types::Chat;

/// Create a group chat with `members` (the caller is always a member and its
/// owner) and share it. Returns the chat id (e.g.
/// `bob_2026-07-28T13-45-07.123Z`).
pub fn create_group_chat(
    ctx: &IdentityContext,
    name: Option<&str>,
    slug: Option<&str>,
    members: &[String],
) -> io::Result<String> {
    let id = create_chat_id(slug, members);
    let mut all_members = vec![ctx.identity.address.clone()];
    all_members.extend(members.iter().cloned());

    let fs_path = get_chat_fs_path(&ctx.root, &id);
    let ark_path = get_chat_ark_path(&id);

    let group_ark_path = format!("{}/group.json", ark_path);
    let (group, _) = create_identity(ctx, &group_ark_path, &all_members)?;

    put(
        ctx,
        &ark_path,
        fs_path.to_str(),
        &Permissions {
            owners: vec![ctx.identity.address.clone()],
            writers: vec![group.address.clone()],
            ..Permissions::default()
        },
        None,
        false
    )?;

    let chat_json_fs_path = fs_path.join("chat.json");
    let chat_json_ark_path = format!("{}/chat.json", ark_path);
    write_chat(&chat_json_fs_path, &Chat {
        id: id.clone(),
        name: name.map(str::to_string),
        last_activity: timestamp::now()
    })?;
    put(
        ctx,
        &chat_json_ark_path,
        chat_json_fs_path.to_str(),
        &Permissions {
            owners: vec![ctx.identity.address.clone()],
            readers: vec![group.address.clone()],
            ..Permissions::default()
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
pub fn add_group_chat_member(ctx: &IdentityContext, chat_id: &str, address: &str) -> io::Result<()> {
    if !is_group_chat(ctx, chat_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot add a member to a direct chat"));
    }

    let ark_path = get_chat_ark_path(chat_id);

    let group_ark_path = format!("{}/group.json", ark_path);
    change_identity_members(ctx, &group_ark_path, &[address.to_string()], &[])
}

/// Drop a member.
pub fn remove_group_chat_member(ctx: &IdentityContext, chat_id: &str, address: &str) -> io::Result<()> {
    if !is_group_chat(ctx, chat_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot remove a member from a direct chat"));
    }

    let fs_path = get_chat_fs_path(&ctx.root, chat_id);
    let ark_path = get_chat_ark_path(chat_id);

    let dir_members = read_metadata_attributes(&fs_path)
        .map(|metadata| metadata.members).unwrap_or_default();
    if dir_members.iter().any(|member| member.address == address) {
        demote_group_chat_member(ctx, chat_id, address)?;
    }

    let group_ark_path = format!("{}/group.json", ark_path);
    change_identity_members(ctx, &group_ark_path, &[], &[address.to_string()])?;

    Ok(())
}

/// Make a member an owner.
pub fn promote_group_chat_member(ctx: &IdentityContext, chat_id: &str, address: &str) -> io::Result<()> {
    if !is_group_chat(ctx, chat_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot promote in a direct chat"));
    }

    let fs_path = get_chat_fs_path(&ctx.root, chat_id);
    let ark_path = get_chat_ark_path(chat_id);

    // Full puts rather than metadata-only ones: a promoted address may have no
    // entry on its server yet, and metadata-only writes have nothing to land on.
    put(ctx, &ark_path, fs_path.to_str(), &owner(address), None, false)?;

    let chat_json_fs_path = fs_path.join("chat.json");
    if chat_json_fs_path.exists() {
        put(ctx, &format!("{}/chat.json", ark_path), chat_json_fs_path.to_str(), &owner(address), None, false)?;
    }

    Ok(())
}

/// Make an owner an ordinary member.
pub fn demote_group_chat_member(ctx: &IdentityContext, chat_id: &str, address: &str) -> io::Result<()> {
    if !is_group_chat(ctx, chat_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot demote in a direct chat"));
    }

    let fs_path = get_chat_fs_path(&ctx.root, chat_id);
    let ark_path = get_chat_ark_path(chat_id);

    put_permissions(ctx, &ark_path, &drop(address))?;

    if fs_path.join("chat.json").exists() {
        put_permissions(ctx, &format!("{}/chat.json", ark_path), &drop(address))?;
    }

    Ok(())
}

pub fn is_group_chat(ctx: &IdentityContext, chat_id: &str) -> bool {
    get_chat_fs_path(&ctx.root, chat_id).join("group.key").exists()
}
