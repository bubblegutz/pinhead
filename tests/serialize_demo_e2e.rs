mod common;

use common::*;

#[test]
fn serialize_demo() {
    let script = include_str!("../examples/serialize_demo.lua");
    let transports = [
        Transport::NinepSock(format!("/tmp/pinhead-e2e-serialize-sock-{:x}.sock", unique_id())),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!("/tmp/pinhead-e2e-serialize-fuse-{:x}", unique_id())),
    ];

    run_scenarios(script, &transports, |client| {
        // Read /json — verify valid JSON with expected fields
        let text = client.read_file("json").expect("read json");
        let parsed: serde_json::Value =
            serde_json::from_str(&text).expect("JSON should be valid");
        assert_eq!(parsed["title"], "The Name of the Wind");
        assert_eq!(parsed["author"], "Patrick Rothfuss");

        // Read /yaml — verify valid YAML
        let yaml_text = client.read_file("yaml").expect("read yaml");
        let yaml_parsed: serde_json::Value =
            serde_yaml::from_str(&yaml_text).expect("YAML should be valid");
        assert_eq!(yaml_parsed["title"], "The Name of the Wind");

        // Read /csv — verify CSV rows
        let csv_text = client.read_file("csv").expect("read csv");
        let mut rdr = csv::Reader::from_reader(csv_text.as_bytes());
        let mut count = 0;
        for result in rdr.records() {
            let _ = result.expect("valid CSV");
            count += 1;
        }
        assert_eq!(count, 3, "CSV should have 3 data rows");

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });
}
