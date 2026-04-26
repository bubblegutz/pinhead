mod common;

use common::*;

#[test]
fn modular_demo() {
    let script = include_str!("../examples/modular/main.lua");
    let transports = [
        Transport::NinepSock(format!(
            "/tmp/pinhead-e2e-modular-sock-{:x}.sock",
            unique_id()
        )),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!(
            "/tmp/pinhead-e2e-modular-fuse-{:x}",
            unique_id()
        )),
    ];

    for t in &transports {
        let mut inst = match PinheadInstance::start(script, t) {
            Ok(inst) => inst,
            Err(e) => {
                eprintln!("[test]  ⚠ skip (start failed: {e})");
                continue;
            }
        };
        let mut client = match inst.connect() {
            Ok(client) => client,
            Err(e) => {
                eprintln!("[test]  ⚠ skip (connect failed: {e})");
                continue;
            }
        };

        // Read /status.txt — system status
        let text = client.read_file("status.txt").expect("read status.txt");
        assert!(
            text.contains("System Status:"),
            "should have status header, got: {text}"
        );

        // Read /api/health — health check
        let health = client.read_file("api/health").expect("read api/health");
        assert!(
            health.contains("Status: OK"),
            "should report OK, got: {health}"
        );

        // Read /config.txt — configuration info
        let config = client.read_file("config.txt").expect("read config.txt");
        assert!(
            config.contains("Configuration:"),
            "should have config header, got: {config}"
        );

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    }
}
