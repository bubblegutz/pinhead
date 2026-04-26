mod common;

use common::{spawn_pinhead, unique_sock_id, wait_for_socket, NinepClient};
use std::fs;
use std::process::Child;
use std::time::Duration;

fn build_script(socket_path: &str, db_path: &str) -> String {
    let content = include_str!("../examples/doc_demo.lua");
    let db_str = format!("\"{db_path}\"");
    content
        .replace("/tmp/pinhead-doc-demo.sock", socket_path)
        .replace("os.getenv(\"PINHEAD_DOC_DB\") or \"/tmp/pinhead-doc-demo.db\"", &db_str)
}

fn kill_pinhead(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn setup_client(socket_path: &str) -> NinepClient {
    let mut client =
        NinepClient::connect(socket_path).expect("connect to pinhead");
    client.version(0, 65536).expect("Tversion");
    client.attach(0, 1).expect("Tattach");
    client
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

fn run_doc_demo_test<F>(test_fn: F)
where
    F: FnOnce(&mut NinepClient),
{
    let sock_id = unique_sock_id();
    let socket_path = format!("/tmp/pinhead-e2e-doc-{:x}.sock", sock_id);
    let db_path = format!("/tmp/pinhead-e2e-doc-{:x}.db", sock_id);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
    let _ = fs::remove_file(&db_path);

    let script = build_script(&socket_path, &db_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = setup_client(&socket_path);
    test_fn(&mut client);

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
    let _ = fs::remove_file(&db_path);
}

#[test]
fn doc_demo_user_route() {
    run_doc_demo_test(|client| {
        let text = read_file_content(client, "user/alice");
        assert!(text.contains("Alice"), "should contain Alice, got: {text}");
        assert!(text.contains("30"), "should contain age 30, got: {text}");
    });
}

#[test]
fn doc_demo_all_route() {
    run_doc_demo_test(|client| {
        let text = read_file_content(client, "all");
        assert!(text.contains("Alice"), "should contain Alice, got: {text}");
        assert!(text.contains("Bob"), "should contain Bob, got: {text}");
        assert!(text.contains("Carol"), "should contain Carol, got: {text}");
    });
}

#[test]
fn doc_demo_count_route() {
    run_doc_demo_test(|client| {
        let text = read_file_content(client, "count");
        assert_eq!(text.trim(), "3", "should have 3 documents, got: {text}");
    });
}

#[test]
fn doc_demo_find_route() {
    run_doc_demo_test(|client| {
        let text = read_file_content(client, "find/engineer");
        assert!(text.contains("Alice"), "should find Alice, got: {text}");
        assert!(text.contains("engineer"), "should mention engineer, got: {text}");
    });
}

#[test]
fn doc_demo_nonexistent_user() {
    run_doc_demo_test(|client| {
        let text = read_file_content(client, "user/nobody");
        assert!(text.contains("not found"), "should say not found, got: {text}");
    });
}

#[test]
fn doc_demo_nonexistent_walk_fails() {
    let sock_id = unique_sock_id();
    let socket_path = format!("/tmp/pinhead-e2e-doc-{:x}.sock", sock_id);
    let db_path = format!("/tmp/pinhead-e2e-doc-{:x}.db", sock_id);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
    let _ = fs::remove_file(&db_path);

    let script = build_script(&socket_path, &db_path);
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
    let _ = fs::remove_file(&db_path);
}
