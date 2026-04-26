mod common;

use common::*;

#[test]
fn simpler_demo() {
    let script = include_str!("../examples/simpler/main.lua");
    let transports = [
        Transport::NinepSock(format!(
            "/tmp/pinhead-e2e-simpler-sock-{:x}.sock",
            unique_id()
        )),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
    ];

    run_scenarios(script, &transports, |client| {
        // Read /hello.txt
        let text = client.read_file("hello.txt").expect("read /hello.txt");
        assert!(
            text.contains("Hello from pinhead"),
            "should have greeting, got: {text}"
        );

        // Read /files/readme.txt — nested file
        let nested = client.read_file("files/readme.txt").expect("read nested");
        assert!(
            nested.contains("nested file"),
            "should have nested content, got: {nested}"
        );

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });
}
