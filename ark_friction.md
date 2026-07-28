# Ark friction from ark_msg's perspective

Outstanding items from building the first non-trivial ark app. Ranked by app-side pain.

## 1. Proposals live outside the sync watch stream

`sync(watch=true)` surfaces reconciled file/dir events via `on_event(EntryEvent)`, but proposal-added / proposal-removed are not part of the stream. Apps that want to show invites in real time end up running a second loop: `ark_msg`'s TUI spawns `sync(watch=true)` in one thread and an interval poll of `list_proposals` in another (`src/tui.rs::spawn_file_watcher` + `spawn_invite_poller`). A `ProposalAdded` / `ProposalRemoved` action on the watch stream (or a `watch_proposals` primitive) would collapse those into one.

- `ark/src/client/sync.rs`, `ark/src/client/proposals.rs`, `ark/src/types.rs::EntryEvent`

## 2. File perms don't inherit from directory perms at write time

`create` and every membership op still make two independent `chmod` calls — one for the dir, one for `conversation.json` — because there's no "apply members from parent" option. A `put` / `chmod` flag like `inherit_readers_from_parent: bool` (or `apply_members_from(parent_path)`) would collapse the dir+doc pair to one round-trip.

## 3. Identity resolution is implicit and can fail late

`chmod` on an encrypted file for a new member calls `resolve_identity`, which fetches from the member's server if not cached. If that server is down, chmod fails after the app assumed it was a local staging op. Split into (a) a pure-local metadata edit and (b) an explicit "prepare identity for encrypted file" step — or expose `prefetch_identity(ctx, addr)` so apps can front-load the fetch at "add contact" time.

- `ark/src/client/chmod.rs`, `ark/src/identity.rs::resolve_identity`

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

- Parent-dir semantics undocumented. The server `create_dir_all`s intermediate paths on both file and dir PUTs (`ark/src/server/put.rs:60,62`), and `authorize` only checks the target's own metadata — no walk-up-tree. So `PUT apps/msg/convos/foo/` works with no `apps/`, `apps/msg/`, `apps/msg/convos/` metadata anywhere. Nothing in README/spec.md says this. Add a spec.md line: "intermediate directories are created without metadata; access checks are per-target, not walked."
- `now_iso_fs` — great helper, only exported by `ark::util`. Its `_fs` suffix (colon-safe for Windows/URL) isn't obvious. Rename or doc.
- `Permission::parse` returns `Option`; a `TryFrom<&str>` impl would compose better with clap/serde.
- `start_test_server` is `#[cfg(test)]` on ark's side, so downstream integration tests can't use it. Every consumer copy-pastes the same 4-line spawn (`TcpListener::bind` + `create_server_context` + `thread::spawn(serve)`). Expose behind a `test-utils` feature, or ship a `TestServer` helper.
