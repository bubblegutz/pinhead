-- pinhead minimal test example
--
-- A minimal filesystem for end-to-end testing through the 9P frontend.
-- Exposes a single file `/testfile.txt` and root directory listing.

route.register("/", {"lookup", "getattr", "open", "release"}, function(params, data)
    return "root"
end)

route.register("/testfile.txt", "lookup", function(params, data)
    return "found testfile.txt"
end)

route.register("/testfile.txt", "getattr", function(params, data)
    return 'mode=file size=27'
end)

route.register("/testfile.txt", "open", function(params, data)
    return "opened"
end)

route.register("/testfile.txt", "release", function(params, data)
    return "released"
end)

route.register("/testfile.txt", "read", function(params, data)
    return "hello from pinhead test!"
end)

route.default(function(params, data)
    return "unmatched"
end)

-- User credentials for SSH auth.
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
end
if env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
end
if env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
end
