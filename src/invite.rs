use std::io;

use crate::paths::CHATS_ROOT;
use crate::types::Invite;

/// List pending chat invites — one per proposal for a chat dir
/// `apps/msg/chats/<chat_id>`. Proposals for files within a chat dir are not
/// invites of their own; they are handled along with the dir.
pub fn list_invites(ctx: &ark::Context) -> io::Result<Vec<Invite>> {
    let prefix = format!("{}/", CHATS_ROOT);
    let mut invites = Vec::new();

    for proposal in ark::list_proposals(ctx)? {
        let Some(index) = proposal.target.find(&prefix) else { continue; };
        let chat_id = proposal.target[index + prefix.len()..].trim_end_matches('/');
        if chat_id.is_empty() || chat_id.contains('/') { continue; }

        invites.push(Invite {
            proposal_id: proposal.id,
            chat_id: chat_id.to_string(),
            proposer: proposal.metadata.modified_by,
            proposed: proposal.metadata.modified,
        });
    }

    Ok(invites)
}

/// Accept an invite: the chat dir, then any pending proposals for files
/// within it (e.g. `chat.json`, the 'all members' group).
pub fn accept_invite(ctx: &ark::Context, invite: &Invite) -> io::Result<()> {
    ark::accept_proposal(ctx, &invite.proposal_id, false)?;

    for id in list_chat_proposal_ids(ctx, &invite.chat_id)? {
        ark::accept_proposal(ctx, &id, false)?;
    }

    Ok(())
}

/// Reject an invite, along with any pending proposals for files within the
/// chat dir.
pub fn reject_invite(ctx: &ark::Context, invite: &Invite) -> io::Result<()> {
    ark::reject_proposal(ctx, &invite.proposal_id)?;

    for id in list_chat_proposal_ids(ctx, &invite.chat_id)? {
        ark::reject_proposal(ctx, &id)?;
    }

    Ok(())
}

/// The ids of the pending proposals for files within a chat dir. Call only
/// once the chat dir's own proposal is gone, or it is included too.
fn list_chat_proposal_ids(ctx: &ark::Context, chat_id: &str) -> io::Result<Vec<String>> {
    let prefix = format!("{}/{}/", CHATS_ROOT, chat_id);

    Ok(ark::list_proposals(ctx)?.into_iter()
        .filter(|proposal| proposal.target.contains(&prefix))
        .map(|proposal| proposal.id)
        .collect())
}
