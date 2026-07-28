use std::io;

use ark::client::list_proposals;
use ark::types::IdentityContext;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::paths::convos_root;

pub struct InviteSummary {
    pub proposal_id: String,
    pub dir_name: String,
    pub proposer: String,
    pub modified: OffsetDateTime,
}

/// List pending share proposals whose target is under `apps/msg/convos/`.
pub fn list(ctx: &IdentityContext) -> io::Result<Vec<InviteSummary>> {
    let marker = format!("/{}/", convos_root());
    let mut out = Vec::new();
    for proposal in list_proposals(ctx)? {
        let Some(idx) = proposal.target.find(&marker) else { continue; };
        let after = &proposal.target[idx + marker.len()..];
        let dir_name = after.split('/').next().unwrap_or("").to_string();
        if dir_name.is_empty() { continue; }

        let modified = OffsetDateTime::parse(&proposal.metadata.modified, &Rfc3339)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        out.push(InviteSummary {
            proposal_id: proposal.id,
            dir_name,
            proposer: proposal.metadata.modified_by,
            modified,
        });
    }
    Ok(out)
}

