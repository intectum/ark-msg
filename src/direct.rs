use std::io;

use crate::chat::{create_chat_id, write_chat};
use crate::paths::get_chat_path;
use crate::types::Chat;

/// Create a direct chat with `addr` and share it. Returns the chat id (e.g.
/// `bob_2026-07-28T13-45-07.123Z`).
pub fn create_direct_chat(
    ctx: &ark::Context,
    name: Option<&str>,
    slug: Option<&str>,
    address: &str,
) -> io::Result<String> {
    let id = create_chat_id(slug, &[address.to_string()]);
    let permissions = ark::owners([ctx.identity.address.clone(), address.to_string()]);

    let path = get_chat_path(&id);

    ark::create_dir_all(ctx, &path)?;
    ark::put(ctx, &path, &permissions, None, false)?;

    let chat_json_path = format!("{}/chat.json", path);
    write_chat(ctx, &chat_json_path, &Chat {
        id: id.clone(),
        name: name.map(str::to_string),
        other_members: vec![address.to_string()],
        last_activity: ark::now()
    })?;
    ark::put(ctx, &chat_json_path, &permissions, None, false)?;

    Ok(id)
}
