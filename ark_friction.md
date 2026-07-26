# Ark friction from ark_msg's perspective

Notes captured while building the first non-trivial ark app. Ranked by app-side pain.

## 1. `sync_io` hardcodes `current_dir()`

An app can't ask ark to sync a specific subtree (e.g. `apps/msg/`) without `chdir`. `sync(ctx, path, ...)` exists one layer down and is public, so we use that — but `chdir` in a library-consuming app is thread-hostile and interferes with any other subsystem that resolves relative paths. Prefer either an optional `path` on `sync_io` or a `sync_at(ctx, path, ...)`.

- `ark/src/client/sync.rs:28`

## 2. ACL change is two calls, silent failure if you forget the second

Every membership mutation is `chmod_io` (writes local xattrs only) then `put_io` (uploads). Forgetting `put_io` leaves the change local-only with no error. A `set_members_io(ctx, path, owners, writers, readers, drops)` that stages **and** uploads would remove a whole class of app bugs.

Related: creating a fresh file with per-message ACLs currently means `put_io` → `chmod_io` → `put_io`, so ark encrypts + relays twice. Letting `put_io` accept an inline `Vec<Member>` override, or a `put_with_members(...)`, would collapse this.

- `ark/src/client/chmod.rs:27`, `ark/src/client/put.rs:26`

## 3. No public list-directory client API

The server returns `Vec<DirectoryEntry>` on GET of a dir, but the client only exposes `get`/`get_io` (streams body to a writer/file) and internal `sync` machinery that parses it. Apps that want to enumerate `apps/msg/convos/` on the *server* (not the synced local mirror) have to fall back to raw `request()` and deserialize JSON themselves. Add `list_io(ctx, path) -> io::Result<Vec<DirectoryEntry>>`.

## 4. `track_io` semantics need doc clarity — and don't help for encrypted files

Two related problems.

(a) The current doc says "add ark metadata to an existing local file/dir so `sync_io` will consider it" — reads like a sync-only helper. It's actually the **"init as ark file" primitive**: writes owner=self metadata locally, no upload. For **plaintext files or dirs** the correct create-and-share pattern is `write → track → chmod → put` (single put, single encrypt, single relay). Without knowing that, the naive `put → chmod → put` path double-uploads.

(b) For **encrypted files** the pattern doesn't work: `track_io` writes metadata but does NOT generate a file key (owner's `member.key = None`). The subsequent `chmod_io` calls `extract_key_from_metadata` and fails with `no key for <self>`. File keys are only minted inside `put`. So the only public-API path for "encrypted file with N members" is `put → chmod → put` — two encrypts, two relays. ark_msg has to eat this on every message send.

Fix suggestions:
- Either let `track_io` accept an eager `members` list and generate + wrap the file key up front, or
- Add a `put_io_with_members(ctx, path, input, members, encryption_algorithm)` shortcut, or
- Let `chmod_io` on an encrypted file with no existing key mint a fresh key and wrap it for all listed members (equivalent to first-time key generation).

Also rename the CLI verb, or add a doc example under `track_io` showing the plaintext three-step pattern.

- `ark/src/client/track.rs`, `ark/src/client/chmod.rs`, `ark/src/client/put.rs`, README `## CLI quickstart`

## 5. Getting sender identity is a filesystem read, not an API

After `sync`, `modified_by` lives in the local file's xattr metadata; apps must call `metadata::read_metadata_attributes(path)` — coupling app logic to storage layout. `head` already returns `Metadata` but is thinly documented as public. Surface a stable `head_io_metadata(ctx, path) -> Metadata` (or expose `head` prominently) so apps can render "who sent this" without touching xattrs.

- `ark/src/client/head.rs`, `ark/src/metadata.rs`

## 6. Share proposals block the "just works" flow — sync has no callback surface

Two related problems.

(a) When Alice creates a convo and adds Bob as writer, Bob's `sync` does nothing until he runs `ark proposals accept N`. The messaging app knows Alice is a legitimate contact (Bob is a member of the dir), but the client still requires manual acceptance.

(b) `sync` only walks 201/204 log entries (co-member writes) and pulls them. Proposals (403 entries) live in the same `.ark/requests/` log but sit in a parallel path: apps must enumerate + call `accept_proposal` themselves, before or after calling `sync`. `sync` also has no hook for **any** event — it just `eprintln`s locally.

Two callbacks on sync would collapse the whole story:

- `on_proposal(&Proposal) -> Decision::Accept | Reject | Skip` — merges the auto-accept step into sync itself. ark_msg's entire `sync.rs` becomes `sync_with(ctx, path, |p| if p.target.contains("apps/msg/convos") { Accept } else { Skip })`.
- `on_synced(&SyncEvent)` fired per file with variants Created/Modified/Conflict — apps can drive progress bars, "new message from X" notifications, conflict prompts. Right now the eprintln output goes to stderr and can't be captured cleanly.

Our workaround: `ark-msg sync` iterates `list_proposals` (reimplemented via raw `request()` because `list_proposals` isn't re-exported — see nice-to-haves) and auto-accepts anything targeting `apps/msg/convos/**`, then calls `ark::client::sync`.

- `ark/src/client/sync.rs`, `ark/src/client/proposals.rs`

## 7. File perms don't inherit from directory perms at write time

Our messaging model needs `sender=owner, others=readers` per message file — distinct from the parent dir's `writer` set. So every `send` is: write body → track → read parent members → chmod file → put. A `put_io` option like `inherit_readers_from_parent: bool` (or `apply_members_from(parent_path)`) would collapse this to one call and remove the double encrypt/relay.

## 8. Identity resolution is implicit and can fail late

`chmod_io` on an encrypted file for a new member calls `resolve_identity`, which fetches from the member's server if not cached. If that server is down, the "local" chmod fails after the app assumed it was a local staging op. Split into (a) a pure-local metadata edit and (b) an explicit "prepare identity for encrypted file" step — or expose `prefetch_identity(ctx, addr)` so apps can front-load the fetch at "add contact" time.

- `ark/src/client/chmod.rs`, `ark/src/identity.rs::resolve_identity`

## 9. No "members changed" watch event

`watch_remote` emits Created/Modified/Deleted on files. An app wanting to notify "Carol was added to this convo" has to diff metadata after each event. A dedicated `MetadataChanged` action (or embedding new metadata in `Modified` events) would remove the diff.

- `ark/src/client/watch.rs`, `ark/src/types.rs::WatchAction`

## 10. Wire-level docs live in `spec.md`; app-developer view lives only in README + Rustdoc

A "building on ark" guide covering the recurring patterns above (sync a subtree, resolve+cache identities, wait for a proposal, message-in-a-directory idiom) would compress the learning curve materially. `msg_spec.md` currently covers only legacy email interop, not app-building.

## 11. Sync is slow — sources are structural, not incidental

`ark-msg` tests take 3-4 seconds for a two-account create+sync+send+read loop on loopback. Not the encryption; not the disk. It's:

1. **Synchronous relay per PUT.** `ark/src/server/relay.rs:15` loops over co-members and forwards the write to each host sequentially before the response returns. N members = N-1 network hops on the critical path. For a real WAN this dominates. Fix: relay in a background thread; return after local write; log relay failures (idempotent retry queue). Or expose `X-Ark-Relay: async` so apps that don't need synchronous consistency can opt in.

2. **`Connection: close` on every request.** `ark/src/client/request.rs:32` unconditionally adds it. No keep-alive, no connection pool. Fresh TCP (+TLS) handshake per call. A `sync` pass does dozens of small requests; each pays the handshake tax. Fix: pool per (host, port), or at least keep-alive within a single sync/accept pass.

3. **`accept_proposal` round-trips through both servers.** Bob's accept flow: GET file from Alice → PUT to Bob's own server → Bob's server relays that PUT back to Alice's server. Alice already has the file. Fix: recognise a proposal-accept as "materialise a copy locally, no relay needed" — a distinct verb, or `X-Ark-Relay: none` on the internal PUT.

4. **No batch endpoint for proposals or log entries.** Every `.http` entry in `/.ark/requests/` costs one GET. `sync` walks all of them; `accept_proposal` re-GETs the one it was handed. A `POST /.ark/requests/batch` returning a JSON array of parsed entries would collapse M requests to 1.

- `ark/src/server/relay.rs`, `ark/src/client/request.rs`, `ark/src/client/proposals.rs`, `ark/src/client/sync.rs`

## Nice-to-haves discovered along the way

- Parent-dir semantics are undocumented. The server actually `create_dir_all`s intermediate paths on both file and dir PUTs (`ark/src/server/put.rs:60,62`), and `authorize` only checks the target's own metadata — no walk-up-tree. So `PUT apps/msg/convos/foo/` works with no `apps/`, `apps/msg/`, `apps/msg/convos/` metadata anywhere. But nothing in README/spec.md says this, so the default app-author guess ("mirror unix mkdir -p, chmod each parent") is wrong and wasteful. Add a spec.md line: "intermediate directories are created without metadata; access checks are per-target, not walked."
- `now_iso_fs` is a great helper but only exported by `ark::util` — its `_fs` suffix (colon-safe for Windows/URL) isn't obvious. Rename or doc.
- `Permission::parse` returns `Option`; a `TryFrom<&str>` impl would compose better with clap/serde.
- `list_proposals` (returns `Vec<Proposal>`) is `pub fn` in `proposals.rs` but NOT re-exported from `client/mod.rs` — only `list_proposals_io` (which prints) is public. Apps that want to filter proposals programmatically (as ark_msg does for auto-accept) must reimplement it via raw `request()` + `parse_request_entry()`. Fix: re-export `list_proposals`.
- `start_test_server` is `#[cfg(test)]` on ark's side, so downstream integration tests can't use it. Every consumer copy-pastes the same 4-line spawn (`TcpListener::bind` + `create_server_context` + `thread::spawn(serve)`). Expose it (behind a `test-utils` feature if you'd rather not ship it always) or ship a `TestServer` helper.
