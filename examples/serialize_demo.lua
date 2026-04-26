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

route.register("/json", {"lookup", "getattr", "read", "open", "release"}, function()
    return json.enc(book)
end)

route.register("/yaml", {"lookup", "getattr", "read", "open", "release"}, function()
    return yaml.enc(book)
end)

route.register("/toml", {"lookup", "getattr", "read", "open", "release"}, function()
    return toml.enc(book)
end)

route.register("/csv", {"lookup", "getattr", "read", "open", "release"}, function()
    return csv.enc(people)
end)

route.register("/roundtrip", {"lookup", "getattr", "read", "open", "release"}, function()
    -- Encode and then decode back to verify round-trip fidelity.
    local encoded = json.enc(book)
    local decoded = json.dec(encoded)
    return json.enc_pretty(decoded)
end)

-- Query routes: encode data, query by path/filter, re-encode result.
route.register("/json_query", {"lookup", "getattr", "read", "open", "release"}, function()
    local data = json.enc(book)
    local result = json.q(data, "title")
    return json.enc(result)
end)

route.register("/yaml_query", {"lookup", "getattr", "read", "open", "release"}, function()
    local data = yaml.enc(book)
    local result = yaml.q(data, "author")
    return json.enc(result)
end)

route.register("/toml_query", {"lookup", "getattr", "read", "open", "release"}, function()
    local data = toml.enc(book)
    local result = toml.q(data, "year")
    return json.enc(result)
end)

route.register("/csv_query", {"lookup", "getattr", "read", "open", "release"}, function()
    local data = csv.enc(people)
    local result = csv.q(data, "name=Alice")
    -- Re-encode the filtered rows as CSV
    return csv.enc(result)
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
}

route.register("/", {"lookup", "getattr", "readdir", "open", "release"}, function()
    return yaml.enc(root_meta)
end)

ninep.listen("sock:/tmp/pinhead-serialize.sock")
