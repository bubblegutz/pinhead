mod common;

use common::{spawn_pinhead, unique_sock_id, wait_for_socket, NinepClient};
use std::fs;
use std::process::Child;
use std::time::Duration;

fn build_script(socket_path: &str, db_path: &str) -> String {
    let content = include_str!("../examples/sql_demo.lua");
    content
        .replace("/tmp/pinhead-sql-demo.sock", socket_path)
        .replace("os.getenv(\"PINHEAD_SQL_DB\") or \"/tmp/pinhead-sql-demo.db\"", &format!("\"{db_path}\""))
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

fn run_sql_demo_test<F>(test_fn: F)
where
    F: FnOnce(&mut NinepClient),
{
    let sock_id = unique_sock_id();
    let socket_path = format!("/tmp/pinhead-e2e-sql-{:x}.sock", sock_id);
    let db_path = format!("/tmp/pinhead-e2e-sql-{:x}.db", sock_id);
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
fn sql_demo_products_route() {
    run_sql_demo_test(|client| {
        let text = read_file_content(client, "products");
        assert!(text.contains("Widget"), "should contain Widget, got: {text}");
        assert!(text.contains("Gadget"), "should contain Gadget, got: {text}");
        assert!(text.contains("Doohickey"), "should contain Doohickey, got: {text}");
        assert!(text.contains("$9.99"), "should contain $9.99, got: {text}");
    });
}

#[test]
fn sql_demo_products_json_route() {
    run_sql_demo_test(|client| {
        let text = read_file_content(client, "products/json");
        assert!(text.contains("Widget"), "should contain Widget, got: {text}");
        assert!(text.contains("9.99"), "should contain 9.99, got: {text}");
        // Verify JSON format
        assert!(text.starts_with("["), "should be a JSON array, got: {text}");
    });
}

#[test]
fn sql_demo_product_by_id() {
    run_sql_demo_test(|client| {
        let text = read_file_content(client, "product/3");
        assert!(text.contains("Doohickey"), "should contain Doohickey, got: {text}");
        assert!(text.contains("4.99"), "should contain 4.99, got: {text}");
    });
}

#[test]
fn sql_demo_product_not_found() {
    run_sql_demo_test(|client| {
        let text = read_file_content(client, "product/999");
        assert!(text.contains("not found"), "should say not found, got: {text}");
    });
}

#[test]
fn sql_demo_low_stock() {
    run_sql_demo_test(|client| {
        let text = read_file_content(client, "low-stock/60");
        // Thingamajig has stock=25, which is < 60; Gadget has stock=50 which is < 60
        assert!(text.contains("Thingamajig"), "should contain Thingamajig, got: {text}");
        assert!(text.contains("Gadget"), "should contain Gadget, got: {text}");
        // Widget has stock=100, which is NOT < 60
        assert!(!text.contains("Widget"), "should NOT contain Widget, got: {text}");
    });
}

#[test]
fn sql_demo_nonexistent_walk_fails() {
    let sock_id = unique_sock_id();
    let socket_path = format!("/tmp/pinhead-e2e-sql-{:x}.sock", sock_id);
    let db_path = format!("/tmp/pinhead-e2e-sql-{:x}.db", sock_id);
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
