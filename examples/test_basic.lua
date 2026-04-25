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

-- Listeners — all sockets come from here, not from Rust.
ninep.listen("sock:/tmp/pinhead-test-basic.sock")
