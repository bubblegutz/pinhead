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

if env.get("PINHEAD_LISTEN") then
    ninep.listen(env.get("PINHEAD_LISTEN"))
end
if env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
end
if env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
end

-- Read an existing env var at config time
local path = env.get("PATH")
print("PATH = " .. (path or "not set"))

-- Set a custom var we'll use in a route
env.set("PINHEAD_MESSAGE", "hello from env!")

-- ── Routes ────────────────────────────────────────────────────────

-- Route that reads back the custom env var we set at config time
route.read("/message", function()
    return env.get("PINHEAD_MESSAGE") or "not set"
end)

-- Route that reads the HOME env var (inherited from the parent process)
route.read("/home", function()
    return env.get("HOME") or "no home"
end)

-- Route that sets an env var and reads it back in the same call
route.read("/ping", function()
    env.set("PINHEAD_PONG", "pong")
    return env.get("PINHEAD_PONG")
end)

-- Route that unsets an env var
route.read("/nuke", function()
    env.set("PINHEAD_NUKE", "boom")
    env.unset("PINHEAD_NUKE")
    local val = env.get("PINHEAD_NUKE")
    return val == nil and "was-nil" or val
end)
