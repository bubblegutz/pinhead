-- modular_demo/main.lua — demonstrates modular Lua code with require()
--
-- This example demonstrates organizing route definitions across multiple files.
-- Uses package.path to find sibling .lua files via require().
-- fs.cwd() is also available to change Lua's working directory at runtime.
-- Example: fs.cwd(os.getenv("PINHEAD_CWD") or ".")

package.path = "examples/modular/?.lua;" .. package.path

-- Load modules
local utils = require("utils")
local routes = require("routes")
local config = require("config")

-- Register all routes
routes.register_routes()
config.register_custom_routes()

-- Root directory
route.readdir("/", function()
    local entries = {"api", "docs", "status.txt", "config.txt"}
    return json.enc(entries)
end)

-- API documentation
route.readdir("/docs", function()
    local entries = {"api.md", "examples"}
    return json.enc(entries)
end)

route.read("/docs/api.md", function()
    return "# pinhead API Documentation\n\n" ..
           "This modular example demonstrates:\n" ..
           "- Modular code organization with require()\n" ..
           "- Utility functions in separate modules\n" ..
           "- Configuration management\n" ..
           "- Dynamic route registration\n\n" ..
           "## Available API endpoints\n" ..
           "- /api/users/ - User management\n" ..
           "- /api/products/ - Product catalog\n" ..
           "- /status.txt - System status\n" ..
           "- /config.txt - Current configuration"
end)

-- System status
route.read("/status.txt", function()
    local mem_usage = utils.get_memory_usage()
    local uptime = utils.get_uptime()

    return "System Status:\n" ..
           "Uptime: " .. uptime .. " seconds\n" ..
           "Memory usage: " .. mem_usage .. " MB\n" ..
           "Routes registered: " .. routes.route_count .. "\n" ..
           "Config loaded from: " .. config.source .. "\n" ..
           "Generated: " .. os.date("%Y-%m-%d %H:%M:%S")
end)

-- Configuration info
route.read("/config.txt", function()
    return "Configuration:\n" ..
           "Mount point: " .. config.mount_point .. "\n" ..
           "Verbose logging: " .. tostring(config.verbose) .. "\n" ..
           "Max handlers: " .. tostring(config.max_handlers) .. "\n" ..
           "Script directory: examples/modular_demo"
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

local listen_addr = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-modular-demo.sock"
ninep.listen(listen_addr)
log.print("9P listener on " .. listen_addr)
local ssh_listen = env.get("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)
log.print("SSH listener on " .. ssh_listen)

local fuse_mount = env.get("PINHEAD_FUSE_MOUNT")
if fuse_mount then
    fuse.mount(fuse_mount)
end
