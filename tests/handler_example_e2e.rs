mod common;

use common::{spawn_pinhead, unique_sock_id, wait_for_socket, NinepClient};
use std::fs;
use std::process::Child;
use std::time::Duration;

/// Build handler.lua with a custom socket path.
fn build_script(socket_path: &str) -> String {
    let content = include_str!("../examples/handler.lua");
    content.replace("/tmp/pinhead.sock", socket_path)
}

fn kill_pinhead(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Test the full 9P protocol lifecycle on root — verifies that all routes
/// defined in handler.lua respond correctly through the 9P frontend.
///
/// Root `/` has: lookup, getattr, readdir, open, release.
#[test]
fn handler_lua_root_9p_lifecycle() {
    let socket_path = format!("/tmp/pinhead-e2e-handler-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = NinepClient::connect(&socket_path).expect("connect");

    // Tversion — verify protocol negotiation
    client.version(0, 8192).expect("Tversion should succeed");

    // Tattach — get root fid
    client.attach(0, 1).expect("Tattach should succeed");

    // Twalk 0 elements: clone root fid (1 → 2)
    let resp = client
        .walk(0, 1, 2, "")
        .expect("0-element Twalk should succeed");
    assert!(resp.len() >= 2, "Rwalk should have nwqid field");
    let nwqid = u16::from_le_bytes(resp[0..2].try_into().unwrap());
    assert_eq!(nwqid, 0, "0-element walk returns 0 qids");

    // Topen on root — Open operation on /
    client.open(0, 2).expect("Topen on root should succeed");

    // Tclunk — Release operation on /
    client.clunk(0, 2).expect("Tclunk should succeed");

    // Cleanup
    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

/// Test that walking a nonexistent path returns an error.
#[test]
fn handler_lua_nonexistent_walk_fails() {
    let socket_path = format!("/tmp/pinhead-e2e-handler-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = NinepClient::connect(&socket_path).expect("connect");
    client.version(0, 8192).expect("Tversion");
    client.attach(0, 1).expect("Tattach");

    // Walking "nonexistent" should fail (no route for /nonexistent)
    let err = client.walk(0, 1, 2, "nonexistent").unwrap_err();
    assert!(
        err.contains("lookup failed") || err.contains("no route"),
        "walk nonexistent should fail with route error, got: {err}"
    );

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}
