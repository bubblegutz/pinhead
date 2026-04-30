//! Synchronous 9P2000 client for use inside Lua handlers.
//!
//! Runs on pinned worker threads (blocking I/O is fine).  Supports Unix
//! sockets and TCP.  Address format: `sock:/path`, `tcp:host:port`, or a
//! raw path (defaults to Unix socket).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;

// ── 9P2000 message types ──────────────────────────────────────────────
const TVERSION: u8 = 100;
const TATTACH: u8 = 104;
const TWALK: u8 = 110;
const TOPEN: u8 = 112;
const TCREATE: u8 = 114;
const TREAD: u8 = 116;
const TWRITE: u8 = 118;
const TCLUNK: u8 = 120;
const TREMOVE: u8 = 122;
const TSTAT: u8 = 124;
const RERROR: u8 = 107;
const NOFID: u32 = 0xFFFFFFFF;

// ── 9P2000 mode flags (for Topen / Tcreate) ───────────────────────────
const DMDIR: u32 = 0x8000_0000;

// ── Connection abstraction ────────────────────────────────────────────

enum Conn {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Unix(s) => s.read(buf),
            Conn::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Unix(s) => s.write(buf),
            Conn::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Unix(s) => s.flush(),
            Conn::Tcp(s) => s.flush(),
        }
    }
}

// ── 9P2000 client ─────────────────────────────────────────────────────

pub struct NinepClient {
    conn: Conn,
    msize: u32,
    /// Next available fid number.
    next_fid: u32,
}

impl NinepClient {
    /// Connect to a 9P2000 server.
    ///
    /// Address format: `sock:/path`, `tcp:host:port`, or a raw path (Unix socket).
    pub fn connect(addr: &str) -> Result<Self, String> {
        let conn = if let Some(path) = addr.strip_prefix("sock:") {
            Conn::Unix(
                UnixStream::connect(path)
                    .map_err(|e| format!("connect unix {path}: {e}"))?,
            )
        } else if let Some(host) = addr.strip_prefix("tcp:") {
            Conn::Tcp(
                TcpStream::connect(host)
                    .map_err(|e| format!("connect tcp {host}: {e}"))?,
            )
        } else {
            Conn::Unix(
                UnixStream::connect(addr)
                    .map_err(|e| format!("connect {addr}: {e}"))?,
            )
        };

        let mut client = Self {
            conn,
            msize: 65536,
            next_fid: 1,
        };
        client.version()?;
        client.attach()?;
        // Reserve fid 1 for root attach, fid 2 for walk/open/read, fid 3+ for temp ops.
        client.next_fid = 4;
        Ok(client)
    }

    // ── Low-level message exchange ──────────────────────────────────────

    fn send_recv(&mut self, msg_type: u8, body: &[u8]) -> Result<Vec<u8>, String> {
        let size = 7u32 + body.len() as u32;
        let mut buf = Vec::with_capacity(size as usize);
        buf.extend_from_slice(&size.to_le_bytes());
        buf.push(msg_type);
        buf.extend_from_slice(&0u16.to_le_bytes()); // tag = 0
        buf.extend_from_slice(body);

        self.conn
            .write_all(&buf)
            .map_err(|e| format!("write: {e}"))?;

        let mut header = [0u8; 7];
        self.conn
            .read_exact(&mut header)
            .map_err(|e| format!("read header: {e}"))?;

        let rsize = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let rtype = header[4];

        let body_len = rsize.saturating_sub(7);
        let mut resp = vec![0u8; body_len];
        if body_len > 0 {
            self.conn
                .read_exact(&mut resp)
                .map_err(|e| format!("read body: {e}"))?;
        }

        if rtype == RERROR {
            let err_len = u16::from_le_bytes(resp[0..2].try_into().unwrap()) as usize;
            let err = String::from_utf8_lossy(&resp[2..2 + err_len]).to_string();
            return Err(format!("9p error: {err}"));
        }

        Ok(resp)
    }

    // ── 9P2000 operations ───────────────────────────────────────────────

    fn version(&mut self) -> Result<(), String> {
        let ms = b"9P2000";
        let mut body = Vec::new();
        body.extend_from_slice(&self.msize.to_le_bytes());
        body.extend_from_slice(&(ms.len() as u16).to_le_bytes());
        body.extend_from_slice(ms);
        let resp = self.send_recv(TVERSION, &body)?;
        self.msize = u32::from_le_bytes(resp[0..4].try_into().unwrap());
        self.msize = self.msize.min(65536);
        Ok(())
    }

    fn attach(&mut self) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // fid
        body.extend_from_slice(&NOFID.to_le_bytes()); // afid
        body.extend_from_slice(&0u16.to_le_bytes()); // uname
        body.extend_from_slice(&0u16.to_le_bytes()); // aname
        let _ = self.send_recv(TATTACH, &body)?;
        Ok(())
    }

    fn walk(&mut self, fid: u32, new_fid: u32, path: &str) -> Result<(), String> {
        let components: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&new_fid.to_le_bytes());
        body.extend_from_slice(&(components.len() as u16).to_le_bytes());
        for comp in &components {
            body.extend_from_slice(&(comp.len() as u16).to_le_bytes());
            body.extend_from_slice(comp.as_bytes());
        }
        let resp = self.send_recv(TWALK, &body)?;
        let nwqid = u16::from_le_bytes(resp[0..2].try_into().unwrap()) as usize;
        if nwqid != components.len() {
            return Err(format!(
                "walk failed: reached {nwqid}/{} components",
                components.len()
            ));
        }
        Ok(())
    }

    fn open(&mut self, fid: u32, mode: u8) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.push(mode);
        let _ = self.send_recv(TOPEN, &body)?;
        Ok(())
    }

    fn create(&mut self, fid: u32, name: &str, perm: u32, mode: u8) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(&perm.to_le_bytes());
        body.push(mode);
        let _ = self.send_recv(TCREATE, &body)?;
        Ok(())
    }

    fn clunk(&mut self, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        let _ = self.send_recv(TCLUNK, &body)?;
        Ok(())
    }

    fn remove_fid(&mut self, fid: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        let _ = self.send_recv(TREMOVE, &body)?;
        Ok(())
    }

    fn alloc_fid(&mut self) -> u32 {
        let fid = self.next_fid;
        self.next_fid += 1;
        fid
    }

    // ── High-level public API ────────────────────────────────────────────

    /// Read an entire file at `path`.
    pub fn read_file(&mut self, path: &str) -> Result<String, String> {
        let fid = self.alloc_fid();
        self.walk(1, fid, path)?;
        self.open(fid, 0)?; // OREAD

        let mut result = Vec::new();
        let mut offset = 0u64;
        loop {
            let mut body = Vec::new();
            body.extend_from_slice(&fid.to_le_bytes());
            body.extend_from_slice(&offset.to_le_bytes());
            body.extend_from_slice(&self.msize.to_le_bytes());
            let resp = self.send_recv(TREAD, &body)?;
            let data_len = u32::from_le_bytes(resp[0..4].try_into().unwrap()) as usize;
            if data_len == 0 {
                break;
            }
            result.extend_from_slice(&resp[4..4 + data_len]);
            offset += data_len as u64;
        }

        let _ = self.clunk(fid);
        Ok(String::from_utf8_lossy(&result).to_string())
    }

    /// Write data to a file at `path`.
    pub fn write_file(&mut self, path: &str, data: &str) -> Result<String, String> {
        let fid = self.alloc_fid();
        self.walk(1, fid, path)?;
        self.open(fid, 1)?; // OWRITE

        let data_bytes = data.as_bytes();
        let mut offset = 0u64;
        while offset < data_bytes.len() as u64 {
            let chunk_len =
                (data_bytes.len() as u64 - offset).min(self.msize as u64 - 24) as usize;
            let chunk = &data_bytes[offset as usize..offset as usize + chunk_len];

            let mut body = Vec::new();
            body.extend_from_slice(&fid.to_le_bytes());
            body.extend_from_slice(&offset.to_le_bytes());
            body.extend_from_slice(&(chunk_len as u32).to_le_bytes());
            body.extend_from_slice(chunk);
            let _ = self.send_recv(TWRITE, &body)?;
            offset += chunk_len as u64;
        }

        let _ = self.clunk(fid);
        Ok(String::new())
    }

    /// Stat a file at `path`.  Returns human-readable text.
    pub fn stat(&mut self, path: &str) -> Result<String, String> {
        let fid = self.alloc_fid();
        self.walk(1, fid, path)?;

        let mut body = Vec::new();
        body.extend_from_slice(&fid.to_le_bytes());
        let resp = self.send_recv(TSTAT, &body)?;

        let stat_len = u16::from_le_bytes(resp[0..2].try_into().unwrap()) as usize;
        let stat_data = &resp[2..2 + stat_len];

        let mut out = String::new();
        let mut off = 0;

        off += 6; // type(2) + dev(4)
        let qid_path =
            u64::from_le_bytes(stat_data[off + 5..off + 13].try_into().unwrap());
        off += 13;
        off += 4; // mode
        off += 8; // atime + mtime
        let length = u64::from_le_bytes(stat_data[off..off + 8].try_into().unwrap());
        off += 8;
        let name_len =
            u16::from_le_bytes(stat_data[off..off + 2].try_into().unwrap()) as usize;
        off += 2;
        let name = String::from_utf8_lossy(&stat_data[off..off + name_len]);

        out.push_str(&format!("{name}\n"));
        out.push_str(&format!("  qid path: {qid_path}\n"));
        out.push_str(&format!("  length: {length}\n"));

        let _ = self.clunk(fid);
        Ok(out)
    }

    /// List directory contents.  Returns one entry per line.
    pub fn ls(&mut self, path: &str, long: bool) -> Result<String, String> {
        let dir_path = path.trim_end_matches('/');
        let dir_path = if dir_path.is_empty() { "/" } else { dir_path };

        let fid = self.alloc_fid();
        self.walk(1, fid, dir_path)?;
        self.open(fid, 0)?; // OREAD

        let mut raw = Vec::new();
        let mut offset = 0u64;
        loop {
            let mut body = Vec::new();
            body.extend_from_slice(&fid.to_le_bytes());
            body.extend_from_slice(&offset.to_le_bytes());
            body.extend_from_slice(&self.msize.to_le_bytes());
            let resp = self.send_recv(TREAD, &body)?;
            let data_len = u32::from_le_bytes(resp[0..4].try_into().unwrap()) as usize;
            if data_len == 0 {
                break;
            }
            raw.extend_from_slice(&resp[4..4 + data_len]);
            offset += data_len as u64;
        }

        let _ = self.clunk(fid);

        let mut out = String::new();
        let mut off = 0;
        while off < raw.len() {
            let entry_size =
                u16::from_le_bytes(raw[off..off + 2].try_into().unwrap()) as usize;
            if entry_size == 0 || off + entry_size > raw.len() {
                break;
            }
            let data_start = off + 6;
            let qid_type = raw[data_start];
            let mode = u32::from_le_bytes(
                raw[data_start + 13..data_start + 17]
                    .try_into()
                    .unwrap(),
            );
            let name_len = u16::from_le_bytes(
                raw[data_start + 33..data_start + 35]
                    .try_into()
                    .unwrap(),
            ) as usize;
            let name =
                String::from_utf8_lossy(&raw[data_start + 35..data_start + 35 + name_len]);

            if long {
                let perm = if mode & DMDIR != 0 { 'd' } else { '-' };
                out.push_str(&format!("{perm}-------  {name}\n"));
            } else {
                let is_dir = qid_type & 0x80 != 0;
                let suffix = if is_dir { "/" } else { "" };
                out.push_str(&format!("{name}{suffix}\n"));
            }

            off += entry_size;
        }

        Ok(out)
    }

    /// Create a directory at `path`.
    ///
    /// Note: requires server-side Tcreate support (pinhead does not currently
    /// dispatch Tcreate — this will return an error until server support is added).
    pub fn mkdir(&mut self, path: &str) -> Result<String, String> {
        let (parent, name) = split_parent_name(path);
        let pfid = self.alloc_fid();
        self.walk(1, pfid, &parent)?;
        self.create(pfid, &name, DMDIR | 0o755, 0)?; // OREAD
        let _ = self.clunk(pfid);
        Ok(String::new())
    }

    /// Create a regular file at `path`.
    ///
    /// Note: requires server-side Tcreate support (pinhead does not currently
    /// dispatch Tcreate — this will return an error until server support is added).
    pub fn create_file(&mut self, path: &str) -> Result<String, String> {
        let (parent, name) = split_parent_name(path);
        let pfid = self.alloc_fid();
        self.walk(1, pfid, &parent)?;
        self.create(pfid, &name, 0o644, 1)?; // OWRITE
        let _ = self.clunk(pfid);
        Ok(String::new())
    }

    /// Remove a file or empty directory at `path`.
    ///
    /// Note: requires server-side Tremove support (pinhead dispatches Tremove
    /// for registered routes but may not support all cases).
    pub fn remove(&mut self, path: &str) -> Result<String, String> {
        let fid = self.alloc_fid();
        self.walk(1, fid, path)?;
        let result = self.remove_fid(fid);
        // Tremove clunks the fid automatically — don't call clunk after remove.
        match result {
            Ok(_) => Ok(String::new()),
            Err(e) => Err(e),
        }
    }
}

/// Split a path into parent directory and last name component.
fn split_parent_name(path: &str) -> (String, String) {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((parent, name)) => {
            let parent = if parent.is_empty() { "/" } else { parent };
            (parent.to_string(), name.to_string())
        }
        None => ("/".to_string(), trimmed.to_string()),
    }
}
