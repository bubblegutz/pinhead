-- pinhead Lua handler script
--
-- Frontend configuration (fuse.*, ninep.*, sshfs.*):
--   fuse.mount("/path")              -- FUSE mount point
--   fuse.unmount("/path")            -- remove a mount
--   fuse.unmountall()                -- remove all mounts
--   ninep.listen("sock:/path")       -- 9P over Unix socket
--   ninep.listen("tcp:ip:port")      -- 9P over TCP
--   ninep.listen("udp:ip:port")      -- 9P over UDP
--   ninep.kill(addr)                 -- stop a 9P listener
--   ninep.killall()                  -- stop all 9P listeners
--   sshfs.listen("ip:port")          -- SSH/SFTP server on TCP
--   sshfs.kill(addr)                 -- stop an SSH listener
--   sshfs.killall()                  -- stop all SSH listeners
--   sshfs.password(pw)               -- set global auth password
--   sshfs.authorized_keys(path)      -- ed25519 authorized_keys file
--   sshfs.userpasswd(user, pw)       -- add a username/password pair
--
-- Load users from a separate file:
--   local users = dofile("users.lua")
--   for _, pair in ipairs(users) do sshfs.userpasswd(pair[1], pair[2]) end
-- users.lua format: return {{"alice", "hunter2"}, {"bob", "letmein"}}

-- Users (inline table; also see dofile pattern above).
local users = {
    {"alice", "hunter2"},
    {"bob", "letmein"},
}
for _, pair in ipairs(users) do
    sshfs.userpasswd(pair[1], pair[2])
end
-- Alternatively, for a single global password (any username accepted):
-- sshfs.password("hunter2")
-- Or use ed25519 public key auth:
-- sshfs.authorized_keys("/home/alice/.ssh/authorized_keys")

-- Listeners — override via PINHEAD_LISTEN / PINHEAD_SSH_LISTEN env vars for e2e tests.
local listen_addr = os.getenv("PINHEAD_LISTEN") or "sock:/tmp/pinhead.sock"
ninep.listen(listen_addr)
local ssh_listen = os.getenv("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)

-- Route registrations -------------------------------------------------------

route.register("/", {"lookup", "getattr", "readdir", "open", "release"}, function(params, data)
    return "root directory"
end)

route.register("/", "getattr", function(params, data)
    return 'mode=directory size=4096'
end)

route.register("/users/{id}/profile", "lookup", function(params, data)
    local id = params["id"]
    return "profile for user " .. id
end)

route.register("/users/{id}/profile", "open", function(params, data)
    return ""
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
