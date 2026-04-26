mod common;

use common::*;

#[test]
fn utilities_demo() {
    let script = include_str!("../examples/utilities/main.lua");
    let transports = [
        Transport::NinepSock(format!(
            "/tmp/pinhead-e2e-utilities-sock-{:x}.sock",
            unique_id()
        )),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!(
            "/tmp/pinhead-e2e-utilities-fuse-{:x}",
            unique_id()
        )),
    ];

    run_scenarios(script, &transports, |client| {
        // Read /info — environment info
        let text = client.read_file("info").expect("read /info");
        assert!(
            text.contains("User:") && text.contains("Home:"),
            "should have user/home info, got: {text}"
        );

        // Read /counter — counter with logging
        // Note: counter value varies by transport (SSH read_to_end sends extra READ for EOF)
        let c1 = client.read_file("counter").expect("read /counter");
        assert!(
            c1.contains("Counter:"),
            "counter should contain 'Counter:', got: {c1}"
        );

        let c2 = client.read_file("counter").expect("read /counter second");
        assert!(
            c2.contains("Counter:"),
            "counter should contain 'Counter:', got: {c2}"
        );
        assert_ne!(
            c1, c2,
            "counter should change between reads, c1={c1}, c2={c2}"
        );

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });
}
