-- pinhead bundle defaults demonstration
--
-- Demonstrates every route bundle with .default() fallback:
--   route.{read,write,readdir,create,unlink,lookup,mkdir,all}.default(func)
--
-- Each bundle registers a specific path handler AND a catch-all .default
-- with distinct output so you can see which handler matched.

-- Read bundle: lookup + getattr + open + read + release + flush
route.read("/docs/{name}", function(params, data)
    return "Document: " .. params["name"]
end)
route.read.default(function(params, data)
    return "Default read: " .. params["path"]
end)

-- Write bundle: lookup + getattr + open + read + write + release + flush + fsync
route.write("/notes/{name}", function(params, data)
    return "Note: " .. params["name"]
end)
route.write.default(function(params, data)
    return "Default write: " .. params["path"]
end)

-- Readdir bundle: lookup + getattr + opendir + readdir + releasedir
route.readdir("/users/{id}", function(params, data)
    return "User: " .. params["id"]
end)
route.readdir.default(function(params, data)
    return "Default readdir: " .. params["path"]
end)

-- Create bundle: lookup + getattr + create + open + read + write + release + flush
route.create("/sessions/{id}", function(params, data)
    return "Session: " .. params["id"]
end)
route.create.default(function(params, data)
    return "Default create: " .. params["path"]
end)

-- Unlink bundle: unlink + lookup + getattr
route.unlink("/trash/{name}", function(params, data)
    return "Trash: " .. params["name"]
end)
route.unlink.default(function(params, data)
    return "Default unlink: " .. params["path"]
end)

-- Mkdir bundle: mkdir + lookup + getattr + opendir + readdir + releasedir
route.mkdir("/projects/{name}", function(params, data)
    return "Project: " .. params["name"]
end)
route.mkdir.default(function(params, data)
    return "Default mkdir: " .. params["path"]
end)

-- Lookup bundle: single op
route.lookup("/api/{version}", function(params, data)
    return "API v" .. params["version"]
end)
route.lookup.default(function(params, data)
    return "Default lookup: " .. params["path"]
end)

-- Global default: ops not covered by any bundle default
route.default(function(params, data)
    return "Global default: unmatched"
end)

-- Users for SSH auth.
local users = {
    {"alice", "hunter2"},
    {"bob", "letmein"},
}
for _, pair in ipairs(users) do
    sshfs.userpasswd(pair[1], pair[2])
end

-- Listeners — override via PINHEAD_LISTEN / PINHEAD_SSH_LISTEN env vars for e2e tests.
if env.get("PINHEAD_LISTEN") then
    ninep.listen(env.get("PINHEAD_LISTEN"))
elseif env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
elseif env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
else
    -- ninep.listen("sock:/tmp/pinhead.sock")
    -- sshfs.listen("127.0.0.1:2222")
    -- fuse.mount("/tmp/pinhead")
end
