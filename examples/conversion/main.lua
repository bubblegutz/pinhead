-- convert_demo.lua — JSON/YAML conversion and auto-detection
--
-- Routes:
--   /tojson   - Write YAML, read JSON (converts YAML to JSON)
--   /toyaml   - Write JSON, read YAML (converts JSON to YAML)
--   /convert  - Write either, read opposite format (auto-detection)
--   /help     - Usage documentation

local cache = {}
local mode = {}

route.write("/tojson", function(_, data)
    local jsonStr, err = json.from_yaml(data or "")
    if err then return "error: " .. err end
    cache["/tojson"] = jsonStr
    log.print("Cache set to: " .. jsonStr)
    return jsonStr
end)

route.read("/tojson", function()
    log.print("Cache read: " .. tostring(cache["/tojson"]))
    return cache["/tojson"] or "{}"
end)

route.write("/toyaml", function(_, data)
    local yamlStr, err = yaml.from_json(data or "")
    if err then return "error: " .. err end
    cache["/toyaml"] = yamlStr
    return yamlStr
end)

route.read("/toyaml", function()
    return cache["/toyaml"] or ""
end)

-- Auto-detection route: write either JSON or YAML, read back the opposite format.
route.write("/convert", function(_, data)
    local trimmed = (data or ""):gsub("^%s+", ""):gsub("%s+$", "")
    if trimmed == "" then
        cache["/convert"] = ""
        mode["/convert"] = nil
        return ""
    end
    local first = trimmed:sub(1, 1)
    if first == "{" or first == "[" then
        local yamlStr, err = yaml.from_json(data)
        if err then return "error (JSON->YAML): " .. err end
        cache["/convert"] = yamlStr
        mode["/convert"] = "yaml"
    else
        local jsonStr, err = json.from_yaml(data)
        if err then return "error (YAML->JSON): " .. err end
        cache["/convert"] = jsonStr
        mode["/convert"] = "json"
    end
    return cache["/convert"]
end)

route.read("/convert", function()
    return cache["/convert"] or ""
end)

route.read("/help", function()
    return [[
JSON/YAML Conversion Example

Routes:
  /tojson    - Write YAML, read JSON
  /toyaml    - Write JSON, read YAML
  /convert   - Write either, read opposite format (auto-detection)
  /help      - This message

Examples:
  echo 'name: test' > /mount/tojson    # Write YAML
  cat /mount/tojson                    # Read JSON

  echo '{"x":1}' > /mount/toyaml
  cat /mount/toyaml

  echo 'key: value' > /mount/convert
  cat /mount/convert                   # Outputs JSON

  echo '{"a":1}' > /mount/convert
  cat /mount/convert                   # Outputs YAML
]]
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
