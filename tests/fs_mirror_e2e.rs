mod common;

use common::*;

#[test]
fn fs_mirror() {
    let id = unique_id();
    let root = format!("/tmp/pinhead-e2e-mirror-root-{:x}", id);

    // Create test fixtures.
    std::fs::create_dir_all(&root).expect("create mirror root");
    std::fs::write(format!("{root}/hello.txt"), b"hello from the mirror\n")
        .expect("write hello.txt");
    std::fs::create_dir_all(format!("{root}/subdir")).expect("create subdir");
    std::fs::write(format!("{root}/subdir/nested.txt"), b"nested content\n")
        .expect("write nested.txt");

    let script = include_str!("../examples/fs_mirror.lua")
        .replace(
            "env.get(\"PINHEAD_MIRROR_ROOT\") or \"/tmp/pinhead-mirror-root\"",
            &format!("\"{root}\""),
        );

    let transports = [
        Transport::NinepSock(format!("/tmp/pinhead-e2e-mirror-sock-{:x}.sock", id)),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
        Transport::Fuse(format!("/tmp/pinhead-e2e-mirror-fuse-{:x}", id)),
    ];

    run_scenarios(&script, &transports, |client| {
        // Read hello.txt — verify content from mirrored root.
        let text = client
            .read_file("hello.txt")
            .expect("read hello.txt");
        assert!(
            text.contains("hello from the mirror"),
            "should contain greeting, got: {text}",
        );

        // Read nested file in subdirectory.
        let nested = client
            .read_file("subdir/nested.txt")
            .expect("read subdir/nested.txt");
        assert!(
            nested.contains("nested content"),
            "should contain nested content, got: {nested}",
        );

        // Walk nonexistent path should fail.
        let err = client
            .walk_nonexistent("nonexistent.txt")
            .expect_err("walk nonexistent.txt");
        assert!(!err.is_empty(), "should give error, got: {err}");
    });

    let _ = std::fs::remove_dir_all(&root);
}

/// Write/mutate operations exercised through FUSE against the mirror.
#[test]
fn fs_mirror_writes() {
    let id = unique_id();
    let root = format!("/tmp/pinhead-e2e-mirror-root-{:x}", id);

    // Create test fixtures.
    std::fs::create_dir_all(&root).expect("create mirror root");
    std::fs::write(format!("{root}/hello.txt"), b"original\n")
        .expect("write hello.txt");
    std::fs::create_dir_all(format!("{root}/subdir")).expect("create subdir");

    let script = include_str!("../examples/fs_mirror.lua")
        .replace(
            "env.get(\"PINHEAD_MIRROR_ROOT\") or \"/tmp/pinhead-mirror-root\"",
            &format!("\"{root}\""),
        );

    let (transport, _mountpoint) = (
        Transport::Fuse(format!("/tmp/pinhead-e2e-mirror-write-{:x}", id)),
        format!("/tmp/pinhead-e2e-mirror-write-{:x}", id),
    );

    run_scenarios(&script, &[transport], |client| {
        // ---- file creation ----
        client.write_file("newfile.txt", "created through FUSE\n").unwrap();
        let text = client.read_file("newfile.txt").unwrap();
        assert!(text.contains("created through FUSE"), "create: got {text}");
        // Verify it was mirrored to real fs.
        assert!(
            std::fs::read_to_string(format!("{root}/newfile.txt"))
                .unwrap()
                .contains("created through FUSE"),
        );

        // ---- file deletion ----
        client.remove("newfile.txt").unwrap();
        let err = client.walk_nonexistent("newfile.txt").expect_err("removed file should not exist");
        assert!(!err.is_empty(), "remove: should error, got: {err}");
        assert!(!std::path::Path::new(&format!("{root}/newfile.txt")).exists());

        // ---- file rename ----
        client.write_file("alpha.txt", "rename me").unwrap();
        client.rename("alpha.txt", "beta.txt").unwrap();
        let beta = client.read_file("beta.txt").unwrap();
        assert_eq!(beta, "rename me");
        let err = client.walk_nonexistent("alpha.txt").expect_err("old name should not exist");
        assert!(!err.is_empty(), "rename: old path should error, got: {err}");

        // ---- directory creation ----
        client.create_dir("newdir").unwrap();
        // Verify the directory exists on the real filesystem.
        assert!(std::path::Path::new(&format!("{root}/newdir")).is_dir());
        // Prove traversal through FUSE works: create + read a file inside newdir.
        client.write_file("newdir/innie.txt", "inside dir\n").unwrap();
        let innie = client.read_file("newdir/innie.txt").unwrap();
        assert!(innie.contains("inside dir"), "traverse into newdir: got {innie}");

        // ---- directory deletion ----
        client.remove("newdir/innie.txt").unwrap();
        client.remove("newdir").unwrap();
        assert!(!std::path::Path::new(&format!("{root}/newdir")).exists());

        // ---- chmod ----
        client.write_file("modetest", "check mode").unwrap();
        client.chmod("modetest", 0o600).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&format!("{root}/modetest")).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "chmod should set 0o600, got {mode:o}");
        }

        // ---- write existing file ----
        client.write_file("hello.txt", "overwritten\n").unwrap();
        let text = client.read_file("hello.txt").unwrap();
        assert!(text.contains("overwritten"), "overwrite: got {text}");
        assert!(
            std::fs::read_to_string(format!("{root}/hello.txt"))
                .unwrap()
                .contains("overwritten"),
        );
    });

    let _ = std::fs::remove_dir_all(&root);
}
