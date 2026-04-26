-- Utility module for modular_demo
-- Provides helper functions for route handlers

local utils = {}

-- Track start time for uptime calculation
local start_time = os.time()

-- Simple memory usage simulation
function utils.get_memory_usage()
    return math.random(50, 200) / 100  -- 0.5 to 2.0 MB
end

-- Calculate uptime in seconds
function utils.get_uptime()
    return os.time() - start_time
end

-- Format bytes to human readable string
function utils.format_bytes(bytes)
    local units = {"B", "KB", "MB", "GB", "TB"}
    local unit_index = 1

    while bytes >= 1024 and unit_index < #units do
        bytes = bytes / 1024
        unit_index = unit_index + 1
    end

    return string.format("%.2f %s", bytes, units[unit_index])
end

-- Generate a unique ID
function utils.generate_id()
    local template = "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx"
    return template:gsub("[xy]", function(c)
        local v = (c == "x") and math.random(0, 0xf) or math.random(8, 0xb)
        return string.format("%x", v)
    end)
end

-- Log message with timestamp
function utils.log(message)
    local timestamp = os.date("%Y-%m-%d %H:%M:%S")
    print(string.format("[%s] %s", timestamp, message))
end

return utils
