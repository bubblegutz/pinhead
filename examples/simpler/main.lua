-- simpler_demo.lua — demonstrates basic routing operations
--
-- Routes:
--   /hello.txt          - Simple text file
--   /counter.txt        - Counter that increments on each read
--   /echo               - Echo handler (write)
--   /files/readme.txt   - Nested file
--   /files/nested/deep.txt - Deeply nested file
--   /uploads/*          - Wildcard create (wildcard via {path})

route.readdir("/", function()
    local entries = {"hello.txt", "echo", "files", "uploads"}
    return json.enc(entries)
end)

route.read("/hello.txt", function()
    return "Hello from pinhead filesystem!\nThis file is served by a Lua handler."
end)

route.write("/echo", function(_, data)
    return "Echo: " .. (data or "")
end)

route.readdir("/files", function()
    local entries = {"readme.txt", "nested"}
    return json.enc(entries)
end)

route.read("/files/readme.txt", function()
    return "This is a nested file.\nYou can create complex directory structures with Lua handlers."
end)

route.readdir("/files/nested", function()
    local entries = {"deep.txt"}
    return json.enc(entries)
end)

route.read("/files/nested/deep.txt", function()
    return "Deeply nested file content."
end)

-- Wildcard create — {path} catches any /uploads/<name>
route.create("/uploads/{path}", function(params, data)
    local filename = params.path or "unknown"
    return "Created upload file: " .. filename .. "\n" ..
           "Path: /uploads/" .. filename .. "\n" ..
           "Time: " .. os.date("%Y-%m-%d %H:%M:%S")
end)

-- User credentials for SSH auth.
local users = {
    {"alice", "hunter2"},
    {"bob", "letmein"},
}
for _, pair in ipairs(users) do
    sshfs.userpasswd(pair[1], pair[2])
    log.debug("added user: " .. pair[1])
end

local listen_addr = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-simpler.sock"
ninep.listen(listen_addr)
log.print("9P listener on " .. listen_addr)
local ssh_listen = env.get("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_listen)
log.print("SSH listener on " .. ssh_listen)

local fuse_mount = env.get("PINHEAD_FUSE_MOUNT")
if fuse_mount then
    fuse.mount(fuse_mount)
end
