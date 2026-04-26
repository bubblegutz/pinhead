use std::time::Instant;

mod common;
use common::*;

#[test]
fn ssh_simple_http() {
    let script = r#"
route.register("/", {"lookup", "getattr", "read", "open", "release"}, function()
    local ok, res = pcall(req.get, "http://example.com/")
    if not ok then
        return "HTTP Error: " .. tostring(res)
    end
    return "Got " .. tostring(#res) .. " bytes"
end)

local users = {{"alice", "hunter2"}}
for _, pair in ipairs(users) do
    sshfs.userpasswd(pair[1], pair[2])
end
local addr = os.getenv("PINHEAD_LISTEN") or "sock:/tmp/pinhead-simple-http.sock"
ninep.listen(addr)
local ssh_addr = os.getenv("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
sshfs.listen(ssh_addr)
"#;

    let addr = format!("127.0.0.1:{}", find_free_port());
    let transport = Transport::Ssh(addr);

    let mut inst = PinheadInstance::start(script, &transport).expect("start");
    let mut client = inst.connect().expect("connect");

    let start = Instant::now();
    let text = client.read_file("").expect("read / via SSH");
    let elapsed = start.elapsed();
    eprintln!("SSH + simple HTTP: {elapsed:?} — {text}");
    assert!(text.contains("bytes"), "got: {text}");

    // Kill pinhead before dropping the SSH client, otherwise
    // ssh2::Session::drop blocks waiting for graceful disconnect.
    drop(inst);
}
