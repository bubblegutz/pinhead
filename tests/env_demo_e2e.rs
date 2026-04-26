mod common;

use common::{read_data, spawn_pinhead, unique_sock_id, wait_for_socket, NinepClient};
use std::fs;
use std::process::Child;
use std::time::Duration;

fn build_script(socket_path: &str) -> String {
    let content = include_str!("../examples/env_demo.lua");
    content.replace("/tmp/pinhead-env-demo.sock", socket_path)
}

fn kill_pinhead(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Helper: perform version + attach + single-element walk (fid 2).
fn setup_walk(
    client: &mut NinepClient,
    path: &str,
) -> Result<Vec<u8>, String> {
    client.version(0, 8192)?;
    client.attach(0, 1)?;
    client.walk(0, 1, 2, path)
}

/// Helper: walk → open → read → return decoded response text.
fn read_route_content(
    client: &mut NinepClient,
    path: &str,
) -> Result<String, String> {
    client.version(0, 8192)?;
    client.attach(0, 1)?;
    client.walk(0, 1, 2, path)?;
    client.open(0, 2)?;
    let resp = client.read(0, 2, 0, 4096)?;
    let content = read_data(&resp);
    Ok(String::from_utf8_lossy(content).to_string())
}

/// Test env.get("HOME") — reads an inherited environment variable.
#[test]
fn env_demo_home_route() {
    let socket_path = format!("/tmp/pinhead-e2e-env-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = NinepClient::connect(&socket_path).expect("connect");

    let text = read_route_content(&mut client, "home")
        .expect("walk to /home should succeed");
    assert!(
        !text.is_empty(),
        "/home should return HOME env var value"
    );

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

/// Test env.set + env.get in the same handler — /ping sets PINHEAD_PONG
/// and reads it back.
#[test]
fn env_demo_ping_route() {
    let socket_path = format!("/tmp/pinhead-e2e-env-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = NinepClient::connect(&socket_path).expect("connect");

    let text = read_route_content(&mut client, "ping")
        .expect("walk to /ping should succeed");
    assert_eq!(text, "pong", "ping route should set and read PINHEAD_PONG");

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

/// Test env.unset — /nuke sets PINHEAD_NUKE, unsets it, then reads nil.
#[test]
fn env_demo_nuke_route() {
    let socket_path = format!("/tmp/pinhead-e2e-env-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = NinepClient::connect(&socket_path).expect("connect");

    let text = read_route_content(&mut client, "nuke")
        .expect("walk to /nuke should succeed");
    assert_eq!(
        text, "was-nil",
        "nuke route should unset PINHEAD_NUKE and return 'was-nil'"
    );

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

/// Test that a walk to a nonexistent path fails.
#[test]
fn env_demo_nonexistent_walk_fails() {
    let socket_path = format!("/tmp/pinhead-e2e-env-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = NinepClient::connect(&socket_path).expect("connect");

    let err = setup_walk(&mut client, "nonexistent").unwrap_err();
    assert!(
        err.contains("lookup failed") || err.contains("no route"),
        "walk nonexistent should fail with route error, got: {err}"
    );

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}
