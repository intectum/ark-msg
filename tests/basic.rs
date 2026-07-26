use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ark::client::init_io;
use ark::context::{create_client_context, create_server_context};
use ark::server::serve;
use ark::types::IdentityContext;
use ark_msg::{convo, message, sync};

fn start_test_server(root: PathBuf) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().unwrap().port();
    let server_ctx = create_server_context(&root, &format!("127.0.0.1:{}", port)).expect("init server identity");
    thread::spawn(move || serve(listener, server_ctx, false));
    port
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

fn init_account(root: &Path, subdir: &str, port: u16, name: &str) -> IdentityContext {
    let dir = root.join(subdir);
    fs::create_dir_all(&dir).unwrap();
    env::set_current_dir(&dir).unwrap();
    init_io(&format!("{}@127.0.0.1:{}", name, port), None).unwrap();
    create_client_context().unwrap()
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
    let port = start_test_server(root.clone());

    let _alice = init_account(&root, "alice", port, "alice");
    let bob_addr = format!("bob@127.0.0.1:{}", port);
    let _bob = init_account(&root, "bob", port, "bob");

    // Alice creates a convo with Bob.
    env::set_current_dir(root.join("alice")).unwrap();
    let alice = create_client_context().unwrap();
    let dir_name = convo::create(&alice, "hello", Some("greeting"), &[bob_addr.clone()]).unwrap();
    assert!(dir_name.starts_with("greeting-"), "dir name should start with slug, got {}", dir_name);

    // A proposal should arrive on Bob's server.
    let bob_requests = root.join("ark/bob/.ark/requests");
    wait_for(|| bob_requests.exists() && fs::read_dir(&bob_requests).map(|d| d.count() > 0).unwrap_or(false),
             "proposal to land on bob's server");

    // Bob syncs: auto-accepts the proposal, pulls the convo dir.
    env::set_current_dir(root.join("bob")).unwrap();
    let bob = create_client_context().unwrap();
    let report = sync::run(&bob).unwrap();
    assert!(report.accepted.len() >= 1, "expected at least one auto-accept, got {:?} skipped={:?} failed={:?}",
            report.accepted, report.skipped, report.failed);

    // A second sync round pulls conversation.json (its proposal is written by
    // alice after the dir is created).
    wait_for(|| {
        let r = sync::run(&bob).unwrap();
        let convos = convo::list(&bob).unwrap();
        !convos.is_empty() && convos[0].dir_name == dir_name && r.failed.is_empty()
    }, "bob to see the convo locally");

    let convos = convo::list(&bob).unwrap();
    assert_eq!(convos.len(), 1);
    assert_eq!(convos[0].dir_name, dir_name);

    // Alice sends a message.
    env::set_current_dir(root.join("alice")).unwrap();
    let alice = create_client_context().unwrap();
    message::send(&alice, &dir_name, b"hello bob").unwrap();

    // Bob syncs and reads.
    env::set_current_dir(root.join("bob")).unwrap();
    let bob = create_client_context().unwrap();
    wait_for(|| {
        let _ = sync::run(&bob).unwrap();
        message::list(&bob, &dir_name).map(|m| !m.is_empty()).unwrap_or(false)
    }, "bob to see the message locally");

    let msgs = message::read(&bob, &dir_name, None).unwrap();
    assert_eq!(msgs.len(), 1, "expected 1 message");
    let (summary, body) = &msgs[0];
    assert_eq!(body, "hello bob");
    assert_eq!(summary.sender, alice.identity.address);
}

#[test]
fn add_remove_promote_demote() {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, _cleanup) = temp_root("ark_msg_membership");
    let port = start_test_server(root.clone());

    let _alice = init_account(&root, "alice", port, "alice");
    let bob_addr = format!("bob@127.0.0.1:{}", port);
    let carol_addr = format!("carol@127.0.0.1:{}", port);
    let _bob = init_account(&root, "bob", port, "bob");
    let _carol = init_account(&root, "carol", port, "carol");

    env::set_current_dir(root.join("alice")).unwrap();
    let alice = create_client_context().unwrap();
    let dir_name = convo::create(&alice, "planning", Some("plan"), &[bob_addr.clone()]).unwrap();

    convo::add_member(&alice, &dir_name, &carol_addr).unwrap();
    let members = convo::members(&alice, &dir_name).unwrap();
    assert!(members.contains(&alice.identity.address));
    assert!(members.contains(&bob_addr));
    assert!(members.contains(&carol_addr));

    convo::promote(&alice, &dir_name, &carol_addr).unwrap();
    let meta = ark::metadata::read_metadata_attributes(&alice.root.join(format!("apps/msg/convos/{}", dir_name))).unwrap();
    let carol = meta.members.iter().find(|m| m.address == carol_addr).unwrap();
    assert_eq!(carol.permission, ark::types::Permission::Owner);

    convo::demote(&alice, &dir_name, &carol_addr).unwrap();
    let meta = ark::metadata::read_metadata_attributes(&alice.root.join(format!("apps/msg/convos/{}", dir_name))).unwrap();
    let carol = meta.members.iter().find(|m| m.address == carol_addr).unwrap();
    assert_eq!(carol.permission, ark::types::Permission::Writer);

    // Cannot demote self while sole owner.
    let err = convo::demote(&alice, &dir_name, &alice.identity.address).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

    convo::remove_member(&alice, &dir_name, &bob_addr).unwrap();
    let members = convo::members(&alice, &dir_name).unwrap();
    assert!(!members.contains(&bob_addr));
}
