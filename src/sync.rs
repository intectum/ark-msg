use std::io;

use ark::client::{accept_proposal, request, sync};
use ark::types::{DirectoryEntry, DirectoryEntryKind, IdentityContext};
use ark::util::{parse_request_entry, resolve_client_url};

use crate::paths::{convos_root, APPS_MSG};

/// Sync the `apps/msg/` subtree with the current account's server (which
/// federates from co-members), then auto-accept any share proposals targeting
/// `apps/msg/convos/**` so a new convo lands automatically.
pub fn run(ctx: &IdentityContext) -> io::Result<AutoAcceptReport> {
    let path = ctx.root.join(APPS_MSG);
    std::fs::create_dir_all(&path)?;
    let report = auto_accept_convo_proposals(ctx)?;
    sync(ctx, &path, false, true)?;
    Ok(report)
}

pub struct AutoAcceptReport {
    pub accepted: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Enumerate `.ark/requests/` and accept every proposal whose target path is
/// under `apps/msg/convos/`. Non-matching proposals are left untouched.
pub fn auto_accept_convo_proposals(ctx: &IdentityContext) -> io::Result<AutoAcceptReport> {
    let mut report = AutoAcceptReport { accepted: Vec::new(), skipped: Vec::new(), failed: Vec::new() };

    let url = resolve_client_url(ctx, "/.ark/requests/")?;
    let (code, _, body) = request(Some(ctx), "GET", &url, &[], &[])?;
    if code == 404 { return Ok(report); }
    if code != 200 {
        return Err(io::Error::new(io::ErrorKind::Other, format!("HTTP {}: {}", code, String::from_utf8_lossy(&body))));
    }

    let entries: Vec<DirectoryEntry> = serde_json::from_slice(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("dir listing: {}", e)))?;
    let target_marker = format!("/{}/", convos_root());

    for entry in entries {
        if !matches!(entry.kind, DirectoryEntryKind::File) { continue; }
        if !entry.name.ends_with(".http") { continue; }

        let entry_url = resolve_client_url(ctx, &format!("/.ark/requests/{}", entry.name))?;
        let (get_code, _, entry_body) = match request(Some(ctx), "GET", &entry_url, &[], &[]) {
            Ok(v) => v,
            Err(_) => { report.skipped.push(entry.name); continue; }
        };
        if get_code != 200 { report.skipped.push(entry.name); continue; }

        let parsed = match parse_request_entry(&entry_body) {
            Ok(v) => v,
            Err(_) => { report.skipped.push(entry.name); continue; }
        };
        if parsed.method != "PUT" || parsed.status != 403 { report.skipped.push(entry.name); continue; }
        if !parsed.target.contains(&target_marker) { report.skipped.push(entry.name); continue; }

        match accept_proposal(ctx, &entry.name, false) {
            Ok(()) => report.accepted.push(entry.name),
            Err(e) => report.failed.push((entry.name, e.to_string())),
        }
    }

    Ok(report)
}
