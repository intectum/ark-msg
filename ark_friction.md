# Ark friction from ark_msg's perspective

Outstanding items from building the first non-trivial ark app. Ranked by app-side pain.

## 1. No public list-directory client API

The server returns `Vec<DirectoryEntry>` on GET of a dir, but there's no `list_io(ctx, path) -> io::Result<Vec<DirectoryEntry>>` in the public client. A general-purpose app enumerating server-side entries falls back to raw `request()` + JSON deserialisation.

## 2. Sync has no callback surface

`sync` walks 201/204 log entries and emits no events; apps enumerate + accept proposals before or after calling `sync`. Two callbacks would collapse this:

- `on_proposal(&Proposal) -> Decision::Accept | Reject | Skip`
- `on_synced(&SyncEvent)` (Created/Modified/Conflict per file)

ark_msg's `auto_accept_convo_proposals` still exists purely because sync can't drive proposal acceptance itself.

- `ark/src/client/sync.rs`, `ark/src/client/proposals.rs`

## 3. File perms don't inherit from directory perms at write time

`create` and every membership op still make two independent `chmod` calls — one for the dir, one for `conversation.json` — because there's no "apply members from parent" option. A `put` / `chmod` flag like `inherit_readers_from_parent: bool` (or `apply_members_from(parent_path)`) would collapse the dir+doc pair to one round-trip.

## 4. Identity resolution is implicit and can fail late

`chmod` on an encrypted file for a new member calls `resolve_identity`, which fetches from the member's server if not cached. If that server is down, chmod fails after the app assumed it was a local staging op. Split into (a) a pure-local metadata edit and (b) an explicit "prepare identity for encrypted file" step — or expose `prefetch_identity(ctx, addr)` so apps can front-load the fetch at "add contact" time.

- `ark/src/client/chmod.rs`, `ark/src/identity.rs::resolve_identity`

## 5. No "members changed" watch event

`watch_remote` emits Created/Modified/Deleted on files. An app wanting to notify "Carol was added to this convo" has to diff metadata after each event. A dedicated `MetadataChanged` action (or embedding new metadata in `Modified` events) would remove the diff.

- `ark/src/client/watch.rs`, `ark/src/types.rs::WatchAction`

## 6. No "building on ark" guide

Wire-level docs live in `spec.md`; app-developer view lives only in README + Rustdoc. A guide covering the recurring patterns (sync a subtree, resolve+cache identities, wait for a proposal, message-in-a-directory idiom) would compress the learning curve materially. `msg_spec.md` currently covers only legacy email interop, not app-building.

## 7. Sync is slow — sources are structural

`ark-msg` two-account create+sync+send+read loop is ~2s. Underlying costs:

1. **Synchronous relay per PUT.** `ark/src/server/relay.rs` forwards writes to co-members sequentially before returning. N members = N-1 network hops on the critical path. Fix: relay in a background thread; return after local write; log relay failures (idempotent retry queue). Or expose `X-Ark-Relay: async` for apps that don't need synchronous consistency.
2. **`Connection: close` on every request.** `ark/src/client/request.rs` adds it unconditionally. Fresh TCP+TLS handshake per call. Fix: pool per (host, port), or at least keep-alive within a single sync/accept pass.
3. **`accept_proposal` round-trips through both servers.** Bob's accept flow: GET from Alice → PUT to own → own server relays back to Alice. Alice already has the file. Fix: recognise a proposal-accept as "materialise a copy locally, no relay needed" — a distinct verb, or `X-Ark-Relay: none` on the internal PUT.
4. **No batch endpoint for proposals or log entries.** Every `.http` entry costs one GET. A `POST /.ark/requests/batch` returning a JSON array would collapse M requests to 1.

- `ark/src/server/relay.rs`, `ark/src/client/request.rs`, `ark/src/client/proposals.rs`, `ark/src/client/sync.rs`

## 8. Client uses raw `eprintln!`/`println!` — no logger, no callback

`ark/src/client/{sync,watch,proposals}.rs` write progress and error messages via `eprintln!`/`println!`. Fine for the CLI; hostile to a TUI. `ark-msg-tui` had to `dup2` `/dev/null` onto fd 2 at startup so ratatui's frame wouldn't be corrupted by lines like `pull: apps/msg/convos/...`, `sync failed for X: Y`, `watch remote: ... (reconnecting)`.

Fix suggestions (any one solves it):
- Route all internal messages through the `log` or `tracing` crate. Apps install their own subscriber (or silence). Zero cost when no subscriber is installed.
- Add an optional `on_event`/`on_log` callback on the top-level client functions.
- At minimum: emit to a `Write` supplied by the caller, defaulting to stderr for the CLI.

- `ark/src/client/sync.rs`, `ark/src/client/watch.rs`, `ark/src/client/proposals.rs`

## 9. `chmod`'s 8-arg positional signature

Every ark_msg call site is `chmod(ctx, path, &[], &[a], &[], &[], false, None)` — six trailing constants for one active parameter. The tail-`false, None` reads as noise: `local_only=false` is the intended default, and `encryption_algorithm` is only meaningful on the first-ever call for that path.

Options: a `ChmodOptions` builder, keyword-style struct arg, or split into `chmod` (default = perform+upload) and `chmod_local` (stage only). The `encryption_algorithm` parameter is doubly awkward — silently required to be `None` when metadata already exists, so a caller that doesn't know the tracked-ness of the target has to try-catch. Consider making it a first-time-only param on a distinct `chmod_seed` verb, or accept it always and ignore when redundant.

- `ark/src/client/chmod.rs::chmod`

## Nice-to-haves

- Parent-dir semantics undocumented. The server `create_dir_all`s intermediate paths on both file and dir PUTs (`ark/src/server/put.rs:60,62`), and `authorize` only checks the target's own metadata — no walk-up-tree. So `PUT apps/msg/convos/foo/` works with no `apps/`, `apps/msg/`, `apps/msg/convos/` metadata anywhere. Nothing in README/spec.md says this. Add a spec.md line: "intermediate directories are created without metadata; access checks are per-target, not walked."
- `now_iso_fs` — great helper, only exported by `ark::util`. Its `_fs` suffix (colon-safe for Windows/URL) isn't obvious. Rename or doc.
- `Permission::parse` returns `Option`; a `TryFrom<&str>` impl would compose better with clap/serde.
- `start_test_server` is `#[cfg(test)]` on ark's side, so downstream integration tests can't use it. Every consumer copy-pastes the same 4-line spawn (`TcpListener::bind` + `create_server_context` + `thread::spawn(serve)`). Expose behind a `test-utils` feature, or ship a `TestServer` helper.
