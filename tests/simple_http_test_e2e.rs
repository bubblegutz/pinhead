mod common;

use common::*;

#[test]
fn simple_http_test() {
    let script = include_str!("../examples/simple-http/main.lua");
    let transports = [
        Transport::NinepSock(format!(
            "/tmp/pinhead-e2e-simple-http-sock-{:x}.sock",
            unique_id()
        )),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!(
            "/tmp/pinhead-e2e-simple-http-fuse-{:x}",
            unique_id()
        )),
    ];

    run_scenarios(script, &transports, |client| {
        // Read /data — makes HTTP request to example.com
        let text = client.read_file("data").expect("read /data");
        assert!(
            text.contains("bytes"),
            "should report byte count, got: {text}"
        );

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });
}
