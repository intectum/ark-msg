use std::fs;
use std::io;

use ark::client::put;
use ark::permissions::owners;
use ark::timestamp;
use ark::types::IdentityContext;

use crate::chat::{create_chat_id, write_chat};
use crate::paths::{get_chat_ark_path, get_chat_fs_path};
use crate::types::Chat;

/// Create a direct chat with `addr` and share it. Returns the chat id (e.g.
/// `bob_2026-07-28T13-45-07.123Z`).
pub fn create_direct_chat(
    ctx: &IdentityContext,
    name: Option<&str>,
    slug: Option<&str>,
    address: &str,
) -> io::Result<String> {
    let id = create_chat_id(slug, &[address.to_string()]);
    let permissions = owners([ctx.identity.address.clone(), address.to_string()]);

    let fs_path = get_chat_fs_path(&ctx.root, &id);
    let ark_path = get_chat_ark_path(&id);

    fs::create_dir_all(&fs_path)?;
    put(ctx, &ark_path, fs_path.to_str(), &permissions, None, false)?;

    let chat_json_fs_path = fs_path.join("chat.json");
    let chat_json_ark_path = format!("{}/chat.json", ark_path);
    write_chat(&chat_json_fs_path, &Chat {
        id: id.clone(),
        name: name.map(str::to_string),
        last_activity: timestamp::now()
    })?;
    put(ctx, &chat_json_ark_path, chat_json_fs_path.to_str(), &permissions, None, false)?;

    Ok(id)
}
