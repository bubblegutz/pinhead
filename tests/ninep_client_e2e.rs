mod common;

/// Test the internal 9P2000 client directly via the Rust API.
/// This validates the underlying implementation used by the Lua `ninep_client.*` table.

#[test]
fn ninep_client_basic_read() {
    let sock = format!("/tmp/pinhead-e2e-ninep-basic-{:x}.sock", common::unique_id());
    let _ = std::fs::remove_file(&sock);

    let script = format!(
        r#"
ninep.listen("sock:{sock}")
route.all("/hello", function(_, _) return "world" end)
"#
    );

    let transport = common::Transport::NinepSock(sock.clone());
    let _inst = common::PinheadInstance::start(&script, &transport).expect("start");

    let mut client = pinhead::frontend::ninep_client::NinepClient::connect(&format!("sock:{sock}"))
        .expect("connect");
    let result = client.read_file("hello").expect("read /hello");
    assert_eq!(result.trim(), "world", "got: {result:?}");
}

#[test]
fn ninep_client_stat() {
    let sock = format!("/tmp/pinhead-e2e-ninep-stat-{:x}.sock", common::unique_id());
    let _ = std::fs::remove_file(&sock);

    let script = format!(
        r#"
ninep.listen("sock:{sock}")
route.all("/hello", function(_, _) return "world" end)
"#
    );

    let transport = common::Transport::NinepSock(sock.clone());
    let _inst = common::PinheadInstance::start(&script, &transport).expect("start");

    let mut client = pinhead::frontend::ninep_client::NinepClient::connect(&format!("sock:{sock}"))
        .expect("connect");
    let stat_result = client.stat("hello").expect("stat /hello");
    assert!(stat_result.contains("qid path:"), "stat should have qid: {stat_result:?}");
    assert!(stat_result.contains("length:"), "stat should have length: {stat_result:?}");
}

#[test]
fn ninep_client_write() {
    let sock = format!("/tmp/pinhead-e2e-ninep-write-{:x}.sock", common::unique_id());
    let _ = std::fs::remove_file(&sock);

    let script = format!(
        r#"
ninep.listen("sock:{sock}")
route.all("/data", function(_, _) return "test data" end)
"#
    );

    let transport = common::Transport::NinepSock(sock.clone());
    let _inst = common::PinheadInstance::start(&script, &transport).expect("start");

    let mut client = pinhead::frontend::ninep_client::NinepClient::connect(&format!("sock:{sock}"))
        .expect("connect");
    // Write (server handles data; in this simple handler, write is ignored)
    let result = client.write_file("data", "new content").expect("write");
    // Write returns empty string on success
    assert_eq!(result, "", "write result: {result:?}");

    // Read back — handler always returns "test data" regardless of writes
    let readback = client.read_file("data").expect("read after write");
    assert_eq!(readback.trim(), "test data", "read after write: {readback:?}");
}
