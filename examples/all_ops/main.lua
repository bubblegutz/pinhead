--[[
all_ops.lua — exercises every filesystem operation across all frontends.

Expected environment variables:
  PINHEAD_LISTEN       — 9P listener address (e.g. "sock:/tmp/foo.sock")
  PINHEAD_SSH_LISTEN   — SSH listener address (e.g. "127.0.0.1:2222")
  PINHEAD_FUSE_MOUNT   — FUSE mount point (e.g. "/tmp/pinhead-fuse")

All three are optional — only listeners that are set will be activated.
--]]

-- ── Frontend configuration ────────────────────────────────────────────

if env.get("PINHEAD_LISTEN") then
    ninep.listen(env.get("PINHEAD_LISTEN"))
end
if env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
end
if env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
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
route.read("/readme", function(_, _)
    return "read ok"
end)

-- Write to a file
route.write("/writeme", function(_, _)
    return "write ok"
end)

-- Create a new file (touch)
route.create("/createme", function(_, _)
    return "create ok"
end)

-- Make a directory
route.mkdir("/newdir", function(_, _)
    return "mkdir ok"
end)

-- Unlink / remove a file
route.unlink("/deleteme", function(_, _)
    return "unlink ok"
end)

-- Read directory contents
route.readdir("/dir", function(_, _)
    return "file1\nfile2\n"
end)

-- Lookup (check existence)
route.lookup("/lookupme", function(_, _)
    return "lookup ok"
end)

-- Get attributes
route.getattr("/getattrme", function(_, _)
    return "getattr ok"
end)

-- Open (prelude to read/write)
route.open("/openme", function(_, _)
    return "open ok"
end)

-- Release (close)
route.release("/releaseme", function(_, _)
    return "release ok"
end)

-- Catch-all handler for any other operation
route.default(function(params, _)
    return "default handler: " .. (params.path or "?")
end)
