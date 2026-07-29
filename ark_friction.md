# Ark friction from ark_msg's perspective

Outstanding items from building the first non-trivial ark app. Ranked by app-side pain.

## 1. Proposals live outside the sync watch stream

`sync(watch=true)` surfaces reconciled file/dir events via `on_event(EntryEvent)`, but proposal-added / proposal-removed are not part of the stream. Apps that want to show invites in real time end up running a second loop: `ark_msg`'s TUI spawns `sync(watch=true)` in one thread and an interval poll of `list_proposals` in another (`src/tui.rs::spawn_file_watcher` + `spawn_invite_poller`). A `ProposalAdded` / `ProposalRemoved` action on the watch stream (or a `watch_proposals` primitive) would collapse those into one.

- `ark/src/client/sync.rs`, `ark/src/client/proposals.rs`, `ark/src/types.rs::EntryEvent`

## 2. File perms don't inherit from directory perms at write time

`create` and every membership op still make two independent `put_permissions` calls — one for the dir, one for `conversation.json` — because there's no "apply members from parent" option. A `put` / `put_permissions` flag like `inherit_readers_from_parent: bool` (or `apply_members_from(parent_path)`) would collapse the dir+doc pair to one round-trip.

## 3. Membership ops hide an extra identity-fetch round-trip

`put_permissions` on an encrypted file for a new member calls `resolve_identity` inside `apply_permission`, which fetches from the member's server if not cached. That fetch is a hidden second network dependency on top of the PUT itself — if the member's server is down, the whole op fails partway through, and the app has no way to front-load or retry the identity step independently. Split into (a) a pure metadata edit and (b) an explicit "wrap file key for member" step — or expose `prefetch_identity(ctx, addr)` so apps can pull identities at "add contact" time.

- `ark/src/client/put.rs`, `ark/src/metadata.rs::apply_permission`, `ark/src/identity.rs::resolve_identity`

## 4. No "members changed" watch event

`watch_remote` emits Created/Modified/Deleted on files. An app wanting to notify "Carol was added to this convo" has to diff metadata after each event. A dedicated `MetadataChanged` action (or embedding new metadata in `Modified` events) would remove the diff.

- `ark/src/client/watch.rs`, `ark/src/types.rs::WatchAction`

## 5. No "building on ark" guide

Wire-level docs live in `spec.md`; app-developer view lives only in README + Rustdoc. A guide covering the recurring patterns (sync a subtree, resolve+cache identities, wait for a proposal, message-in-a-directory idiom) would compress the learning curve materially. `msg_spec.md` currently covers only legacy email interop, not app-building.

## 6. Sync is slow — sources are structural

`ark-msg` two-account create+sync+send+read loop is ~2s. Underlying costs:

1. **`Connection: close` on every request.** `ark/src/client/request.rs` adds it unconditionally. Fresh TCP+TLS handshake per call. Fix: pool per (host, port), or at least keep-alive within a single sync/accept pass.
2. **`accept_proposal` round-trips through both servers.** Bob's accept flow: GET from Alice → PUT to own → own server relays back to Alice. Alice already has the file. Fix: recognise a proposal-accept as "materialise a copy locally, no relay needed" — a distinct verb, or `X-Ark-Relay: none` on the internal PUT.
3. **No batch endpoint for proposals or log entries.** Every `.http` entry costs one GET. A `POST /.ark/requests/batch` returning a JSON array would collapse M requests to 1.

- `ark/src/client/request.rs`, `ark/src/client/proposals.rs`, `ark/src/client/sync.rs`

## Nice-to-haves

- `Permission::parse` returns `Option`; a `TryFrom<&str>` impl would compose better with clap/serde.
