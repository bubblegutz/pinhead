-- serialize_demo.lua — demonstrates json.enc, .dec, .q, yaml, toml, csv
--
-- Each route returns data serialized in a different format, so the test can
-- request any route, decode the response, and verify the round-trip.

local book = {
    title = "The Name of the Wind",
    author = "Patrick Rothfuss",
    year = 2007,
    tags = {"fantasy", "fiction"},
}

local people = {
    {name = "Alice", age = 30, role = "engineer"},
    {name = "Bob",   age = 25, role = "designer"},
    {name = "Carol", age = 35, role = "manager"},
}

route.read("/json", function()
    return json.enc(book)
end)

route.read("/yaml", function()
    return yaml.enc(book)
end)

route.read("/toml", function()
    return toml.enc(book)
end)

route.read("/csv", function()
    return csv.enc(people)
end)

route.read("/roundtrip", function()
    -- Encode and then decode back to verify round-trip fidelity.
    local encoded = json.enc(book)
    local decoded = json.dec(encoded)
    return json.enc_pretty(decoded)
end)

-- Query routes: encode data, query by path/filter, re-encode result.
route.read("/json_query", function()
    local data = json.enc(book)
    local result = json.q(data, "title")
    return json.enc(result)
end)

route.read("/yaml_query", function()
    local data = yaml.enc(book)
    local result = yaml.q(data, "author")
    return json.enc(result)
end)

route.read("/toml_query", function()
    local data = toml.enc(book)
    local result = toml.q(data, "year")
    return json.enc(result)
end)

route.read("/csv_query", function()
    local data = csv.enc(people)
    local result = csv.q(data, "name=Alice")
    -- Re-encode the filtered rows as CSV
    return csv.enc(result)
end)

-- jq query routes: use full jq filter expressions via json.jq
route.read("/jq_title", function()
    local data = json.enc(book)
    local result = json.jq(data, ".title")
    return json.enc(result)
end)

route.read("/jq_tags", function()
    local data = json.enc(book)
    local result = json.jq(data, ".tags[]")
    return json.enc(result)
end)

route.read("/jq_filtered", function()
    local data = json.enc(people)
    local result = json.jq(data, '.[] | select(.age > 30)')
    return json.enc(result)
end)

local root_meta = {
    json    = "GET /json   → JSON serialization",
    yaml    = "GET /yaml   → YAML serialization",
    toml    = "GET /toml   → TOML serialization",
    csv     = "GET /csv    → CSV serialization",
    roundtrip = "GET /roundtrip → JSON encode → decode → pretty encode",
    json_query = "GET /json_query → query JSON by path",
    yaml_query = "GET /yaml_query → query YAML by path",
    toml_query = "GET /toml_query → query TOML by path",
    csv_query  = "GET /csv_query  → query CSV by filter",
    jq_title   = "GET /jq_title   → jq query `.title`",
    jq_tags    = "GET /jq_tags    → jq query `.tags[]`",
    jq_filtered = "GET /jq_filtered → jq query `.[] | select(.age > 30)`",
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
    log.debug("added user: " .. pair[1])
end

if env.get("PINHEAD_LISTEN") then
    ninep.listen(env.get("PINHEAD_LISTEN"))
elseif env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
elseif env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
else
    -- ninep.listen("sock:/tmp/pinhead.sock")
    -- sshfs.listen("127.0.0.1:2222")
    -- fuse.mount("/tmp/pinhead")
end
