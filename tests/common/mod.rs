use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

/// Generate a unique socket identifier to avoid collisions between
/// parallel test binaries (which share the same process ID).
static NEXT_SOCK_ID: AtomicU16 = AtomicU16::new(1);
pub fn unique_sock_id() -> u64 {
    let n = NEXT_SOCK_ID.fetch_add(1, Ordering::Relaxed) as u64;
    (std::process::id() as u64) << 16 | n
}

// ── 9P Message helpers ──────────────────────────────────────────────────────

/// Build a 9P request message (size + type + tag + body).
pub fn build_9p_msg(msg_type: u8, tag: u16, body: &[u8]) -> Vec<u8> {
    let size = 7 + body.len();
    let mut buf = Vec::with_capacity(size);
    buf.extend_from_slice(&(size as u32).to_le_bytes());
    buf.push(msg_type);
    buf.extend_from_slice(&tag.to_le_bytes());
    buf.extend_from_slice(body);
    buf
}

/// Read a 9P reply from a stream.
/// Returns (message_type, body) where body excludes the 7-byte header.
pub fn read_9p_reply(stream: &mut UnixStream) -> (u8, Vec<u8>) {
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

/// Decode a 9P2000 string (2-byte length prefix + UTF-8 data).
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

/// Extract the data portion of a Rread response (skips 4-byte count header).
pub fn read_data(resp: &[u8]) -> &[u8] {
    if resp.len() < 4 {
        return &[];
    }
    let count = u32::from_le_bytes(resp[0..4].try_into().unwrap()) as usize;
    let data_end = 4 + count;
    &resp[4..data_end.min(resp.len())]
}

/// Check if a response is Rerror (type 107) and return Err if so.
pub fn check_error(msg_type: u8, body: &[u8]) -> Result<(), String> {
    if msg_type == 107 {
        let (err, _) = decode_string(body);
        return Err(if err.is_empty() { "unknown error".into() } else { err });
    }
    Ok(())
}

// ── High-level 9P client ────────────────────────────────────────────────────

pub struct NinepClient {
    stream: UnixStream,
}

impl NinepClient {
    /// Connect to a Unix socket at `path` with retries (up to ~2 seconds).
    pub fn connect(socket_path: &str) -> Result<Self, String> {
        let start = Instant::now();
        loop {
            match UnixStream::connect(socket_path) {
                Ok(stream) => return Ok(Self { stream }),
                Err(e) => {
                    if start.elapsed() > Duration::from_secs(2) {
                        return Err(format!("connect to {socket_path}: {e}"));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    /// Send Tversion and verify Rversion.
    pub fn version(&mut self, tag: u16, msize: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&msize.to_le_bytes());
        let version = b"9P2000";
        body.extend_from_slice(&(version.len() as u16).to_le_bytes());
        body.extend_from_slice(version);

        self.stream
            .write_all(&build_9p_msg(100, tag, &body))
            .map_err(|e| format!("Tversion write: {e}"))?;

        let (msg_type, resp) = read_9p_reply(&mut self.stream);
        check_error(msg_type, &resp)?;
        if resp.len() < 4 {
            return Err("Rversion: too short".into());
        }
        let resp_msize = u32::from_le_bytes(resp[0..4].try_into().unwrap());
        if resp_msize < 512 {
            return Err(format!("Rversion: msize too small: {resp_msize}"));
        }
        Ok(())
    }

    /// Send Tattach and verify Rattach.
    pub fn attach(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // afid (none)
        body.extend_from_slice(&1u16.to_le_bytes()); // uname length
        body.push(b'u');
        body.extend_from_slice(&0u16.to_le_bytes()); // aname length

        self.stream
            .write_all(&build_9p_msg(104, tag, &body))
            .map_err(|e| format!("Tattach write: {e}"))?;

        let (msg_type, resp) = read_9p_reply(&mut self.stream);
        check_error(msg_type, &resp)?;
        if resp.len() < 13 {
            return Err(format!("Rattach: too short ({} bytes)", resp.len()));
        }
        Ok(())
    }

    /// Send Twalk with one or more path elements.
    /// Returns the Rwalk body.
    pub fn walk(
        &mut self,
        tag: u16,
        fid: u32,
        newfid: u32,
        path: &str,
    ) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&newfid.to_le_bytes());
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        body.extend_from_slice(&(parts.len() as u16).to_le_bytes());
        for part in &parts {
            body.extend_from_slice(&(part.len() as u16).to_le_bytes());
            body.extend_from_slice(part.as_bytes());
        }

        self.stream
            .write_all(&build_9p_msg(110, tag, &body))
            .map_err(|e| format!("Twalk write: {e}"))?;

        let (msg_type, resp) = read_9p_reply(&mut self.stream);
        check_error(msg_type, &resp)?;
        Ok(resp)
    }

    /// Send Topen and verify Ropen.
    pub fn open(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.push(0); // mode = OREAD

        self.stream
            .write_all(&build_9p_msg(112, tag, &body))
            .map_err(|e| format!("Topen write: {e}"))?;

        let (msg_type, resp) = read_9p_reply(&mut self.stream);
        check_error(msg_type, &resp)?;
        Ok(())
    }

    /// Send Tread and return the response body.
    pub fn read(&mut self, tag: u16, fid: u32, offset: u64, count: u32) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&offset.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());

        self.stream
            .write_all(&build_9p_msg(116, tag, &body))
            .map_err(|e| format!("Tread write: {e}"))?;

        let (msg_type, body) = read_9p_reply(&mut self.stream);
        check_error(msg_type, &body)?;
        Ok(body)
    }

    /// Send Tclunk and verify Rclunk.
    pub fn clunk(&mut self, tag: u16, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());

        self.stream
            .write_all(&build_9p_msg(120, tag, &body))
            .map_err(|e| format!("Tclunk write: {e}"))?;

        let (msg_type, resp) = read_9p_reply(&mut self.stream);
        check_error(msg_type, &resp)?;
        Ok(())
    }

    /// Return a mutable reference to the underlying stream (for custom ops).
    pub fn stream(&mut self) -> &mut UnixStream {
        &mut self.stream
    }
}

/// Spawn the pinhead binary with a Lua script that listens on `socket_path`.
/// Returns the child process handle and the socket path.
pub fn spawn_pinhead(script_content: &str, socket_path: &str) -> Result<Child, String> {
    let script_path = format!("{socket_path}.lua");
    std::fs::write(&script_path, script_content)
        .map_err(|e| format!("write script: {e}"))?;

    let binary = std::env!("CARGO_BIN_EXE_pinhead");
    let child = Command::new(binary)
        .arg(&script_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn pinhead: {e}"))?;

    Ok(child)
}

/// Wait for a Unix socket to become available at `path`.
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
