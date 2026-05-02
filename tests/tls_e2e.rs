use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::Duration;

fn find_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn send_9p_msg(w: &mut impl Write, msg_type: u8, body: &[u8]) {
    let size = 7 + body.len();
    w.write_all(&(size as u32).to_le_bytes()).unwrap();
    w.write_all(&[msg_type]).unwrap();
    w.write_all(&0u16.to_le_bytes()).unwrap();
    w.write_all(body).unwrap();
}

fn recv_9p(r: &mut dyn Read) -> (u8, Vec<u8>) {
    let mut h = [0u8; 7];
    if r.read_exact(&mut h).is_err() { return (0, vec![]); }
    let sz = u32::from_le_bytes(h[0..4].try_into().unwrap()) as usize;
    let mut b = vec![0u8; sz.saturating_sub(7)];
    if !b.is_empty() { let _ = r.read_exact(&mut b); }
    (h[4], b)
}

#[test]
fn tls_basic() {
    let port = find_free_port();
    let addr = format!("127.0.0.1:{port}");

    let script = format!(
        "ninep.tls_cert(\"/tmp/pinhead-tls-cert.pem\")\n\
         ninep.tls_key(\"/tmp/pinhead-tls-key.pem\")\n\
         ninep.listen(\"tls:{addr}\")\n\
         route.all(\"/hello\",function(_,_)return\"world\"end)"
    );
    let sp = format!("/tmp/pt-{:x}.lua", std::process::id());
    std::fs::write(&sp, &script).unwrap();
    let mut ph = Command::new(std::env!("CARGO_BIN_EXE_ph"))
        .arg(&sp).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();

    for _ in 0..30 {
        if TcpStream::connect(&addr).is_ok() { break; }
        std::thread::sleep(Duration::from_millis(100));
    }

    let mut ssl = Command::new("openssl")
        .args(["s_client", "-connect", &addr, "-quiet"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().unwrap();

    let mut sin = ssl.stdin.take().unwrap();
    let ver = b"9P2000";
    let mut body = Vec::new();
    body.extend_from_slice(&65536u32.to_le_bytes());
    body.extend_from_slice(&(ver.len() as u16).to_le_bytes());
    body.extend_from_slice(ver);
    send_9p_msg(&mut sin, 100, &body); // Tversion
    drop(sin);

    let mut sout = ssl.stdout.take().unwrap();
    let (t, resp) = recv_9p(&mut sout);
    assert_eq!(t, 101, "expected Rversion, got type={t}");
    assert!(resp.len() >= 4, "Rversion too short");
    assert!(u32::from_le_bytes(resp[0..4].try_into().unwrap()) >= 512);

    let _ = ph.kill(); let _ = ph.wait(); let _ = ssl.wait();
}
