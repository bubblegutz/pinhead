mod common;

use common::read_data;
use common::{spawn_pinhead, unique_sock_id, wait_for_socket, NinepClient};
use std::fs;
use std::process::Child;
use std::time::Duration;

fn build_script(socket_path: &str) -> String {
    let content = include_str!("../examples/test_basic.lua");
    content.replace("/tmp/pinhead-test-basic.sock", socket_path)
}

fn kill_pinhead(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Test the full 9P lifecycle on a file route: walk → open → read → clunk.
///
/// test_basic.lua registers `/testfile.txt` with handlers for
/// lookup, getattr, open, release, and read.
#[test]
fn test_basic_file_read() {
    let socket_path = format!("/tmp/pinhead-e2e-file-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = NinepClient::connect(&socket_path).expect("connect");

    // Tversion
    client.version(0, 8192).expect("Tversion");

    // Tattach — get root fid (1)
    client.attach(0, 1).expect("Tattach");

    // Twalk from root to testfile.txt (fid 2)
    let resp = client
        .walk(0, 1, 2, "testfile.txt")
        .expect("Twalk to testfile.txt should succeed");
    assert!(resp.len() >= 2, "Rwalk should have nwqid");
    let nwqid = u16::from_le_bytes(resp[0..2].try_into().unwrap());
    assert_eq!(nwqid, 1, "walk of 1 element should return 1 qid");

    // Topen on testfile.txt
    client.open(0, 2).expect("Topen on testfile.txt");

    // Tread on testfile.txt — should return the handler's response raw
    let resp = client
        .read(0, 2, 0, 4096)
        .expect("Tread on testfile.txt");
    let content = read_data(&resp);
    let text = String::from_utf8_lossy(content);
    assert_eq!(
        text, "hello from pinhead test!",
        "Read on /testfile.txt should return the handler's text"
    );

    // Tclunk
    client.clunk(0, 2).expect("Tclunk");

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

/// Test that walking a nonexistent path fails correctly.
#[test]
fn test_basic_nonexistent_walk_fails() {
    let socket_path = format!("/tmp/pinhead-e2e-file-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = NinepClient::connect(&socket_path).expect("connect");
    client.version(0, 8192).expect("Tversion");
    client.attach(0, 1).expect("Tattach");

    let err = client.walk(0, 1, 2, "nope").unwrap_err();
    assert!(
        err.contains("lookup failed") || err.contains("no route"),
        "walk nonexistent should fail with route error, got: {err}"
    );

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}
