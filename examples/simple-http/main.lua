-- simple_http_test.lua — minimal HTTP request to test SSH transport
route.register("/data", {"lookup", "getattr", "read", "open", "release"}, function()
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

if env.get("PINHEAD_LISTEN") then
    ninep.listen(env.get("PINHEAD_LISTEN"))
elseif env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
else
    -- ninep.listen("sock:/tmp/pinhead.sock")
    -- sshfs.listen("127.0.0.1:2222")
end
