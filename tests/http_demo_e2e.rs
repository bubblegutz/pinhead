mod common;

use common::{spawn_pinhead, unique_sock_id, wait_for_socket, NinepClient};
use std::fs;
use std::process::Child;
use std::time::Duration;

fn build_script(socket_path: &str) -> String {
    let content = include_str!("../examples/http_demo.lua");
    content.replace("/tmp/pinhead-http-demo.sock", socket_path)
}

fn kill_pinhead(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_file_content(client: &mut NinepClient, path: &str) -> String {
    let _resp = client
        .walk(0, 1, 2, path)
        .unwrap_or_else(|e| panic!("Twalk to {path}: {e}"));

    client.open(0, 2).unwrap_or_else(|e| panic!("Topen {path}: {e}"));

    let resp = client
        .read(0, 2, 0, 65536)
        .unwrap_or_else(|e| panic!("Tread {path}: {e}"));

    let content = common::read_data(&resp);
    let text = String::from_utf8_lossy(content).to_string();

    client.clunk(0, 2).unwrap_or_else(|e| panic!("Tclunk {path}: {e}"));

    text
}

fn setup_client(socket_path: &str) -> NinepClient {
    let mut client =
        NinepClient::connect(socket_path).expect("connect to pinhead");
    client.version(0, 65536).expect("Tversion");
    client.attach(0, 1).expect("Tattach");
    client
}

/// Test that /post fetches and formats a blog post from JSONPlaceholder.
/// The Lua script wraps req.get in pcall, so even without network access
/// we get a graceful error message instead of a crash.
#[test]
fn http_demo_post_route() {
    let socket_path = format!("/tmp/pinhead-e2e-http-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = setup_client(&socket_path);
    let text = read_file_content(&mut client, "post");

    if text.starts_with("HTTP Error:") {
        eprintln!("Note: /post route returned HTTP error (no network?): {text}");
    } else {
        assert!(text.contains("Post #1"), "should mention post #1, got: {text}");
        assert!(text.contains("Title:"), "should have title field, got: {text}");
        assert!(text.contains("======"), "should have separator line");
    }

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

/// Test that /http_params returns HTTP diagnostics including sent headers
/// and query parameters. The Lua script formats these identically in both
/// success and failure paths, so these assertions work without network.
#[test]
fn http_demo_params_route() {
    let socket_path = format!("/tmp/pinhead-e2e-http-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = setup_client(&socket_path);
    let text = read_file_content(&mut client, "http_params");

    // Present in both success and failure paths
    assert!(
        text.contains("HTTP Request Diagnostics"),
        "should have diagnostics header, got: {text}"
    );
    assert!(
        text.contains("X-Demo: pinhead"),
        "should mention X-Demo header, got: {text}"
    );
    assert!(
        text.contains("Accept: application/json"),
        "should mention Accept header, got: {text}"
    );
    assert!(
        text.contains("name: Alice") || text.contains("name = Alice"),
        "should mention name param, got: {text}"
    );
    assert!(
        text.contains("role: engineer") || text.contains("role = engineer"),
        "should mention role param, got: {text}"
    );

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

/// Test that walking a nonexistent path fails correctly.
#[test]
fn http_demo_nonexistent_walk_fails() {
    let socket_path = format!("/tmp/pinhead-e2e-http-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = setup_client(&socket_path);
    let err = client.walk(0, 1, 2, "nope").expect_err("walk should fail");
    assert!(
        err.contains("lookup failed") || err.contains("no route"),
        "walk nonexistent should fail with route error, got: {err}"
    );

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}
