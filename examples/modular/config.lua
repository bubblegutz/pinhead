-- Configuration module for modular_demo
-- Provides configuration settings and custom route definitions

local config = {}

-- Basic configuration
config.mount_point = "/tmp/pinhead-modular"
config.verbose = true
config.max_handlers = 8
config.source = "examples/modular_demo/config.lua"

-- Feature flags
config.features = {
    user_management = true,
    product_catalog = true,
    file_operations = true,
    metrics = true,
    logging = true,
}

-- Get feature status
function config.is_feature_enabled(feature_name)
    return config.features[feature_name] or false
end

-- Custom routes registered dynamically
function config.register_custom_routes()
    route.read("/api/info", function(params)
        return "Modular pinhead Example\nVersion: 1.0.0\nModules: utils, routes, config\nMount point: " .. config.mount_point
    end)

    route.readdir("/api/modules", function(params)
        local entries = {"utils", "routes", "config", "main"}
        return json.enc(entries)
    end)

    route.read("/api/modules/{name}", function(params)
        local module_name = params.name
        if module_name == "utils" then
            return "Utility module\nProvides helper functions for route handlers\nIncludes: memory usage, uptime, formatting, validation"
        elseif module_name == "routes" then
            return "Routes module\nDefines all API route handlers\nIncludes: user management, product catalog, system routes"
        elseif module_name == "config" then
            return "Configuration module\nProvides settings and custom routes\nCurrent mount point: " .. config.mount_point
        elseif module_name == "main" then
            return "Main script\nOrchestrates module loading and route registration\nSets up package.path for require()"
        else
            return "Unknown module: " .. module_name
        end
    end)

    route.write("/api/echo", function(_, data)
        return "Echo (from custom route): " .. (data or "")
    end)
end

-- Get all configuration as a table
function config.get_all()
    local all_config = {}
    for k, v in pairs(config) do
        if type(v) ~= "function" then
            all_config[k] = v
        end
    end
    return all_config
end

return config
