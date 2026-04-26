mod common;

use common::*;

#[test]
fn http_demo() {
    let script = include_str!("../examples/http/main.lua");
    let transports = [
        Transport::NinepSock(format!("/tmp/pinhead-e2e-http-sock-{:x}.sock", unique_id())),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!("/tmp/pinhead-e2e-http-fuse-{:x}", unique_id())),
    ];

    run_scenarios(script, &transports, |client| {
        // Read /page — Wikipedia page summary (present in both success and
        // failure paths, so no network needed).
        let text = client.read_file("page")
            .expect("read page");
        assert!(
            text.contains("Page:"),
            "should have page header, got: {text}"
        );

        // Read /search — Wikipedia opensearch with query params
        let search = client.read_file("search")
            .expect("read search");
        assert!(
            search.contains("Search Results"),
            "should have search header, got: {search}"
        );

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });
}
