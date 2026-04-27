/// Test that blocking_send/blocking_recv work from within the tokio runtime.
#[tokio::test(flavor = "current_thread")]
async fn test_blocking_send_recv_in_runtime() {
    // Create a Lua VM with store APIs.
    let lua = mlua::Lua::new();
    let (_doc_reg, _sql_reg) = pinhead::store::register_lua_apis(&lua).unwrap();

    let script = r#"
        local h = doc.open("/tmp/test_blocking.db")
        doc.set(h, "k1", {x = 1})
        local n = doc.count(h)
        assert(n == 1, "count should be 1, got " .. tostring(n))

        local val = doc.get(h, "k1")
        assert(val ~= nil, "val should not be nil")
        assert(val.x == 1, "val.x should be 1")

        local all = doc.all(h)
        assert(#all == 1, "all count should be 1")

        doc.close(h)
    "#;

    match lua.load(script).exec() {
        Ok(()) => eprintln!("SUCCESS"),
        Err(e) => panic!("LUA ERROR: {e}"),
    }

    let _ = std::fs::remove_file("/tmp/test_blocking.db");
}
