mod common;

use common::{spawn_pinhead, unique_sock_id, wait_for_socket, NinepClient};
use std::collections::HashMap;
use std::fs;
use std::process::Child;
use std::time::Duration;

fn build_script(socket_path: &str) -> String {
    let content = include_str!("../examples/serialize_demo.lua");
    content.replace("/tmp/pinhead-serialize.sock", socket_path)
}

fn kill_pinhead(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_file_content(client: &mut NinepClient, path: &str) -> String {
    // Walk from root (fid 1) to the file (fid 2)
    let _resp = client
        .walk(0, 1, 2, path)
        .unwrap_or_else(|e| panic!("Twalk to {path}: {e}"));

    // Open fid 2
    client.open(0, 2).unwrap_or_else(|e| panic!("Topen {path}: {e}"));

    // Read
    let resp = client
        .read(0, 2, 0, 65536)
        .unwrap_or_else(|e| panic!("Tread {path}: {e}"));

    let content = common::read_data(&resp);
    let text = String::from_utf8_lossy(content).to_string();

    // Clunk
    client.clunk(0, 2).unwrap_or_else(|e| panic!("Tclunk {path}: {e}"));

    text
}

/// Connect, version, attach — returns a ready client.
fn setup_client(socket_path: &str) -> NinepClient {
    let mut client =
        NinepClient::connect(socket_path).expect("connect to pinhead");
    client.version(0, 65536).expect("Tversion");
    client.attach(0, 1).expect("Tattach");
    client
}

#[test]
fn serialize_json_roundtrip() {
    let socket_path = format!("/tmp/pinhead-e2e-serialize-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = setup_client(&socket_path);
    let text = read_file_content(&mut client, "json");

    // Parse as JSON and verify fields
    let parsed: serde_json::Value =
        serde_json::from_str(&text).expect("JSON should be valid");
    assert_eq!(parsed["title"], "The Name of the Wind");
    assert_eq!(parsed["author"], "Patrick Rothfuss");
    assert_eq!(parsed["year"], 2007);
    assert_eq!(parsed["tags"][0], "fantasy");

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

#[test]
fn serialize_yaml_roundtrip() {
    let socket_path = format!("/tmp/pinhead-e2e-serialize-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = setup_client(&socket_path);
    let text = read_file_content(&mut client, "yaml");

    // Parse as YAML and verify fields
    let parsed: serde_json::Value =
        serde_yaml::from_str(&text).expect("YAML should be valid");
    assert_eq!(parsed["title"], "The Name of the Wind");
    assert_eq!(parsed["author"], "Patrick Rothfuss");
    assert_eq!(parsed["year"], 2007);

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

#[test]
fn serialize_toml_roundtrip() {
    let socket_path = format!("/tmp/pinhead-e2e-serialize-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = setup_client(&socket_path);
    let text = read_file_content(&mut client, "toml");

    // Parse as TOML and verify fields
    let parsed: serde_json::Value =
        toml::from_str(&text).expect("TOML should be valid");
    assert_eq!(parsed["title"], "The Name of the Wind");
    assert_eq!(parsed["author"], "Patrick Rothfuss");
    assert_eq!(parsed["year"], 2007);

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

#[test]
fn serialize_csv_roundtrip() {
    let socket_path = format!("/tmp/pinhead-e2e-serialize-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = setup_client(&socket_path);
    let text = read_file_content(&mut client, "csv");

    // Parse CSV and verify rows
    let mut rdr = csv::Reader::from_reader(text.as_bytes());
    let mut rows: Vec<HashMap<String, String>> = Vec::new();
    for result in rdr.records() {
        let rec = result.expect("valid CSV record");
        let mut map = HashMap::new();
        // Columns are sorted alphabetically: age (0), name (1), role (2)
        map.insert("age".into(), rec.get(0).unwrap_or("").into());
        map.insert("name".into(), rec.get(1).unwrap_or("").into());
        map.insert("role".into(), rec.get(2).unwrap_or("").into());
        rows.push(map);
    }

    assert_eq!(rows.len(), 3, "CSV should have 3 data rows");
    assert_eq!(rows[0]["name"], "Alice");
    assert_eq!(rows[0]["age"], "30");
    assert_eq!(rows[0]["role"], "engineer");
    assert_eq!(rows[1]["name"], "Bob");
    assert_eq!(rows[1]["age"], "25");
    assert_eq!(rows[1]["role"], "designer");
    assert_eq!(rows[2]["name"], "Carol");
    assert_eq!(rows[2]["age"], "35");
    assert_eq!(rows[2]["role"], "manager");

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

#[test]
fn serialize_json_roundtrip_fidelity() {
    let socket_path = format!("/tmp/pinhead-e2e-serialize-{:x}.sock", unique_sock_id());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));

    let script = build_script(&socket_path);
    let mut child = spawn_pinhead(&script, &socket_path).expect("spawn pinhead");
    wait_for_socket(&socket_path).expect("socket should appear");
    std::thread::sleep(Duration::from_millis(200));

    let mut client = setup_client(&socket_path);
    let text = read_file_content(&mut client, "roundtrip");

    // Verify the round-trip produces valid pretty JSON with correct data
    let parsed: serde_json::Value =
        serde_json::from_str(&text).expect("roundtrip JSON should be valid");
    assert_eq!(parsed["title"], "The Name of the Wind");
    assert_eq!(parsed["author"], "Patrick Rothfuss");
    assert_eq!(parsed["year"], 2007);
    assert_eq!(parsed["tags"][0], "fantasy");
    assert_eq!(parsed["tags"][1], "fiction");

    // Verify pretty-printing (has newlines)
    assert!(text.contains('\n'), "pretty JSON should have newlines");

    kill_pinhead(&mut child);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(format!("{socket_path}.lua"));
}

#[test]
fn serialize_nonexistent_walk_fails() {
    let socket_path = format!("/tmp/pinhead-e2e-serialize-{:x}.sock", unique_sock_id());
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
