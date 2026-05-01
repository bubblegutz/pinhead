-- utilities_demo.lua — demonstrates env, log.print, log.debug
--
-- Routes:
--   /info       - System info using env vars
--   /counter    - Counter with logging
--   /debug-test - Debug mode toggle

local max_items = tonumber(env.get("MAX_ITEMS")) or 10
local default_greeting = env.get("GREETING") or "Hello"

route.read("/info", function()
    local info = {}
    table.insert(info, "User: " .. (env.get("USER") or "unknown"))
    table.insert(info, "Home: " .. (env.get("HOME") or "unknown"))
    table.insert(info, "Max items: " .. max_items)
    table.insert(info, "Default greeting: " .. default_greeting)
    table.insert(info, "")
    table.insert(info, "All environment variables:")
    for k, v in pairs(env) do
        table.insert(info, k .. "=" .. v)
    end
    return table.concat(info, "\n")
end)

local counter = 0
route.read("/counter", function()
    counter = counter + 1
    log.print("Counter accessed: " .. counter)
    log.debug("Counter debug: " .. counter)
    return "Counter: " .. counter
end)

route.read("/debug-test", function()
    if env.get("DEBUG_MODE") == "1" then
        log.debug("Debug mode is enabled")
        return "Debug mode is ON"
    else
        return "Debug mode is OFF (set DEBUG_MODE=1 to enable)"
    end
end)

route.readdir("/", function()
    local entries = {"info", "counter", "debug-test"}
    return json.enc(entries)
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
