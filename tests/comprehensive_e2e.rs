mod common;

use common::*;

#[test]
fn comprehensive() {
    let script = include_str!("../examples/comprehensive/main.lua");
    // FUSE excluded: many intermediate path components (e.g. `inventory`,
    // `serialize`, `users`) that aren't real directories would cause ENOTDIR.
    let transports = [
        Transport::NinepSock(format!(
            "/tmp/pinhead-e2e-comprehensive-sock-{:x}.sock",
            unique_id()
        )),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
    ];

    run_scenarios(script, &transports, |client| {
        // ── /info — environment variables ──
        let text = client.read_file("info").expect("read /info");
        assert!(
            text.contains("User:"),
            "/info should contain 'User:', got: {text:?}"
        );
        assert!(
            text.contains("Home:"),
            "/info should contain 'Home:', got: {text:?}"
        );

        // ── /counter — stateful counter with logging ──
        let first = client.read_file("counter").expect("read /counter (1st)");
        let second = client.read_file("counter").expect("read /counter (2nd)");
        // Counter is shared across all ops (lookup/open/read each trigger the
        // handler), so we verify statefulness by checking values differ.
        assert!(
            first.contains("Counter:"),
            "counter should return a value, got: {first:?}"
        );
        assert!(
            second.contains("Counter:"),
            "counter should return a value, got: {second:?}"
        );
        assert_ne!(first, second, "consecutive counter reads must differ");

        // ── /users/alice — doc.get by path param ──
        let text = client.read_file("users/alice").expect("read /users/alice");
        assert!(
            text.contains("Alice"),
            "/users/alice should contain Alice, got: {text:?}"
        );
        assert!(
            text.contains("engineer"),
            "/users/alice should contain engineer, got: {text:?}"
        );

        // ── /users/nonexistent — doc.get unknown key ──
        let text = client
            .read_file("users/nonexistent")
            .expect("read /users/nonexistent");
        assert!(
            text.contains("not found"),
            "/users/nonexistent should return not-found, got: {text:?}"
        );

        // ── /users/find/engineer — doc.find by JSON path ──
        let text = client
            .read_file("users/find/engineer")
            .expect("read /users/find/engineer");
        assert!(
            text.contains("Alice"),
            "/users/find/engineer should contain Alice, got: {text:?}"
        );
        assert!(
            text.contains("Carol"),
            "/users/find/engineer should contain Carol, got: {text:?}"
        );

        // ── /inventory/products — sql.query all ──
        let text = client
            .read_file("inventory/products")
            .expect("read /inventory/products");
        assert!(
            text.contains("Widget"),
            "/inventory/products should contain Widget, got: {text:?}"
        );
        assert!(
            text.contains("Doohickey"),
            "/inventory/products should contain Doohickey, got: {text:?}"
        );

        // ── /inventory/product/1 — sql.row single ──
        let text = client
            .read_file("inventory/product/1")
            .expect("read /inventory/product/1");
        assert!(
            text.contains("Widget"),
            "/inventory/product/1 should contain Widget, got: {text:?}"
        );
        assert!(
            text.contains("9.99"),
            "/inventory/product/1 should contain $9.99, got: {text:?}"
        );

        // ── /inventory/low-stock/100 — sql.query with param ──
        let text = client
            .read_file("inventory/low-stock/100")
            .expect("read /inventory/low-stock/100");
        assert!(
            text.contains("Gadget"),
            "/inventory/low-stock/100 should contain Gadget (stock=50), got: {text:?}"
        );

        // ── /serialize/json — json.enc ──
        let text = client
            .read_file("serialize/json")
            .expect("read /serialize/json");
        assert!(
            text.contains("Patrick Rothfuss"),
            "/serialize/json should contain author, got: {text:?}"
        );
        assert!(
            text.contains("fantasy"),
            "/serialize/json should contain 'fantasy', got: {text:?}"
        );

        // ── /serialize/yaml — yaml.enc ──
        let text = client
            .read_file("serialize/yaml")
            .expect("read /serialize/yaml");
        assert!(
            text.contains("The Name of the Wind"),
            "/serialize/yaml should contain title, got: {text:?}"
        );

        // ── /serialize/toml — toml.enc ──
        let text = client
            .read_file("serialize/toml")
            .expect("read /serialize/toml");
        assert!(
            text.contains("Patrick Rothfuss") || text.contains("2007"),
            "/serialize/toml should contain book data, got: {text:?}"
        );

        // ── /serialize/csv — csv.enc ──
        let text = client
            .read_file("serialize/csv")
            .expect("read /serialize/csv");
        assert!(
            text.contains("Alice") && text.contains("Bob") && text.contains("Carol"),
            "/serialize/csv should contain all people, got: {text:?}"
        );

        // ── /serialize/roundtrip — json.enc → json.dec → json.enc_pretty ──
        let text = client
            .read_file("serialize/roundtrip")
            .expect("read /serialize/roundtrip");
        // Pretty-printed JSON has newlines and indentation
        assert!(
            text.contains("\n"),
            "/serialize/roundtrip should be pretty-printed (newlines), got: {text:?}"
        );
        assert!(
            text.contains("Patrick Rothfuss"),
            "/serialize/roundtrip should contain author, got: {text:?}"
        );

        // ── /serialize/jq — json.jq filter ──
        let text = client
            .read_file("serialize/jq")
            .expect("read /serialize/jq");
        assert!(
            text.contains("4.5"),
            "/serialize/jq should contain rating 4.5, got: {text:?}"
        );

        // ── /wiki/page — req.get with decode=json ──
        let text = client.read_file("wiki/page").expect("read /wiki/page");
        assert!(
            text.contains("Title:") || text.contains("HTTP Error:"),
            "/wiki/page should show Title or HTTP Error, got: {text:?}"
        );

        // ── /wiki/search — req.get with headers + query ──
        let text = client
            .read_file("wiki/search")
            .expect("read /wiki/search");
        assert!(
            text.contains("Search Results")
                || text.contains("Search Error")
                || text.contains("Error"),
            "/wiki/search should show results or error, got: {text:?}"
        );

        // ── Unmatched path should return route error ──
        let err = client
            .walk_nonexistent("nope")
            .expect_err("walk nonexistent");
        assert!(
            err.contains("no route") || err.contains("lookup failed") || !err.is_empty(),
            "walk nonexistent should give an error, got: {err:?}"
        );
    });
}
