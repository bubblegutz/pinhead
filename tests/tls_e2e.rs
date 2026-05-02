use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::Duration;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn mux(w: &mut impl Write, id: u32, p: &[u8]) {
    w.write_all(&id.to_le_bytes()).unwrap();
    w.write_all(&(p.len() as u32).to_le_bytes()).unwrap();
    w.write_all(p).unwrap();
}

fn t9p(w: &mut impl Write, t: u8, body: &[u8]) {
    let size = 7 + body.len();
    let mut msg = Vec::with_capacity(size);
    msg.extend_from_slice(&(size as u32).to_le_bytes());
    msg.push(t);
    msg.extend_from_slice(&0u16.to_le_bytes());
    msg.extend_from_slice(body);
    mux(w, 1, &msg);
}

fn r9p(r: &mut dyn Read) -> Vec<u8> {
    let mut h = [0u8; 8];
    if r.read_exact(&mut h).is_err() { return vec![]; }
    let plen = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
    let mut p = vec![0u8; plen];
    if plen > 0 { let _ = r.read_exact(&mut p); }
    p
}

fn walk(p: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&1u32.to_le_bytes());    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&(p.len() as u16).to_le_bytes()); b.extend_from_slice(p.as_bytes());
    b
}

#[test]
fn tls_full_session() {
    let port = free_port();
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
    for _ in 0..30 { if TcpStream::connect(&addr).is_ok() { break; }
        std::thread::sleep(Duration::from_millis(100)); }

    let mut ssl = Command::new("openssl")
        .args(["s_client", "-connect", &addr, "-quiet"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().unwrap();
    let mut si = ssl.stdin.take().unwrap();
    let mut so = ssl.stdout.take().unwrap();

    // Tversion
    let ver = b"9P2000";
    let mut b = Vec::new();
    b.extend_from_slice(&65536u32.to_le_bytes());
    b.extend_from_slice(&(ver.len() as u16).to_le_bytes());
    b.extend_from_slice(ver);
    t9p(&mut si, 100, &b);
    assert_eq!(r9p(&mut so)[4], 101, "Rversion");

    // Tattach: fid=1, afid=NOFID, uname="none", aname=""
    let mut a = Vec::new();
    a.extend_from_slice(&1u32.to_le_bytes());
    a.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes());
    a.extend_from_slice(&4u16.to_le_bytes()); a.extend_from_slice(b"none");
    a.extend_from_slice(&0u16.to_le_bytes());
    t9p(&mut si, 104, &a);
    assert_eq!(r9p(&mut so)[4], 105, "Rattach");

    // Twalk: fid=1, newfid=2, /hello
    t9p(&mut si, 110, &walk("hello"));
    assert_eq!(r9p(&mut so)[4], 111, "Rwalk");

    // Topen: fid=2, mode=0
    t9p(&mut si, 112, &[2, 0, 0, 0, 0]);
    assert_eq!(r9p(&mut so)[4], 113, "Ropen");

    // Tread: fid=2, offset=0, count=4096
    let mut rd = Vec::new();
    rd.extend_from_slice(&2u32.to_le_bytes());
    rd.extend_from_slice(&0u64.to_le_bytes());
    rd.extend_from_slice(&4096u32.to_le_bytes());
    t9p(&mut si, 116, &rd);
    let r = r9p(&mut so);
    assert_eq!(r[4], 117, "Rread");
    let count = u32::from_le_bytes(r[7..11].try_into().unwrap()) as usize;
    let data = String::from_utf8_lossy(&r[11..11+count]);
    assert!(data.contains("world"), "content: {data}");

    drop(si);
    let _ = ph.kill(); let _ = ph.wait(); let _ = ssl.wait();
}
