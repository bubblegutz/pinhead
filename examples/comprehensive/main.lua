-- pinhead comprehensive example
--
-- Demonstrates ALL API surfaces in a single virtual filesystem:
--   env, log, json, yaml, toml, csv, doc, sql, req, route bundles, bundles
--   with .default(), route.default fallback, fs (via os-backed data), SSH
--
-- Route map:
--   /                          readdir → listing
--   /info                      env vars + log.print
--   /counter                   stateful counter + log.debug
--   /users/{name}              doc.get by key
--   /users/find/{role}         doc.find by $.role
--   /inventory/products        sql.query all products
--   /inventory/product/{id}    sql.row single product
--   /inventory/low-stock/{max} sql.query with parameterized WHERE
--   /serialize/json            json.enc
--   /serialize/yaml            yaml.enc
--   /serialize/toml            toml.enc
--   /serialize/csv             csv.enc
--   /serialize/roundtrip       json.enc → json.dec → json.enc_pretty
--   /serialize/jq              json.jq filter
--   /wiki/page                 req.get with decode, pcall
--   /wiki/search               req.get with headers+query+decode
--   /*                         route.default fallback

-- ── Database paths (env-var configurable for test isolation) ────────────────
local doc_db_path = env.get("PINHEAD_DOC_DB") or "/tmp/pinhead-comp-doc.db"
local sql_db_path = env.get("PINHEAD_SQL_DB") or "/tmp/pinhead-comp-sql.db"

-- ── Lazy doc store init ───────────────────────────────────────────────────
local doc_h = doc.open(doc_db_path)
local doc_seeded = false
local function ensure_doc_seeded()
    if not doc_seeded then
        doc.set(doc_h, "alice", {name = "Alice", age = 30, role = "engineer"})
        doc.set(doc_h, "bob",   {name = "Bob",   age = 25, role = "designer"})
        doc.set(doc_h, "carol", {name = "Carol", age = 35, role = "engineer"})
        doc_seeded = true
    end
end

-- ── Lazy SQL init ─────────────────────────────────────────────────────────
local sql_h = sql.open(sql_db_path)
local sql_seeded = false
local function ensure_sql_seeded()
    if not sql_seeded then
        sql.exec(sql_h, "CREATE TABLE IF NOT EXISTS products (id INTEGER PRIMARY KEY, name TEXT, price REAL, stock INTEGER)")
        sql.exec(sql_h, "INSERT OR IGNORE INTO products (id, name, price, stock) VALUES (1, 'Widget', 9.99, 100)")
        sql.exec(sql_h, "INSERT OR IGNORE INTO products (id, name, price, stock) VALUES (2, 'Gadget', 24.99, 50)")
        sql.exec(sql_h, "INSERT OR IGNORE INTO products (id, name, price, stock) VALUES (3, 'Doohickey', 4.99, 200)")
        sql_seeded = true
    end
end

-- ── Shared serialization data ──────────────────────────────────────────────
local book = {
    title = "The Name of the Wind",
    author = "Patrick Rothfuss",
    year = 2007,
    rating = 4.5,
    tags = {"fantasy", "fiction"},
}

local people = {
    {name = "Alice", age = 30, role = "engineer"},
    {name = "Bob",   age = 25, role = "designer"},
    {name = "Carol", age = 35, role = "manager"},
}

-- ── Stateful counter ──────────────────────────────────────────────────────
local counter = 0

-- ── Routes ─────────────────────────────────────────────────────────────────

-- Root readdir
route.readdir("/", function()
    local entries = {
        "info", "counter", "users", "inventory",
        "serialize", "wiki",
    }
    return json.enc(entries)
end)

-- /info — environment variables with logging
route.read("/info", function()
    log.print("Info route accessed")
    local info = {}
    table.insert(info, "User: " .. (env.get("USER") or "unknown"))
    table.insert(info, "Home: " .. (env.get("HOME") or "unknown"))
    table.insert(info, "Shell: " .. (env.get("SHELL") or "unknown"))
    table.insert(info, "Lang: " .. (env.get("LANG") or "unknown"))
    return table.concat(info, "\n")
end)

-- /counter — stateful counter with debug logging
route.read("/counter", function()
    counter = counter + 1
    log.debug("Counter value: " .. counter)
    return "Counter: " .. counter
end)

-- /users/{name} — doc.get by path param
route.read("/users/{name}", function(params)
    ensure_doc_seeded()
    local val = doc.get(doc_h, params.name)
    if val == nil then
        return json.enc({error = "not found"})
    end
    return json.enc_pretty(val)
end)

-- /users/find/{role} — doc.find by json_extract on $.role
route.read("/users/find/{role}", function(params)
    ensure_doc_seeded()
    local results = doc.find(doc_h, "$.role", params.role)
    return json.enc_pretty(results)
end)

-- /inventory/products — sql.query all products
route.read("/inventory/products", function()
    ensure_sql_seeded()
    local rows = sql.query(sql_h, "SELECT * FROM products ORDER BY id")
    local out = {}
    for _, row in ipairs(rows) do
        local line = string.format("%d | %s | $%.2f | stock: %d", row.id, row.name, row.price, row.stock)
        table.insert(out, line)
    end
    return table.concat(out, "\n")
end)

-- /inventory/product/{id} — sql.row single product
route.read("/inventory/product/{id}", function(params)
    ensure_sql_seeded()
    local row = sql.row(sql_h, "SELECT * FROM products WHERE id = ?1", tonumber(params.id))
    if row == nil then
        return json.enc({error = "not found"})
    end
    return json.enc_pretty(row)
end)

-- /inventory/low-stock/{max} — sql.query with parameterized WHERE
route.read("/inventory/low-stock/{max}", function(params)
    ensure_sql_seeded()
    local rows = sql.query(sql_h, "SELECT * FROM products WHERE stock < ?1 ORDER BY stock", tonumber(params.max))
    return json.enc_pretty(rows)
end)

-- /serialize/json — json.enc
route.read("/serialize/json", function()
    return json.enc(book)
end)

-- /serialize/yaml — yaml.enc
route.read("/serialize/yaml", function()
    return yaml.enc(book)
end)

-- /serialize/toml — toml.enc
route.read("/serialize/toml", function()
    return toml.enc(book)
end)

-- /serialize/csv — csv.enc
route.read("/serialize/csv", function()
    return csv.enc(people)
end)

-- /serialize/roundtrip — json.enc → json.dec → json.enc_pretty
route.read("/serialize/roundtrip", function()
    local encoded = json.enc(book)
    local decoded = json.dec(encoded)
    return json.enc_pretty(decoded)
end)

-- /serialize/jq — json.jq filter
route.read("/serialize/jq", function()
    local data = json.enc(book)
    local result = json.jq(data, ".rating")
    return json.enc(result)
end)

-- /wiki/page — req.get with decode=json, error handled by type check
route.read("/wiki/page", function()
    local res = req.get(
        "https://en.wikipedia.org/api/rest_v1/page/summary/Rust_(programming_language)",
        {decode = "json", headers = {["User-Agent"] = "pinhead/0.1"}})
    if type(res) == "table" and res.error then
        return "HTTP Error: " .. res.error
    end
    local data = res.body
    return string.format([[
Page: %s
======
Title: %s
Description: %s
]], data.title or "unknown", data.title or "unknown",
   data.description or "unknown")
end)

-- /wiki/search — req.get with headers + query params + decode
route.read("/wiki/search", function()
    local opts = {
        headers = {["User-Agent"] = "pinhead/0.1"},
        query = {action = "opensearch", search = "Rust", format = "json", limit = "2"},
        decode = "json",
    }
    local res = req.get("https://en.wikipedia.org/w/api.php", opts)
    if type(res) == "string" then
        return string.format([[
Search Error
============
Request Failed: %s
]], res)
    end
    if type(res) == "table" and res.error then
        return string.format([[
Search Error
============
Request Failed: %s
]], res.error)
    end
    local data = res.body
    local lines = {}
    table.insert(lines, "Search Results")
    table.insert(lines, "==============")
    table.insert(lines, "")
    table.insert(lines, "Response Status: " .. tostring(res.status))
    table.insert(lines, "Success (ok): " .. tostring(res.ok))
    table.insert(lines, "")
    if type(data) == "table" and type(data[2]) == "table" then
        for _, v in ipairs(data[2]) do
            table.insert(lines, "  - " .. tostring(v))
        end
    end
    return table.concat(lines, "\n")
end)

-- ── Default handler for unmatched paths ───────────────────────────────────
route.default(function(params, _)
    local path = params["path"] or "unknown"
    return "Unmatched path: " .. path
end)

-- ── Frontend configuration ────────────────────────────────────────────────

-- SSH user credentials.
local users = {
    {"alice", "hunter2"},
    {"bob", "letmein"},
}
for _, pair in ipairs(users) do
    sshfs.userpasswd(pair[1], pair[2])
end

-- Listeners — override via env vars for e2e tests.
if env.get("PINHEAD_LISTEN") then
    ninep.listen(env.get("PINHEAD_LISTEN"))
end
if env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
end
if env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
end
