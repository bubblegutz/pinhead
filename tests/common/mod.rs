#![allow(dead_code)]
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

// ── Combined stream trait ────────────────────────────────────────────────────

pub trait NinepStream: Read + Write {}
impl NinepStream for UnixStream {}
impl NinepStream for TcpStream {}

// ── Transport abstraction ──────────────────────────────────────────────────

/// All frontend protocols available in pinhead.
#[derive(Debug, Clone)]
pub enum Transport {
    /// 9P2000 over Unix socket.
    NinepSock(String),
    /// 9P2000 over TCP.
    NinepTcp(String),
    /// 9P2000 over UDP (datagram-based, non-standard but supported).
    NinepUdp(String),
    /// SSH/SFTP over TCP (password auth).
    Ssh(String),
    /// FUSE mount (local directory).
    Fuse(String),
}

impl Transport {
    /// The value for `PINHEAD_LISTEN` (or `PINHEAD_SSH_LISTEN` for SSH).
    pub fn listen_str(&self) -> String {
        match self {
            Transport::NinepSock(p) => format!("sock:{p}"),
            Transport::NinepTcp(a) => format!("tcp:{a}"),
            Transport::NinepUdp(a) => format!("udp:{a}"),
            Transport::Ssh(a) => a.clone(),
            Transport::Fuse(p) => p.clone(),
        }
    }
}

// ── TestClient trait ──────────────────────────────────────────────────────

/// Unified interface so every test function works with any transport.
pub trait TestClient {
    /// Walk → open → read → close and return decoded text.
    fn read_file(&mut self, path: &str) -> Result<String, String>;

    /// Attempt to walk/open a non-existent path, returning the error string.
    fn walk_nonexistent(&mut self, path: &str) -> Result<String, String>;

    /// Write content to a file (create if not exist).
    fn write_file(&mut self, path: &str, content: &str) -> Result<(), String> {
        let _ = (path, content);
        Err("write_file not implemented for this transport".into())
    }

    /// Remove a file or empty directory.
    fn remove(&mut self, path: &str) -> Result<(), String> {
        let _ = path;
        Err("remove not implemented for this transport".into())
    }

    /// Rename/move a file or directory.
    fn rename(&mut self, old: &str, new: &str) -> Result<(), String> {
        let _ = (old, new);
        Err("rename not implemented for this transport".into())
    }

    /// Create a directory.
    fn create_dir(&mut self, path: &str) -> Result<(), String> {
        let _ = path;
        Err("create_dir not implemented for this transport".into())
    }

    /// Change file mode/permissions.
    fn chmod(&mut self, path: &str, mode: u32) -> Result<(), String> {
        let _ = (path, mode);
        Err("chmod not implemented for this transport".into())
    }

    /// List directory entry names.
    fn read_dir_names(&mut self, path: &str) -> Result<Vec<String>, String> {
        let _ = path;
        Err("read_dir_names not implemented for this transport".into())
    }
}

// ── Port allocation ────────────────────────────────────────────────────────

pub fn find_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    listener.local_addr().unwrap().port()
}

// ── Unique ID for test isolation ───────────────────────────────────────────

static NEXT_ID: AtomicU16 = AtomicU16::new(1);
pub fn unique_id() -> u64 {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed) as u64;
    (std::process::id() as u64) << 16 | n
}

// ── Pinhead instance (env-var-based spawn + cleanup) ───────────────────────

pub struct PinheadInstance {
    child: Child,
    transport: Transport,
    cleanup_paths: Vec<String>,
}

impl PinheadInstance {
    /// Spawn pinhead with the given script content and transport.
    pub fn start(script: &str, transport: &Transport) -> Result<Self, String> {
        let id = unique_id();
        let script_path = format!("/tmp/pinhead-e2e-{:x}.lua", id);
        fs::write(&script_path, script).map_err(|e| format!("write script: {e}"))?;

        let listen_val = transport.listen_str();
        let binary = std::env!("CARGO_BIN_EXE_ph");
        let mut cmd = Command::new(binary);
        cmd.arg(&script_path)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());

        match transport {
            Transport::Ssh(_) => {
                cmd.env("PINHEAD_SSH_LISTEN", &listen_val);
                let placeholder = format!("/tmp/pinhead-e2e-ssh-placeholder-{:x}.sock", id);
                cmd.env("PINHEAD_LISTEN", format!("sock:{}", placeholder));
            }
            Transport::Fuse(_) => {
                cmd.env("PINHEAD_FUSE_MOUNT", &listen_val);
                let placeholder = format!("/tmp/pinhead-e2e-fuse-placeholder-{:x}.sock", id);
                cmd.env("PINHEAD_LISTEN", format!("sock:{}", placeholder));
            }
            _ => {
                cmd.env("PINHEAD_LISTEN", &listen_val);
            }
        }

        let child = cmd.spawn().map_err(|e| format!("spawn pinhead: {e}"))?;

        let mut cleanup = vec![script_path];

        let ready = match transport {
            Transport::NinepSock(path) => {
                cleanup.push(path.clone());
                wait_for_socket(path)
            }
            Transport::NinepTcp(_) => Ok(()),
            Transport::NinepUdp(addr) => wait_for_udp(addr),
            Transport::Ssh(addr) => {
                let placeholder = format!("/tmp/pinhead-e2e-ssh-placeholder-{:x}.sock", id);
                cleanup.push(placeholder);
                wait_for_port(addr)
            }
            Transport::Fuse(path) => {
                cleanup.push(path.clone());
                let placeholder = format!("/tmp/pinhead-e2e-fuse-placeholder-{:x}.sock", id);
                cleanup.push(placeholder);
                fs::create_dir_all(path).map_err(|e| format!("create mount dir: {e}"))?;
                wait_for_fuse_mount(path)
            }
        };

        if let Err(e) = ready {
            // Kill the child we already spawned before returning — prevents
            // orphaned pinhead processes that hold ports/sockets.
            let _ = Command::new("kill")
                .args(["-9", &child.id().to_string()])
                .status();
            return Err(e);
        }

        Ok(Self {
            child,
            transport: transport.clone(),
            cleanup_paths: cleanup,
        })
    }

    /// Open a test client connection matching this instance's transport.
    pub fn connect(&mut self) -> Result<Box<dyn TestClient>, String> {
        match &self.transport {
            Transport::NinepSock(path) => {
                let mut client = NinepClient::connect_unix(path)?;
                setup_client(&mut client)?;
                Ok(Box::new(client))
            }
            Transport::NinepTcp(addr) => {
                let mut client = NinepClient::connect_tcp(addr)?;
                setup_client(&mut client)?;
                Ok(Box::new(client))
            }
            Transport::NinepUdp(addr) => {
                let mut client = UdpNinepClient::connect(addr)?;
                setup_client_udp(&mut client)?;
                Ok(Box::new(client))
            }
            Transport::Ssh(addr) => {
                let client = SshClient::connect(addr, "alice", "hunter2")?;
                Ok(Box::new(client))
            }
            Transport::Fuse(path) => {
                Ok(Box::new(FuseClient { mountpoint: path.clone() }))
            }
        }
    }
}

impl Drop for PinheadInstance {
    fn drop(&mut self) {
        // Kill child first. Pure-Rust fuser handles unmount internally
        // via BackgroundSession drop when the child process exits.
        let _ = self.child.kill();
        for _ in 0..100 {
            if self.child.try_wait().unwrap().is_some() { break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for p in &self.cleanup_paths {
            if std::path::Path::new(p).is_dir() {
                let _ = fs::remove_dir_all(p);
            } else {
                let _ = fs::remove_file(p);
            }
        }
    }
}

// ── Readiness detection ────────────────────────────────────────────────────

pub fn wait_for_socket(path: &str) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if std::path::Path::new(path).exists() {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(5) {
            return Err(format!("socket {path} did not appear within 5s"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn wait_for_port(addr: &str) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(5) {
            return Err(format!("tcp {addr} did not accept within 5s"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn wait_for_udp(addr: &str) -> Result<(), String> {
    let start = Instant::now();
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("bind udp probe: {e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();

    let mut body = Vec::new();
    body.extend_from_slice(&65536u32.to_le_bytes());
    let version = b"9P2000";
    body.extend_from_slice(&(version.len() as u16).to_le_bytes());
    body.extend_from_slice(version);
    let version_req = build_9p_msg(100, 0, &body);

    loop {
        let _ = socket.send_to(&version_req, addr);
        let mut buf = [0u8; 64];
        if socket.recv_from(&mut buf).is_ok() {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(5) {
            return Err(format!("udp {addr} did not respond within 5s"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn wait_for_fuse_mount(path: &str) -> Result<(), String> {
    let start = Instant::now();
    // First wait for the mount directory to exist.
    loop {
        if let Ok(m) = std::fs::metadata(path)
            && m.is_dir() {
                break;
            }
        if start.elapsed() > Duration::from_secs(5) {
            return Err(format!("fuse mount dir {path} did not appear within 5s"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Then wait for the FUSE mount to appear in /proc/mounts.
    loop {
        if let Ok(mounts) = std::fs::read_to_string("/proc/mounts")
            && mounts.lines().any(|line| line.contains(path)) {
                return Ok(());
            }
        if start.elapsed() > Duration::from_secs(5) {
            return Err(format!("fuse mount {path} did not appear in /proc/mounts within 5s"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ── 9P message helpers ─────────────────────────────────────────────────────

pub fn build_9p_msg(msg_type: u8, tag: u16, body: &[u8]) -> Vec<u8> {
    let size = 7 + body.len();
    let mut buf = Vec::with_capacity(size);
    buf.extend_from_slice(&(size as u32).to_le_bytes());
    buf.push(msg_type);
    buf.extend_from_slice(&tag.to_le_bytes());
    buf.extend_from_slice(body);
    buf
}

pub fn read_9p_reply(stream: &mut dyn Read) -> (u8, Vec<u8>) {
    let mut header = [0u8; 7];
    if stream.read_exact(&mut header).is_err() {
        return (0, Vec::new());
    }
    let msg_type = header[4];
    let size = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
    let body_len = size.saturating_sub(7);
    let mut body = vec![0u8; body_len];
    if body_len > 0 {
        let _ = stream.read_exact(&mut body);
    }
    (msg_type, body)
}

pub fn decode_string(data: &[u8]) -> (String, &[u8]) {
    if data.len() < 2 {
        return (String::new(), data);
    }
    let len = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
    let available = data.len().saturating_sub(2);
    let actual_len = len.min(available);
    let s = String::from_utf8_lossy(&data[2..2 + actual_len]).to_string();
    (s, &data[2 + actual_len..])
}

pub fn read_data(resp: &[u8]) -> &[u8] {
    if resp.len() < 4 {
        return &[];
    }
    let count = u32::from_le_bytes(resp[0..4].try_into().unwrap()) as usize;
    let data_end = 4 + count;
    &resp[4..data_end.min(resp.len())]
}

pub fn check_error(msg_type: u8, body: &[u8]) -> Result<(), String> {
    if msg_type == 107 {
        let (err, _) = decode_string(body);
        return Err(if err.is_empty() {
            "unknown error".into()
        } else {
            err
        });
    }
    Ok(())
}

// ── 9P stat format ─────────────────────────────────────────────────────────
//
// 9P2000 stat entry layout (wire format):
//   size[2]   — total entry size including this field
//   type[2]   — file type
//   dev[4]    — device number
//   qid[13]   — QID (type[1], version[4], path[8])
//   mode[4]   — permission bits
//   atime[4]  — access time
//   mtime[4]  — modification time
//   length[8] — file length
//   name[s]   — name (length[2] + data)
//   uid[s]    — owner name
//   gid[s]    — group name
//   muid[s]   — modifier name
//
// name_len field starts at offset 41 from the start of the entry:
//   size(2) + type(2) + dev(4) + qid(13) + mode(4) + atime(4) + mtime(4) + length(8) = 41
fn parse_readdir_entries(raw: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut off = 0;
    while off + 2 <= raw.len() {
        let entry_size = u16::from_le_bytes(raw[off..off+2].try_into().unwrap()) as usize;
        if entry_size == 0 || off + entry_size > raw.len() { break; }
        const NAME_START: usize = 41;
        if entry_size < NAME_START + 2 { off += entry_size; continue; }
        let name_len = u16::from_le_bytes(raw[off+NAME_START..off+NAME_START+2].try_into().unwrap()) as usize;
        if NAME_START + 2 + name_len > entry_size { off += entry_size; continue; }
        let name = String::from_utf8_lossy(&raw[off+NAME_START+2..off+NAME_START+2+name_len]).to_string();
        if !name.is_empty() && name != "." && name != ".." {
            names.push(name);
        }
        off += entry_size + 2; // +2 for the size field itself
    }
    names.sort();
    names
}

// ── 9P Client (stream-based: sock + tcp) ────────────────────────────────────

pub struct NinepClient {
    stream: Box<dyn NinepStream + Send>,
}

impl NinepClient {
    pub fn connect_unix(path: &str) -> Result<Self, String> {
        let start = Instant::now();
        loop {
            match UnixStream::connect(path) {
                Ok(stream) => return Ok(Self { stream: Box::new(stream) }),
                Err(e) => {
                    if start.elapsed() > Duration::from_secs(3) {
                        return Err(format!("connect unix {path}: {e}"));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    pub fn connect_tcp(addr: &str) -> Result<Self, String> {
        let start = Instant::now();
        loop {
            match TcpStream::connect(addr) {
                Ok(stream) => return Ok(Self { stream: Box::new(stream) }),
                Err(e) => {
                    if start.elapsed() > Duration::from_secs(3) {
                        return Err(format!("connect tcp {addr}: {e}"));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    pub fn send_msg(&mut self, msg_type: u8, tag: u16, body: &[u8]) -> Result<(), String> {
        self.stream
            .write_all(&build_9p_msg(msg_type, tag, body))
            .map_err(|e| format!("{msg_type} write: {e}"))
    }

    pub fn recv_reply(&mut self) -> Result<(u8, Vec<u8>), String> {
        let (msg_type, body) = read_9p_reply(&mut self.stream);
        check_error(msg_type, &body)?;
        Ok((msg_type, body))
    }

    pub fn version(&mut self, tag: u16, msize: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&msize.to_le_bytes());
        let version = b"9P2000";
        body.extend_from_slice(&(version.len() as u16).to_le_bytes());
        body.extend_from_slice(version);
        self.send_msg(100, tag, &body)?;
        let (_, resp) = self.recv_reply()?;
        if resp.len() < 4 {
            return Err("Rversion: too short".into());
        }
        let resp_msize = u32::from_le_bytes(resp[0..4].try_into().unwrap());
        if resp_msize < 512 {
            return Err(format!("Rversion: msize too small: {resp_msize}"));
        }
        Ok(())
    }

    pub fn attach(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.push(b'u');
        body.extend_from_slice(&0u16.to_le_bytes());
        self.send_msg(104, tag, &body)?;
        let (_, resp) = self.recv_reply()?;
        if resp.len() < 13 {
            return Err(format!("Rattach: too short ({} bytes)", resp.len()));
        }
        Ok(())
    }

    pub fn walk(&mut self, tag: u16, fid: u32, newfid: u32, path: &str) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&newfid.to_le_bytes());
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        body.extend_from_slice(&(parts.len() as u16).to_le_bytes());
        for part in &parts {
            body.extend_from_slice(&(part.len() as u16).to_le_bytes());
            body.extend_from_slice(part.as_bytes());
        }
        self.send_msg(110, tag, &body)?;
        let (_, body) = self.recv_reply()?;
        Ok(body)
    }

    pub fn open(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.push(0);
        self.send_msg(112, tag, &body)?;
        self.recv_reply()?;
        Ok(())
    }

    pub fn read(&mut self, tag: u16, fid: u32, offset: u64, count: u32) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&offset.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());
        self.send_msg(116, tag, &body)?;
        let (_, body) = self.recv_reply()?;
        Ok(body)
    }

    pub fn clunk(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        self.send_msg(120, tag, &body)?;
        self.recv_reply()?;
        Ok(())
    }

    pub fn write(&mut self, tag: u16, fid: u32, offset: u64, data: &[u8]) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&offset.to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(data);
        self.send_msg(118, tag, &body)?;
        self.recv_reply()?;
        Ok(())
    }

    pub fn create(&mut self, tag: u16, fid: u32, name: &str, perm: u32, mode: u8) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(&perm.to_le_bytes());
        body.push(mode);
        self.send_msg(114, tag, &body)?;
        self.recv_reply()?;
        Ok(())
    }

    pub fn remove_fid(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        self.send_msg(122, tag, &body)?;
        self.recv_reply()?;
        Ok(())
    }

    /// Walk to a path via a fresh fid then call Tcreate with DMDIR.
    pub fn create_dir_9p(&mut self, path: &str) -> Result<(), String> {
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("/");
        let name = path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path);
        let parent = if parent.is_empty() { "/" } else { parent };
        let pfid = 3u32;
        self.walk(0, 1, pfid, parent)?;
        self.create(0, pfid, name, 0x800001ed, 0)?;
        self.clunk(0, pfid)?;
        Ok(())
    }

    /// Walk to a path then call Tremove.
    pub fn remove_path(&mut self, path: &str) -> Result<(), String> {
        let rfid = 3u32;
        self.walk(0, 1, rfid, path)?;
        self.remove_fid(0, rfid)?;
        Ok(())
    }
}

impl TestClient for NinepClient {
    fn read_file(&mut self, path: &str) -> Result<String, String> {
        self.walk(0, 1, 2, path)?;
        self.open(0, 2)?;
        let resp = self.read(0, 2, 0, 65536)?;
        let content = read_data(&resp);
        let text = String::from_utf8_lossy(content).to_string();
        self.clunk(0, 2)?;
        Ok(text)
    }

    fn walk_nonexistent(&mut self, path: &str) -> Result<String, String> {
        let err = self.walk(0, 1, 2, path).unwrap_err();
        Err(err)
    }

    fn write_file(&mut self, path: &str, content: &str) -> Result<(), String> {
        let wfid = 3u32;
        self.walk(0, 1, wfid, path)?;
        self.open(0, wfid)?;
        self.write(0, wfid, 0, content.as_bytes())?;
        self.clunk(0, wfid)?;
        Ok(())
    }

    fn create_dir(&mut self, path: &str) -> Result<(), String> {
        self.create_dir_9p(path)
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        self.remove_path(path)
    }

    fn read_dir_names(&mut self, path: &str) -> Result<Vec<String>, String> {
        let dir = if path.is_empty() || path == "/" { "/" } else { path.trim_end_matches('/') };
        let dfid = 3u32;
        self.walk(0, 1, dfid, dir)?;
        self.open(0, dfid)?;
        let resp = self.read(0, dfid, 0, 65536)?;
        self.clunk(0, dfid)?;
        let raw = read_data(&resp);
        let names = parse_readdir_entries(raw);
        Ok(names)
    }
}

// ── 9P Client (UDP) ─────────────────────────────────────────────────────────

pub struct UdpNinepClient {
    socket: UdpSocket,
    addr: String,
}

impl UdpNinepClient {
    pub fn connect(addr: &str) -> Result<Self, String> {
        let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("bind udp: {e}"))?;
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set timeout: {e}"))?;
        Ok(Self {
            socket,
            addr: addr.to_string(),
        })
    }

    pub fn exchange(&self, msg_type: u8, tag: u16, body: &[u8]) -> Result<Vec<u8>, String> {
        let req = build_9p_msg(msg_type, tag, body);
        self.socket
            .send_to(&req, &self.addr)
            .map_err(|e| format!("udp send: {e}"))?;

        let mut buf = vec![0u8; 65536];
        let (len, _) = self
            .socket
            .recv_from(&mut buf)
            .map_err(|e| format!("udp recv: {e}"))?;

        if len < 7 {
            return Err("udp reply too short".into());
        }
        let resp_msg_type = buf[4];
        let resp_body = buf[7..len].to_vec();
        check_error(resp_msg_type, &resp_body)?;
        Ok(resp_body)
    }

    pub fn version(&mut self, tag: u16, msize: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&msize.to_le_bytes());
        let version = b"9P2000";
        body.extend_from_slice(&(version.len() as u16).to_le_bytes());
        body.extend_from_slice(version);
        let resp = self.exchange(100, tag, &body)?;
        if resp.len() < 4 {
            return Err("Rversion: too short".into());
        }
        let resp_msize = u32::from_le_bytes(resp[0..4].try_into().unwrap());
        if resp_msize < 512 {
            return Err(format!("Rversion: msize too small: {resp_msize}"));
        }
        Ok(())
    }

    pub fn attach(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.push(b'u');
        body.extend_from_slice(&0u16.to_le_bytes());
        let resp = self.exchange(104, tag, &body)?;
        if resp.len() < 13 {
            return Err(format!("Rattach: too short ({} bytes)", resp.len()));
        }
        Ok(())
    }

    pub fn walk(&mut self, tag: u16, fid: u32, newfid: u32, path: &str) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&newfid.to_le_bytes());
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        body.extend_from_slice(&(parts.len() as u16).to_le_bytes());
        for part in &parts {
            body.extend_from_slice(&(part.len() as u16).to_le_bytes());
            body.extend_from_slice(part.as_bytes());
        }
        self.exchange(110, tag, &body)
    }

    pub fn open(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.push(0);
        self.exchange(112, tag, &body)?;
        Ok(())
    }

    pub fn read(&mut self, tag: u16, fid: u32, offset: u64, count: u32) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&offset.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());
        self.exchange(116, tag, &body)
    }

    pub fn clunk(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        self.exchange(120, tag, &body)?;
        Ok(())
    }

    pub fn write(&mut self, tag: u16, fid: u32, offset: u64, data: &[u8]) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&offset.to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(data);
        self.exchange(118, tag, &body)?;
        Ok(())
    }

    pub fn create(&mut self, tag: u16, fid: u32, name: &str, perm: u32, mode: u8) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(&perm.to_le_bytes());
        body.push(mode);
        self.exchange(114, tag, &body)?;
        Ok(())
    }

    pub fn remove_fid(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        self.exchange(122, tag, &body)?;
        Ok(())
    }

    pub fn create_dir_9p(&mut self, path: &str) -> Result<(), String> {
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("/");
        let name = path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path);
        let parent = if parent.is_empty() { "/" } else { parent };
        let pfid = 3u32;
        self.walk(0, 1, pfid, parent)?;
        self.create(0, pfid, name, 0x800001ed, 0)?;
        self.clunk(0, pfid)?;
        Ok(())
    }

    pub fn remove_path(&mut self, path: &str) -> Result<(), String> {
        let rfid = 3u32;
        self.walk(0, 1, rfid, path)?;
        self.remove_fid(0, rfid)?;
        Ok(())
    }
}

impl TestClient for UdpNinepClient {
    fn read_file(&mut self, path: &str) -> Result<String, String> {
        self.walk(0, 1, 2, path)?;
        self.open(0, 2)?;
        let resp = self.read(0, 2, 0, 65536)?;
        let content = read_data(&resp);
        let text = String::from_utf8_lossy(content).to_string();
        self.clunk(0, 2)?;
        Ok(text)
    }

    fn walk_nonexistent(&mut self, path: &str) -> Result<String, String> {
        let err = self.walk(0, 1, 2, path).unwrap_err();
        Err(err)
    }

    fn write_file(&mut self, path: &str, content: &str) -> Result<(), String> {
        let wfid = 3u32;
        self.walk(0, 1, wfid, path)?;
        self.open(0, wfid)?;
        self.write(0, wfid, 0, content.as_bytes())?;
        self.clunk(0, wfid)?;
        Ok(())
    }

    fn create_dir(&mut self, path: &str) -> Result<(), String> {
        self.create_dir_9p(path)
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        self.remove_path(path)
    }

    fn read_dir_names(&mut self, path: &str) -> Result<Vec<String>, String> {
        let dir = if path.is_empty() || path == "/" { "/" } else { path.trim_end_matches('/') };
        let dfid = 3u32;
        self.walk(0, 1, dfid, dir)?;
        self.open(0, dfid)?;
        let resp = self.read(0, dfid, 0, 65536)?;
        self.clunk(0, dfid)?;
        let raw = read_data(&resp);
        let names = parse_readdir_entries(raw);
        Ok(names)
    }
}

// ── SSH/SFTP Client ─────────────────────────────────────────────────────────

pub struct SshClient {
    _tcp: TcpStream,
    session: ssh2::Session,
    sftp: Option<ssh2::Sftp>,
}

impl SshClient {
    pub fn connect(addr: &str, user: &str, pass: &str) -> Result<Self, String> {
        let tcp = TcpStream::connect(addr).map_err(|e| format!("ssh tcp connect: {e}"))?;
        let mut session = ssh2::Session::new().map_err(|e| format!("ssh session: {e}"))?;
        session.set_tcp_stream(tcp.try_clone().map_err(|e| format!("tcp clone: {e}"))?);
        session.handshake().map_err(|e| format!("ssh handshake: {e}"))?;
        session
            .userauth_password(user, pass)
            .map_err(|e| format!("ssh auth: {e}"))?;
        if !session.authenticated() {
            return Err("ssh: not authenticated".into());
        }
        Ok(Self {
            _tcp: tcp,
            session,
            sftp: None,
        })
    }

    fn get_sftp(&mut self) -> Result<&mut ssh2::Sftp, String> {
        if self.sftp.is_none() {
            let s = self
                .session
                .sftp()
                .map_err(|e| format!("sftp: {e}"))?;
            self.sftp = Some(s);
        }
        Ok(self.sftp.as_mut().unwrap())
    }
}

impl TestClient for SshClient {
    fn read_file(&mut self, path: &str) -> Result<String, String> {
        let sftp = self.get_sftp()?;
        let mut file = sftp
            .open(path)
            .map_err(|e| format!("sftp open {path}: {e}"))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|e| format!("sftp read {path}: {e}"))?;
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    fn walk_nonexistent(&mut self, path: &str) -> Result<String, String> {
        let sftp = self.get_sftp()?;
        let err = match sftp.open(path) {
            Err(e) => e,
            Ok(mut file) => {
                let mut buf = Vec::new();
                let _ = file.read_to_end(&mut buf);
                return Err("expected path to not exist, but it was opened".into());
            }
        };
        Err(err.message().to_string())
    }

    fn write_file(&mut self, path: &str, content: &str) -> Result<(), String> {
        use std::path::Path;
        let sftp = self.get_sftp()?;
        let mut file = sftp
            .create(Path::new(path))
            .map_err(|e| format!("sftp create {path}: {e}"))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("sftp write {path}: {e}"))?;
        Ok(())
    }

    fn create_dir(&mut self, path: &str) -> Result<(), String> {
        use std::path::Path;
        let sftp = self.get_sftp()?;
        sftp.mkdir(Path::new(path), 0o755)
            .map_err(|e| format!("sftp mkdir {path}: {e}"))
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        use std::path::Path;
        let sftp = self.get_sftp()?;
        let p = Path::new(path);
        if let Err(e) = sftp.unlink(p) {
            sftp.rmdir(p).map_err(|e2| format!("sftp remove {path}: unlink: {e}, rmdir: {e2}"))
        } else {
            Ok(())
        }
    }

    fn read_dir_names(&mut self, path: &str) -> Result<Vec<String>, String> {
        let sftp = self.get_sftp()?;
        let dir_path = if path.is_empty() || path == "/" { "." } else { path.trim_end_matches('/') };
        let entries = sftp
            .readdir(dir_path)
            .map_err(|e| format!("sftp readdir {dir_path}: {e}"))?;
        let mut names: Vec<String> = entries
            .into_iter()
            .map(|(name, _)| name.to_string_lossy().to_string())
            .filter(|n| n != "." && n != "..")
            .collect();
        names.sort();
        Ok(names)
    }
}

// ── FUSE Client ────────────────────────────────────────────────────────────

pub struct FuseClient {
    mountpoint: String,
}

impl TestClient for FuseClient {
    fn read_file(&mut self, path: &str) -> Result<String, String> {
        let full_path = format!("{}/{}", self.mountpoint, path.trim_start_matches('/'));
        std::fs::read_to_string(&full_path).map_err(|e| format!("fuse read: {e}"))
    }

    fn walk_nonexistent(&mut self, path: &str) -> Result<String, String> {
        let full_path = format!("{}/{}", self.mountpoint, path.trim_start_matches('/'));
        match std::fs::metadata(&full_path) {
            Ok(_) => Err("expected path to not exist, but metadata succeeded".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn write_file(&mut self, path: &str, content: &str) -> Result<(), String> {
        let full_path = format!("{}/{}", self.mountpoint, path.trim_start_matches('/'));
        std::fs::write(&full_path, content).map_err(|e| format!("fuse write: {e}"))
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        let full_path = format!("{}/{}", self.mountpoint, path.trim_start_matches('/'));
        if std::fs::metadata(&full_path).map(|m| m.is_dir()).unwrap_or(false) {
            std::fs::remove_dir(&full_path).map_err(|e| format!("fuse rmdir: {e}"))
        } else {
            std::fs::remove_file(&full_path).map_err(|e| format!("fuse unlink: {e}"))
        }
    }

    fn rename(&mut self, old: &str, new: &str) -> Result<(), String> {
        let full_old = format!("{}/{}", self.mountpoint, old.trim_start_matches('/'));
        let full_new = format!("{}/{}", self.mountpoint, new.trim_start_matches('/'));
        std::fs::rename(&full_old, &full_new).map_err(|e| format!("fuse rename: {e}"))
    }

    fn create_dir(&mut self, path: &str) -> Result<(), String> {
        let full_path = format!("{}/{}", self.mountpoint, path.trim_start_matches('/'));
        std::fs::create_dir(&full_path).map_err(|e| format!("fuse mkdir: {e}"))
    }

    fn chmod(&mut self, path: &str, mode: u32) -> Result<(), String> {
        let full_path = format!("{}/{}", self.mountpoint, path.trim_start_matches('/'));
        let perm = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(&full_path, perm).map_err(|e| format!("fuse chmod: {e}"))
    }

    fn read_dir_names(&mut self, path: &str) -> Result<Vec<String>, String> {
        let full_path = format!("{}/{}", self.mountpoint, path.trim_start_matches('/'));
        let entries = std::fs::read_dir(&full_path).map_err(|e| format!("fuse readdir: {e}"))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("fuse readdir entry: {e}"))?;
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();
        Ok(names)
    }
}

// ── 9P setup helpers ────────────────────────────────────────────────────────

pub fn setup_client(client: &mut NinepClient) -> Result<(), String> {
    client.version(0, 65536)?;
    client.attach(0, 1)?;
    Ok(())
}

pub fn setup_client_udp(client: &mut UdpNinepClient) -> Result<(), String> {
    client.version(0, 65536)?;
    client.attach(0, 1)?;
    Ok(())
}

// ── Scenario runner ─────────────────────────────────────────────────────────

/// Kill any orphaned pinhead processes from previous test runs that
/// were killed before their Drop handler could run.
pub fn cleanup_orphans() {
    // Stale ph processes left by killed test runs block ports/sockets.
    // Use -x (exact match) so we don't accidentally match the test harness.
    for _ in 0..3 {
        let status = Command::new("pkill")
            .args(["-9", "-x", "ph"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if status.map(|s| !s.success()).unwrap_or(true) {
            break; // no more to kill
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Unmount stale FUSE mounts from previous killed runs (dead mounts
    // appear in /proc/mounts and fool wait_for_fuse_mount).
    let _ = Command::new("sh")
        .args([
            "-c",
            "grep 'pinhead-e2e' /proc/mounts 2>/dev/null | awk '{print $2}' | xargs -r fusermount -uz 2>/dev/null || true",
        ])
        .status();

    // Clean up all stale temp artifacts (sockets, scripts, db files, mount
    // dirs) from previous killed runs.
    let _ = Command::new("sh")
        .args([
            "-c",
            r#"rm -rf /tmp/pinhead-e2e-*.lua /tmp/pinhead-e2e-*.sock /tmp/pinhead-e2e-*.db /tmp/pinhead-e2e-*.db-shm /tmp/pinhead-e2e-*.db-wal 2>/dev/null; find /tmp -maxdepth 1 -name 'pinhead-e2e-*fuse-*' -type d -exec rm -rf {} + 2>/dev/null"#,
        ])
        .status();
}

pub fn run_scenarios(
    script: &str,
    transports: &[Transport],
    test_fn: impl Fn(&mut dyn TestClient),
) {
    cleanup_orphans();

    for t in transports {
        eprintln!("[test] transport: {}", t.listen_str());
        let mut inst = match PinheadInstance::start(script, t) {
            Ok(inst) => inst,
            Err(e) => {
                eprintln!("[test]  ⚠ skip (start failed: {e})");
                continue;
            }
        };
        let mut client = match inst.connect() {
            Ok(client) => client,
            Err(e) => {
                eprintln!("[test]  ⚠ skip (connect failed: {e})");
                continue;
            }
        };
        test_fn(&mut *client);
    }
}
