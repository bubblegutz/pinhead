-- simple_http_test.lua — minimal HTTP request to test SSH transport
route.register("/data", {"lookup", "getattr", "read", "open", "release"}, function()
    local res = req.get("http://example.com/")
    if type(res) == "table" and res.error then
        return "HTTP Error: " .. res.error
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
end
if env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
end
