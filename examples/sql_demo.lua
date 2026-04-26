-- sql_demo.lua — demonstrates the sql.* raw SQL API
--
-- Opens a database, registers routes that lazy-initialise tables
-- and seed data on first request (avoids blocking issues during setup).

local db_path = env.get("PINHEAD_SQL_DB") or "/tmp/pinhead-sql-demo.db"
local h = sql.open(db_path)

-- Lazy init: create tables and seed data on first request.
local initialized = false
local function ensure_initialized()
    if not initialized then
        sql.exec(h, "CREATE TABLE IF NOT EXISTS products (id INTEGER PRIMARY KEY, name TEXT, price REAL, stock INTEGER)")
        sql.exec(h, "INSERT OR IGNORE INTO products (id, name, price, stock) VALUES (1, 'Widget', 9.99, 100)")
        sql.exec(h, "INSERT OR IGNORE INTO products (id, name, price, stock) VALUES (2, 'Gadget', 24.99, 50)")
        sql.exec(h, "INSERT OR IGNORE INTO products (id, name, price, stock) VALUES (3, 'Doohickey', 4.99, 200)")
        sql.exec(h, "INSERT OR IGNORE INTO products (id, name, price, stock) VALUES (4, 'Thingamajig', 49.99, 25)")
        initialized = true
    end
end

-- /products — list all products
route.register("/products", {"lookup", "getattr", "read", "open", "release"}, function()
    ensure_initialized()
    local rows = sql.query(h, "SELECT * FROM products ORDER BY id")
    local out = {}
    for _, row in ipairs(rows) do
        local line = string.format("%d | %s | $%.2f | stock: %d", row.id, row.name, row.price, row.stock)
        table.insert(out, line)
    end
    return table.concat(out, "\n")
end)

-- /products/json — list all products as JSON
route.register("/products/json", {"lookup", "getattr", "read", "open", "release"}, function()
    ensure_initialized()
    local rows = sql.query(h, "SELECT * FROM products ORDER BY id")
    return json.enc_pretty(rows)
end)

-- /product/<id> — get a single product by id (returns JSON)
route.register("/product/{id}", {"lookup", "getattr", "read", "open", "release"}, function(params)
    ensure_initialized()
    local row = sql.row(h, "SELECT * FROM products WHERE id = ?1", tonumber(params.id))
    if row == nil then
        return json.enc({error = "not found"})
    end
    return json.enc_pretty(row)
end)

-- /low-stock — find products with stock < threshold
route.register("/low-stock/{max}", {"lookup", "getattr", "read", "open", "release"}, function(params)
    ensure_initialized()
    local rows = sql.query(h, "SELECT * FROM products WHERE stock < ?1 ORDER BY stock", tonumber(params.max))
    return json.enc_pretty(rows)
end)

local root_meta = {
    products      = "GET /products       → products table (text)",
    products_json = "GET /products/json  → products table (JSON)",
    product_by_id = "GET /product/3      → single product by ID (JSON)",
    low_stock     = "GET /low-stock/100  → products with low stock (JSON)",
}

route.register("/", {"lookup", "getattr", "readdir", "open", "release"}, function()
    return yaml.enc(root_meta)
end)

-- User credentials for SSH auth.
local users = {
    {"alice", "hunter2"},
    {"bob", "letmein"},
}
for _, pair in ipairs(users) do
    sshfs.userpasswd(pair[1], pair[2])
end

local listen_addr = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-sql-demo.sock"
ninep.listen(listen_addr)
local ssh_listen = env.get("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)

-- FUSE mount — activated via PINHEAD_FUSE_MOUNT env var for e2e tests.
local fuse_mount = env.get("PINHEAD_FUSE_MOUNT")
if fuse_mount then
    fuse.mount(fuse_mount)
end
