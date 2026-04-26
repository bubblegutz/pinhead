mod common;

use common::*;

#[test]
fn fs_ops() {
    let script = include_str!("../examples/fs_ops.lua");
    let transports = [
        Transport::NinepSock(format!("/tmp/pinhead-e2e-fs-ops-sock-{:x}.sock", unique_id())),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!("/tmp/pinhead-e2e-fs-ops-fuse-{:x}", unique_id())),
    ];

    run_scenarios(script, &transports, |client| {
        // Read results and verify every operation passed.
        let text = client.read_file("results").unwrap_or_else(|e| {
            panic!("read /results: {e}")
        });

        // First line should be the summary: "N passed, 0 failed"
        let lines: Vec<&str> = text.lines().collect();
        assert!(!lines.is_empty(), "results should have at least a summary line");

        let summary = lines[0];

        // No FAIL lines should appear
        let failures: Vec<String> = lines.iter().filter(|l| l.starts_with("FAIL")).map(|s| s.to_string()).collect();
        if !failures.is_empty() {
            panic!("fs_ops had failures:\n  {}", failures.join("\n  "));
        }

        assert!(
            summary.contains("passed") && summary.contains("0 failed"),
            "expected all tests to pass, got summary: {summary}"
        );

        // Verify specific operations were tested by checking PASS lines
        let pass_count = lines.iter().filter(|l| l.starts_with("PASS")).count();
        assert!(
            pass_count > 0,
            "expected at least one PASS, got: {text}"
        );
    });
}
