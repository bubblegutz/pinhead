-- pinhead Lua handler script
--
-- Routes are registered via `route.register(pattern, ops, func)` where `ops`
-- is a single operation name (string), a list of names (table), or nil/true
-- for all operations.  Each handler receives (params, data):
--   params : table  (path parameters, e.g. {id="42"})
--   data   : string or nil (write payload)
--
-- Frontend configuration is done via the `fuse.*` and `ninep.*` namespaces:
--   fuse.mount("/path/to/mountpoint")     -- FUSE mount point
--   fuse.unmount("/path/to/mountpoint")   -- remove a mount
--   fuse.unmountall()                      -- remove all mounts
--   ninep.listen("sock:/tmp/pinhead.sock") -- 9P over Unix socket
--   ninep.listen("tcp:127.0.0.1:5640")     -- 9P over TCP
--   ninep.listen("udp:127.0.0.1:5641")     -- 9P over UDP
--   ninep.kill("sock:/tmp/pinhead.sock")   -- stop a listener
--   ninep.killall()                         -- stop all listeners

route.register("/", {"lookup", "getattr", "readdir"}, function(params, data)
    return "root directory"
end)

route.register("/", "getattr", function(params, data)
    return 'mode=directory size=4096'
end)

route.register("/users/{id}/profile", "lookup", function(params, data)
    local id = params["id"]
    return "profile for user " .. id
end)

route.register("/users/{id}/profile", "read", function(params, data)
    local id = params["id"]
    return '{"user":"' .. id .. '","name":"User ' .. id .. '","email":"user' .. id .. '@example.com"}'
end)

route.register("/users/{id}/profile", "getattr", function(params, data)
    return 'mode=file size=128'
end)

route.register("/files/{path}", "read", function(params, data)
    local path = params["path"]
    return "contents of " .. path
end)

route.register("/files/{path}", "lookup", function(params, data)
    return "file: " .. params["path"]
end)

route.register("/files/{path}", "getattr", function(params, data)
    return 'mode=file size=64'
end)

-- Default handler for unmatched paths.
route.default(function(params, data)
    return "unmatched path"
end)
