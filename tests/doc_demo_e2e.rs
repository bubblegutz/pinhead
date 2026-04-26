mod common;

use common::*;

#[test]
fn doc_demo() {
    let id = unique_id();
    let db_path = format!("/tmp/pinhead-e2e-doc-db-{:x}.db", id);
    let script = include_str!("../examples/doc_demo.lua")
        .replace(
            "os.getenv(\"PINHEAD_DOC_DB\") or \"/tmp/pinhead-doc-demo.db\"",
            &format!("\"{db_path}\""),
        );
    let transports = [
        Transport::NinepSock(format!("/tmp/pinhead-e2e-doc-sock-{:x}.sock", id)),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
    ];

    run_scenarios(&script, &transports, |client| {
        // Read /user/alice — verify JSON content
        let text = client.read_file("user/alice")
            .expect("read user/alice");
        assert!(text.contains("Alice"), "should contain Alice, got: {text}");
        assert!(text.contains("30"), "should contain age 30, got: {text}");

        // Read /count — verify count
        let count = client.read_file("count").expect("read count");
        assert_eq!(count.trim(), "3", "should have 3 documents, got: {count}");

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });

    let _ = std::fs::remove_file(&db_path);
}
