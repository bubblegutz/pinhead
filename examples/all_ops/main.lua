--[[
all_ops.lua — exercises every filesystem operation across all frontends.

Expected environment variables:
  PINHEAD_LISTEN       — 9P listener address (e.g. "sock:/tmp/foo.sock")
  PINHEAD_SSH_LISTEN   — SSH listener address (e.g. "127.0.0.1:2222")
  PINHEAD_FUSE_MOUNT   — FUSE mount point (e.g. "/tmp/pinhead-fuse")

All three are optional — only listeners that are set will be activated.
--]]

-- ── Frontend configuration ────────────────────────────────────────────

local addr = os.getenv("PINHEAD_LISTEN")
if addr then
    ninep.listen(addr)
end

local ssh_addr = os.getenv("PINHEAD_SSH_LISTEN")
if ssh_addr then
    sshfs.userpasswd("alice", "hunter2")
    sshfs.listen(ssh_addr)
end

local fuse_mount = os.getenv("PINHEAD_FUSE_MOUNT")
if fuse_mount then
    fuse.mount(fuse_mount)
end

-- ── Worker pool ───────────────────────────────────────────────────────

worker.min(1)
worker.max(4)
worker.ttl(60)

-- ── Route registrations — every operation type ────────────────────────

-- Read a file
route.read("/readme", function(params, data)
    return "read ok"
end)

-- Write to a file
route.write("/writeme", function(params, data)
    return "write ok"
end)

-- Create a new file (touch)
route.create("/createme", function(params, data)
    return "create ok"
end)

-- Make a directory
route.mkdir("/newdir", function(params, data)
    return "mkdir ok"
end)

-- Unlink / remove a file
route.unlink("/deleteme", function(params, data)
    return "unlink ok"
end)

-- Read directory contents
route.readdir("/dir", function(params, data)
    return "file1\nfile2\n"
end)

-- Lookup (check existence)
route.lookup("/lookupme", function(params, data)
    return "lookup ok"
end)

-- Get attributes
route.getattr("/getattrme", function(params, data)
    return "getattr ok"
end)

-- Open (prelude to read/write)
route.open("/openme", function(params, data)
    return "open ok"
end)

-- Release (close)
route.release("/releaseme", function(params, data)
    return "release ok"
end)

-- Catch-all handler for any other operation
route.default(function(params, data)
    return "default handler: " .. (params.path or "?")
end)
