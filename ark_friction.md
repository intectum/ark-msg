# Ark friction from ark_msg's perspective

Outstanding items from building the first non-trivial ark app. Ranked by app-side pain.

## 1. Proposals live outside the sync watch stream

`sync(watch=true)` surfaces reconciled file/dir events via `on_event(EntryEvent)`, but proposal-added / proposal-removed are not part of the stream. Apps that want to show invites in real time end up running a second loop: `ark_msg`'s TUI spawns `sync(watch=true)` in one thread and an interval poll of `list_proposals` in another (`src/tui.rs::spawn_file_watcher` + `spawn_invite_poller`). A `ProposalAdded` / `ProposalRemoved` action on the watch stream (or a `watch_proposals` primitive) would collapse those into one.

- `ark/src/client/sync.rs`, `ark/src/client/proposals.rs`, `ark/src/types.rs::EntryEvent`

## 2. Membership ops fan out to one PUT per file

Every membership change on a convo fires N `put_permissions` — one for the dir, one per shared file — because there's no server-side "apply to dir + N children in one relay pass." A recursive flag on `put_permissions` (dir → subtree), or a batch request endpoint, would collapse the fan-out.

## 3. Membership ops hide an extra identity-fetch round-trip

`put_permissions` on an encrypted file for a new member calls `resolve_identity` inside `apply_permission`, which fetches from the member's server if not cached. That fetch is a hidden second network dependency on top of the PUT itself — if the member's server is down, the whole op fails partway through, and the app has no way to front-load or retry the identity step independently. Split into (a) a pure metadata edit and (b) an explicit "wrap file key for member" step — or expose `prefetch_identity(ctx, addr)` so apps can pull identities at "add contact" time.

- `ark/src/client/put.rs`, `ark/src/metadata.rs::apply_permission`, `ark/src/identity.rs::resolve_identity`

## 4. `Metadata` events carry no delta

`sync` now emits `EntryAction::Metadata` for metadata-only remote changes, but the event carries only `path` + `action`. By the time it fires, `write_metadata_attributes` has already overwritten local xattrs with the new state, and `LocalMetadata` doesn't snapshot prior members. An app wanting to notify "Carol was added" has no ark-provided way to compute the delta — it needs its own out-of-band snapshot. Fix: carry the pre-write member list in the event, or stash it in `LocalMetadata` before overwriting.

- `ark/src/client/sync.rs` (write at line 335, emit at line 340), `ark/src/types.rs::EntryEvent`, `ark/src/types.rs::LocalMetadata`

## 5. Cold sync burst cost

Steady-state watch mode is fast — long-lived stream, events delivered without polling. Cold sync (or any burst that fetches many files at once) still pays two structural costs:

1. **`Connection: close` on every request.** `ark/src/client/request.rs:29` adds it unconditionally. Fresh TCP+TLS handshake per call. Fix: pool per (host, port), or at least keep-alive within a single sync pass.
2. **No batch endpoint for log entries.** Every `.http` entry in the fetched log costs one GET. A `POST /.ark/requests/batch` returning a JSON array would collapse M requests to 1.

- `ark/src/client/request.rs`, `ark/src/client/sync.rs`

## Nice-to-haves

- `Permission::parse` returns `Option`; a `TryFrom<&str>` impl would compose better with clap/serde.
