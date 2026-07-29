use std::collections::BTreeMap;
use std::io;

use ark::client::list_proposals;
use ark::types::IdentityContext;
use time::OffsetDateTime;

use crate::paths::convos_root;

#[derive(Clone)]
pub struct InviteSummary {
    pub proposal_ids: Vec<String>,
    pub dir_name: String,
    pub proposer: String,
    pub modified: OffsetDateTime,
}

/// List pending convo invites. Proposals under `apps/msg/convos/<dir>/**`
/// are grouped by `<dir>` into one invite each.
pub fn list(ctx: &IdentityContext) -> io::Result<Vec<InviteSummary>> {
    let marker = format!("/{}/", convos_root());
    let mut by_dir: BTreeMap<String, InviteSummary> = BTreeMap::new();

    for proposal in list_proposals(ctx)? {
        let Some(idx) = proposal.target.find(&marker) else { continue; };
        let after = &proposal.target[idx + marker.len()..];
        let dir_name = after.split('/').next().unwrap_or("").to_string();
        if dir_name.is_empty() { continue; }

        let modified = proposal.metadata.modified;

        by_dir.entry(dir_name.clone())
            .and_modify(|inv| {
                inv.proposal_ids.push(proposal.id.clone());
                if modified < inv.modified { inv.modified = modified; }
            })
            .or_insert_with(|| InviteSummary {
                proposal_ids: vec![proposal.id.clone()],
                dir_name,
                proposer: proposal.metadata.modified_by.clone(),
                modified,
            });
    }

    Ok(by_dir.into_values().collect())
}
