mod common;

use common::*;

#[test]
fn test_bundle_defaults() {
    let script = include_str!("../examples/bundle_defaults/main.lua");
    // FUSE excluded: kernel-level path resolution requires intermediate
    // components (e.g. `docs` in `/docs/manual.pdf`) to be marked as
    // directories. Bundle defaults match /{*path} but return file-type
    // entries, causing ENOTDIR on intermediate components.
    let transports = [
        Transport::NinepSock(format!(
            "/tmp/pinhead-e2e-bundle-defaults-sock-{:x}.sock",
            unique_id()
        )),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
    ];

    run_scenarios(script, &transports, |client| {
        // ── Specific route.read path → handler returns "Document: <name>" ──
        let text = client
            .read_file("docs/readme.txt")
            .expect("read docs/readme.txt");
        assert_eq!(
            text, "Document: readme.txt",
            "specific route.read should match /docs/{{name}}, got: {text:?}"
        );

        // ── Specific route.read with a different name ──
        let text = client
            .read_file("docs/manual.pdf")
            .expect("read docs/manual.pdf");
        assert_eq!(
            text, "Document: manual.pdf",
            "specific route.read with different name, got: {text:?}"
        );

        // ── Unmatched path → whichever bundle default registered last
        //    for the shared ops (lookup, open, read) wins the merge.
        //    Since route.create.default is registered after route.read.default,
        //    it overwrites the read ops on /{*path}. ──
        let text = client
            .read_file("other/random")
            .expect("read other/random via bundle default");
        assert_eq!(
            text, "Default create: other/random",
            "bundle default (last wins for shared ops), got: {text:?}"
        );

        // ── Deep nested unmatched path ──
        let text = client
            .read_file("a/b/c/d/e")
            .expect("read deep nested path via bundle default");
        assert_eq!(
            text, "Default create: a/b/c/d/e",
            "deep path via bundle default, got: {text:?}"
        );
    });
}

#[test]
fn test_bundle_defaults_nonexistent() {
    let script = include_str!("../examples/bundle_defaults/main.lua");
    // FUSE excluded — same reason as test_bundle_defaults.
    let transports = [
        Transport::NinepSock(format!(
            "/tmp/pinhead-e2e-bd-nonexistent-sock-{:x}.sock",
            unique_id()
        )),
        Transport::NinepTcp(format!("127.0.0.1:{}", find_free_port())),
        Transport::NinepUdp(format!("127.0.0.1:{}", find_free_port())),
        Transport::Ssh(format!("127.0.0.1:{}", find_free_port())),
    ];

    run_scenarios(script, &transports, |client| {
        // Since route.read.default registers /{*path} with lookup,
        // ALL paths are technically walkable. We verify that specific
        // paths still work and non-specific paths return the default
        // handler's content.

        // Specific route.write path works (notes path — write bundle includes
        // lookup, getattr, open, read so read_file works)
        let text = client
            .read_file("notes/shopping")
            .expect("read notes/shopping");
        assert_eq!(
            text, "Note: shopping",
            "route.write specific match for notes path, got: {text:?}"
        );

        // Specific route.create path works (create bundle includes lookup,
        // getattr, open, read so read_file works)
        let text = client
            .read_file("sessions/abc123")
            .expect("read sessions/abc123");
        assert_eq!(
            text, "Session: abc123",
            "route.create specific match, got: {text:?}"
        );

        // Default handler for unmatched paths — last registered bundle
        // default for shared ops wins (route.create.default for read ops)
        let text = client
            .read_file("unknown/stuff")
            .expect("read unknown/stuff via default");
        assert_eq!(
            text, "Default create: unknown/stuff",
            "read ops fall back to last bundle default, got: {text:?}"
        );
    });
}
