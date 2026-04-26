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
local listen_addr = os.getenv("PINHEAD_LISTEN") or "sock:/tmp/pinhead-test-basic.sock"
ninep.listen(listen_addr)
local ssh_listen = os.getenv("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)
