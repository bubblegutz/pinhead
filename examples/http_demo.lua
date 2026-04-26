-- http_demo.lua — demonstrates req.* HTTP client with built-in decode
--
-- Makes requests to public REST APIs with req's decode option,
-- so the response body is already parsed — no manual json.dec() needed.
-- Serves formatted results as files on the virtual filesystem.

route.register("/", {"lookup", "getattr", "readdir", "open", "release"}, function()
    local meta = {
        page   = "GET /page    → fetches a Wikipedia page summary",
        search = "GET /search  → Wikipedia opensearch with query params",
    }
    return yaml.enc(meta)
end)

-- Fetch a Wikipedia page summary with automatic JSON decoding.
route.register("/page", {"lookup", "getattr", "read", "open", "release"}, function()
    local ok, res = pcall(req.get,
        "https://en.wikipedia.org/api/rest_v1/page/summary/Rust_(programming_language)",
        {decode = "json"})
    if not ok then
        return "HTTP Error: " .. tostring(res)
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
route.register("/search", {"lookup", "getattr", "read", "open", "release"}, function()
    local opts = {
        headers = {["User-Agent"] = "pinhead/0.1"},
        query = {action = "opensearch", search = "Rust", format = "json", limit = "2"},
        decode = "json",
    }
    local ok, res = pcall(req.get, "https://en.wikipedia.org/w/api.php", opts)
    if not ok then
        return string.format([[
Search Error
============

Request Failed: %s
]], tostring(res))
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
end

local listen_addr = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-http-demo.sock"
ninep.listen(listen_addr)
local ssh_listen = env.get("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)

-- FUSE mount — activated via PINHEAD_FUSE_MOUNT env var for e2e tests.
local fuse_mount = env.get("PINHEAD_FUSE_MOUNT")
if fuse_mount then
    fuse.mount(fuse_mount)
end
