-- fs_ops.lua — exercise every fs.* API function against a temp directory
--
-- Runs all operations during script init and exposes results at /results.
-- Covers: mkdir_all, write, read, readdir, stat, copy, remove, remove_all,
-- rename, chmod, mode_string, utimens.

-- Unique temp directory using os.tmpname as a base
local base = os.tmpname()
os.remove(base)
base = base .. "-fs-ops"

local results = {}
local function pass(name)
    table.insert(results, "PASS " .. name)
end
local function fail(name, msg)
    table.insert(results, "FAIL " .. name .. ": " .. tostring(msg))
end

-- Helper: wrap a fs.* call and report pass/fail
local function check(name, ok_val, expected)
    if expected == nil then
        -- just check truthy
        if ok_val then
            pass(name)
        else
            fail(name, "returned nil/false")
        end
    elseif type(expected) == "function" then
        -- custom check
        if expected(ok_val) then
            pass(name)
        else
            fail(name, "unexpected value: " .. tostring(ok_val))
        end
    else
        -- exact match
        if ok_val == expected then
            pass(name)
        else
            fail(name, "expected " .. tostring(expected) .. ", got " .. tostring(ok_val))
        end
    end
end

-- 1. fs.mkdir_all — create nested directories a/b/c
local nested = base .. "/a/b/c"
if fs.mkdir_all(nested) then
    local st = fs.stat(nested)
    check("mkdir_all", st ~= nil and st.is_dir, true)
else
    fail("mkdir_all", "could not create " .. nested)
end

-- 2. fs.write + fs.read roundtrip
local fpath = nested .. "/hello.txt"
if fs.write(fpath, "world") then
    local content = fs.read(fpath)
    check("write+read", content, "world")
else
    fail("write+read", "write returned nil")
end

-- 3. fs.readdir on the nested directory (should show hello.txt)
local entries = fs.readdir(nested)
if entries then
    local names = {}
    for _, e in ipairs(entries) do
        table.insert(names, e.name)
    end
    local found = false
    for _, n in ipairs(names) do
        if n == "hello.txt" then found = true end
    end
    check("readdir", found, true)
else
    fail("readdir", "readdir returned nil")
end

-- 4. fs.copy — copy hello.txt to hello-copy.txt
local copy_path = nested .. "/hello-copy.txt"
if fs.copy(fpath, copy_path) then
    local content = fs.read(copy_path)
    check("copy", content, "world")
else
    fail("copy", "copy returned nil")
end

-- 5. fs.rename — rename copy to hello-renamed.txt
local rename_path = nested .. "/hello-renamed.txt"
if fs.rename(copy_path, rename_path) then
    -- verify old path is gone
    local old_st = fs.stat(copy_path)
    -- verify new path exists with content
    local new_st = fs.stat(rename_path)
    local new_content = fs.read(rename_path)
    check("rename", old_st == nil and new_st ~= nil and new_content == "world", true)
else
    fail("rename", "rename returned nil")
end

-- 6. fs.remove — remove hello-renamed.txt
if fs.remove(rename_path) then
    local st = fs.stat(rename_path)
    check("remove", st == nil, true)
else
    fail("remove", "remove returned nil")
end

-- 7. fs.mode_string — format octal modes
check("mode_string(755)", fs.mode_string(tonumber("755", 8)), "rwxr-xr-x")
check("mode_string(644)", fs.mode_string(tonumber("644", 8)), "rw-r--r--")
check("mode_string(600)", fs.mode_string(tonumber("600", 8)), "rw-------")
check("mode_string(000)", fs.mode_string(0), "---------")

-- 8. fs.chmod — change mode on hello.txt
if fs.chmod(fpath, tonumber("600", 8)) then
    local st = fs.stat(fpath)
    if st and st.mode then
        -- Compare just the permission bits
        local perm_bits = st.mode % 512  -- 0o7777 -> 0o777
        check("chmod", perm_bits == tonumber("600", 8), true)
    else
        fail("chmod", "stat after chmod returned nil or no mode")
    end
else
    fail("chmod", "chmod returned nil")
end

-- 9. fs.utimens — set mtime to epoch 1000000
if fs.utimens(fpath, 1000000, 1000000) then
    local st = fs.stat(fpath)
    if st and st.mtime then
        check("utimens", st.mtime >= 1000000 and st.mtime < 2000000, true)
    else
        fail("utimens", "stat after utimens returned nil or no mtime")
    end
else
    fail("utimens", "utimens returned nil")
end

-- 11. fs.stat — verify stat on a regular file and nonexistent
local st_file = fs.stat(fpath)
check("stat(file)", st_file ~= nil and not st_file.is_dir, true)
local st2 = fs.stat("/nonexistent-path-fs-ops-test-xyzzy")
check("stat(nonexistent)", st2 == nil, true)

-- 12. fs.remove_all — recursively delete a/b
if fs.remove_all(base .. "/a") then
    local sta = fs.stat(base .. "/a")
    check("remove_all", sta == nil, true)
else
    fail("remove_all", "remove_all returned nil")
end

-- 13. fs.mkdir — single directory creation
local singledir = base .. "/singledir"
if fs.mkdir(singledir) then
    local st = fs.stat(singledir)
    check("mkdir", st ~= nil and st.is_dir, true)
else
    fail("mkdir", "mkdir returned nil")
end

-- Final cleanup
fs.remove_all(base)

-- Aggregate results
local pass_count = 0
local fail_count = 0
for _, line in ipairs(results) do
    if line:find("^PASS") then
        pass_count = pass_count + 1
    elseif line:find("^FAIL") then
        fail_count = fail_count + 1
    end
end
local summary = string.format("%d passed, %d failed", pass_count, fail_count)
table.insert(results, 1, summary)

local result_text = table.concat(results, "\n")

-- Expose results as a virtual file
route.lookup("/results", function()
    return "results"
end)
route.getattr("/results", function()
    return "mode=file size=" .. #result_text
end)
route.open("/results", function()
    return ""
end)
route.release("/results", function()
    return ""
end)
route.read("/results", function()
    return result_text
end)

route.all("/", function()
    return ""
end)
route.register("/", "readdir", function()
    return "results"
end)

route.default(function()
    return ""
end)

-- Standard frontend setup
if env.get("PINHEAD_LISTEN") then
    ninep.listen(env.get("PINHEAD_LISTEN"))
end
if env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
end
if env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
end
