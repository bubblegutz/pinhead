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
    log.debug("added user: " .. pair[1])
end
-- Alternatively, for a single global password (any username accepted):
-- sshfs.password("hunter2")
-- Or use ed25519 public key auth:
-- sshfs.authorized_keys("/home/alice/.ssh/authorized_keys")

-- Listeners — override via PINHEAD_LISTEN / PINHEAD_SSH_LISTEN env vars for e2e tests.
local listen_addr = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead.sock"
ninep.listen(listen_addr)
log.print("9P listener on " .. listen_addr)
local ssh_listen = env.get("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)
log.print("SSH listener on " .. ssh_listen)

-- FUSE mount — activated via PINHEAD_FUSE_MOUNT env var for e2e tests.
local fuse_mount = env.get("PINHEAD_FUSE_MOUNT")
if fuse_mount then
    fuse.mount(fuse_mount)
end

-- Route registrations -------------------------------------------------------

route.readdir("/", function(_params, _data)
    return "root directory"
end)

route.getattr("/", function(_params, _data)
    return 'mode=directory size=4096'
end)

route.lookup("/users/{id}/profile", function(params, _data)
    local id = params["id"]
    return "profile for user " .. id
end)

route.open("/users/{id}/profile", function(_params, _data)
    return ""
end)

route.read("/users/{id}/profile", function(params, _data)
    local id = params["id"]
    return '{"user":"' .. id .. '","name":"User ' .. id .. '","email":"user' .. id .. '@example.com"}'
end)

route.getattr("/users/{id}/profile", function(_params, _data)
    return 'mode=file size=128'
end)

route.read("/files/{path}", function(params, _data)
    local path = params["path"]
    return "contents of " .. path
end)

route.lookup("/files/{path}", function(params, _data)
    return "file: " .. params["path"]
end)

route.getattr("/files/{path}", function(_params, _data)
    return 'mode=file size=64'
end)

-- Default handler for unmatched paths.
route.default(function(params, data)
    return "unmatched path"
end)
