//! Minimal 9P2000 server frontend backed by the pinhead path router.
//!
//! Implements a 9P2000 wire protocol server over Unix sockets.
//! Session state (fid/qid tracking) is managed internally.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use rustls::pki_types::CertificateDer;
use std::sync::Arc as StdArc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio_rustls::TlsAcceptor;

use crate::fsop::FsOperation;
use crate::router::Request;

// ── Socket cleanup guard ─────────────────────────────────────────────
/// Removes the socket file on drop (i.e. when the server future is
/// cancelled or the function returns).
struct SocketCleanup {
    path: String,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ── 9P2000 message types ──────────────────────────────────────────────
const TVERSION: u8 = 100;
const RVERSION: u8 = 101;
const TATTACH: u8 = 104;
const RATTACH: u8 = 105;
const TFLUSH: u8 = 108;
const RFLUSH: u8 = 109;
const TWALK: u8 = 110;
const RWALK: u8 = 111;
const TOPEN: u8 = 112;
const ROPEN: u8 = 113;
const TCREATE: u8 = 114;
const RCREATE: u8 = 115;
const TREAD: u8 = 116;
const RREAD: u8 = 117;
const TWRITE: u8 = 118;
const RWRITE: u8 = 119;
const TCLUNK: u8 = 120;
const RCLUNK: u8 = 121;
const TREMOVE: u8 = 122;
const RREMOVE: u8 = 123;
const TSTAT: u8 = 124;
const RSTAT: u8 = 125;
const TWSTAT: u8 = 126;
const RWSTAT: u8 = 127;

// ── Qid ───────────────────────────────────────────────────────────────
#[derive(Clone, Copy, Debug, Default)]
struct Qid {
    ty: u8,
    version: u32,
    path: u64,
}

fn encode_qid(buf: &mut Vec<u8>, qid: Qid) {
    buf.push(qid.ty);
    buf.extend_from_slice(&qid.version.to_le_bytes());
    buf.extend_from_slice(&qid.path.to_le_bytes());
}

// ── Stat (directory entry) ────────────────────────────────────────────
fn encode_stat(buf: &mut Vec<u8>, name: &str, qid: Qid, length: u64, is_dir: bool) {
    // Build the stat body first to compute size.
    let mut body = Vec::new();

    let mode = if is_dir { 0x800001edu32 } else { 0x1edu32 };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;

    body.extend_from_slice(&0u16.to_le_bytes()); // type
    body.extend_from_slice(&0u32.to_le_bytes()); // dev
    encode_qid(&mut body, qid);
    body.extend_from_slice(&mode.to_le_bytes());
    body.extend_from_slice(&now.to_le_bytes()); // atime
    body.extend_from_slice(&now.to_le_bytes()); // mtime
    body.extend_from_slice(&length.to_le_bytes());
    // short string: 2-byte length + data
    body.extend_from_slice(&(name.len() as u16).to_le_bytes());
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(&2u16.to_le_bytes()); // uid
    body.extend_from_slice(b"0");
    body.extend_from_slice(&2u16.to_le_bytes()); // gid
    body.extend_from_slice(b"0");
    body.extend_from_slice(&2u16.to_le_bytes()); // muid
    body.extend_from_slice(b"0");

    let size = body.len() as u16;
    buf.extend_from_slice(&size.to_le_bytes());
    buf.extend_from_slice(&body);
}

// ── Fid / Qid state ───────────────────────────────────────────────────
struct NinepState {
    fids: HashMap<u32, FidEntry>,
    paths: HashMap<u64, PathEntry>,
    next_qid: u64,
}

#[derive(Clone)]
struct FidEntry {
    qid: u64,
    path: String,
    open: bool,
    is_dir: bool,
}

#[derive(Clone)]
struct PathEntry {
    path: String,
    is_dir: bool,
}

impl NinepState {
    fn new() -> Self {
        let mut paths = HashMap::new();
        paths.insert(
            0,
            PathEntry {
                path: "/".into(),
                is_dir: true,
            },
        );
        eprintln!("[ninep] NinepState::new() paths.len={}", paths.len());
        Self {
            fids: HashMap::new(),
            paths,
            next_qid: 1,
        }
    }

    fn alloc_qid(&mut self, path: &str, is_dir: bool) -> u64 {
        // Check if path already has a qid
        for (&qid, entry) in &self.paths {
            if entry.path == path {
                return qid;
            }
        }
        let qid = self.next_qid;
        self.next_qid += 1;
        self.paths.insert(
            qid,
            PathEntry {
                path: path.to_string(),
                is_dir,
            },
        );
        qid
    }

    fn set_fid(&mut self, fid: u32, qid: u64, path: String, is_dir: bool) {
        self.fids.insert(
            fid,
            FidEntry {
                qid,
                path,
                open: false,
                is_dir,
            },
        );
    }
}

// ── Shared frontend ───────────────────────────────────────────────────
struct Shared {
    state: Mutex<NinepState>,
    router_tx: mpsc::Sender<Request>,
    msize: RwLock<u32>,
}

/// Start a 9P2000 server on the given Unix socket path.
///
/// For each incoming connection, a new task is spawned to handle the 9P
/// protocol, forwarding filesystem operations to the pinhead router.
/// Start a 9P2000 server on a Unix socket.
///
/// Each connection runs a single 9P session (version → attach → ...).
/// The path is cleaned up on exit via [`SocketCleanup`].
pub async fn serve(
    router_tx: mpsc::Sender<Request>,
    socket_path: &str,
) -> std::io::Result<()> {
    // Remove stale socket.
    let _ = tokio::fs::remove_file(socket_path).await;
    let listener = UnixListener::bind(socket_path)?;
    eprintln!("[9p] listening on {socket_path}");

    // SocketCleanup runs Drop when this future is cancelled/aborted,
    // ensuring the socket file is removed on exit.
    let _cleanup = SocketCleanup {
        path: socket_path.to_string(),
    };

    let shared = Arc::new(Shared {
        state: Mutex::new(NinepState::new()),
        router_tx,
        msize: RwLock::new(8192),
    });

    loop {
        let (mut stream, _) = listener.accept().await?;
        let shared = shared.clone();
        tokio::spawn(async move {
            let buf = vec![0u8; 65536];
            loop {
                // Read 7-byte fixed header
                let mut header = [0u8; 7];
                match NinepStream::read_exact(&mut stream, &mut header).await {
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => {
                        eprintln!("[9p] read error: {e}");
                        break;
                    }
                }

                let size = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
                let msg_type = header[4];
                let tag = u16::from_le_bytes(header[5..7].try_into().unwrap());

                // Read the rest of the message body
                let body_len = size.saturating_sub(7);
                if body_len > buf.len() {
                    eprintln!("[9p] message too large: {body_len}");
                    break;
                }

                let mut body = vec![0u8; body_len];
                if body_len > 0
                    && let Err(e) = NinepStream::read_exact(&mut stream, &mut body).await {
                        eprintln!("[9p] read body error: {e}");
                        break;
                    }

                if let Err(e) = handle_message(&shared, &mut stream, msg_type, tag, &body).await {
                    eprintln!("[9p] handler error: {e}");
                    // Send Rlerror or just drop
                    send_error(&mut stream, tag, &e).await;
                }
            }
            let _ = NinepStream::shutdown(&mut stream).await;
        });
    }
}

// ── Message dispatch ──────────────────────────────────────────────────
async fn handle_message(
    shared: &Shared,
    stream: &mut impl NinepStream,
    msg_type: u8,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    match msg_type {
        TVERSION => handle_version(shared, stream, tag, body).await,
        TATTACH => handle_attach(shared, stream, tag, body).await,
        TWALK => handle_walk(shared, stream, tag, body).await,
        TOPEN => handle_open(shared, stream, tag, body).await,
        TREAD => handle_read(shared, stream, tag, body).await,
        TWRITE => handle_write(shared, stream, tag, body).await,
        TCLUNK => handle_clunk(shared, stream, tag, body).await,
        TCREATE => handle_create(shared, stream, tag, body).await,
        TREMOVE => handle_remove(shared, stream, tag, body).await,
        TSTAT => handle_stat(shared, stream, tag, body).await,
        TWSTAT => handle_wstat(shared, stream, tag, body).await,
        TFLUSH => handle_flush(stream, tag).await,
        _ => Err(format!("unknown message type: {msg_type}")),
    }
}

async fn send_reply(stream: &mut impl NinepStream, msg_type: u8, tag: u16, body: &[u8]) {
    let size = 7 + body.len();
    let mut buf = Vec::with_capacity(size);
    buf.extend_from_slice(&(size as u32).to_le_bytes());
    buf.push(msg_type);
    buf.extend_from_slice(&tag.to_le_bytes());
    buf.extend_from_slice(body);
    let _ = stream.write_all(&buf).await;
}

async fn send_error(stream: &mut impl NinepStream, tag: u16, err: &str) {
    let bytes = err.as_bytes();
    let len = bytes.len().min(65535) as u16;
    let mut body = Vec::with_capacity(2 + len as usize);
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(&bytes[..len as usize]);
    send_reply(stream, RERROR, tag, &body).await;
}

// ── Handlers ──────────────────────────────────────────────────────────
async fn handle_version(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: msize(u32), version(s)
    if body.len() < 4 {
        return Err("short version message".into());
    }
    let msize = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let _version = if body.len() > 4 {
        let vlen = u16::from_le_bytes(body[4..6].try_into().unwrap()) as usize;
        if 6 + vlen <= body.len() {
            String::from_utf8_lossy(&body[6..6 + vlen]).to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let msize = msize.clamp(4096, 65536);
    *shared.msize.write().await = msize;

    let mut reply = Vec::new();
    reply.extend_from_slice(&msize.to_le_bytes());
    // version string "9P2000"
    reply.extend_from_slice(&6u16.to_le_bytes());
    reply.extend_from_slice(b"9P2000");

    send_reply(stream, RVERSION, tag, &reply).await;
    Ok(())
}

async fn handle_attach(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), afid(u32), uname(s), aname(s)
    if body.len() < 8 {
        return Err("short attach message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let _afid = u32::from_le_bytes(body[4..8].try_into().unwrap());
    // Skip strings for now

    // Register root fid
    let mut state = shared.state.lock().await;
    state.set_fid(fid, 0, "/".into(), true);

    let qid = Qid {
        ty: 0x80, // QTDIR
        version: 0,
        path: 0,
    };
    let mut reply = Vec::new();
    encode_qid(&mut reply, qid);
    send_reply(stream, RATTACH, tag, &reply).await;
    Ok(())
}

async fn handle_walk(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), newfid(u32), nwname(u16), [wname(s) ...]
    if body.len() < 9 {
        return Err("short walk message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let new_fid = u32::from_le_bytes(body[4..8].try_into().unwrap());
    let nwname = u16::from_le_bytes(body[8..10].try_into().unwrap()) as usize;

    let parent_path;
    let parent_qid;
    let parent_is_dir;
    {
        let state = shared.state.lock().await;
        let parent = state.fids.get(&fid).ok_or("unknown fid")?.clone();
        parent_path = parent.path;
        parent_qid = parent.qid;
        parent_is_dir = parent.is_dir;
    }

    let mut cur_path = parent_path;
    let mut cur_qid = parent_qid;
    let mut wqids = Vec::new();
    let mut offset = 10;
    let mut is_dir = parent_is_dir;

    for i in 0..nwname {
        if offset + 2 > body.len() {
            return Err("short walk wname".into());
        }
        let nlen = u16::from_le_bytes(body[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
        if offset + nlen > body.len() {
            return Err("short walk wname data".into());
        }
        let child = String::from_utf8_lossy(&body[offset..offset + nlen]).to_string();
        offset += nlen;

        // Build the full path and route it
        let full_path = if cur_path.ends_with('/') {
            format!("{cur_path}{child}")
        } else {
            format!("{cur_path}/{child}")
        };

        // Only validate the final segment through the router — intermediate
        // segments may match parameterized routes (e.g. /user/{name}) and
        // aren't complete paths the router can resolve.
        let is_last = i + 1 == nwname;
        if is_last {
            // Send Lookup to router to verify path exists
            let (reply_tx, reply_rx) = oneshot::channel();
            let req = Request {
                op: FsOperation::Lookup,
                path: full_path.clone(),
                data: Bytes::new(),
                reply: reply_tx,
            };
            shared
                .router_tx
                .send(req)
                .await
                .map_err(|_| "router gone".to_string())?;
            reply_rx
                .await
                .map_err(|_| "handler gone".to_string())?
                .map_err(|e| format!("lookup failed: {e}"))?;
        }

        // Allocate qid
        is_dir = child.is_empty() || child == "." || child == "..";
        let mut state = shared.state.lock().await;
        cur_qid = state.alloc_qid(&full_path, is_dir);
        if is_last {
            eprintln!("[ninep walk] AFTER WALK paths={:?}",
                state.paths.iter().map(|(k,v)| (k, &v.path)).collect::<Vec<_>>());
        }
        cur_path = full_path;

        let qid_type = if is_dir { 0x80 } else { 0x00 };
        wqids.push(Qid {
            ty: qid_type,
            version: 0,
            path: cur_qid,
        });
    }

    {
        let mut state = shared.state.lock().await;
        state.set_fid(new_fid, cur_qid, cur_path, is_dir);
    }

    let mut reply = Vec::new();
    reply.extend_from_slice(&(nwname as u16).to_le_bytes()); // nwqid
    for q in &wqids {
        encode_qid(&mut reply, *q);
    }
    send_reply(stream, RWALK, tag, &reply).await;
    Ok(())
}

async fn handle_open(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), mode(u8)
    if body.len() < 5 {
        return Err("short open message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let _mode = body[4];

    let (path, is_dir) = {
        let state = shared.state.lock().await;
        let entry = state.fids.get(&fid).ok_or("unknown fid")?;
        (entry.path.clone(), entry.is_dir)
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let req = Request {
        op: if is_dir {
            FsOperation::OpenDir
        } else {
            FsOperation::Open
        },
        path,
        data: Bytes::new(),
        reply: reply_tx,
    };
    shared
        .router_tx
        .send(req)
        .await
        .map_err(|_| "router gone".to_string())?;
    reply_rx
        .await
        .map_err(|_| "handler gone".to_string())?
        .map_err(|e| format!("open failed: {e}"))?;

    let mut state = shared.state.lock().await;
    if let Some(entry) = state.fids.get_mut(&fid) {
        entry.open = true;
    }

    let msize = *shared.msize.read().await;
    let mut reply = Vec::new();
    encode_qid(
        &mut reply,
        Qid {
            ty: 0,
            version: 0,
            path: 0,
        },
    );
    reply.extend_from_slice(&msize.to_le_bytes()); // iounit
    send_reply(stream, ROPEN, tag, &reply).await;
    Ok(())
}

// ── Create handler ────────────────────────────────────────────────────

const DMDIR: u32 = 0x8000_0000;

async fn handle_create(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    if body.len() < 9 {
        return Err("short create message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let name_len = u16::from_le_bytes(body[4..6].try_into().unwrap()) as usize;
    if 6 + name_len + 5 > body.len() {
        return Err("short create name".into());
    }
    let name =
        String::from_utf8_lossy(&body[6..6 + name_len]).to_string();
    let perm = u32::from_le_bytes(
        body[6 + name_len..6 + name_len + 4]
            .try_into()
            .unwrap(),
    );
    let _mode = body[6 + name_len + 4];

    // Look up the parent path from the fid.
    let parent_path = {
        let state = shared.state.lock().await;
        state
            .fids
            .get(&fid)
            .ok_or("unknown fid")?
            .path
            .clone()
    };

    let is_dir = (perm & DMDIR) != 0;
    let full_path = if parent_path.ends_with('/') {
        format!("{parent_path}{name}")
    } else {
        format!("{parent_path}/{name}")
    };

    let op = if is_dir {
        FsOperation::MkDir
    } else {
        FsOperation::Create
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let req = Request {
        op,
        path: full_path.clone(),
        data: Bytes::new(),
        reply: reply_tx,
    };
    shared
        .router_tx
        .send(req)
        .await
        .map_err(|_| "router gone".to_string())?;
    reply_rx
        .await
        .map_err(|_| "handler gone".to_string())?
        .map_err(|e| format!("{} failed: {e}", op.as_str()))?;

    // Allocate qid for the new file / directory
    let mut state = shared.state.lock().await;
    let qid_path = state.alloc_qid(&full_path, is_dir);
    state.set_fid(fid, qid_path, full_path, is_dir);
    // Mark the fid as opened (like Topen does).
    if let Some(entry) = state.fids.get_mut(&fid) {
        entry.open = true;
    }
    drop(state);

    let mut reply = Vec::new();
    encode_qid(
        &mut reply,
        Qid {
            ty: if is_dir { 0x80 } else { 0x00 },
            version: 0,
            path: qid_path,
        },
    );
    send_reply(stream, RCREATE, tag, &reply).await;
    Ok(())
}

async fn handle_read(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), offset(u64), count(u32)
    if body.len() < 16 {
        return Err("short read message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let offset = u64::from_le_bytes(body[4..12].try_into().unwrap());
    let _count = u32::from_le_bytes(body[12..16].try_into().unwrap());

    let entry = {
        let state = shared.state.lock().await;
        state.fids.get(&fid).cloned().ok_or("unknown fid")?
    };

    if entry.is_dir {
        // ReadDir: send ReadDir to router
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            op: FsOperation::ReadDir,
            path: entry.path.clone(),
            data: Bytes::new(),
            reply: reply_tx,
        };
        shared
            .router_tx
            .send(req)
            .await
            .map_err(|_| "router gone".to_string())?;
        let _resp = reply_rx
            .await
            .map_err(|_| "handler gone".to_string())?
            .map_err(|e| format!("readdir failed: {e}"))?;

        // Build directory listing from known paths
        let state = shared.state.lock().await;
        let prefix = if entry.path.ends_with('/') {
            entry.path.clone()
        } else {
            format!("{}/", entry.path)
        };
        eprintln!("[ninep readdir] prefix={prefix:?} state paths={:?}",
            state.paths.iter().map(|(k,v)| (k, &v.path)).collect::<Vec<_>>());

        let mut data = Vec::new();
        for (&qid, pent) in &state.paths {
            if pent.path == entry.path {
                continue;
            }
            if !pent.path.starts_with(&prefix) {
                continue;
                // Make sure it's a direct child (no nested /)
            }
            let rest = &pent.path[prefix.len()..];
            if rest.contains('/') {
                continue;
            }

            let name = if rest.is_empty() { "." } else { rest };
            let qid = Qid {
                ty: if pent.is_dir { 0x80 } else { 0x00 },
                version: 0,
                path: qid,
            };
            let length = 0;
            encode_stat(&mut data, name, qid, length, pent.is_dir);
        }

        // Check if we need to add a "." entry
        if offset == 0 {
            // Add "." for root
            let qid = Qid {
                ty: 0x80,
                version: 0,
                path: 0,
            };
            let mut dir_data = Vec::new();
            encode_stat(&mut dir_data, ".", qid, 0, true);
            dir_data.extend_from_slice(&data);
            data = dir_data;
        }

        let offset_us = offset as usize;
        let chunk: Vec<u8> = if offset_us < data.len() {
            data[offset_us..].to_vec()
        } else {
            Vec::new()
        };

        let mut reply = Vec::new();
        reply.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        reply.extend_from_slice(&chunk);
        send_reply(stream, RREAD, tag, &reply).await;
    } else {
        // Regular file read: send Read to router
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            op: FsOperation::Read,
            path: entry.path.clone(),
            data: Bytes::new(),
            reply: reply_tx,
        };
        shared
            .router_tx
            .send(req)
            .await
            .map_err(|_| "router gone".to_string())?;
        let resp = reply_rx
            .await
            .map_err(|_| "handler gone".to_string())?
            .map_err(|e| format!("read failed: {e}"))?;

        let offset_us = offset as usize;
        let chunk: Vec<u8> = if offset_us < resp.data.len() {
            resp.data[offset_us..].to_vec()
        } else {
            Vec::new()
        };

        let mut reply = Vec::new();
        reply.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        reply.extend_from_slice(&chunk);
        send_reply(stream, RREAD, tag, &reply).await;
    }
    Ok(())
}

async fn handle_write(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), offset(u64), count(u32), data[...]
    if body.len() < 16 {
        return Err("short write message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let _offset = u64::from_le_bytes(body[4..12].try_into().unwrap());
    let count = u32::from_le_bytes(body[12..16].try_into().unwrap()) as usize;

    let entry = {
        let state = shared.state.lock().await;
        state.fids.get(&fid).cloned().ok_or("unknown fid")?
    };

    let data = Bytes::from(body[16..16 + count.min(body.len() - 16)].to_vec());
    let (reply_tx, reply_rx) = oneshot::channel();
    let req = Request {
        op: FsOperation::Write,
        path: entry.path,
        data,
        reply: reply_tx,
    };
    shared
        .router_tx
        .send(req)
        .await
        .map_err(|_| "router gone".to_string())?;
    let _resp = reply_rx
        .await
        .map_err(|_| "handler gone".to_string())?
        .map_err(|e| format!("write failed: {e}"))?;

    let mut reply = Vec::new();
    reply.extend_from_slice(&(count as u32).to_le_bytes());
    send_reply(stream, RWRITE, tag, &reply).await;
    Ok(())
}

async fn handle_clunk(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    if body.len() < 4 {
        return Err("short clunk message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());

    {
        let state = shared.state.lock().await;
        let path = state.fids.get(&fid).map(|e| e.path.clone());
        if let Some(path) = path {
            // Send Release to router
            drop(state);
            let (reply_tx, reply_rx) = oneshot::channel();
            let req = Request {
                op: FsOperation::Release,
                path,
                data: Bytes::new(),
                reply: reply_tx,
            };
            if shared.router_tx.send(req).await.is_ok() {
                let _ = reply_rx.await;
            }
        }
    }

    let mut state = shared.state.lock().await;
    state.fids.remove(&fid);

    send_reply(stream, RCLUNK, tag, &[]).await;
    Ok(())
}

async fn handle_remove(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    if body.len() < 4 {
        return Err("short remove message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());

    let (path, is_dir) = {
        let state = shared.state.lock().await;
        let entry = state.fids.get(&fid).ok_or("unknown fid")?;
        (entry.path.clone(), entry.is_dir)
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let req = Request {
        op: if is_dir {
            FsOperation::RmDir
        } else {
            FsOperation::Unlink
        },
        path,
        data: Bytes::new(),
        reply: reply_tx,
    };
    shared
        .router_tx
        .send(req)
        .await
        .map_err(|_| "router gone".to_string())?;
    reply_rx
        .await
        .map_err(|_| "handler gone".to_string())?
        .map_err(|e| format!("remove failed: {e}"))?;

    send_reply(stream, RREMOVE, tag, &[]).await;
    Ok(())
}

async fn handle_stat(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    if body.len() < 4 {
        return Err("short stat message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());

    let entry = {
        let state = shared.state.lock().await;
        state.fids.get(&fid).cloned().ok_or("unknown fid")?
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let req = Request {
        op: FsOperation::GetAttr,
        path: entry.path.clone(),
        data: Bytes::new(),
        reply: reply_tx,
    };
    shared
        .router_tx
        .send(req)
        .await
        .map_err(|_| "router gone".to_string())?;
    let resp = reply_rx
        .await
        .map_err(|_| "handler gone".to_string())?
        .map_err(|e| format!("getattr failed: {e}"))?;

    let qid = Qid {
        ty: if entry.is_dir { 0x80 } else { 0x00 },
        version: 0,
        path: entry.qid,
    };

    let name = entry
        .path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("/");

    let mut stat_buf = Vec::new();
    encode_stat(&mut stat_buf, name, qid, resp.data.len() as u64, entry.is_dir);

    let mut reply = Vec::new();
    reply.extend_from_slice(&(stat_buf.len() as u16).to_le_bytes());
    reply.extend_from_slice(&stat_buf);
    send_reply(stream, RSTAT, tag, &reply).await;
    Ok(())
}

async fn handle_flush(stream: &mut impl NinepStream, tag: u16) -> Result<(), String> {
    send_reply(stream, RFLUSH, tag, &[]).await;
    Ok(())
}

// ── WStat handler (SetAttr / Rename) ─────────────────────────────────

async fn handle_wstat(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    if body.len() < 4 {
        return Err("short wstat message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());

    let (path, qid) = {
        let state = shared.state.lock().await;
        let entry = state.fids.get(&fid).cloned().ok_or("unknown fid")?;
        (entry.path, entry.qid)
    };

    // Parse the stat data: size[2] data[size-2]
    let stat_data = &body[4..];
    if stat_data.len() < 2 {
        return Err("short wstat data".into());
    }
    let stat_len = u16::from_le_bytes(stat_data[0..2].try_into().unwrap()) as usize;
    let stat_data = &stat_data[2..2 + stat_len.min(stat_data.len() - 2)];

    // Skip type(2) + dev(4)
    if stat_data.len() < 6 {
        send_reply(stream, RWSTAT, tag, &[]).await;
        return Ok(());
    }
    let mut off = 6usize;

    // qid(13) — WStat typically sends a zero qid or the file's qid; we ignore it.
    off = off.saturating_add(13);

    // mode(4)
    let has_mode = stat_data.len() >= off + 4;
    if has_mode {
        off += 4;
    }

    // atime(4) + mtime(4) with old value all-1s check
    let has_time = stat_data.len() >= off + 8;
    if has_time {
        off += 8;
    }

    // length(8) — non-zero = truncate
    let has_length = stat_data.len() >= off + 8;
    if has_length {
        off += 8;
    }

    // name(2+n) — non-empty = rename
    let mut new_name: Option<String> = None;
    if stat_data.len() >= off + 2 {
        let name_len = u16::from_le_bytes(stat_data[off..off + 2].try_into().unwrap()) as usize;
        off += 2;
        if name_len > 0 && off + name_len <= stat_data.len() {
            let name_str = String::from_utf8_lossy(&stat_data[off..off + name_len]).to_string();
            let current_name = path.rsplit('/').next().unwrap_or("");
            if name_str != current_name && !name_str.is_empty() {
                new_name = Some(name_str);
            }
        }
    }

    // Dispatch
    if let Some(ref name) = new_name {
        // Rename
        let new_path = if path == "/" {
            format!("/{name}")
        } else {
            let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("/");
            if parent == "/" {
                format!("/{name}")
            } else {
                format!("{parent}/{name}")
            }
        };
        let data = Bytes::from(new_path.clone());
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            op: FsOperation::Rename,
            path: path.clone(),
            data,
            reply: reply_tx,
        };
        shared
            .router_tx
            .send(req)
            .await
            .map_err(|_| "router gone")?;
        reply_rx
            .await
            .map_err(|_| "handler gone")?
            .map_err(|e| format!("rename failed: {e}"))?;

        // Update the path in the ninep state
        let mut state = shared.state.lock().await;
        if let Some(entry) = state.fids.get_mut(&fid) {
            entry.path = new_path.clone();
        }
        state.paths.insert(qid, PathEntry {
            path: new_path,
            is_dir: false,
        });
    } else if has_mode || has_time || has_length {
        // SetAttr (mode, time, or truncate)
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            op: FsOperation::SetAttr,
            path: path.clone(),
            data: Bytes::new(),
            reply: reply_tx,
        };
        shared
            .router_tx
            .send(req)
            .await
            .map_err(|_| "router gone")?;
        reply_rx
            .await
            .map_err(|_| "handler gone")?
            .map_err(|e| format!("setattr failed: {e}"))?;
    }

    send_reply(stream, RWSTAT, tag, &[]).await;
    Ok(())
}

// ── TCP 9P2000 server ─────────────────────────────────────────────────

/// Start a 9P2000 server on the given TCP address.
///
/// Each incoming connection is handled in its own task with the same
/// protocol as the Unix socket variant.
/// Start a 9P2000 server over TCP with the frame-based mux.
///
/// `max_conns` limits concurrent connections (None = unlimited).
/// Beyond the limit, new connections queue in the accept loop.
pub async fn serve_tcp(
    router_tx: mpsc::Sender<Request>,
    addr: &str,
    max_conns: Option<usize>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    eprintln!("[9p-tcp] listening on {addr}");

    let shared = Arc::new(Shared {
        state: Mutex::new(NinepState::new()),
        router_tx,
        msize: RwLock::new(8192),
    });

    let sem = max_conns.map(|n| StdArc::new(tokio::sync::Semaphore::new(n)));

    loop {
        let (stream, peer) = listener.accept().await?;
        eprintln!("[9p-tcp] connection from {peer}");

        // Acquire permit (queues if at max_conns).  Unwrap is safe:
        // the semaphore is never closed.
        let permit = match sem.clone() {
            Some(s) => Some(s.acquire_owned().await.unwrap()),
            None => None,
        };

        let shared = shared.clone();
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<(u32, Vec<u8>)>();
        let (frame_tx, frame_rx) = mpsc::unbounded_channel::<(u32, Vec<u8>)>();
        let writer = MuxWriter { tx: frame_tx };

        tokio::spawn(async move {
            run_server_mux(stream, stream_tx, frame_rx).await;
            drop(permit);
        });

        let mut streams: HashMap<u32, mpsc::UnboundedSender<Vec<u8>>> = HashMap::new();
        while let Some((stream_id, payload)) = stream_rx.recv().await {
            if let Some(tx) = streams.get(&stream_id) {
                let _ = tx.send(payload);
            } else {
                let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
                let _ = tx.send(payload);
                streams.insert(stream_id, tx);
                let writer = writer.clone();
                let mux = MuxStream { writer, stream_id, rx, buf: Vec::new(), pos: 0 };
                let s = shared.clone();
                tokio::spawn(async move {
                    run_connection(mux, s).await;
                });
            }
        }
    }
}

// ── TLS 9P2000 server ──────────────────────────────────────────────────

/// Start a 9P2000 server over TCP with TLS encryption + mux.
///
/// PEM file must contain the server certificate chain followed by the
/// private key (in two PEM blocks).  Compatible with standard TLS 1.2+.
pub async fn serve_tcp_tls(
    router_tx: mpsc::Sender<Request>,
    addr: &str,
    cert_path: &str,
    key_path: &str,
) -> std::io::Result<()> {
    use rustls_pemfile::{certs, private_key};

    let cert_file = &mut std::io::BufReader::new(
        std::fs::File::open(cert_path).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::NotFound, format!("cert {cert_path}: {e}"))
        })?,
    );
    let key_file = &mut std::io::BufReader::new(
        std::fs::File::open(key_path).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::NotFound, format!("key {key_path}: {e}"))
        })?,
    );

    let cert_chain: Vec<CertificateDer> = certs(cert_file)
        .filter_map(|r| r.ok())
        .collect();
    let priv_key = private_key(key_file)
        .ok()
        .flatten()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no private key found"))?;

    let tls_config = StdArc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, priv_key)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?,
    );
    let acceptor = TlsAcceptor::from(tls_config);

    let listener = TcpListener::bind(addr).await?;
    eprintln!("[9p-tls] listening on {addr}");

    let shared = StdArc::new(Shared {
        state: Mutex::new(NinepState::new()),
        router_tx,
        msize: RwLock::new(8192),
    });

    loop {
        let (stream, peer) = listener.accept().await?;
        eprintln!("[9p-tls] connection from {peer}");
        let shared = shared.clone();
        let acceptor = acceptor.clone();

        tokio::spawn(async move {
            // TLS handshake, then mux over TLS (same as TCP mux).
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[9p-tls] handshake error: {e}");
                    return;
                }
            };

            let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<(u32, Vec<u8>)>();
            let (frame_tx, frame_rx) = mpsc::unbounded_channel::<(u32, Vec<u8>)>();
            let writer = MuxWriter { tx: frame_tx };

            tokio::spawn(async move {
                run_server_mux(tls_stream, stream_tx, frame_rx).await;
            });

            let mut streams: HashMap<u32, mpsc::UnboundedSender<Vec<u8>>> = HashMap::new();
            while let Some((stream_id, payload)) = stream_rx.recv().await {
                if let Some(tx) = streams.get(&stream_id) {
                    let _ = tx.send(payload);
                } else {
                    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
                    let _ = tx.send(payload);
                    streams.insert(stream_id, tx);
                    let writer = writer.clone();
                    let mux = MuxStream { writer, stream_id, rx, buf: Vec::new(), pos: 0 };
                    let s = shared.clone();
                    tokio::spawn(async move {
                        run_connection(mux, s).await;
                    });
                }
            }
        });
    }
}

// ── UDP 9P2000 server ─────────────────────────────────────────────────

/// Start a 9P2000 server over UDP.
///
/// Each datagram is treated as a complete 9P message and the response is
/// sent back to the same peer address.  This is non-standard but useful
/// for lightweight / embedded scenarios.
pub async fn serve_udp(
    router_tx: mpsc::Sender<Request>,
    addr: &str,
) -> std::io::Result<()> {
    let socket = tokio::net::UdpSocket::bind(addr).await?;
    eprintln!("[9p-udp] listening on {addr}");

    let shared = Arc::new(Shared {
        state: Mutex::new(NinepState::new()),
        router_tx,
        msize: RwLock::new(65536),
    });

    let mut buf = vec![0u8; 65536];
    loop {
        let (len, peer) = socket.recv_from(&mut buf).await?;
        let data = buf[..len].to_vec();
        let shared = shared.clone();

        // Process the message and send the reply back to the same peer.
        let reply = handle_udp_message(&shared, &data).await;
        if let Some(reply) = reply {
            let _ = socket.send_to(&reply, &peer).await;
        }
    }
}

/// Process a single 9P message for UDP (no persistent connection).
async fn handle_udp_message(
    shared: &Shared,
    data: &[u8],
) -> Option<Vec<u8>> {
    if data.len() < 7 {
        return None;
    }
    let msg_type = data[4];
    let tag = u16::from_le_bytes(data[5..7].try_into().unwrap());

    // Build a virtual stream wrapper to capture the reply bytes.
    let mut reply_buf = Vec::new();
    let mut virt = VirtualStream { buf: &mut reply_buf };

    let _ = match msg_type {
        TVERSION => handle_version(shared, &mut virt, tag, &data[7..]).await,
        TATTACH => handle_attach(shared, &mut virt, tag, &data[7..]).await,
        TWALK => handle_walk(shared, &mut virt, tag, &data[7..]).await,
        TOPEN => handle_open(shared, &mut virt, tag, &data[7..]).await,
        TREAD => handle_read(shared, &mut virt, tag, &data[7..]).await,
        TWRITE => handle_write(shared, &mut virt, tag, &data[7..]).await,
        TCLUNK => handle_clunk(shared, &mut virt, tag, &data[7..]).await,
        TCREATE => handle_create(shared, &mut virt, tag, &data[7..]).await,
        TREMOVE => handle_remove(shared, &mut virt, tag, &data[7..]).await,
        TSTAT => handle_stat(shared, &mut virt, tag, &data[7..]).await,
        TWSTAT => handle_wstat(shared, &mut virt, tag, &data[7..]).await,
        TFLUSH => handle_flush(&mut virt, tag).await,
        _ => {
            // Send Rlerror for unknown message types.
            send_error(&mut virt, tag, &format!("unknown msg type {msg_type}")).await;
            Ok(())
        }
    };

    Some(reply_buf)
}

// ── Connection runner (shared between Unix and TCP) ───────────────────

async fn run_connection(stream: impl NinepStream, shared: Arc<Shared>) {
    let mut stream = stream;
    let buf = vec![0u8; 65536];
    loop {
        let mut header = [0u8; 7];
        match stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                eprintln!("[9p] read error: {e}");
                break;
            }
        }

        let size = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let msg_type = header[4];
        let tag = u16::from_le_bytes(header[5..7].try_into().unwrap());

        let body_len = size.saturating_sub(7);
        if body_len > buf.len() {
            eprintln!("[9p] message too large: {body_len}");
            break;
        }

        let mut body = vec![0u8; body_len];
        if body_len > 0
            && let Err(e) = stream.read_exact(&mut body).await {
                eprintln!("[9p] read body error: {e}");
                break;
            }

        if let Err(e) = handle_message(&shared, &mut stream, msg_type, tag, &body).await {
            eprintln!("[9p] handler error: {e}");
            send_error(&mut stream, tag, &e).await;
        }
    }
    let _ = stream.shutdown().await;
}

// ── Stream abstraction ────────────────────────────────────────────────

/// Trait abstracting over UnixStream and TcpStream for 9P message I/O.
trait NinepStream: Send {
    fn read_exact<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
    fn shutdown(&mut self) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
}

impl NinepStream for UnixStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        AsyncReadExt::read_exact(self, buf).await?;
        Ok(())
    }
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        AsyncWriteExt::write_all(self, buf).await
    }
    async fn shutdown(&mut self) -> std::io::Result<()> {
        AsyncWriteExt::shutdown(self).await
    }
}

impl NinepStream for TcpStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        AsyncReadExt::read_exact(self, buf).await?;
        Ok(())
    }
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        AsyncWriteExt::write_all(self, buf).await
    }
    async fn shutdown(&mut self) -> std::io::Result<()> {
        AsyncWriteExt::shutdown(self).await
    }
}

impl NinepStream for tokio_rustls::server::TlsStream<TcpStream> {
    async fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        AsyncReadExt::read_exact(self, buf).await?;
        Ok(())
    }
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        AsyncWriteExt::write_all(self, buf).await
    }
    async fn shutdown(&mut self) -> std::io::Result<()> {
        AsyncWriteExt::shutdown(self).await
    }
}

/// Virtual stream that captures written bytes into a Vec (for UDP replies).
struct VirtualStream<'a> {
    buf: &'a mut Vec<u8>,
}

impl NinepStream for VirtualStream<'_> {
    async fn read_exact(&mut self, _buf: &mut [u8]) -> std::io::Result<()> {
        // Reads don't apply to virtual streams used for UDP replies.
        Ok(())
    }
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.buf.extend_from_slice(buf);
        Ok(())
    }
    async fn shutdown(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// We need RERROR message type for error replies
const RERROR: u8 = 107;

// ── 9P multiplexer (mux) — frame-based stream multiplexing for TCP ────
//
// Wire format: [stream_id:4][payload_len:4][payload...]  (all LE)
// Each stream is a single request/response pair.

/// Handle for sending frames to a mux connection's writer task.
#[derive(Clone)]
pub(crate) struct MuxWriter {
    tx: mpsc::UnboundedSender<(u32, Vec<u8>)>,
}

impl MuxWriter {
    fn send(&self, stream_id: u32, payload: Vec<u8>) -> Result<(), String> {
        self.tx
            .send((stream_id, payload))
            .map_err(|_| "mux writer gone".to_string())
    }
}

/// Build a mux frame: [stream_id:4][payload_len:4][payload...] (all LE).
pub fn encode_mux_frame(stream_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + payload.len());
    buf.extend_from_slice(&stream_id.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}


/// Parse a mux frame header, returning (stream_id, payload_len).
pub fn decode_mux_header(header: &[u8]) -> (u32, usize) {
    let stream_id = u32::from_le_bytes(header[0..4].try_into().unwrap());
    let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    (stream_id, len)
}

/// A virtual stream backed by the mux protocol.  Reads dequeue from a
/// channel fed by the mux reader, writes send frames via the mux writer.
struct MuxStream {
    writer: MuxWriter,
    stream_id: u32,
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl NinepStream for MuxStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        let mut need = buf.len();
        let mut offset = 0;
        while need > 0 {
            // Refill from channel if buffer is exhausted.
            if self.pos >= self.buf.len() {
                match self.rx.recv().await {
                    Some(data) => { self.buf = data; self.pos = 0; }
                    None => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "mux stream closed")),
                }
            }
            let avail = (self.buf.len() - self.pos).min(need);
            buf[offset..offset + avail].copy_from_slice(&self.buf[self.pos..self.pos + avail]);
            self.pos += avail;
            offset += avail;
            need -= avail;
        }
        Ok(())
    }
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.writer
            .send(self.stream_id, buf.to_vec())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    }
    async fn shutdown(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run a muxed connection.  Reads frames from `frame_rx` and
/// delivers streams through `stream_tx`.  Generic over any
/// AsyncRead+AsyncWrite stream (e.g. TcpStream, TlsStream).
pub(crate) async fn run_server_mux<S>(
    stream: S,
    stream_tx: mpsc::UnboundedSender<(u32, Vec<u8>)>,
    mut frame_rx: mpsc::UnboundedReceiver<(u32, Vec<u8>)>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let (reader, mut tcp_writer) = tokio::io::split(stream);

    tokio::spawn(async move {
        while let Some((stream_id, payload)) = frame_rx.recv().await {
            let frame = encode_mux_frame(stream_id, &payload);
            if tcp_writer.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    let mut reader = reader;
    loop {
        let mut header = [0u8; 8];
        if reader.read_exact(&mut header).await.is_err() {
            break;
        }
        let (stream_id, len) = decode_mux_header(&header);
        let mut payload = vec![0u8; len];
        if len > 0 && reader.read_exact(&mut payload).await.is_err() {
            break;
        }
        let _ = stream_tx.send((stream_id, payload));
    }
    drop(stream_tx);
}
