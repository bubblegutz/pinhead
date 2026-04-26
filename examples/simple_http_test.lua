-- simple_http_test.lua — minimal HTTP request to test SSH transport
route.register("/", {"lookup", "getattr", "read", "open", "release"}, function()
    local ok, res = pcall(req.get, "http://example.com/")
    if not ok then
        return "HTTP Error: " .. tostring(res)
    end
    return "Got " .. tostring(#res) .. " bytes from example.com"
end)

-- User credentials for SSH auth
local users = {{"alice", "hunter2"}}
for _, pair in ipairs(users) do
    sshfs.userpasswd(pair[1], pair[2])
end

local listen_addr = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-http-test.sock"
ninep.listen(listen_addr)
local ssh_listen = env.get("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)
