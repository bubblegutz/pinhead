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

local listen_addr = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-utilities.sock"
ninep.listen(listen_addr)
log.print("9P listener on " .. listen_addr)
local ssh_listen = env.get("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)
log.print("SSH listener on " .. ssh_listen)

local fuse_mount = env.get("PINHEAD_FUSE_MOUNT")
if fuse_mount then
    fuse.mount(fuse_mount)
end
