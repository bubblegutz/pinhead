-- http_demo.lua — demonstrates req.* HTTP client with built-in decode
--
-- Makes requests to public REST APIs with req's decode option,
-- so the response body is already parsed — no manual json.dec() needed.
-- Serves formatted results as files on the virtual filesystem.

route.register("/", {"lookup", "getattr", "readdir", "open", "release"}, function()
    local meta = {
        post        = "GET /post         → fetches a blog post from JSONPlaceholder",
        http_params = "GET /http_params  → HTTP request diagnostics (headers, query, errors)",
    }
    return yaml.enc(meta)
end)

-- Fetch a blog post from JSONPlaceholder with automatic JSON decoding.
-- The `decode = "json"` option parses the response body into a Lua table,
-- so we access fields directly via res.body.
route.register("/post", {"lookup", "getattr", "read", "open", "release"}, function()
    local ok, res = pcall(req.get, "https://jsonplaceholder.typicode.com/posts/1", {decode = "json"})
    if not ok then
        return "HTTP Error: " .. tostring(res)
    end

    local data = res.body
    return string.format([[
Post #%d
=======
Title: %s
Author (userId): %d

%s
]], data.id, data.title, data.userId, data.body)
end)

-- Make an HTTP request with custom headers and query params,
-- then format the full request/response diagnostics as a text file.
--
-- Demonstrates:
--   - Setting request headers
--   - Passing query parameters
--   - Built-in JSON decoding (no manual json.dec)
--   - Iterating response tables
--   - Graceful error handling with pcall
route.register("/http_params", {"lookup", "getattr", "read", "open", "release"}, function()
    local opts = {
        headers = {["X-Demo"] = "pinhead", ["Accept"] = "application/json"},
        query = {name = "Alice", role = "engineer", active = "true"},
        decode = "json",
    }
    local ok, res = pcall(req.get, "https://httpbin.org/get", opts)
    if not ok then
        return string.format([[
HTTP Request Diagnostics
========================

Request Failed: %s

Requested Headers:
  X-Demo: pinhead
  Accept: application/json

Requested Query Parameters:
  name: Alice
  role: engineer
  active: true
]], tostring(res))
    end

    local data = res.body
    local lines = {}
    table.insert(lines, "HTTP Request Diagnostics")
    table.insert(lines, "========================")
    table.insert(lines, "")
    table.insert(lines, "Response Status: " .. tostring(res.status))
    table.insert(lines, "Success (ok): " .. tostring(res.ok))
    table.insert(lines, "Origin: " .. (data.origin or "unknown"))
    table.insert(lines, "URL: " .. (data.url or "unknown"))
    table.insert(lines, "Content-Type: " .. (res.headers["Content-Type"] or "unknown"))
    table.insert(lines, "")

    table.insert(lines, "Sent Headers:")
    if data.headers then
        for k, v in pairs(data.headers) do
            table.insert(lines, "  " .. k .. ": " .. tostring(v))
        end
    end
    table.insert(lines, "")

    table.insert(lines, "Sent Query Parameters:")
    if data.args then
        for k, v in pairs(data.args) do
            table.insert(lines, "  " .. k .. " = " .. tostring(v))
        end
    end

    return table.concat(lines, "\n")
end)

ninep.listen("sock:/tmp/pinhead-http-demo.sock")
