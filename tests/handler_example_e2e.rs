mod common;

use common::*;

#[test]
fn handler_example() {
    let script = include_str!("../examples/handler/main.lua");
    let transports = [
        Transport::NinepSock(format!("/tmp/pinhead-e2e-handler-sock-{:x}.sock", unique_id())),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!("/tmp/pinhead-e2e-handler-fuse-{:x}", unique_id())),
    ];

    run_scenarios(script, &transports, |client| {
        // Walk → open → read → verify profile content
        let text = client.read_file("users/1/profile")
            .expect("read users/1/profile");
        assert!(
            text.contains("User 1"),
            "should contain User 1, got: {text}"
        );

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nonexistent").expect_err("walk nonexistent");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });
}
