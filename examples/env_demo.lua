-- pinhead Lua handler script — env.* API example
--
-- Demonstrates reading, setting, and unsetting environment variables
-- from within Lua route handlers.
--
-- env.get("KEY")   -> string | nil
-- env.set("KEY", v)
-- env.unset("KEY")

-- User credentials for SSH auth.
local users = {
    {"alice", "hunter2"},
    {"bob", "letmein"},
}
for _, pair in ipairs(users) do
    sshfs.userpasswd(pair[1], pair[2])
end

local listen_addr = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-env-demo.sock"
ninep.listen(listen_addr)
local ssh_listen = env.get("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)

-- FUSE mount — activated via PINHEAD_FUSE_MOUNT env var for e2e tests.
local fuse_mount = env.get("PINHEAD_FUSE_MOUNT")
if fuse_mount then
    fuse.mount(fuse_mount)
end

-- Read an existing env var at config time
local path = env.get("PATH")
print("PATH = " .. (path or "not set"))

-- Set a custom var we'll use in a route
env.set("PINHEAD_MESSAGE", "hello from env!")

-- ── Routes ────────────────────────────────────────────────────────

-- Route that reads back the custom env var we set at config time
route.register("/message", {"lookup", "getattr", "read", "open", "release"}, function()
    return env.get("PINHEAD_MESSAGE") or "not set"
end)

-- Route that reads the HOME env var (inherited from the parent process)
route.register("/home", {"lookup", "getattr", "read", "open", "release"}, function()
    return env.get("HOME") or "no home"
end)

-- Route that sets an env var and reads it back in the same call
route.register("/ping", {"lookup", "getattr", "read", "open", "release"}, function()
    env.set("PINHEAD_PONG", "pong")
    return env.get("PINHEAD_PONG")
end)

-- Route that unsets an env var
route.register("/nuke", {"lookup", "getattr", "read", "open", "release"}, function()
    env.set("PINHEAD_NUKE", "boom")
    env.unset("PINHEAD_NUKE")
    local val = env.get("PINHEAD_NUKE")
    return val == nil and "was-nil" or val
end)
