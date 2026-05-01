mod common;

use common::*;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

const MINIMAL_SCRIPT: &str = r#"
route.register("/testfile.txt", "lookup", function(params, data)
    return "found testfile.txt"
end)
route.register("/testfile.txt", "getattr", function(params, data)
    return 'mode=file size=16'
end)
route.register("/testfile.txt", "open", function(params, data)
    return "opened"
end)
route.register("/testfile.txt", "release", function(params, data)
    return "released"
end)
route.register("/testfile.txt", "read", function(params, data)
    return "hello from shebang test!"
end)
route.default(function(params, data)
    return "unmatched"
end)

local a = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-e2e-cli.sock"
ninep.listen(a)
"#;

const SHEBANG_SCRIPT: &str = "#!/path/to/pinhead\n-- shebang test script\n";

#[test]
fn test_shebang_execution() {
    let id = unique_id();
    let script_path = format!("/tmp/pinhead-e2e-shebang-{:x}.lua", id);
    let sock_path = format!("/tmp/pinhead-e2e-shebang-{:x}.sock", id);

    // Write script with shebang line at the top.
    let full_script = format!("{}{}", SHEBANG_SCRIPT, MINIMAL_SCRIPT);
    fs::write(&script_path, &full_script).expect("write shebang script");

    let binary = std::env!("CARGO_BIN_EXE_ph");
    let mut child = Command::new(binary)
        .arg(&script_path)
        .env("PINHEAD_LISTEN", format!("sock:{}", sock_path))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn pinhead with shebang script");

    // Wait for socket, then verify the filesystem works.
    wait_for_socket(&sock_path).expect("shebang socket ready");

    let mut client = NinepClient::connect_unix(&sock_path).expect("connect shebang");
    setup_client(&mut client).expect("setup shebang");
    let text = client
        .read_file("testfile.txt")
        .expect("read testfile.txt via shebang");
    assert_eq!(text, "hello from shebang test!");

    // Cleanup.
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&script_path);
    let _ = fs::remove_file(&sock_path);
}

#[test]
fn test_piped_execution() {
    let id = unique_id();
    let sock_path = format!("/tmp/pinhead-e2e-pipe-{:x}.sock", id);

    let binary = std::env!("CARGO_BIN_EXE_ph");
    let mut child = Command::new(binary)
        .env("PINHEAD_LISTEN", format!("sock:{}", sock_path))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn pinhead with piped stdin");

    // Pipe the script to pinhead's stdin.
    let mut stdin = child.stdin.take().expect("take piped stdin");
    stdin
        .write_all(MINIMAL_SCRIPT.as_bytes())
        .expect("write script to stdin");
    drop(stdin); // close stdin to signal EOF

    // Wait for socket, then verify the filesystem works.
    wait_for_socket(&sock_path).expect("pipe socket ready");

    let mut client = NinepClient::connect_unix(&sock_path).expect("connect pipe");
    setup_client(&mut client).expect("setup pipe");
    let text = client
        .read_file("testfile.txt")
        .expect("read testfile.txt via pipe");
    assert_eq!(text, "hello from shebang test!");

    // Cleanup.
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&sock_path);
}

#[test]
fn test_bundle_default() {
    let id = unique_id();
    let sock_path = format!("/tmp/pinhead-e2e-bundle-default-{:x}.sock", id);

    // Script with a specific route.read and a route.read.default fallback.
    let script = format!(
        r#"
route.read("/specific/readme.txt", function(params, data)
    return "specific content"
end)

route.read.default(function(params, data)
    -- Handles all read-bundle ops (lookup, getattr, open, read, release, flush)
    -- on any unmatched path.
    return "default content"
end)

local a = env.get("PINHEAD_LISTEN") or "sock:/tmp/pinhead-e2e-bd.sock"
ninep.listen(a)
"#
    );

    let sock = format!("sock:{}", sock_path);
    let binary = std::env!("CARGO_BIN_EXE_ph");
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .env("PINHEAD_LISTEN", &sock)
        .spawn()
        .expect("spawn pinhead with bundle default");

    let mut stdin = child.stdin.take().expect("take piped stdin");
    stdin
        .write_all(script.as_bytes())
        .expect("write script to stdin");
    drop(stdin);

    wait_for_socket(&sock_path).expect("bundle default socket ready");

    let mut client = NinepClient::connect_unix(&sock_path).expect("connect bundle default");
    setup_client(&mut client).expect("setup bundle default");

    // Specific path should return its handler's content.
    let text = client
        .read_file("specific/readme.txt")
        .expect("read specific/readme.txt");
    assert_eq!(text, "specific content", "specific route should win");

    // Unmatched path should fall back to the bundle default.
    let text = client
        .read_file("other/random.txt")
        .expect("read other/random.txt via bundle default");
    assert_eq!(text, "default content", "bundle default should handle unmatched paths");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&sock_path);
}
