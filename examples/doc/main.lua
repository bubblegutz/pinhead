-- doc_demo.lua — demonstrates the doc.* document store API
--
-- Opens a database, registers routes that lazy-initialise data on
-- first request (avoids tokio blocking issues during setup).

local db_path = env.get("PINHEAD_DOC_DB") or "/tmp/pinhead-doc-demo.db"
local h = doc.open(db_path)

-- Lazy seed: populate documents on first route hit.
local seeded = false
local function ensure_seeded()
    if not seeded then
        doc.set(h, "alice", {name = "Alice", age = 30, role = "engineer"})
        doc.set(h, "bob",   {name = "Bob",   age = 25, role = "designer"})
        doc.set(h, "carol", {name = "Carol", age = 35, role = "manager"})
        seeded = true
    end
end

-- /user/<name> — fetch a single user document by key
route.read("/user/{name}", function(params)
    ensure_seeded()
    local val = doc.get(h, params.name)
    if val == nil then
        return json.enc({error = "not found"})
    end
    return json.enc_pretty(val)
end)

-- /all — list all documents
route.read("/all", function()
    ensure_seeded()
    return json.enc_pretty(doc.all(h))
end)

-- /count — number of stored documents
route.read("/count", function()
    ensure_seeded()
    return tostring(doc.count(h))
end)

-- /find/<role> — find documents by json_extract on $.role
route.read("/find/{role}", function(params)
    ensure_seeded()
    local results = doc.find(h, "$.role", params.role)
    return json.enc_pretty(results)
end)

local root_meta = {
    {name="alice", path="/user/alice", desc="GET /user/alice  → Alice's document"},
    {name="bob",   path="/user/bob",   desc="GET /user/bob    → Bob's document"},
    {name="carol", path="/user/carol", desc="GET /user/carol  → Carol's document"},
    {name="all",   path="/all",        desc="GET /all          → all documents"},
    {name="count", path="/count",      desc="GET /count        → document count"},
    {name="find",  path="/find/engineer", desc="GET /find/{role} → find by role"},
}

route.readdir("/", function()
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

local listen_addr = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-doc-demo.sock"
ninep.listen(listen_addr)
local ssh_listen = env.get("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)

-- FUSE mount — activated via PINHEAD_FUSE_MOUNT env var for e2e tests.
local fuse_mount = env.get("PINHEAD_FUSE_MOUNT")
if fuse_mount then
    fuse.mount(fuse_mount)
end
