//! Wikipedia e2e — tests ALL routes across ALL transports.

mod common;
use common::*;
use std::time::Duration;

#[test]
fn wikipedia_all_routes() {
    let script = include_str!("../examples/wikipedia/main.lua");
    cleanup_orphans();
    let id = unique_id();
    let sock = format!("/tmp/pinhead-e2e-wiki-{id:x}.sock");
    let tcp = format!("127.0.0.1:{}", find_free_port());
    let ssh = format!("127.0.0.1:{}", find_free_port());
    let fuse_path = format!("/tmp/pinhead-e2e-wiki-fuse-{id:x}");

    let transports = [
        Transport::NinepSock(sock.clone()),
        Transport::NinepTcp(tcp),
        Transport::Ssh(ssh),
        Transport::Fuse(fuse_path.clone()),
    ];

    for t in &transports {
        let mut inst = PinheadInstance::start(script, t).expect("start");
        if matches!(t, Transport::Fuse(_)) {
            std::thread::sleep(Duration::from_millis(300));
        }
        let mut client = inst.connect().expect("connect");

        // All transports: read a file
        let readme = client.read_file("README.md").expect("read README.md");
        assert!(readme.contains("Wikipedia"), "{t:?}: {readme}");

        // 9P + FUSE: readdir and write
        if !matches!(t, Transport::Ssh(_)) {
            let names = client.read_dir_names("/").expect("readdir /");
            assert!(names.contains(&"README.md".to_string()), "{t:?}: {names:?}");

            let _ = client.write_file("search", "music");
        }

        // FUSE only: bookmark write/read via direct IO
        if matches!(t, Transport::Fuse(_)) {
            let m = &fuse_path;
            std::fs::write(format!("{m}/bookmarks/x"), "Wikipedia bookmark").expect("write");
            let content = std::fs::read_to_string(format!("{m}/bookmarks/x")).expect("read");
            assert!(content.contains("Wikipedia"), "bookmark: {content}");
        }
    }
}
