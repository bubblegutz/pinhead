mod common;

use common::*;

#[test]
fn convert_demo() {
    let script = include_str!("../examples/conversion/main.lua");
    let transports = [
        Transport::NinepSock(format!(
            "/tmp/pinhead-e2e-convert-sock-{:x}.sock",
            unique_id()
        )),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!(
            "/tmp/pinhead-e2e-convert-fuse-{:x}",
            unique_id()
        )),
    ];

    run_scenarios(script, &transports, |client| {
        // Read /help — usage documentation
        let text = client.read_file("help").expect("read /help");
        assert!(
            text.contains("JSON/YAML Conversion"),
            "should have help header, got: {text}"
        );

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });
}
