# Ark friction from ark_msg's perspective

Outstanding items from building the first non-trivial ark app. Ranked by app-side pain.

## 1. Proposals live outside the sync watch stream

`sync(watch=true)` surfaces reconciled file/dir events via `on_event(EntryEvent)`, but proposal-added / proposal-removed are not part of the stream. Apps that want to show invites in real time end up running a second loop: `ark_msg`'s TUI spawns `sync(watch=true)` in one thread and an interval poll of `list_proposals` in another (`src/tui.rs::spawn_file_watcher` + `spawn_invite_poller`). A `ProposalAdded` / `ProposalRemoved` action on the watch stream (or a `watch_proposals` primitive) would collapse those into one.

- `ark/src/client/sync.rs`, `ark/src/client/proposals.rs`, `ark/src/types.rs::EntryEvent`

## 2. Membership ops fan out to one PUT per file

Every membership change on a chat fires N `put_permissions` — one for the dir, one per shared file — because there's no server-side "apply to dir + N children in one relay pass." A recursive flag on `put_permissions` (dir → subtree), or a batch request endpoint, would collapse the fan-out.

App-side this is the most visible duplication: `promote_group_chat_member`, `demote_group_chat_member` and `remove_group_chat_member` each repeat the same "write the chat dir, then write `chat.json` with the identical permissions" pair (`src/group.rs`). Permissions inherited down a subtree would delete the repetition outright.

## 3. `put_permissions` cannot introduce a file to a new member

Adding a member with `put_permissions` is metadata-only, and the relayed write is rejected `409 metadata put requires existing file` on the new member's server because they have no entry at that path yet — silently, since the relay result is not reported back. The member simply never sees the file. Metadata-only writes should create the entry when the member is new, or `put_permissions` should fall back to a full write.

Directories escape this: `ark/src/server/put.rs:40` skips the existence check when `is_dir`, so the relayed write still runs `create_dir_all`. Only regular files are affected — which splits `ark_msg`'s membership ops in two. Drops (`demote_group_chat_member`, `remove_group_chat_member`) introduce nothing, so every remaining member already holds the entry and `put_permissions` is safe. `promote_group_chat_member` grants a direct entry to an address that may have none, so it re-uploads both bodies with a full `put` to be sure the `chat.json` write lands.

- `ark/src/client/put.rs`, `ark/src/server/put.rs`, `ark/src/server/relay.rs`

## 4. Membership ops hide an extra identity-fetch round-trip

`put_permissions` on an encrypted file for a new member calls `resolve_identity` inside `apply_permission`, which fetches from the member's server if not cached. That fetch is a hidden second network dependency on top of the PUT itself — if the member's server is down, the whole op fails partway through, and the app has no way to front-load or retry the identity step independently. Split into (a) a pure metadata edit and (b) an explicit "wrap file key for member" step — or expose `prefetch_identity(ctx, addr)` so apps can pull identities at "add contact" time.

- `ark/src/client/put.rs`, `ark/src/metadata.rs::apply_permission`, `ark/src/identity.rs::resolve_identity`

## 5. `Metadata` events carry no delta

`sync` now emits `EntryAction::Metadata` for metadata-only remote changes, but the event carries only `path` + `action`. By the time it fires, `write_metadata_attributes` has already overwritten local xattrs with the new state, and `LocalMetadata` doesn't snapshot prior members. An app wanting to notify "Carol was added" has no ark-provided way to compute the delta — it needs its own out-of-band snapshot. Fix: carry the pre-write member list in the event, or stash it in `LocalMetadata` before overwriting.

- `ark/src/client/sync.rs` (write at line 335, emit at line 340), `ark/src/types.rs::EntryEvent`, `ark/src/types.rs::LocalMetadata`

## 6. Cold sync burst cost

Steady-state watch mode is fast — long-lived stream, events delivered without polling. Cold sync (or any burst that fetches many files at once) still pays two structural costs:

1. **`Connection: close` on every request.** `ark/src/client/request.rs:29` adds it unconditionally. Fresh TCP+TLS handshake per call. Fix: pool per (host, port), or at least keep-alive within a single sync pass.
2. **No batch endpoint for log entries.** Every `.http` entry in the fetched log costs one GET. A `POST /.ark/requests/batch` returning a JSON array would collapse M requests to 1.

- `ark/src/client/request.rs`, `ark/src/client/sync.rs`

## 7. A dir has no ark-level link to the group that governs it

The tie between a chat dir and its group identity is pure app convention: `ark_msg` writes the group to `<chat>/group.json` and every op rebuilds that path with `format!("{}/group.json", ark_path)` — and `is_group_chat` leans on the matching `<chat>/group.key`. Ark neither creates the association nor validates it, so a chat whose `group.json` is missing or renamed just looks like a direct chat. Something like "this dir's permissions are governed by identity X", stored by ark, would make the relationship discoverable instead of guessed.

## 8. A share of several paths arrives as unrelated proposals

Sharing a chat is one act, but it reaches the invitee as N independent proposals — the dir, `chat.json`, `group.json`, `group.key` — with nothing tying them together. The app has to re-derive the grouping from the target paths: `list_invites` treats the proposal for `apps/msg/chats/<chat_id>` as the invite and `accept_invite` sweeps up whatever else is pending under that dir (`src/invite.rs`).

The sweep is a snapshot, so a proposal that lands after it is orphaned: its dir is already accepted, no dir proposal remains, and no invite will ever carry it. In `ark_msg` the window is the gap between the dir `put` and the `chat.json` put in `create_direct_chat` / `create_group_chat` — milliseconds, but an invitee who accepts inside it joins a chat that stays unnamed forever. Auto-accepting stray proposals under an owned dir would close it, at the cost of letting any non-member drop files into the chat by proposing them.

Fix: let a proposer group related paths into one proposal (a share id on the request entry, accepted or rejected as a unit), so the invitee's decision covers everything the share was meant to include, whenever each part lands.

- `ark/src/client/proposals.rs`, `ark/src/types.rs::Proposal`, `ark/src/server/relay.rs`

## Nice-to-haves

- `Permission::parse` returns `Option`; a `TryFrom<&str>` impl would compose better with clap/serde.
- Nothing states that an address *is* a group. Asking directly is expensive — `parse_address` to skip bare accounts, then `resolve_identity` (a network fetch for everyone but the creator, who alone holds the public group document) to test `members.is_some()`. `src/group.rs::is_group_chat` avoids all of it by testing for `<chat>/group.key`, which every member holds and which `ark/src/metadata.rs::resolve_key_from_members` locates the same way. Cheap, but it infers a structural fact from a key file: before sync pulls the key a group reads as direct, and a removed member's stale copy still reads as a group. A group flag on `Member` would state what the key file implies.
- `create_identity` returns `(Identity, Key)`. The `Identity` half removed a read-back from disk in `create_group_chat` — good. The `Key` half is dead weight for an app that never touches the secret directly: ark already wraps it for members via `readers` inside `create_identity`.

## What ark makes easy

Worth recording alongside the friction, since these are the parts that carried their weight:

- `ark::permissions::{owner, drop, reader, writer, ...}` — one-address permission changes read as one line (`let permissions = owner(addr);`), with no `..Permissions::default()` noise. Only multi-role permissions (owner + writer in `create_group_chat`) need the struct literal.
- `create_identity` handles the whole group setup — keypair, local write, public identity `put`, and per-member key wrapping — from one call with a member list. Because it grants each member `reader` on the key, the key file itself becomes a local, zero-round-trip "this is a group" marker.
- `change_identity_members` covers add and drop in one idempotent call, and because the chat's permissions name the group, `add_group_chat_member` is that single call and nothing else.
