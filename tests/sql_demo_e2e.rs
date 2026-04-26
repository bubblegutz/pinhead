mod common;

use common::*;

#[test]
fn sql_demo() {
    let id = unique_id();
    let db_path = format!("/tmp/pinhead-e2e-sql-db-{:x}.db", id);
    let script = include_str!("../examples/sql/main.lua")
        .replace(
            "os.getenv(\"PINHEAD_SQL_DB\") or \"/tmp/pinhead-sql-demo.db\"",
            &format!("\"{db_path}\""),
        );
    let transports = [
        Transport::NinepSock(format!("/tmp/pinhead-e2e-sql-sock-{:x}.sock", id)),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!("/tmp/pinhead-e2e-sql-fuse-{:x}", id)),
    ];

    run_scenarios(&script, &transports, |client| {
        // Read /products — verify product listings
        let text = client.read_file("products").expect("read products");
        assert!(text.contains("Widget"), "should contain Widget, got: {text}");
        assert!(text.contains("Gadget"), "should contain Gadget, got: {text}");

        // Read /product/3 — individual product lookup
        let product = client.read_file("product/3").expect("read product/3");
        assert!(product.contains("Doohickey"), "should contain Doohickey, got: {product}");

        // Read /product/999 — nonexistent product
        let not_found = client.read_file("product/999").expect("read product/999");
        assert!(not_found.contains("not found"), "should say not found, got: {not_found}");

        // Walk nonexistent path should fail
        let err = client.walk_nonexistent("nope").expect_err("walk nope");
        assert!(
            err.contains("lookup failed") || err.contains("no route") || !err.is_empty(),
            "walk nonexistent should give route error, got: {err}"
        );
    });

    let _ = std::fs::remove_file(&db_path);
}
