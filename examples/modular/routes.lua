-- Routes module for modular_demo
-- Defines route handlers organized by functionality

local routes = {}
local utils = require("utils")

-- User data storage (in-memory for example)
local users = {
    {id = "1", name = "Alice", email = "alice@example.com", role = "admin"},
    {id = "2", name = "Bob", email = "bob@example.com", role = "user"},
    {id = "3", name = "Charlie", email = "charlie@example.com", role = "user"},
}

-- Product catalog
local products = {
    {id = "100", name = "Widget", price = 9.99, stock = 42},
    {id = "101", name = "Gadget", price = 19.99, stock = 15},
    {id = "102", name = "Thingy", price = 4.99, stock = 100},
}

-- Register all routes in this module
function routes.register_routes()
    -- User management routes
    route.readdir("/api/users", routes.list_users)
    route.read("/api/users/{id}", routes.get_user)
    route.write("/api/users", routes.create_user)
    route.unlink("/api/users/{id}", routes.delete_user)

    -- Product catalog routes
    route.readdir("/api/products", routes.list_products)
    route.read("/api/products/{id}", routes.get_product)
    route.write("/api/products", routes.create_product)

    -- System routes
    route.read("/api/health", routes.health_check)
    route.readdir("/api/metrics", routes.list_metrics)
    route.read("/api/metrics/{name}", routes.get_metric)

    -- File operations
    route.create("/tmp/{filename}", routes.create_temp_file)
    route.unlink("/tmp/{filename}", routes.remove_temp_file)

    routes.route_count = 13
end

-- User management handlers
function routes.list_users(params)
    local user_list = {}
    for _, user in ipairs(users) do
        table.insert(user_list, user.id)
    end
    return json.enc(user_list)
end

function routes.get_user(params)
    local user_id = params.id
    for _, user in ipairs(users) do
        if user.id == user_id then
            return json.enc({id = user.id, name = user.name, email = user.email, role = user.role})
        end
    end
    return "User not found"
end

function routes.create_user(params, data)
    local name = data:match("name=([^&]+)") or "Unknown"
    local email = data:match("email=([^&]+)") or ""

    local new_user = {
        id = utils.generate_id(),
        name = name,
        email = email,
        role = "user",
    }

    table.insert(users, new_user)
    return "User created: " .. new_user.id .. "\nName: " .. new_user.name .. "\nEmail: " .. new_user.email
end

function routes.delete_user(params)
    local user_id = params.id
    for i, user in ipairs(users) do
        if user.id == user_id then
            table.remove(users, i)
            utils.log("Deleted user: " .. user_id)
            return "User deleted: " .. user_id
        end
    end
    return "User not found"
end

-- Product catalog handlers
function routes.list_products(params)
    local product_list = {}
    for _, product in ipairs(products) do
        table.insert(product_list, product.id .. ": " .. product.name .. " - $" .. product.price .. " (stock: " .. product.stock .. ")")
    end
    return table.concat(product_list, "\n")
end

function routes.get_product(params)
    local product_id = params.id
    for _, product in ipairs(products) do
        if product.id == product_id then
            return "ID: " .. product.id .. "\nName: " .. product.name .. "\nPrice: $" .. product.price .. "\nStock: " .. product.stock
        end
    end
    return "Product not found"
end

function routes.create_product(params, data)
    local name = data:match("name=([^&]+)") or "New Product"
    local price = tonumber(data:match("price=([^&]+)")) or 0
    local stock = tonumber(data:match("stock=([^&]+)")) or 0

    local new_product = {
        id = tostring(math.random(1000, 9999)),
        name = name,
        price = price,
        stock = stock,
    }

    table.insert(products, new_product)
    return "Product created: " .. new_product.id .. "\nName: " .. new_product.name .. "\nPrice: $" .. new_product.price .. "\nStock: " .. new_product.stock
end

-- System handlers
function routes.health_check(params)
    local mem_usage = utils.get_memory_usage()
    local uptime = utils.get_uptime()

    return "Status: OK\nUptime: " .. uptime .. " seconds\nMemory: " .. mem_usage .. " MB\nUsers: " .. #users .. "\nProducts: " .. #products
end

function routes.list_metrics(params)
    local entries = {"health", "users", "products", "uptime", "memory"}
    return json.enc(entries)
end

function routes.get_metric(params)
    local metric_name = params.name
    if metric_name == "users" then return "Total users: " .. #users
    elseif metric_name == "products" then return "Total products: " .. #products
    elseif metric_name == "uptime" then return "Uptime: " .. utils.get_uptime() .. " seconds"
    elseif metric_name == "memory" then return "Memory usage: " .. utils.get_memory_usage() .. " MB"
    else return "Unknown metric: " .. metric_name end
end

-- File operation handlers
function routes.create_temp_file(params)
    local filename = params.filename
    utils.log("Creating temp file: " .. filename)
    return "Temporary file created: " .. filename .. "\nPath: /tmp/" .. filename .. "\nTime: " .. os.date("%Y-%m-%d %H:%M:%S")
end

function routes.remove_temp_file(params)
    local filename = params.filename
    utils.log("Removing temp file: " .. filename)
    return "Temporary file removed: " .. filename
end

return routes
