use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ark_msg::paths::APPS_MSG;
use ark_msg::{chat, direct, group, invite, message};

fn sync_msg(ctx: &ark::Context) {
    ark::create_dir_all(ctx, APPS_MSG).unwrap();
    ark::sync(ctx, APPS_MSG, false, true, |_| false, |_| false).unwrap();
}

// Shared lock: ark_msg uses cwd for context resolution, so tests within a
// single binary must serialize cwd changes.
static CWD_LOCK: Mutex<()> = Mutex::new(());

struct Cleanup { prev: PathBuf, dir: PathBuf }
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.prev);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn temp_root(prefix: &str) -> (PathBuf, Cleanup) {
    let prev = env::current_dir().unwrap_or_else(|_| env::temp_dir());
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = env::temp_dir().join(format!("{}_{}_{}", prefix, std::process::id(), nanos));
    fs::create_dir_all(&dir).unwrap();
    let cleanup = Cleanup { prev, dir: dir.clone() };
    (dir, cleanup)
}

fn init_account(root: &Path, subdir: &str, port: u16, name: &str) -> ark::Context {
    let dir = root.join(subdir);
    fs::create_dir_all(&dir).unwrap();
    env::set_current_dir(&dir).unwrap();
    ark::init(&dir, &format!("{}@127.0.0.1:{}", name, port), None, false).unwrap();
    ark::create_client_context().unwrap()
}

fn has_member(members: &[ark_msg::types::ChatMember], address: &str) -> bool {
    members.iter().any(|member| member.address == address)
}

fn wait_for<F: FnMut() -> bool>(mut cond: F, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() { return; }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for: {}", label);
}

#[test]
fn end_to_end_two_accounts() {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _cleanup) = temp_root("ark_msg_e2e");
    let port = ark::start_test_server(root.clone());

    let _alice = init_account(&root, "alice", port, "alice");
    let bob_addr = format!("bob@127.0.0.1:{}", port);
    let _bob = init_account(&root, "bob", port, "bob");

    // Alice creates a chat with Bob.
    env::set_current_dir(root.join("alice")).unwrap();
    let alice = ark::create_client_context().unwrap();
    let chat_id = direct::create_direct_chat(&alice, Some("hello"), Some("greeting"), &bob_addr).unwrap();
    assert!(chat_id.starts_with("greeting_"), "chat id should start with slug, got {}", chat_id);

    // Both members own a direct chat, and there is no 'all members' group.
    let chat_path = format!("/apps/msg/chats/{}", chat_id);
    assert!(!ark::exists(&alice, &format!("{}/group.json", chat_path)));
    let meta = ark::read_metadata_attributes(&alice, &chat_path).unwrap();
    for address in [&alice.identity.address, &bob_addr] {
        let member = meta.members.iter().find(|m| &m.address == address).unwrap();
        assert_eq!(member.permission, ark::Permission::Owner);
    }

    // Membership is fixed for a direct chat.
    let err = group::add_group_chat_member(&alice, &chat_id, &format!("carol@127.0.0.1:{}", port)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

    // A proposal should arrive on Bob's server.
    let bob_requests = root.join("ark/bob/.ark/requests");
    wait_for(|| bob_requests.exists() && fs::read_dir(&bob_requests).map(|d| d.count() > 0).unwrap_or(false),
             "proposal to land on bob's server");

    // Bob lists invites and accepts the chat share.
    env::set_current_dir(root.join("bob")).unwrap();
    let bob = ark::create_client_context().unwrap();
    let invites = invite::list_invites(&bob).unwrap();
    assert!(!invites.is_empty(), "expected at least one pending invite");
    let chat_invite = invites.iter().find(|i| i.chat_id == chat_id).expect("invite for created chat");
    invite::accept_invite(&bob, chat_invite).unwrap();

    // Sync so bob pulls the chat dir and its contents.
    wait_for(|| {
        sync_msg(&bob);
        let chats = chat::list_chats(&bob).unwrap();
        !chats.is_empty() && chats[0].id == chat_id
    }, "bob to see the chat locally");

    let chats = chat::list_chats(&bob).unwrap();
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].id, chat_id);

    // Alice sends a message.
    env::set_current_dir(root.join("alice")).unwrap();
    let alice = ark::create_client_context().unwrap();
    message::send_message(&alice, &chat_id, b"hello bob").unwrap();

    // Bob syncs and reads.
    env::set_current_dir(root.join("bob")).unwrap();
    let bob = ark::create_client_context().unwrap();
    wait_for(|| {
        sync_msg(&bob);
        message::list_messages(&bob, &chat_id).map(|m| !m.is_empty()).unwrap_or(false)
    }, "bob to see the message locally");

    let msgs = message::read_messages(&bob, &chat_id, None).unwrap();
    assert_eq!(msgs.len(), 1, "expected 1 message");
    assert_eq!(msgs[0].body, "hello bob");
    assert_eq!(msgs[0].sender, alice.identity.address);
}

#[test]
fn add_remove_promote_demote() {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _cleanup) = temp_root("ark_msg_membership");
    let port = ark::start_test_server(root.clone());

    let _alice = init_account(&root, "alice", port, "alice");
    let bob_addr = format!("bob@127.0.0.1:{}", port);
    let carol_addr = format!("carol@127.0.0.1:{}", port);
    let _bob = init_account(&root, "bob", port, "bob");
    let _carol = init_account(&root, "carol", port, "carol");

    env::set_current_dir(root.join("alice")).unwrap();
    let alice = ark::create_client_context().unwrap();
    // A group chat with a single member: it can grow, unlike a direct chat.
    let chat_id = group::create_group_chat(&alice, Some("planning"), Some("plan"), std::slice::from_ref(&bob_addr)).unwrap();
    let chat_path = format!("/apps/msg/chats/{}", chat_id);

    // Members reach a group chat through the group, so carol gets no direct
    // entry on the dir.
    group::add_group_chat_member(&alice, &chat_id, &carol_addr).unwrap();
    let members = chat::get_chat_members(&alice, &chat_id).unwrap();
    assert!(has_member(&members, &alice.identity.address));
    assert!(has_member(&members, &bob_addr));
    assert!(has_member(&members, &carol_addr));
    let meta = ark::read_metadata_attributes(&alice, &chat_path).unwrap();
    assert!(!meta.members.iter().any(|m| m.address == carol_addr));

    group::promote_group_chat_member(&alice, &chat_id, &carol_addr).unwrap();
    let meta = ark::read_metadata_attributes(&alice, &chat_path).unwrap();
    let carol = meta.members.iter().find(|m| m.address == carol_addr).unwrap();
    assert_eq!(carol.permission, ark::Permission::Owner);

    // Owners are marked by their entry on the dir; those reached through the
    // group are ordinary members.
    let members = chat::get_chat_members(&alice, &chat_id).unwrap();
    assert!(members.iter().any(|m| m.address == alice.identity.address && m.owner));
    assert!(members.iter().any(|m| m.address == carol_addr && m.owner));
    assert!(members.iter().any(|m| m.address == bob_addr && !m.owner));
    // The group stands for its members rather than being one of them.
    assert!(!members.iter().any(|m| m.address.contains("group.json")));

    // Demoting drops the direct entry, leaving the group's writer permission.
    group::demote_group_chat_member(&alice, &chat_id, &carol_addr).unwrap();
    let meta = ark::read_metadata_attributes(&alice, &chat_path).unwrap();
    assert!(!meta.members.iter().any(|m| m.address == carol_addr));
    let members = chat::get_chat_members(&alice, &chat_id).unwrap();
    assert!(members.iter().any(|m| m.address == carol_addr && !m.owner));

    group::remove_group_chat_member(&alice, &chat_id, &bob_addr).unwrap();
    assert!(!has_member(&chat::get_chat_members(&alice, &chat_id).unwrap(), &bob_addr));

    // Shrinking to two members keeps the group, so bob can rejoin it.
    let group_address = ark::read_identity(&alice, &format!("{}/group.json", chat_path)).unwrap().address;
    group::add_group_chat_member(&alice, &chat_id, &bob_addr).unwrap();
    let group = ark::read_identity(&alice, &format!("{}/group.json", chat_path)).unwrap();
    assert_eq!(group.address, group_address, "the group should be reused");
    assert!(group.members.unwrap().contains(&bob_addr));

    // Cannot demote self while sole owner.
    let err = group::demote_group_chat_member(&alice, &chat_id, &alice.identity.address).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn display_names_fall_back_to_members() {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _cleanup) = temp_root("ark_msg_names");
    let port = ark::start_test_server(root.clone());

    let _alice = init_account(&root, "alice", port, "alice");
    let bob_addr = format!("bob@127.0.0.1:{}", port);
    let carol_addr = format!("carol@127.0.0.1:{}", port);
    let _bob = init_account(&root, "bob", port, "bob");
    let _carol = init_account(&root, "carol", port, "carol");

    env::set_current_dir(root.join("alice")).unwrap();
    let alice = ark::create_client_context().unwrap();

    let named = direct::create_direct_chat(&alice, Some("hello"), None, &bob_addr).unwrap();
    let direct_chat = direct::create_direct_chat(&alice, None, None, &bob_addr).unwrap();
    let group_chat = group::create_group_chat(&alice, None, None, &[bob_addr.clone(), carol_addr.clone()]).unwrap();

    // A name wins; without one, the other members stand in — self is never
    // one of them.
    let display_name = |chat_id: &str| chat::get_chat_display_name(&chat::read_chat(&alice, chat_id));
    assert_eq!(display_name(&named), "hello");
    assert_eq!(display_name(&direct_chat), bob_addr);
    assert_eq!(display_name(&group_chat), format!("{}, {}", bob_addr, carol_addr));
}

#[test]
fn end_to_end_three_accounts() {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _cleanup) = temp_root("ark_msg_group");
    let port = ark::start_test_server(root.clone());

    let _alice = init_account(&root, "alice", port, "alice");
    let bob_addr = format!("bob@127.0.0.1:{}", port);
    let carol_addr = format!("carol@127.0.0.1:{}", port);
    let _bob = init_account(&root, "bob", port, "bob");
    let _carol = init_account(&root, "carol", port, "carol");

    env::set_current_dir(root.join("alice")).unwrap();
    let alice = ark::create_client_context().unwrap();
    let chat_id = group::create_group_chat(&alice, Some("team"), Some("team"), &[bob_addr.clone(), carol_addr.clone()]).unwrap();

    // The non-owner permissions go to an 'all members' group holding all three.
    let chat_path = format!("/apps/msg/chats/{}", chat_id);
    let group = ark::read_identity(&alice, &format!("{}/group.json", chat_path)).unwrap();
    let group_members = group.members.clone().unwrap();
    assert_eq!(group_members.len(), 3);
    assert!(group_members.contains(&alice.identity.address));
    let meta = ark::read_metadata_attributes(&alice, &chat_path).unwrap();
    let group_member = meta.members.iter().find(|m| m.address == group.address).unwrap();
    assert_eq!(group_member.permission, ark::Permission::Writer);

    message::send_message(&alice, &chat_id, b"hello team").unwrap();

    // Both members reach the chat through the group alone.
    for name in ["bob", "carol"] {
        env::set_current_dir(root.join(name)).unwrap();
        let ctx = ark::create_client_context().unwrap();
        wait_for(|| {
            join_chats(&ctx);
            message::read_messages(&ctx, &chat_id, None).map(|m| !m.is_empty()).unwrap_or(false)
        }, &format!("{} to see the message locally", name));

        let msgs = message::read_messages(&ctx, &chat_id, None).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].body, "hello team");
        assert_eq!(chat::get_chat_members(&ctx, &chat_id).unwrap().len(), 3);

        // The group's key reaches every member, so each can tell a group chat
        // from a direct one without resolving anything.
        assert!(group::is_group_chat(&ctx, &chat_id));

        // The group document reaches them too, rather than being left to a
        // resolve of its address that caches and goes stale.
        let group = ark::read_identity(&ctx, &format!("{}/group.json", chat_path)).unwrap();
        assert_eq!(group.members.unwrap().len(), 3);
    }

    // A member with no direct entry can send too.
    env::set_current_dir(root.join("carol")).unwrap();
    let carol = ark::create_client_context().unwrap();
    message::send_message(&carol, &chat_id, b"hi from carol").unwrap();

    env::set_current_dir(root.join("bob")).unwrap();
    let bob = ark::create_client_context().unwrap();
    wait_for(|| {
        join_chats(&bob);
        message::list_messages(&bob, &chat_id).map(|m| m.len() == 2).unwrap_or(false)
    }, "bob to see carol's message");

    let msgs = message::read_messages(&bob, &chat_id, None).unwrap();
    assert_eq!(msgs[1].body, "hi from carol");
    assert_eq!(msgs[1].sender, carol.identity.address);

    // A membership change reaches a member's own copy of the group, so their
    // view of who is in the chat follows the owner's.
    env::set_current_dir(root.join("alice")).unwrap();
    group::remove_group_chat_member(&alice, &chat_id, &carol_addr).unwrap();

    env::set_current_dir(root.join("bob")).unwrap();
    wait_for(|| {
        join_chats(&bob);
        ark::read_identity(&bob, &format!("{}/group.json", chat_path)).ok()
            .and_then(|group| group.members)
            .is_some_and(|members| !members.contains(&carol_addr))
    }, "bob to see carol dropped from the group");
}

/// A promotion changes permissions alone, with no file body behind it. The
/// watch a running client holds must report that, or the change is not seen
/// until its next start.
#[test]
fn promotion_reaches_a_watching_member() {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _cleanup) = temp_root("ark_msg_watch");
    let port = ark::start_test_server(root.clone());

    let _alice = init_account(&root, "alice", port, "alice");
    let bob_addr = format!("bob@127.0.0.1:{}", port);
    let _bob = init_account(&root, "bob", port, "bob");

    env::set_current_dir(root.join("alice")).unwrap();
    let alice = ark::create_client_context().unwrap();
    let chat_id = group::create_group_chat(&alice, Some("watched"), Some("watched"), std::slice::from_ref(&bob_addr)).unwrap();
    let chat_path = format!("/apps/msg/chats/{}", chat_id);

    env::set_current_dir(root.join("bob")).unwrap();
    let bob = ark::create_client_context().unwrap();
    wait_for(|| {
        join_chats(&bob);
        ark::exists(&bob, &chat_path)
    }, "bob to join the chat");

    // Bob keeps watching, as a running client does.
    let watching = ark::create_client_context().unwrap();
    std::thread::spawn(move || {
        let _ = ark::sync(&watching, APPS_MSG, true, true, |_| false, |_| false);
    });
    std::thread::sleep(Duration::from_millis(500));

    env::set_current_dir(root.join("alice")).unwrap();
    group::promote_group_chat_member(&alice, &chat_id, &bob_addr).unwrap();

    // No sync call of bob's own: the watch alone has to bring it.
    wait_for(|| {
        ark::read_metadata_attributes(&bob, &chat_path)
            .map(|metadata| metadata.members.iter()
                .any(|member| member.address == bob_addr && member.permission == ark::Permission::Owner))
            .unwrap_or(false)
    }, "bob's watch to see the promotion");
}

/// Sync and accept every pending chat invite.
fn join_chats(ctx: &ark::Context) {
    sync_msg(ctx);
    for pending in invite::list_invites(ctx).unwrap() {
        let _ = invite::accept_invite(ctx, &pending);
    }
    sync_msg(ctx);
}

#[test]
fn removal_is_visible_to_the_removed_member() {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _cleanup) = temp_root("ark_msg_removal");
    let port = ark::start_test_server(root.clone());

    let _alice = init_account(&root, "alice", port, "alice");
    let bob_addr = format!("bob@127.0.0.1:{}", port);
    let _bob = init_account(&root, "bob", port, "bob");

    env::set_current_dir(root.join("alice")).unwrap();
    let alice = ark::create_client_context().unwrap();
    let chat_id = group::create_group_chat(&alice, Some("standup"), Some("standup"), std::slice::from_ref(&bob_addr)).unwrap();
    let chat_path = format!("/apps/msg/chats/{}", chat_id);

    env::set_current_dir(root.join("bob")).unwrap();
    let bob = ark::create_client_context().unwrap();
    wait_for(|| {
        join_chats(&bob);
        ark::exists(&bob, &chat_path)
    }, "bob to join the chat");
    assert!(chat::is_chat_member(&bob, &chat_id).unwrap());

    env::set_current_dir(root.join("alice")).unwrap();
    group::remove_group_chat_member(&alice, &chat_id, &bob_addr).unwrap();

    // The drop is not relayed to bob, so his mirror still has him in the
    // chat — only alice's copy of it knows better.
    env::set_current_dir(root.join("bob")).unwrap();
    sync_msg(&bob);
    assert!(has_member(&chat::get_chat_members(&bob, &chat_id).unwrap(), &bob_addr));
    assert!(!chat::is_chat_member(&bob, &chat_id).unwrap());

    // The owner cannot be dropped from their own chat.
    env::set_current_dir(root.join("alice")).unwrap();
    assert!(chat::is_chat_member(&alice, &chat_id).unwrap());
}
