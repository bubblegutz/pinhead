-- http_demo.lua — demonstrates req.* HTTP client with built-in decode
--
-- Makes requests to public REST APIs with req's decode option,
-- so the response body is already parsed — no manual json.dec() needed.
-- Serves formatted results as files on the virtual filesystem.

route.readdir("/", function()
    local meta = {
        page   = "GET /page    → fetches a Wikipedia page summary",
        search = "GET /search  → Wikipedia opensearch with query params",
    }
    return yaml.enc(meta)
end)

-- Fetch a Wikipedia page summary with automatic JSON decoding.
route.read("/page", function()
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
Extract: %s
]], data.title or "unknown", data.title or "unknown",
   data.description or "unknown", data.extract or "unknown")
end)

-- Query Wikipedia's opensearch API with custom headers and query params,
-- demonstrating req.get with options and graceful pcall error handling.
route.read("/search", function()
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
end
if env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
end
if env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
end
