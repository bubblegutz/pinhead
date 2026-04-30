//! End-to-end test exercising filesystem operations across transports.

mod common;
use common::*;

fn all_transports() -> Vec<Transport> {
    let sock = format!("/tmp/pinhead-e2e-allops-sock-{:x}.sock", unique_id());
    let tcp = format!("127.0.0.1:{}", find_free_port());
    let udp = format!("127.0.0.1:{}", find_free_port());
    let ssh = format!("127.0.0.1:{}", find_free_port());
    let fuse = format!("/tmp/pinhead-e2e-allops-fuse-{:x}", unique_id());
    vec![
        Transport::NinepSock(sock),
        Transport::NinepTcp(tcp),
        Transport::NinepUdp(udp),
        Transport::Ssh(ssh),
        Transport::Fuse(fuse),
    ]
}

const SCRIPT: &str = r#"
local addr = os.getenv("PINHEAD_LISTEN") or "sock:/tmp/default.sock"
local ssh_addr = os.getenv("PINHEAD_SSH_LISTEN") or "127.0.0.1:2222"
local fuse_mount = os.getenv("PINHEAD_FUSE_MOUNT") or "/tmp/pinhead-fuse"

ninep.listen(addr)
sshfs.userpasswd("alice", "hunter2")
sshfs.listen(ssh_addr)
fuse.mount(fuse_mount)
worker.min(1)

route.read("/readme", function(_, _) return "read ok" end)
route.write("/writeme", function(_, _) return "write ok" end)
route.create("/createme", function(_, _) return "create ok" end)
route.mkdir("/newdir", function(_, _) return "mkdir ok" end)
route.unlink("/deleteme", function(_, _) return "unlink ok" end)
route.readdir("/", function(_, _) return "dir\n" end)
route.all("/allops", function(_, _) return "all ok" end)
"#;

#[test]
fn all_ops_read() {
    for t in &all_transports() {
        let mut inst = PinheadInstance::start(SCRIPT, t).expect("start");
        let mut client = inst.connect().expect("connect");

        let text = client.read_file("readme").expect("read /readme");
        assert!(text.contains("read ok"), "{t:?}: {text}");

        let text = client.read_file("allops").expect("read /allops");
        assert!(text.contains("all ok"), "{t:?}: {text}");
    }
}
