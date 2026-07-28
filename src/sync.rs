use std::io;

use ark::client::{accept_proposal, list_proposals, sync};
use ark::types::IdentityContext;

use crate::paths::{convos_root, APPS_MSG};

/// Sync the `apps/msg/` subtree with the current account's server (which
/// federates from co-members), then auto-accept any share proposals targeting
/// `apps/msg/convos/**` so a new convo lands automatically.
pub fn run(ctx: &IdentityContext) -> io::Result<AutoAcceptReport> {
    let path = ctx.root.join(APPS_MSG);
    std::fs::create_dir_all(&path)?;
    let report = auto_accept_convo_proposals(ctx)?;
    sync(ctx, &path, false, true, |_| {}, |_| false)?;
    Ok(report)
}

pub struct AutoAcceptReport {
    pub accepted: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Enumerate pending proposals and accept every one whose target path is
/// under `apps/msg/convos/`. Non-matching proposals are left untouched.
pub fn auto_accept_convo_proposals(ctx: &IdentityContext) -> io::Result<AutoAcceptReport> {
    let mut report = AutoAcceptReport { accepted: Vec::new(), skipped: Vec::new(), failed: Vec::new() };
    let target_marker = format!("/{}/", convos_root());

    for proposal in list_proposals(ctx)? {
        if !proposal.target.contains(&target_marker) {
            report.skipped.push(proposal.id);
            continue;
        }
        match accept_proposal(ctx, &proposal.id, false) {
            Ok(()) => report.accepted.push(proposal.id),
            Err(e) => report.failed.push((proposal.id, e.to_string())),
        }
    }

    Ok(report)
}
