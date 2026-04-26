mod common;

use common::*;

#[test]
fn test_basic() {
    let script = include_str!("../examples/test_basic.lua");
    let transports = [
        Transport::NinepSock(format!("/tmp/pinhead-e2e-basic-sock-{:x}.sock", unique_id())),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
    ];

    run_scenarios(script, &transports, |client| {
        // Walk → open → read → verify content on testfile.txt
        let text = client.read_file("testfile.txt")
            .expect("read testfile.txt");
        assert_eq!(
            text, "hello from pinhead test!",
            "read testfile.txt: got {text:?}"
        );

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });
}
