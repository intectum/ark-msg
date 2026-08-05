use std::path::{Path, PathBuf};

pub const APPS_MSG: &str = "apps/msg";

pub fn chats_root() -> String {
    format!("{}/chats", APPS_MSG)
}

/// The chat's ark path, e.g. `/apps/msg/chats/<chat_id>`.
pub fn get_chat_ark_path(chat_id: &str) -> String {
    format!("/{}/{}", chats_root(), chat_id)
}

/// The chat's dir in the local mirror under `root`.
pub fn get_chat_fs_path(root: &Path, chat_id: &str) -> PathBuf {
    root.join(chats_root()).join(chat_id)
}

/// Sanitize a caller-provided slug: keep `[a-z0-9-]`, collapse others to `-`,
/// trim leading/trailing dashes, cap at 32 chars, fall back to `"chat"` if
/// empty.
pub fn sanitize_slug(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = true;
    for c in input.trim().chars().flat_map(|c| c.to_lowercase()) {
        let ok = c.is_ascii_alphanumeric() || c == '-' || c == '_';
        if ok {
            out.push(if c == '_' { '-' } else { c });
            last_dash = c == '-';
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    let capped: String = trimmed.chars().take(32).collect();
    if capped.is_empty() { "chat".to_string() } else { capped }
}

/// Derive a default slug from the first member's address local-part, or
/// `"chat"` if none.
pub fn default_slug(members: &[String]) -> String {
    members
        .first()
        .and_then(|addr| addr.split('@').next())
        .map(sanitize_slug)
        .unwrap_or_else(|| "chat".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_lowercases_and_replaces() {
        assert_eq!(sanitize_slug("Hello World!"), "hello-world");
        assert_eq!(sanitize_slug("  spaced  "), "spaced");
        assert_eq!(sanitize_slug("____"), "chat");
        assert_eq!(sanitize_slug(""), "chat");
        assert_eq!(sanitize_slug("under_score"), "under-score");
    }

    #[test]
    fn default_slug_uses_local_part() {
        assert_eq!(default_slug(&["bob@host:8080".to_string()]), "bob");
        assert_eq!(default_slug(&[]), "chat");
    }

}
