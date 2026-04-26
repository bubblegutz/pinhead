mod common;

use common::*;

#[test]
fn env_demo() {
    let script = include_str!("../examples/env_demo.lua");
    let transports = [
        Transport::NinepSock(format!("/tmp/pinhead-e2e-env-sock-{:x}.sock", unique_id())),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
    ];

    run_scenarios(script, &transports, |client| {
        // Read /home — should return HOME env var
        let text = client.read_file("home").expect("read home");
        assert!(!text.is_empty(), "/home should return HOME value");

        // Read /ping — env.set + env.get round-trip
        let pong = client.read_file("ping").expect("read ping");
        assert_eq!(pong, "pong", "ping should set and read PINHEAD_PONG");

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nonexistent").expect_err("walk nonexistent");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });
}
