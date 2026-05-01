-- fs_mirror.lua — passthrough/mirror filesystem
--
-- Mirrors a real directory tree through pinhead's virtual filesystem.
-- Set PINHEAD_MIRROR_ROOT to the directory to mirror (default: /tmp/pinhead-mirror-root).

local root = env.get("PINHEAD_MIRROR_ROOT") or "/tmp/pinhead-mirror-root"

-- Resolve a virtual path component to a real filesystem path.
local function real_path(rest)
    if rest == nil or rest == "" then
        return root
    end
    return root .. "/" .. rest
end

-- lookup / getattr: stat the real path and return metadata.
local function handle_getattr(params)
    local path = real_path(params.rest)
    local st = fs.stat(path)
    if st == nil then
        error("ENOENT")
    end
    if st.is_dir then
        return "mode=dir size=" .. st.size
    end
    return "mode=file size=" .. st.size
end

route.lookup("/{*rest}", handle_getattr)
route.getattr("/{*rest}", handle_getattr)

-- readdir: list the real directory.
route.register("/{*rest}", "readdir", function(params)
    local path = real_path(params.rest)
    local entries = fs.readdir(path)
    if entries == nil then
        error("ENOENT")
    end
    local lines = {}
    for _, e in ipairs(entries) do
        table.insert(lines, e.name)
    end
    return table.concat(lines, "\n")
end)

-- read: read the real file.
route.register("/{*rest}", "read", function(params)
    local path = real_path(params.rest)
    local content = fs.read(path)
    if content == nil then
        error("ENOENT")
    end
    return content
end)

-- create: create a new file (empty) on the real filesystem.
route.register("/{*rest}", "create", function(params)
    local path = real_path(params.rest)
    local ok = fs.write(path, "")
    if not ok then
        error("EIO")
    end
    return "mode=file size=0"
end)

-- write: overwrite the real file with received data.
route.register("/{*rest}", "write", function(params, data)
    local path = real_path(params.rest)
    if data == nil or data == "" then
        return ""
    end
    local ok = fs.write(path, data)
    if not ok then
        error("EIO")
    end
    return ""
end)

-- mkdir: create a directory on the real filesystem.
route.register("/{*rest}", "mkdir", function(params)
    local path = real_path(params.rest)
    local ok = fs.mkdir(path)
    if not ok then
        error("EIO")
    end
    return "mode=dir size=4096"
end)

-- rmdir: remove a directory on the real filesystem.
route.register("/{*rest}", "rmdir", function(params)
    local path = real_path(params.rest)
    local ok = fs.remove(path)
    if not ok then
        error("ENOENT")
    end
    return ""
end)

-- unlink: remove a file on the real filesystem.
route.register("/{*rest}", "unlink", function(params)
    local path = real_path(params.rest)
    local ok = fs.remove(path)
    if not ok then
        error("ENOENT")
    end
    return ""
end)

-- rename: move a file/dir on the real filesystem.
-- data contains the new virtual path (set by the FUSE frontend).
route.register("/{*rest}", "rename", function(params, data)
    local old_path = real_path(params.rest)
    -- data is the full new virtual path (e.g., "/beta.txt") — strip leading /
    local new_rest = data:sub(2)
    if new_rest == "" then
        new_rest = nil
    end
    local new_path = real_path(new_rest)
    local ok = fs.rename(old_path, new_path)
    if not ok then
        error("ENOENT")
    end
    return ""
end)

-- setattr: change attributes on the real filesystem.
-- data is a semicolon-separated string of key=value pairs.
route.register("/{*rest}", "setattr", function(params, data)
    local path = real_path(params.rest)
    if data ~= nil and data ~= "" then
        for part in string.gmatch(data, "([^;]+)") do
            local key, val = string.match(part, "([^=]+)=(.+)")
            if key == "mode" then
                fs.chmod(path, tonumber(val))
            elseif key == "uid" then
                -- chown requires both uid and gid; if only uid is set, use -1 (no change)
                -- but we don't have -1 semantics, so skip chown for now
            elseif key == "gid" then
                -- same
            elseif key == "mtime" then
                local st = fs.stat(path)
                if st then
                    fs.utimens(path, tonumber(val), tonumber(val))
                end
            elseif key == "size" then
                -- truncate not directly supported by fs.*
            end
        end
    end
    return handle_getattr(params)
end)

-- open / release: no-op.
route.open("/{*rest}", function() return "" end)
route.release("/{*rest}", function() return "" end)
route.register("/{*rest}", "opendir", function() return "" end)
route.register("/{*rest}", "releasedir", function() return "" end)
route.register("/{*rest}", "flush", function() return "" end)
route.register("/{*rest}", "fsync", function() return "" end)
route.register("/{*rest}", "fsyncdir", function() return "" end)

-- Default handler for unmatched paths (shouldn't happen with wildcard).
route.default(function(_, _)
    return ""
end)

-- Setup frontends.
if env.get("PINHEAD_LISTEN") then
    ninep.listen(env.get("PINHEAD_LISTEN"))
end
if env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
end
if env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
end
